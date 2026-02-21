//! Tauri IPC-команды для захвата экрана (Этапы 2–3).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::algorithm::{auto_zoom, cursor_smoothing};
use crate::capture::recorder::{
    get_monitor_scale_factor, get_monitor_size, spawn_ffmpeg, start_capture,
};
use crate::capture::state::{ActiveRecording, RecorderState};
use crate::models::events::{EventsFile, InputEvent, SCHEMA_VERSION as EVENTS_VERSION};
use crate::models::project::{
    Project, ProjectSettings, Timeline, SCHEMA_VERSION as PROJECT_VERSION,
};
use crate::telemetry::logger::{self, TelemetryState};

// ─── IPC команды ──────────────────────────────────────────────────────────────

/// Запускает запись экрана указанного монитора.
/// Возвращает уникальный ID записи, который потребуется при вызове `stop_recording`.
#[tauri::command]
pub async fn start_recording(
    state: tauri::State<'_, RecorderState>,
    telemetry: tauri::State<'_, TelemetryState>,
    monitor_index: u32,
) -> Result<String, String> {
    let mut guard = state.0.lock().await;

    if guard.is_some() {
        return Err("Recording already in progress".to_string());
    }

    // Генерируем ID и создаём директорию проекта.
    let recording_id = uuid::Uuid::new_v4().to_string();
    let output_dir = project_dir(&recording_id)?;
    std::fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Failed to create output directory: {e}"))?;

    log::info!(
        "start_recording: id={recording_id} dir={}",
        output_dir.display()
    );

    // Определяем физическое разрешение монитора.
    let (width, height) = get_monitor_size(monitor_index)?;
    let scale_factor = get_monitor_scale_factor(monitor_index).unwrap_or_else(|err| {
        log::warn!("start_recording: failed to resolve monitor scale factor: {err}");
        1.0
    });
    log::info!("start_recording: monitor={monitor_index} resolution={width}x{height}");

    // Запускаем FFmpeg.
    let raw_mp4 = output_dir.join("raw.mp4");
    let (ffmpeg_process, ffmpeg_stdin) = spawn_ffmpeg(width, height, &raw_mp4)?;

    // Флаг остановки: разделяется между командой и обработчиком кадров.
    let stop_flag = Arc::new(AtomicBool::new(false));

    // Запускаем поток WGC-захвата.
    let capture_thread = start_capture(monitor_index, stop_flag.clone(), ffmpeg_stdin)?;

    let start_ms = chrono::Utc::now().timestamp_millis() as u64;

    // Запускаем телеметрию (Этап 3).
    let telemetry_processor = logger::start_session(&telemetry.0, start_ms);

    *guard = Some(ActiveRecording {
        recording_id: recording_id.clone(),
        stop_flag,
        capture_thread,
        ffmpeg_process,
        output_dir,
        width,
        height,
        scale_factor,
        start_ms,
        telemetry_processor,
    });

    Ok(recording_id)
}

/// Останавливает запись, ждёт завершения FFmpeg, сохраняет project.json и events.json.
#[tauri::command]
pub async fn stop_recording(
    state: tauri::State<'_, RecorderState>,
    telemetry: tauri::State<'_, TelemetryState>,
    recording_id: String,
) -> Result<(), String> {
    // Забираем сессию из стейта.
    let rec = state.0.lock().await.take().ok_or("No active recording")?;

    if rec.recording_id != recording_id {
        let active_id = rec.recording_id.clone();
        *state.0.lock().await = Some(rec);
        return Err(format!(
            "Recording ID mismatch: active={active_id}, requested={recording_id}"
        ));
    }

    log::info!("stop_recording: id={recording_id}");

    // Сигнализируем WGC-обработчику остановиться.
    rec.stop_flag.store(true, Ordering::Relaxed);

    // Сигнализируем телеметрии завершить сессию.
    logger::stop_session(&telemetry.0);

    let output_dir = rec.output_dir.clone();
    let width = rec.width;
    let height = rec.height;
    let scale_factor = rec.scale_factor;
    let start_ms = rec.start_ms;

    // Финализация в блокирующем потоке (join + FFmpeg wait + telemetry join — блокирующие операции).
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        // Ждём завершения потока WGC-захвата.
        // Когда поток завершится, ScreenRecorder дропнется → ffmpeg_stdin закрывается.
        match rec.capture_thread.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => log::warn!("Capture thread finished with error: {e}"),
            Err(_) => log::error!("Capture thread panicked"),
        }

        // Ждём, пока FFmpeg завершит кодирование.
        let mut child = rec.ffmpeg_process;
        match child.wait() {
            Ok(status) if status.success() => {
                log::info!("FFmpeg finished OK");
            }
            Ok(status) => {
                return Err(format!("FFmpeg exited with non-zero status: {status}"));
            }
            Err(e) => {
                return Err(format!("Failed to wait for FFmpeg process: {e}"));
            }
        }

        // Ждём завершения процессора телеметрии и получаем события.
        let telemetry_events = rec.telemetry_processor.join().unwrap_or_default();
        log::info!(
            "stop_recording: collected {} telemetry events",
            telemetry_events.len()
        );

        // Рассчитываем длительность.
        let end_ms = chrono::Utc::now().timestamp_millis() as u64;
        let duration_ms = end_ms.saturating_sub(start_ms);

        // Сохраняем project.json и events.json.
        save_recording_files(
            &output_dir,
            &recording_id,
            width,
            height,
            scale_factor,
            start_ms,
            duration_ms,
            telemetry_events,
        )?;

        log::info!(
            "stop_recording: saved project, duration={}ms, path={}",
            duration_ms,
            output_dir.display()
        );

        Ok(())
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Путь к директории проекта: `{Videos}/NeuroScreenCaster/{id}/`.
fn project_dir(recording_id: &str) -> Result<std::path::PathBuf, String> {
    let base = dirs::video_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Videos")))
        .ok_or("Failed to resolve Videos directory")?;

    Ok(base.join("NeuroScreenCaster").join(recording_id))
}

/// Сохраняет `project.json` и `events.json` в директорию проекта.
fn save_recording_files(
    output_dir: &std::path::Path,
    recording_id: &str,
    width: u32,
    height: u32,
    scale_factor: f64,
    start_ms: u64,
    duration_ms: u64,
    events: Vec<InputEvent>,
) -> Result<(), String> {
    let settings = ProjectSettings::default();
    let zoom_segments = auto_zoom::build_auto_zoom_segments(&events, width, height, duration_ms);
    let smoothed_cursor_path =
        cursor_smoothing::smooth_cursor_path(&events, settings.cursor.smoothing_factor);

    log::info!(
        "save_recording_files: auto_zoom_segments={} smoothed_cursor_points={}",
        zoom_segments.len(),
        smoothed_cursor_path.len()
    );
    // ── project.json ──────────────────────────────────────────────────────────
    let project = Project {
        schema_version: PROJECT_VERSION,
        id: recording_id.to_string(),
        name: format_recording_name(start_ms),
        created_at: start_ms,
        video_path: "raw.mp4".to_string(),
        events_path: "events.json".to_string(),
        duration_ms,
        video_width: width,
        video_height: height,
        timeline: Timeline { zoom_segments },
        settings,
    };

    let project_json = serde_json::to_string_pretty(&project)
        .map_err(|e| format!("Failed to serialize project.json: {e}"))?;
    std::fs::write(output_dir.join("project.json"), project_json)
        .map_err(|e| format!("Failed to write project.json: {e}"))?;

    // ── events.json ───────────────────────────────────────────────────────────
    let events_file = EventsFile {
        schema_version: EVENTS_VERSION,
        recording_id: recording_id.to_string(),
        start_time_ms: start_ms,
        screen_width: width,
        screen_height: height,
        scale_factor,
        events,
    };

    let events_json = serde_json::to_string_pretty(&events_file)
        .map_err(|e| format!("Failed to serialize events.json: {e}"))?;
    std::fs::write(output_dir.join("events.json"), events_json)
        .map_err(|e| format!("Failed to write events.json: {e}"))?;

    Ok(())
}

/// Форматирует человекочитаемое имя записи из Unix timestamp (мс).
fn format_recording_name(start_ms: u64) -> String {
    use chrono::{TimeZone, Utc};
    let dt = Utc
        .timestamp_millis_opt(start_ms as i64)
        .single()
        .unwrap_or_else(Utc::now);
    format!("Recording {}", dt.format("%Y-%m-%d %H:%M:%S"))
}

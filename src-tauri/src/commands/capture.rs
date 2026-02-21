//! Tauri IPC commands for recording.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::algorithm::{camera_engine, cursor_smoothing};
use crate::capture::recorder::{
    find_ffmpeg_exe, get_monitor_scale_factor, get_monitor_size, start_capture,
};
use crate::capture::state::{ActiveRecording, RecorderState};
use crate::models::events::{EventsFile, InputEvent, SCHEMA_VERSION as EVENTS_VERSION};
use crate::models::project::{
    Project, ProjectSettings, Timeline, SCHEMA_VERSION as PROJECT_VERSION,
};
use crate::telemetry::logger::{self, TelemetryState};

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

    let recording_id = uuid::Uuid::new_v4().to_string();
    let output_dir = project_dir(&recording_id)?;
    std::fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Failed to create output directory: {e}"))?;

    log::info!(
        "start_recording: id={recording_id} dir={}",
        output_dir.display()
    );

    let (width, height) = get_monitor_size(monitor_index)?;
    let scale_factor = get_monitor_scale_factor(monitor_index).unwrap_or_else(|err| {
        log::warn!("start_recording: failed to resolve monitor scale factor: {err}");
        1.0
    });
    log::info!("start_recording: monitor={monitor_index} resolution={width}x{height}");

    let raw_mp4 = output_dir.join("raw.mp4");
    let stop_flag = Arc::new(AtomicBool::new(false));
    let capture_thread = start_capture(monitor_index, stop_flag.clone(), raw_mp4, width, height)?;

    let start_ms = chrono::Utc::now().timestamp_millis() as u64;
    let telemetry_processor = logger::start_session(&telemetry.0, start_ms);

    *guard = Some(ActiveRecording {
        recording_id: recording_id.clone(),
        stop_flag,
        capture_thread,
        output_dir,
        width,
        height,
        scale_factor,
        start_ms,
        telemetry_processor,
    });

    Ok(recording_id)
}

#[tauri::command]
pub async fn stop_recording(
    state: tauri::State<'_, RecorderState>,
    telemetry: tauri::State<'_, TelemetryState>,
    recording_id: String,
) -> Result<(), String> {
    let rec = state.0.lock().await.take().ok_or("No active recording")?;

    if rec.recording_id != recording_id {
        let active_id = rec.recording_id.clone();
        *state.0.lock().await = Some(rec);
        return Err(format!(
            "Recording ID mismatch: active={active_id}, requested={recording_id}"
        ));
    }

    log::info!("stop_recording: id={recording_id}");

    rec.stop_flag.store(true, Ordering::Relaxed);
    logger::stop_session(&telemetry.0);

    let output_dir = rec.output_dir.clone();
    let width = rec.width;
    let height = rec.height;
    let scale_factor = rec.scale_factor;
    let start_ms = rec.start_ms;

    tokio::task::spawn_blocking(move || -> Result<(), String> {
        match rec.capture_thread.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => log::warn!("Capture thread finished with error: {e}"),
            Err(_) => log::error!("Capture thread panicked"),
        }

        let telemetry_events = rec.telemetry_processor.join().unwrap_or_default();
        log::info!(
            "stop_recording: collected {} telemetry events",
            telemetry_events.len()
        );

        let end_ms = chrono::Utc::now().timestamp_millis() as u64;
        let duration_ms = end_ms.saturating_sub(start_ms);

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

/// Path to project directory: `{Videos}/NeuroScreenCaster/{id}/`.
fn project_dir(recording_id: &str) -> Result<std::path::PathBuf, String> {
    let base = dirs::video_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Videos")))
        .ok_or("Failed to resolve Videos directory")?;

    Ok(base.join("NeuroScreenCaster").join(recording_id))
}

/// Writes `project.json` and `events.json` into project directory.
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
    let output_aspect_ratio = settings.export.width as f64 / settings.export.height.max(1) as f64;
    let camera_config = camera_engine::SmartCameraConfig::default();
    let zoom_segments = camera_engine::build_smart_camera_segments(
        &events,
        width,
        height,
        duration_ms,
        output_aspect_ratio,
        &camera_config,
    );
    let smoothed_cursor_path =
        cursor_smoothing::smooth_cursor_path(&events, settings.cursor.smoothing_factor);
    let proxy_video_path = match build_editor_proxy(output_dir) {
        Ok(path) => path,
        Err(err) => {
            log::warn!("save_recording_files: failed to build proxy video: {err}");
            None
        }
    };

    log::info!(
        "save_recording_files: auto_zoom_segments={} smoothed_cursor_points={} proxy={}",
        zoom_segments.len(),
        smoothed_cursor_path.len(),
        proxy_video_path.as_deref().unwrap_or("none")
    );

    let project = Project {
        schema_version: PROJECT_VERSION,
        id: recording_id.to_string(),
        name: format_recording_name(start_ms),
        created_at: start_ms,
        video_path: "raw.mp4".to_string(),
        proxy_video_path,
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

fn build_editor_proxy(output_dir: &std::path::Path) -> Result<Option<String>, String> {
    let source = output_dir.join("raw.mp4");
    if !source.exists() {
        return Ok(None);
    }

    let ffmpeg = find_ffmpeg_exe();
    let proxy_name = "proxy-edit.mp4";
    let proxy_path = output_dir.join(proxy_name);

    let status = std::process::Command::new(&ffmpeg)
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(&source)
        .arg("-an")
        .arg("-c:v")
        .arg("libx264")
        .arg("-preset")
        .arg("veryfast")
        .arg("-crf")
        .arg("17")
        .arg("-g")
        .arg("15")
        .arg("-keyint_min")
        .arg("15")
        .arg("-sc_threshold")
        .arg("0")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-movflags")
        .arg("+faststart")
        .arg(&proxy_path)
        .status()
        .map_err(|e| format!("Failed to run ffmpeg ({}) for proxy: {e}", ffmpeg.display()))?;

    if !status.success() {
        return Ok(None);
    }

    Ok(Some(proxy_name.to_string()))
}

fn format_recording_name(start_ms: u64) -> String {
    use chrono::{TimeZone, Utc};

    let dt = Utc
        .timestamp_millis_opt(start_ms as i64)
        .single()
        .unwrap_or_else(Utc::now);
    format!("Recording {}", dt.format("%Y-%m-%d %H:%M:%S"))
}

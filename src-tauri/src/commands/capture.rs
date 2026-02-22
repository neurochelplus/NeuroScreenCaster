//! Tauri IPC commands for recording.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::algorithm::{camera_engine, cursor_smoothing};
use crate::capture::preview::{NativePreviewFrame, NativePreviewState};
use crate::capture::recorder::RecordingQuality;
use crate::capture::recorder::{
    find_ffmpeg_exe, get_monitor_scale_factor, get_monitor_size, start_capture, DEFAULT_TARGET_FPS,
};
use crate::capture::state::{ActiveRecording, AutoZoomTriggerMode, RecorderState};
use crate::models::events::{EventsFile, InputEvent, SCHEMA_VERSION as EVENTS_VERSION};
use crate::models::project::{
    Project, ProjectSettings, Timeline, SCHEMA_VERSION as PROJECT_VERSION,
};
use crate::telemetry::logger::{self, TelemetryState};
use serde::Deserialize;
#[cfg(target_os = "windows")]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_LCONTROL, VK_RCONTROL,
};

#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
enum RecordingQualityOption {
    Low,
    #[default]
    Balanced,
    High,
}

impl RecordingQualityOption {
    fn as_recorder_quality(self) -> RecordingQuality {
        match self {
            RecordingQualityOption::Low => RecordingQuality::Low,
            RecordingQualityOption::Balanced => RecordingQuality::Balanced,
            RecordingQualityOption::High => RecordingQuality::High,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StartRecordingOptions {
    auto_zoom_trigger_mode: Option<AutoZoomTriggerMode>,
    quality: Option<RecordingQualityOption>,
    target_fps: Option<u32>,
}

#[tauri::command]
pub async fn start_native_preview(
    preview: tauri::State<'_, NativePreviewState>,
    window: tauri::WebviewWindow,
    monitor_index: Option<u32>,
) -> Result<(), String> {
    if let Err(err) = set_window_excluded_from_capture(&window, true) {
        log::warn!("start_native_preview: failed to exclude window from capture: {err}");
    }
    tokio::time::sleep(Duration::from_millis(80)).await;

    let mut guard = preview.0.lock().await;
    match guard.start_session(monitor_index.unwrap_or(0)) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = set_window_excluded_from_capture(&window, false);
            Err(err)
        }
    }
}

#[tauri::command]
pub async fn get_native_preview_frame(
    preview: tauri::State<'_, NativePreviewState>,
) -> Result<Option<NativePreviewFrame>, String> {
    let guard = preview.0.lock().await;
    Ok(guard.latest_frame())
}

#[tauri::command]
pub async fn stop_native_preview(
    preview: tauri::State<'_, NativePreviewState>,
    state: tauri::State<'_, RecorderState>,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    {
        let mut guard = preview.0.lock().await;
        guard.stop_session();
    }

    let has_active_recording = state.0.lock().await.is_some();
    if !has_active_recording {
        if let Err(err) = set_window_excluded_from_capture(&window, false) {
            log::warn!("stop_native_preview: failed to restore window capture visibility: {err}");
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn is_ctrl_pressed(telemetry: tauri::State<'_, TelemetryState>) -> Result<bool, String> {
    let hook_state = telemetry.0.is_ctrl_pressed.load(Ordering::Relaxed);
    Ok(is_ctrl_pressed_now().unwrap_or(hook_state))
}

#[cfg(target_os = "windows")]
fn is_ctrl_pressed_now() -> Option<bool> {
    // High-order bit is set when key is currently down.
    let left_down = unsafe { GetAsyncKeyState(VK_LCONTROL.0 as i32) } < 0;
    let right_down = unsafe { GetAsyncKeyState(VK_RCONTROL.0 as i32) } < 0;
    let generic_down = unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) } < 0;
    Some(left_down || right_down || generic_down)
}

#[cfg(not(target_os = "windows"))]
fn is_ctrl_pressed_now() -> Option<bool> {
    None
}

#[tauri::command]
pub async fn start_recording(
    state: tauri::State<'_, RecorderState>,
    telemetry: tauri::State<'_, TelemetryState>,
    preview: tauri::State<'_, NativePreviewState>,
    window: tauri::WebviewWindow,
    monitor_index: u32,
    options: Option<StartRecordingOptions>,
) -> Result<String, String> {
    let mut guard = state.0.lock().await;

    if guard.is_some() {
        return Err("Recording already in progress".to_string());
    }

    let options = options.unwrap_or_default();
    let auto_zoom_trigger_mode = options.auto_zoom_trigger_mode.unwrap_or_default();
    let quality = options.quality.unwrap_or_default().as_recorder_quality();
    let target_fps = sanitize_recording_fps(options.target_fps.unwrap_or(DEFAULT_TARGET_FPS));

    {
        let mut preview_guard = preview.0.lock().await;
        preview_guard.stop_session();
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

    if let Err(err) = set_window_excluded_from_capture(&window, true) {
        log::warn!("start_recording: failed to exclude window from capture: {err}");
    }

    let raw_mp4 = output_dir.join("raw.mp4");
    let stop_flag = Arc::new(AtomicBool::new(false));
    let pause_flag = Arc::new(AtomicBool::new(false));
    let capture_thread = match start_capture(
        monitor_index,
        stop_flag.clone(),
        pause_flag.clone(),
        raw_mp4,
        width,
        height,
        target_fps,
        quality,
    ) {
        Ok(thread) => thread,
        Err(err) => {
            let _ = set_window_excluded_from_capture(&window, false);
            return Err(err);
        }
    };

    let start_ms = chrono::Utc::now().timestamp_millis() as u64;
    let telemetry_processor = logger::start_session(&telemetry.0, start_ms);
    logger::set_paused(&telemetry.0, false);

    *guard = Some(ActiveRecording {
        recording_id: recording_id.clone(),
        stop_flag,
        pause_flag,
        capture_thread,
        output_dir,
        width,
        height,
        scale_factor,
        start_ms,
        pause_started_at_ms: None,
        pause_ranges_ms: Vec::new(),
        auto_zoom_trigger_mode,
        telemetry_processor,
    });

    Ok(recording_id)
}

#[tauri::command]
pub async fn stop_recording(
    state: tauri::State<'_, RecorderState>,
    telemetry: tauri::State<'_, TelemetryState>,
    window: tauri::WebviewWindow,
    recording_id: String,
) -> Result<(), String> {
    let mut rec = state.0.lock().await.take().ok_or("No active recording")?;

    if rec.recording_id != recording_id {
        let active_id = rec.recording_id.clone();
        *state.0.lock().await = Some(rec);
        return Err(format!(
            "Recording ID mismatch: active={active_id}, requested={recording_id}"
        ));
    }

    log::info!("stop_recording: id={recording_id}");

    let end_ms = chrono::Utc::now().timestamp_millis() as u64;
    if let Some(pause_started_at_ms) = rec.pause_started_at_ms.take() {
        rec.pause_ranges_ms.push((pause_started_at_ms, end_ms));
    }
    rec.pause_flag.store(false, Ordering::Relaxed);
    rec.stop_flag.store(true, Ordering::Relaxed);
    logger::set_paused(&telemetry.0, false);
    logger::stop_session(&telemetry.0);

    let output_dir = rec.output_dir.clone();
    let width = rec.width;
    let height = rec.height;
    let scale_factor = rec.scale_factor;
    let start_ms = rec.start_ms;
    let auto_zoom_trigger_mode = rec.auto_zoom_trigger_mode;
    let pause_ranges_ms = rec.pause_ranges_ms.clone();
    let paused_total_ms = total_pause_duration_ms(&pause_ranges_ms);

    let stop_result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        match rec.capture_thread.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => log::warn!("Capture thread finished with error: {e}"),
            Err(_) => log::error!("Capture thread panicked"),
        }

        let telemetry_events = rec.telemetry_processor.join().unwrap_or_default();
        let telemetry_events =
            normalize_events_for_pauses(telemetry_events, start_ms, &pause_ranges_ms);
        log::info!(
            "stop_recording: collected {} telemetry events",
            telemetry_events.len()
        );

        let duration_ms = end_ms
            .saturating_sub(start_ms)
            .saturating_sub(paused_total_ms);

        save_recording_files(
            &output_dir,
            &recording_id,
            width,
            height,
            scale_factor,
            start_ms,
            duration_ms,
            auto_zoom_trigger_mode,
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
    .map_err(|e| format!("Task join error: {e}"))?;

    if let Err(err) = set_window_excluded_from_capture(&window, false) {
        log::warn!("stop_recording: failed to restore window capture visibility: {err}");
    }

    stop_result?;
    Ok(())
}

#[tauri::command]
pub async fn pause_recording(
    state: tauri::State<'_, RecorderState>,
    telemetry: tauri::State<'_, TelemetryState>,
    recording_id: String,
) -> Result<(), String> {
    let mut guard = state.0.lock().await;
    let rec = guard.as_mut().ok_or("No active recording")?;
    if rec.recording_id != recording_id {
        return Err(format!(
            "Recording ID mismatch: active={}, requested={recording_id}",
            rec.recording_id
        ));
    }
    if rec.pause_started_at_ms.is_some() {
        return Ok(());
    }

    let now_ms = chrono::Utc::now().timestamp_millis() as u64;
    rec.pause_started_at_ms = Some(now_ms);
    rec.pause_flag.store(true, Ordering::Relaxed);
    logger::set_paused(&telemetry.0, true);
    Ok(())
}

#[tauri::command]
pub async fn resume_recording(
    state: tauri::State<'_, RecorderState>,
    telemetry: tauri::State<'_, TelemetryState>,
    recording_id: String,
) -> Result<(), String> {
    let mut guard = state.0.lock().await;
    let rec = guard.as_mut().ok_or("No active recording")?;
    if rec.recording_id != recording_id {
        return Err(format!(
            "Recording ID mismatch: active={}, requested={recording_id}",
            rec.recording_id
        ));
    }
    let Some(paused_at_ms) = rec.pause_started_at_ms.take() else {
        return Ok(());
    };

    let now_ms = chrono::Utc::now().timestamp_millis() as u64;
    if now_ms > paused_at_ms {
        rec.pause_ranges_ms.push((paused_at_ms, now_ms));
    }
    rec.pause_flag.store(false, Ordering::Relaxed);
    logger::set_paused(&telemetry.0, false);
    Ok(())
}

/// Path to project directory: `{Videos}/NeuroScreenCaster/{id}/`.
fn project_dir(recording_id: &str) -> Result<std::path::PathBuf, String> {
    let base = dirs::video_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Videos")))
        .ok_or("Failed to resolve Videos directory")?;

    Ok(base.join("NeuroScreenCaster").join(recording_id))
}

fn sanitize_recording_fps(raw_fps: u32) -> u32 {
    if raw_fps >= 45 {
        60
    } else {
        30
    }
}

fn camera_config_for_trigger_mode(
    auto_zoom_trigger_mode: AutoZoomTriggerMode,
) -> camera_engine::SmartCameraConfig {
    let mut config = camera_engine::SmartCameraConfig::default();
    config.click_activation_mode = match auto_zoom_trigger_mode {
        AutoZoomTriggerMode::SingleClick => camera_engine::ClickActivationMode::SingleClick,
        AutoZoomTriggerMode::MultiClickWindow => {
            camera_engine::ClickActivationMode::MultiClickWindow
        }
        AutoZoomTriggerMode::CtrlClick => camera_engine::ClickActivationMode::CtrlClick,
    };

    match auto_zoom_trigger_mode {
        AutoZoomTriggerMode::SingleClick | AutoZoomTriggerMode::CtrlClick => {
            config.min_clicks_to_activate = 1;
        }
        AutoZoomTriggerMode::MultiClickWindow => {
            config.min_clicks_to_activate = 2;
            config.activation_window_ms = 3_000;
        }
    }

    config
}

fn total_pause_duration_ms(pause_ranges_ms: &[(u64, u64)]) -> u64 {
    pause_ranges_ms
        .iter()
        .map(|(start, end)| end.saturating_sub(*start))
        .sum()
}

fn normalize_events_for_pauses(
    events: Vec<InputEvent>,
    start_ms: u64,
    pause_ranges_abs_ms: &[(u64, u64)],
) -> Vec<InputEvent> {
    if events.is_empty() || pause_ranges_abs_ms.is_empty() {
        return events;
    }

    let mut events = events;
    events.sort_by_key(InputEvent::ts);

    let mut pause_ranges = pause_ranges_abs_ms
        .iter()
        .map(|(start, end)| {
            (
                start.saturating_sub(start_ms),
                end.saturating_sub(start_ms)
                    .max(start.saturating_sub(start_ms)),
            )
        })
        .filter(|(start, end)| end > start)
        .collect::<Vec<_>>();
    if pause_ranges.is_empty() {
        return events;
    }
    pause_ranges.sort_by_key(|(start, _)| *start);

    let mut merged: Vec<(u64, u64)> = Vec::new();
    for (start, end) in pause_ranges {
        if let Some((_, last_end)) = merged.last_mut() {
            if start <= *last_end {
                *last_end = (*last_end).max(end);
                continue;
            }
        }
        merged.push((start, end));
    }

    let mut normalized_events = Vec::with_capacity(events.len());
    let mut range_idx = 0usize;
    let mut shift_ms = 0u64;

    for mut event in events {
        let raw_ts = event.ts();
        while range_idx < merged.len() && merged[range_idx].1 <= raw_ts {
            shift_ms = shift_ms.saturating_add(merged[range_idx].1 - merged[range_idx].0);
            range_idx += 1;
        }

        if range_idx < merged.len() {
            let (pause_start, pause_end) = merged[range_idx];
            if raw_ts >= pause_start && raw_ts < pause_end {
                continue;
            }
        }

        set_event_ts(&mut event, raw_ts.saturating_sub(shift_ms));
        normalized_events.push(event);
    }

    normalized_events
}

fn set_event_ts(event: &mut InputEvent, ts: u64) {
    match event {
        InputEvent::Move { ts: event_ts, .. }
        | InputEvent::Click { ts: event_ts, .. }
        | InputEvent::MouseUp { ts: event_ts, .. }
        | InputEvent::Scroll { ts: event_ts, .. }
        | InputEvent::KeyDown { ts: event_ts, .. }
        | InputEvent::KeyUp { ts: event_ts, .. } => {
            *event_ts = ts;
        }
    }
}

fn set_window_excluded_from_capture(
    window: &tauri::WebviewWindow,
    excluded_from_capture: bool,
) -> Result<(), String> {
    window
        .set_content_protected(excluded_from_capture)
        .map_err(|e| format!("Failed to set content protection: {e}"))
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
    auto_zoom_trigger_mode: AutoZoomTriggerMode,
    events: Vec<InputEvent>,
) -> Result<(), String> {
    let settings = ProjectSettings::default();
    let output_aspect_ratio = settings.export.width as f64 / settings.export.height.max(1) as f64;
    let camera_config = camera_config_for_trigger_mode(auto_zoom_trigger_mode);
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
        "save_recording_files: smart_camera_segments={} smoothed_cursor_points={} proxy={}",
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

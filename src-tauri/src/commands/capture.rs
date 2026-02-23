//! Tauri IPC commands for recording.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::algorithm::{camera_engine, cursor_smoothing};
use crate::capture::audio_loopback::start_system_loopback_capture;
use crate::capture::preview::{NativePreviewFrame, NativePreviewState};
use crate::capture::recorder::RecordingQuality;
use crate::capture::recorder::{
    apply_no_window_flags, find_ffmpeg_exe, get_monitor_scale_factor, get_monitor_size,
    start_capture, DEFAULT_TARGET_FPS,
};
use crate::capture::state::{
    ActiveRecording, AudioCaptureBackend, AudioCaptureProcess, AudioCaptureSession,
    AutoZoomTriggerMode, RecorderState, RecordingAudioMode,
};
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
    audio_capture_mode: Option<RecordingAudioMode>,
    microphone_device: Option<String>,
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

#[tauri::command]
pub async fn list_audio_input_devices() -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(list_microphone_input_devices)
        .await
        .map_err(|e| format!("Failed to fetch audio devices: {e}"))?
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
    let audio_mode = options.audio_capture_mode.unwrap_or_default();
    let microphone_device = options.microphone_device.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });

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
    let mut audio_capture_session =
        start_audio_capture_session(&output_dir, audio_mode, microphone_device.as_deref())?;
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
            stop_audio_capture_session(&mut audio_capture_session);
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
        audio_mode,
        microphone_device,
        audio_capture_session,
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
    let audio_mode = rec.audio_mode;
    let microphone_device = rec.microphone_device.clone();
    let mut audio_capture_session = rec.audio_capture_session.take();
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
            audio_mode,
            microphone_device,
            end_ms,
            pause_ranges_ms.clone(),
            audio_capture_session.take(),
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
            // "Single click" / "Ctrl+click" should react to each valid click,
            // so disable long coalescing/debouncing defaults.
            config.click_cluster_gap_ms = 1;
            config.min_zoom_interval_ms = 1;
        }
        AutoZoomTriggerMode::MultiClickWindow => {
            config.min_clicks_to_activate = 2;
            config.activation_window_ms = 3_000;
            config.click_cluster_gap_ms = 300;
            config.min_zoom_interval_ms = 2_000;
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

fn list_dshow_audio_devices() -> Result<Vec<String>, String> {
    let ffmpeg = find_ffmpeg_exe();
    let mut command = Command::new(&ffmpeg);
    apply_no_window_flags(&mut command);

    let output = command
        .arg("-hide_banner")
        .arg("-list_devices")
        .arg("true")
        .arg("-f")
        .arg("dshow")
        .arg("-i")
        .arg("dummy")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| {
            format!(
                "Failed to list audio devices via ffmpeg ({}): {e}",
                ffmpeg.display()
            )
        })?;

    let listing = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let mut dedup = HashSet::new();
    let mut devices = Vec::new();
    for line in listing.lines() {
        if !line.contains("(audio)") {
            continue;
        }
        let Some(first_quote) = line.find('"') else {
            continue;
        };
        let rem = &line[first_quote + 1..];
        let Some(second_quote_rel) = rem.find('"') else {
            continue;
        };
        let name = rem[..second_quote_rel].trim();
        if name.is_empty() {
            continue;
        }
        if dedup.insert(name.to_lowercase()) {
            devices.push(name.to_string());
        }
    }

    Ok(devices)
}

fn is_likely_system_audio_device(name: &str) -> bool {
    let lower = name.to_lowercase();
    let patterns = [
        "stereo mix",
        "what u hear",
        "wave out",
        "virtual-audio-capturer",
        "loopback",
        "mixage st",
    ];
    patterns.iter().any(|token| lower.contains(token))
}

fn list_microphone_input_devices() -> Result<Vec<String>, String> {
    let all_devices = list_dshow_audio_devices()?;
    if all_devices.is_empty() {
        return Ok(Vec::new());
    }

    let microphones: Vec<String> = all_devices
        .iter()
        .filter(|name| !is_likely_system_audio_device(name))
        .cloned()
        .collect();
    if microphones.is_empty() {
        return Ok(all_devices);
    }

    Ok(microphones)
}

fn resolve_system_audio_device(all_devices: &[String]) -> Option<String> {
    let priority = [
        "virtual-audio-capturer",
        "stereo mix",
        "what u hear",
        "wave out",
        "loopback",
        "mixage st",
    ];

    for token in priority {
        if let Some(device) = all_devices
            .iter()
            .find(|name| name.to_lowercase().contains(token))
        {
            return Some(device.clone());
        }
    }
    None
}

fn resolve_microphone_device(
    all_devices: &[String],
    requested: Option<&str>,
) -> Result<String, String> {
    let non_system_devices: Vec<String> = all_devices
        .iter()
        .filter(|name| !is_likely_system_audio_device(name))
        .cloned()
        .collect();
    let available = if non_system_devices.is_empty() {
        all_devices.to_vec()
    } else {
        non_system_devices
    };

    if available.is_empty() {
        return Err("No microphone devices found via ffmpeg dshow".to_string());
    }

    if let Some(requested_name) = requested {
        if let Some(found) = available
            .iter()
            .find(|name| name.eq_ignore_ascii_case(requested_name))
        {
            return Ok(found.clone());
        }
    }

    Ok(available[0].clone())
}

fn spawn_audio_capture_process(
    device_name: &str,
    output_path: &Path,
) -> Result<AudioCaptureProcess, String> {
    let ffmpeg = find_ffmpeg_exe();
    let mut command = Command::new(&ffmpeg);
    apply_no_window_flags(&mut command);

    let mut child = command
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-f")
        .arg("dshow")
        .arg("-i")
        .arg(format!("audio={device_name}"))
        .arg("-ac")
        .arg("2")
        .arg("-ar")
        .arg("48000")
        .arg("-c:a")
        .arg("pcm_s16le")
        .arg(output_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to spawn ffmpeg audio capture for '{device_name}': {e}"))?;

    std::thread::sleep(Duration::from_millis(150));
    match child.try_wait() {
        Ok(Some(status)) => {
            return Err(format!(
                "Audio capture process for '{device_name}' exited early with status: {status}"
            ));
        }
        Ok(None) => {}
        Err(err) => {
            return Err(format!(
                "Failed to check ffmpeg audio capture status for '{device_name}': {err}"
            ));
        }
    }

    Ok(AudioCaptureProcess {
        backend: AudioCaptureBackend::FfmpegChild(child),
        output_path: output_path.to_path_buf(),
    })
}

fn start_audio_capture_session(
    output_dir: &Path,
    mode: RecordingAudioMode,
    requested_microphone: Option<&str>,
) -> Result<Option<AudioCaptureSession>, String> {
    if mode == RecordingAudioMode::NoAudio {
        return Ok(None);
    }

    let wants_system = matches!(
        mode,
        RecordingAudioMode::SystemOnly | RecordingAudioMode::MicrophoneAndSystem
    );
    let wants_microphone = matches!(
        mode,
        RecordingAudioMode::MicrophoneOnly | RecordingAudioMode::MicrophoneAndSystem
    );

    let all_devices = match list_dshow_audio_devices() {
        Ok(devices) => devices,
        Err(err) => {
            log::warn!("start_audio_capture_session: failed to list dshow audio devices: {err}");
            Vec::new()
        }
    };
    if wants_microphone && all_devices.is_empty() {
        return Err(
            "No microphone input devices available via ffmpeg dshow. Unable to start microphone capture."
                .to_string(),
        );
    }

    let mut session = AudioCaptureSession {
        system_capture: None,
        microphone_capture: None,
    };

    if wants_system {
        let system_path = output_dir.join("audio-system.wav");
        match start_system_loopback_capture(system_path.clone()) {
            Ok(native_loopback) => {
                session.system_capture = Some(AudioCaptureProcess {
                    backend: AudioCaptureBackend::NativeLoopback {
                        stop_flag: native_loopback.stop_flag,
                        join_handle: native_loopback.join_handle,
                    },
                    output_path: system_path,
                });
            }
            Err(native_err) => {
                log::warn!(
                    "start_audio_capture_session: WASAPI loopback unavailable, falling back to dshow loopback: {native_err}"
                );
                let system_device = resolve_system_audio_device(&all_devices).ok_or_else(|| {
                    format!(
                        "System audio capture failed via WASAPI ({native_err}) and no dshow loopback device was found."
                    )
                })?;
                session.system_capture = Some(
                    spawn_audio_capture_process(&system_device, &system_path).map_err(|ffmpeg_err| {
                        format!(
                            "System audio capture failed via WASAPI ({native_err}) and dshow fallback '{system_device}' failed: {ffmpeg_err}"
                        )
                    })?,
                );
            }
        }
    }

    if wants_microphone {
        let microphone_device = resolve_microphone_device(&all_devices, requested_microphone)?;
        let microphone_path = output_dir.join("audio-microphone.wav");
        match spawn_audio_capture_process(&microphone_device, &microphone_path) {
            Ok(process) => {
                session.microphone_capture = Some(process);
            }
            Err(err) => {
                let mut cleanup_session = Some(session);
                let _ = stop_audio_capture_session(&mut cleanup_session);
                return Err(err);
            }
        }
    }

    Ok(Some(session))
}

fn stop_ffmpeg_child(child: &mut Child) {
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(b"q\n");
        let _ = stdin.flush();
    }

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

fn stop_audio_capture_process(process: AudioCaptureProcess) -> PathBuf {
    let AudioCaptureProcess {
        backend,
        output_path,
    } = process;

    match backend {
        AudioCaptureBackend::FfmpegChild(mut child) => {
            stop_ffmpeg_child(&mut child);
        }
        AudioCaptureBackend::NativeLoopback {
            stop_flag,
            join_handle,
        } => {
            stop_flag.store(true, Ordering::Relaxed);
            match join_handle.join() {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    log::warn!("WASAPI loopback capture thread finished with error: {err}");
                }
                Err(_) => {
                    log::warn!("WASAPI loopback capture thread panicked");
                }
            }
        }
    }

    output_path
}

fn stop_audio_capture_session(
    session: &mut Option<AudioCaptureSession>,
) -> (Option<PathBuf>, Option<PathBuf>) {
    let Some(mut captured) = session.take() else {
        return (None, None);
    };

    let system_path = captured
        .system_capture
        .take()
        .map(stop_audio_capture_process);
    let microphone_path = captured
        .microphone_capture
        .take()
        .map(stop_audio_capture_process);
    (system_path, microphone_path)
}

fn keep_ranges_after_pauses(
    start_ms: u64,
    end_ms: u64,
    pause_ranges_ms: &[(u64, u64)],
) -> Vec<(u64, u64)> {
    let total_ms = end_ms.saturating_sub(start_ms);
    if total_ms == 0 {
        return Vec::new();
    }

    let mut pauses: Vec<(u64, u64)> = pause_ranges_ms
        .iter()
        .map(|(start, end)| {
            (
                start.saturating_sub(start_ms),
                end.saturating_sub(start_ms).min(total_ms),
            )
        })
        .filter(|(start, end)| end > start)
        .collect();
    if pauses.is_empty() {
        return vec![(0, total_ms)];
    }
    pauses.sort_by_key(|(start, _)| *start);

    let mut merged: Vec<(u64, u64)> = Vec::new();
    for (start, end) in pauses {
        if let Some((_, last_end)) = merged.last_mut() {
            if start <= *last_end {
                *last_end = (*last_end).max(end);
                continue;
            }
        }
        merged.push((start, end));
    }

    let mut keep = Vec::new();
    let mut cursor = 0u64;
    for (pause_start, pause_end) in merged {
        if pause_start > cursor {
            keep.push((cursor, pause_start));
        }
        cursor = cursor.max(pause_end);
    }
    if cursor < total_ms {
        keep.push((cursor, total_ms));
    }
    keep
}

fn format_seconds(ms: u64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

fn trim_audio_track_to_active_ranges(
    input_path: &Path,
    output_path: &Path,
    keep_ranges_ms: &[(u64, u64)],
) -> Result<(), String> {
    if keep_ranges_ms.is_empty() {
        return Err("No active (non-paused) ranges available for audio trimming".to_string());
    }

    let mut chain = Vec::new();
    for (index, (start_ms, end_ms)) in keep_ranges_ms.iter().enumerate() {
        chain.push(format!(
            "[0:a]atrim=start={}:end={},asetpts=PTS-STARTPTS[a{}]",
            format_seconds(*start_ms),
            format_seconds(*end_ms),
            index
        ));
    }

    if keep_ranges_ms.len() == 1 {
        chain.push("[a0]anull[aout]".to_string());
    } else {
        let labels = (0..keep_ranges_ms.len())
            .map(|idx| format!("[a{}]", idx))
            .collect::<String>();
        chain.push(format!(
            "{}concat=n={}:v=0:a=1[aout]",
            labels,
            keep_ranges_ms.len()
        ));
    }

    let filter = chain.join(";");
    let ffmpeg = find_ffmpeg_exe();
    let mut command = Command::new(&ffmpeg);
    apply_no_window_flags(&mut command);

    let status = command
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(input_path)
        .arg("-filter_complex")
        .arg(filter)
        .arg("-map")
        .arg("[aout]")
        .arg("-c:a")
        .arg("pcm_s16le")
        .arg(output_path)
        .status()
        .map_err(|e| {
            format!(
                "Failed to run ffmpeg ({}) for audio trimming: {e}",
                ffmpeg.display()
            )
        })?;

    if !status.success() {
        return Err("FFmpeg audio trimming failed".to_string());
    }

    Ok(())
}

fn mix_audio_tracks(
    microphone_path: &Path,
    system_path: &Path,
    output_path: &Path,
) -> Result<(), String> {
    let ffmpeg = find_ffmpeg_exe();
    let mut command = Command::new(&ffmpeg);
    apply_no_window_flags(&mut command);

    let status = command
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(microphone_path)
        .arg("-i")
        .arg(system_path)
        .arg("-filter_complex")
        .arg("[0:a][1:a]amix=inputs=2:normalize=0:dropout_transition=0[aout]")
        .arg("-map")
        .arg("[aout]")
        .arg("-c:a")
        .arg("pcm_s16le")
        .arg(output_path)
        .status()
        .map_err(|e| {
            format!(
                "Failed to run ffmpeg ({}) for audio mixing: {e}",
                ffmpeg.display()
            )
        })?;

    if !status.success() {
        return Err("FFmpeg audio mixing failed".to_string());
    }

    Ok(())
}

fn mux_audio_into_raw_video(output_dir: &Path, audio_path: &Path) -> Result<(), String> {
    let raw_video_path = output_dir.join("raw.mp4");
    if !raw_video_path.exists() {
        return Ok(());
    }
    if !audio_path.exists() {
        return Ok(());
    }

    let muxed_path = output_dir.join("raw-with-audio.mp4");
    let ffmpeg = find_ffmpeg_exe();
    let mut command = Command::new(&ffmpeg);
    apply_no_window_flags(&mut command);

    let status = command
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(&raw_video_path)
        .arg("-i")
        .arg(audio_path)
        .arg("-map")
        .arg("0:v:0")
        .arg("-map")
        .arg("1:a:0")
        .arg("-c:v")
        .arg("copy")
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg("192k")
        .arg("-movflags")
        .arg("+faststart")
        .arg(&muxed_path)
        .status()
        .map_err(|e| {
            format!(
                "Failed to run ffmpeg ({}) for video+audio mux: {e}",
                ffmpeg.display()
            )
        })?;

    if !status.success() {
        return Err("FFmpeg mux (video+audio) failed".to_string());
    }

    let backup_path = output_dir.join("raw-video-only.mp4");
    let _ = std::fs::remove_file(&backup_path);
    std::fs::rename(&raw_video_path, &backup_path)
        .map_err(|e| format!("Failed to backup raw.mp4 before mux replacement: {e}"))?;
    std::fs::rename(&muxed_path, &raw_video_path).map_err(|e| {
        let _ = std::fs::rename(&backup_path, &raw_video_path);
        format!("Failed to replace raw.mp4 with muxed file: {e}")
    })?;

    Ok(())
}

fn finalize_recording_audio(
    output_dir: &Path,
    audio_capture_session: &mut Option<AudioCaptureSession>,
    mode: RecordingAudioMode,
    start_ms: u64,
    end_ms: u64,
    pause_ranges_ms: &[(u64, u64)],
) -> Result<(), String> {
    if mode == RecordingAudioMode::NoAudio {
        let _ = stop_audio_capture_session(audio_capture_session);
        return Ok(());
    }

    let (system_raw, microphone_raw) = stop_audio_capture_session(audio_capture_session);
    let keep_ranges = keep_ranges_after_pauses(start_ms, end_ms, pause_ranges_ms);
    if keep_ranges.is_empty() {
        return Ok(());
    }

    let prepare_track = |raw: Option<PathBuf>, label: &str| -> Result<Option<PathBuf>, String> {
        let Some(raw_path) = raw else {
            return Ok(None);
        };
        let metadata = match std::fs::metadata(&raw_path) {
            Ok(metadata) => metadata,
            Err(_) => return Ok(None),
        };
        if metadata.len() == 0 {
            return Ok(None);
        }

        let total_ms = end_ms.saturating_sub(start_ms);
        if keep_ranges.len() == 1 && keep_ranges[0].0 == 0 && keep_ranges[0].1 >= total_ms {
            return Ok(Some(raw_path));
        }

        let trimmed_path = output_dir.join(format!("audio-{}-trimmed.wav", label));
        trim_audio_track_to_active_ranges(&raw_path, &trimmed_path, &keep_ranges)?;
        Ok(Some(trimmed_path))
    };

    let system_prepared = prepare_track(system_raw, "system")?;
    let microphone_prepared = prepare_track(microphone_raw, "microphone")?;

    let final_audio = match mode {
        RecordingAudioMode::NoAudio => None,
        RecordingAudioMode::SystemOnly => system_prepared,
        RecordingAudioMode::MicrophoneOnly => microphone_prepared,
        RecordingAudioMode::MicrophoneAndSystem => match (microphone_prepared, system_prepared) {
            (Some(microphone), Some(system)) => {
                let mixed_path = output_dir.join("audio-mixed.wav");
                mix_audio_tracks(&microphone, &system, &mixed_path)?;
                Some(mixed_path)
            }
            (Some(microphone), None) => Some(microphone),
            (None, Some(system)) => Some(system),
            (None, None) => None,
        },
    };

    if let Some(audio_path) = final_audio {
        mux_audio_into_raw_video(output_dir, &audio_path)?;
    }

    Ok(())
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
    audio_mode: RecordingAudioMode,
    microphone_device: Option<String>,
    end_ms: u64,
    pause_ranges_ms: Vec<(u64, u64)>,
    mut audio_capture_session: Option<AudioCaptureSession>,
    events: Vec<InputEvent>,
) -> Result<(), String> {
    if let Err(err) = finalize_recording_audio(
        output_dir,
        &mut audio_capture_session,
        audio_mode,
        start_ms,
        end_ms,
        &pause_ranges_ms,
    ) {
        log::warn!("save_recording_files: audio finalize failed: {err}");
    }

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
        "save_recording_files: smart_camera_segments={} smoothed_cursor_points={} proxy={} audio_mode={:?} microphone={}",
        zoom_segments.len(),
        smoothed_cursor_path.len(),
        proxy_video_path.as_deref().unwrap_or("none"),
        audio_mode,
        microphone_device.as_deref().unwrap_or("default")
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

    let mut command = std::process::Command::new(&ffmpeg);
    apply_no_window_flags(&mut command);

    let status = command
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(&source)
        .arg("-map")
        .arg("0:v:0")
        .arg("-map")
        .arg("0:a?")
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
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg("160k")
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

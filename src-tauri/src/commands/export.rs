use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use rfd::FileDialog;
use serde::Serialize;

use crate::algorithm::cursor_smoothing::smooth_cursor_path;
use crate::capture::recorder::{apply_no_window_flags, find_ffmpeg_exe};
use crate::commands::cursor::resolve_cursor_asset_for_render;
use crate::models::events::{EventsFile, InputEvent, SCHEMA_VERSION as EVENTS_SCHEMA_VERSION};
use crate::models::project::{
    CameraSpring, NormalizedRect, PanKeyframe, Project, TargetPoint, ZoomSegment, SCHEMA_VERSION,
};

const DEFAULT_SPRING_MASS: f64 = 1.0;
const DEFAULT_SPRING_STIFFNESS: f64 = 170.0;
const DEFAULT_SPRING_DAMPING: f64 = 26.0;
const CURSOR_SIZE_TO_FRAME_RATIO: f64 = 0.03;
const CLICK_PULSE_MIN_SCALE: f64 = 0.82;
const CLICK_PULSE_TOTAL_MS: f64 = 150.0;
const CLICK_PULSE_DOWN_MS: f64 = 65.0;
const CURSOR_TIMING_OFFSET_MS: u64 = 45;
const MAX_CURSOR_SAMPLES_FOR_EXPR: usize = 90;
const MAX_CLICK_EVENTS_FOR_EXPR: usize = 90;
const MAX_CAMERA_STATES_FOR_ANALYTIC_EXPR: usize = 64;
const MAX_CAMERA_POINTS_FOR_EXPR: usize = 480;
const CAMERA_FALLBACK_SAMPLE_RATE_HZ: f64 = 20.0;
const MIN_CLICK_PULSE_GAP_MS: u64 = 120;
const ENABLE_CUSTOM_CURSOR_OVERLAY_EXPORT: bool = false;
const VECTOR_CURSOR_SAMPLE_FPS: f64 = 18.0;
const VECTOR_CURSOR_ASS_BASE_HEIGHT: f64 = 112.0;
const VECTOR_CURSOR_ASS_PATH: &str = "m 0 0 l 0 90 l 22 70 l 35 110 l 50 102 l 38 63 l 72 63 l 0 0";

#[derive(Debug, Clone, Copy)]
struct SpringParams {
    mass: f64,
    stiffness: f64,
    damping: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportStatus {
    pub is_running: bool,
    pub progress: f64,
    pub message: String,
    pub output_path: Option<String>,
    pub error: Option<String>,
    pub started_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
}

impl Default for ExportStatus {
    fn default() -> Self {
        Self {
            is_running: false,
            progress: 0.0,
            message: "Idle".to_string(),
            output_path: None,
            error: None,
            started_at_ms: None,
            finished_at_ms: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct ExportState(pub Arc<Mutex<ExportStatus>>);

#[derive(Debug, Clone, Copy)]
struct AxisSpringState {
    value: f64,
    velocity: f64,
}

#[derive(Debug, Clone, Copy)]
struct AxisSpringSegment {
    start: f64,
    velocity: f64,
    target: f64,
}

#[derive(Debug, Clone)]
struct CameraState {
    start_frame: f64,
    end_frame: f64,
    spring: SpringParams,
    zoom: AxisSpringSegment,
    offset_x: AxisSpringSegment,
    offset_y: AxisSpringSegment,
}

#[derive(Debug, Clone)]
struct SegmentRuntime {
    start_ts: u64,
    end_ts: u64,
    base_rect: NormalizedRect,
    target_points: Vec<TargetPoint>,
    spring: SpringParams,
}

#[derive(Debug, Clone, Copy, Default)]
struct MediaProbe {
    duration_ms: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Debug, Clone)]
struct CursorOverlayPlan {
    cursor_png_path: PathBuf,
    filter_chain: String,
}

#[tauri::command]
pub async fn get_export_status(
    state: tauri::State<'_, ExportState>,
) -> Result<ExportStatus, String> {
    let status = state
        .0
        .lock()
        .map_err(|_| "Failed to access export status".to_string())?
        .clone();
    Ok(status)
}

#[tauri::command]
pub async fn reset_export_status(state: tauri::State<'_, ExportState>) -> Result<(), String> {
    let mut status = state
        .0
        .lock()
        .map_err(|_| "Failed to access export status".to_string())?;
    if status.is_running {
        return Err("Cannot reset status while export is running".to_string());
    }
    *status = ExportStatus::default();
    Ok(())
}

#[tauri::command]
pub async fn pick_export_folder(initial_dir: Option<String>) -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(move || {
        let mut dialog = FileDialog::new();
        if let Some(raw) = initial_dir {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                let candidate = PathBuf::from(trimmed);
                if candidate.exists() {
                    dialog = dialog.set_directory(candidate);
                }
            }
        }
        Ok(dialog
            .pick_folder()
            .map(|path| path.to_string_lossy().to_string()))
    })
    .await
    .map_err(|e| format!("Failed to open folder dialog: {e}"))?
}

#[tauri::command]
pub async fn start_export(
    state: tauri::State<'_, ExportState>,
    project_path: String,
    width: Option<u32>,
    height: Option<u32>,
    fps: Option<u32>,
    codec: Option<String>,
    output_path: Option<String>,
) -> Result<(), String> {
    let project_file = resolve_project_file(&project_path)?;
    let project = load_project_file(&project_file)?;
    let project_dir = project_file.parent().ok_or_else(|| {
        format!(
            "Project path has no parent directory: {}",
            project_file.display()
        )
    })?;

    let source_video = resolve_media_path(project_dir, &project.video_path)?;
    if !source_video.exists() {
        return Err(format!(
            "Source video not found: {}",
            source_video.display()
        ));
    }

    let events = match load_events_file(project_dir, &project.events_path) {
        Ok(events) => Some(events),
        Err(err) => {
            log::warn!("start_export: cannot load events file: {err}");
            None
        }
    };

    let probe = probe_media_info(&source_video);
    let source_duration_ms = probe.duration_ms.unwrap_or(project.duration_ms).max(1);
    let source_width = probe.width.unwrap_or(project.video_width).max(1);
    let source_height = probe.height.unwrap_or(project.video_height).max(1);

    let target_width = width
        .unwrap_or(project.settings.export.width)
        .clamp(320, 7680);
    let target_height = height
        .unwrap_or(project.settings.export.height)
        .clamp(240, 4320);
    let target_fps = fps.unwrap_or(project.settings.export.fps).clamp(10, 120);
    let target_codec = codec
        .unwrap_or(project.settings.export.codec.clone())
        .trim()
        .to_lowercase();

    if !matches!(target_codec.as_str(), "h264" | "h265" | "vp9") {
        return Err(format!("Unsupported codec: {target_codec}"));
    }

    let output_video = resolve_output_path(project_dir, &project.id, output_path)?;
    if let Some(parent) = output_video.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create export output directory {}: {e}",
                parent.display()
            )
        })?;
    }

    {
        let mut status = state
            .0
            .lock()
            .map_err(|_| "Failed to access export status".to_string())?;

        if status.is_running {
            return Err("Another export is already running".to_string());
        }

        *status = ExportStatus {
            is_running: true,
            progress: 0.0,
            message: format!(
                "Starting export {}x{} @ {}fps ({})",
                target_width, target_height, target_fps, target_codec
            ),
            output_path: Some(output_video.to_string_lossy().to_string()),
            error: None,
            started_at_ms: Some(now_ms()),
            finished_at_ms: None,
        };
    }

    let status_state = state.0.clone();
    let project_for_export = project.clone();
    std::thread::Builder::new()
        .name("nsc-export".to_string())
        .spawn(move || {
            run_export_job(
                status_state,
                source_video,
                output_video,
                project_for_export,
                events,
                target_width,
                target_height,
                target_fps,
                target_codec,
                source_duration_ms,
                source_width,
                source_height,
            )
        })
        .map_err(|e| format!("Failed to spawn export thread: {e}"))?;

    Ok(())
}

fn run_export_job(
    status_state: Arc<Mutex<ExportStatus>>,
    source_video: PathBuf,
    output_video: PathBuf,
    project: Project,
    events: Option<EventsFile>,
    width: u32,
    height: u32,
    fps: u32,
    codec: String,
    source_duration_ms: u64,
    source_width: u32,
    source_height: u32,
) {
    let filter_build = build_export_filter_graph(
        &project,
        events.as_ref(),
        width,
        height,
        fps,
        source_duration_ms,
        source_width,
        source_height,
    );

    let (filter_graph, cursor_image_input, cursor_temp_file) = match filter_build {
        Ok(result) => result,
        Err(err) => {
            update_status(&status_state, |status| {
                status.is_running = false;
                status.finished_at_ms = Some(now_ms());
                status.message = "Export failed".to_string();
                status.error = Some(err);
            });
            return;
        }
    };

    let result = execute_ffmpeg_export(
        &status_state,
        &source_video,
        cursor_image_input.as_deref(),
        &output_video,
        &filter_graph,
        &codec,
        source_duration_ms,
    );

    if let Some(path) = cursor_temp_file {
        let _ = std::fs::remove_file(path);
    }

    update_status(&status_state, |status| {
        status.is_running = false;
        status.finished_at_ms = Some(now_ms());
        match result {
            Ok(()) => {
                status.progress = 1.0;
                status.message = "Export finished".to_string();
                status.output_path = Some(output_video.to_string_lossy().to_string());
                status.error = None;
            }
            Err(err) => {
                status.message = "Export failed".to_string();
                status.error = Some(err);
            }
        }
    });
}

fn execute_ffmpeg_export(
    status_state: &Arc<Mutex<ExportStatus>>,
    source_video: &Path,
    cursor_image: Option<&Path>,
    output_video: &Path,
    filter_graph: &str,
    codec: &str,
    source_duration_ms: u64,
) -> Result<(), String> {
    let filter_script_path = std::env::temp_dir().join(format!("nsc-filter-{}.txt", now_ms()));
    std::fs::write(&filter_script_path, filter_graph).map_err(|e| {
        format!(
            "Failed to write temporary FFmpeg filter script {}: {e}",
            filter_script_path.display()
        )
    })?;

    let ffmpeg = find_ffmpeg_exe();

    let mut command = Command::new(&ffmpeg);
    apply_no_window_flags(&mut command);
    command.arg("-y").arg("-i").arg(source_video);

    if let Some(cursor_image_path) = cursor_image {
        command
            .arg("-loop")
            .arg("1")
            .arg("-i")
            .arg(cursor_image_path);
    }

    command
        .arg("-filter_complex_script")
        .arg(&filter_script_path)
        .arg("-map")
        .arg("[vout]")
        .arg("-map")
        .arg("0:a?");

    match codec {
        "h264" => {
            command
                .arg("-c:v")
                .arg("libx264")
                .arg("-preset")
                .arg("ultrafast")
                .arg("-crf")
                .arg("18")
                .arg("-pix_fmt")
                .arg("yuv420p");
        }
        "h265" => {
            command
                .arg("-c:v")
                .arg("libx265")
                .arg("-preset")
                .arg("ultrafast")
                .arg("-crf")
                .arg("24")
                .arg("-pix_fmt")
                .arg("yuv420p");
        }
        "vp9" => {
            command
                .arg("-c:v")
                .arg("libvpx-vp9")
                .arg("-b:v")
                .arg("0")
                .arg("-crf")
                .arg("33")
                .arg("-pix_fmt")
                .arg("yuv420p");
        }
        _ => {
            let _ = std::fs::remove_file(&filter_script_path);
            return Err(format!("Unsupported codec: {codec}"));
        }
    };

    command.arg("-c:a").arg("aac").arg("-b:a").arg("192k");

    let mut child = command
        .arg(output_video)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            let _ = std::fs::remove_file(&filter_script_path);
            format!(
                "Failed to start FFmpeg export ({}): {e}",
                ffmpeg.to_string_lossy()
            )
        })?;

    let mut stderr_tail: VecDeque<String> = VecDeque::new();
    if let Some(stderr) = child.stderr.take() {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            let line = match line {
                Ok(line) => line,
                Err(_) => continue,
            };

            stderr_tail.push_back(line.clone());
            if stderr_tail.len() > 50 {
                stderr_tail.pop_front();
            }

            if let Some(time_ms) = extract_ffmpeg_time_ms(&line) {
                let progress = (time_ms as f64 / source_duration_ms as f64).clamp(0.0, 0.99);
                update_status(status_state, |status| {
                    status.progress = progress;
                    status.message = format!("Exporting... {}%", (progress * 100.0).round() as u32);
                });
            }
        }
    }

    let exit_status = child.wait().map_err(|e| {
        let _ = std::fs::remove_file(&filter_script_path);
        format!("Failed to wait for FFmpeg export: {e}")
    })?;

    if !exit_status.success() {
        let stderr_excerpt = stderr_tail
            .iter()
            .filter(|line| {
                line.contains("Error")
                    || line.contains("error")
                    || line.contains("Invalid")
                    || line.contains("Failed")
                    || line.contains("failed")
            })
            .cloned()
            .collect::<Vec<_>>();
        let _ = std::fs::remove_file(&filter_script_path);
        if stderr_excerpt.is_empty() {
            return Err(format!("FFmpeg export failed with status: {exit_status}"));
        }
        return Err(format!(
            "FFmpeg export failed with status: {exit_status}\n{}",
            stderr_excerpt.join("\n")
        ));
    }

    let _ = std::fs::remove_file(&filter_script_path);
    Ok(())
}

fn build_export_filter_graph(
    project: &Project,
    events: Option<&EventsFile>,
    target_width: u32,
    target_height: u32,
    target_fps: u32,
    source_duration_ms: u64,
    source_width: u32,
    source_height: u32,
) -> Result<(String, Option<PathBuf>, Option<PathBuf>), String> {
    let render_fps = target_fps as f64;
    let camera_states = build_camera_states(
        project,
        source_duration_ms,
        project.duration_ms.max(1),
        source_width.max(1),
        source_height.max(1),
        render_fps,
    );

    let zoom_expr = build_camera_value_expr(&camera_states, |state| state.zoom, 1.0, render_fps);
    let offset_x_expr =
        build_camera_value_expr(&camera_states, |state| state.offset_x, 0.0, render_fps);
    let offset_y_expr =
        build_camera_value_expr(&camera_states, |state| state.offset_y, 0.0, render_fps);

    let mut input_chain: Vec<String> = Vec::new();
    let mut cursor_overlay_filter = None;
    let mut cursor_input_path = None;
    let mut cursor_temp_file = None;

    // Upsample to target FPS before camera transforms to match preview smoothness.
    input_chain.push(format!("fps={target_fps}"));

    if ENABLE_CUSTOM_CURSOR_OVERLAY_EXPORT {
        if let Some(plan) = build_cursor_overlay_plan(
            project,
            events,
            &camera_states,
            source_duration_ms,
            project.duration_ms.max(1),
            source_width.max(1),
            source_height.max(1),
            target_width.max(1),
            target_height.max(1),
            render_fps,
        )? {
            cursor_input_path = Some(plan.cursor_png_path);
            cursor_overlay_filter = Some(plan.filter_chain);
        }
    } else if let Some(events_file) = events {
        if !events_file.events.is_empty() {
            match build_vector_cursor_ass_file(
                project,
                events_file,
                &camera_states,
                source_duration_ms,
                project.duration_ms.max(1),
                source_width.max(1),
                source_height.max(1),
                target_width.max(1),
                target_height.max(1),
                render_fps,
            ) {
                Ok(ass) => {
                    let escaped = escape_filter_path(&ass);
                    cursor_overlay_filter =
                        Some(format!("[framed]subtitles=filename='{escaped}'[vout]"));
                    cursor_temp_file = Some(ass);
                }
                Err(err) => {
                    log::warn!("build_export_filter_graph: vector cursor overlay disabled: {err}");
                }
            }
        }
    }

    input_chain.push("split=2[base][zoom]".to_string());

    let post_camera_chain = if let Some(cursor_overlay_filter) = cursor_overlay_filter {
        format!(
            "[cam]scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black[framed];\
             {cursor_overlay_filter}",
            w = target_width,
            h = target_height,
            cursor_overlay_filter = cursor_overlay_filter
        )
    } else {
        format!(
            "[cam]scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black[vout]",
            w = target_width,
            h = target_height
        )
    };

    let graph = format!(
        "{input};\
         [zoom]scale=w='iw*({zoom})':h='ih*({zoom})':eval=frame[scaled];\
         [base][scaled]overlay=x='-max(0,min({x},overlay_w-main_w))':y='-max(0,min({y},overlay_h-main_h))':eval=frame[cam];\
         {post_camera}",
        input = input_chain.join(","),
        zoom = zoom_expr,
        x = offset_x_expr,
        y = offset_y_expr,
        post_camera = post_camera_chain
    );

    Ok((graph, cursor_input_path, cursor_temp_file))
}

fn build_camera_states(
    project: &Project,
    source_duration_ms: u64,
    project_duration_ms: u64,
    source_width: u32,
    source_height: u32,
    source_fps: f64,
) -> Vec<CameraState> {
    let safe_fps = source_fps.max(1.0);
    let runtime_segments = build_runtime_segments(project);
    let mut anchors = vec![0, project_duration_ms];
    for segment in &runtime_segments {
        anchors.push(segment.start_ts);
        anchors.push(segment.end_ts);
        anchors.extend(segment.target_points.iter().map(|point| point.ts));
    }
    anchors.sort_unstable();
    anchors.dedup();

    let sw = source_width as f64;
    let sh = source_height as f64;
    let full_rect = NormalizedRect {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };
    let default_camera = rect_to_camera_values(full_rect, sw, sh);
    let default_spring = default_spring_params();
    let mut zoom_state = AxisSpringState {
        value: default_camera.0,
        velocity: 0.0,
    };
    let mut offset_x_state = AxisSpringState {
        value: default_camera.1,
        velocity: 0.0,
    };
    let mut offset_y_state = AxisSpringState {
        value: default_camera.2,
        velocity: 0.0,
    };

    let mut states: Vec<CameraState> = Vec::new();
    for pair in anchors.windows(2) {
        let start_ts = pair[0];
        let end_ts = pair[1];
        if end_ts <= start_ts {
            continue;
        }

        let (target_camera, spring) =
            if let Some(segment) = resolve_runtime_segment(&runtime_segments, start_ts) {
                let target_rect = target_rect_at_ts(segment, start_ts);
                (rect_to_camera_values(target_rect, sw, sh), segment.spring)
            } else {
                (default_camera, default_spring)
            };

        let start_ms = map_time_ms(start_ts, project_duration_ms, source_duration_ms);
        let end_ms = map_time_ms(end_ts, project_duration_ms, source_duration_ms);
        if end_ms <= start_ms {
            continue;
        }

        let start_frame = start_ms as f64 / 1000.0 * safe_fps;
        let end_frame = end_ms as f64 / 1000.0 * safe_fps;
        if end_frame <= start_frame {
            continue;
        }

        states.push(CameraState {
            start_frame,
            end_frame,
            spring,
            zoom: AxisSpringSegment {
                start: zoom_state.value,
                velocity: zoom_state.velocity,
                target: target_camera.0,
            },
            offset_x: AxisSpringSegment {
                start: offset_x_state.value,
                velocity: offset_x_state.velocity,
                target: target_camera.1,
            },
            offset_y: AxisSpringSegment {
                start: offset_y_state.value,
                velocity: offset_y_state.velocity,
                target: target_camera.2,
            },
        });

        let dt_seconds = (end_frame - start_frame).max(0.0) / safe_fps;
        zoom_state = evaluate_spring_axis(zoom_state, target_camera.0, spring, dt_seconds);
        offset_x_state = evaluate_spring_axis(offset_x_state, target_camera.1, spring, dt_seconds);
        offset_y_state = evaluate_spring_axis(offset_y_state, target_camera.2, spring, dt_seconds);
    }

    states
}

fn build_runtime_segments(project: &Project) -> Vec<SegmentRuntime> {
    let mut segments = project.timeline.zoom_segments.clone();
    segments.sort_by_key(|segment| segment.start_ts);

    let mut runtime: Vec<SegmentRuntime> = Vec::new();
    for segment in segments {
        let start_ts = segment.start_ts.min(project.duration_ms);
        let end_ts = segment.end_ts.min(project.duration_ms);
        if end_ts <= start_ts {
            continue;
        }

        let base_rect = normalize_segment_rect(segment.initial_rect.clone());
        let target_points = if segment.target_points.is_empty() {
            normalize_target_points(
                target_points_from_legacy_pan(&segment, &base_rect),
                start_ts,
                end_ts,
                &base_rect,
            )
        } else {
            normalize_target_points(segment.target_points.clone(), start_ts, end_ts, &base_rect)
        };

        runtime.push(SegmentRuntime {
            start_ts,
            end_ts,
            base_rect,
            target_points,
            spring: normalize_spring_params(&segment.spring),
        });
    }

    runtime.sort_by_key(|segment| segment.start_ts);
    runtime
}

fn normalize_target_points(
    points: Vec<TargetPoint>,
    start_ts: u64,
    end_ts: u64,
    fallback_rect: &NormalizedRect,
) -> Vec<TargetPoint> {
    let mut normalized = points
        .into_iter()
        .map(|point| TargetPoint {
            ts: point.ts.clamp(start_ts, end_ts),
            rect: normalize_segment_rect(point.rect),
        })
        .collect::<Vec<_>>();
    normalized.sort_by_key(|point| point.ts);

    let mut dedup: Vec<TargetPoint> = Vec::new();
    for point in normalized {
        if let Some(last) = dedup.last_mut() {
            if last.ts == point.ts {
                *last = point;
                continue;
            }
        }
        dedup.push(point);
    }

    if dedup.is_empty() {
        return vec![
            TargetPoint {
                ts: start_ts,
                rect: fallback_rect.clone(),
            },
            TargetPoint {
                ts: end_ts,
                rect: fallback_rect.clone(),
            },
        ];
    }

    if dedup.first().is_some_and(|point| point.ts > start_ts) {
        let rect = dedup[0].rect.clone();
        dedup.insert(0, TargetPoint { ts: start_ts, rect });
    }

    if dedup.last().is_some_and(|point| point.ts < end_ts) {
        let rect = dedup
            .last()
            .expect("target points has last element")
            .rect
            .clone();
        dedup.push(TargetPoint { ts: end_ts, rect });
    }

    dedup
}

fn target_points_from_legacy_pan(
    segment: &ZoomSegment,
    base_rect: &NormalizedRect,
) -> Vec<TargetPoint> {
    let mut pan_trajectory = segment.pan_trajectory.clone();
    pan_trajectory.sort_by_key(|keyframe| keyframe.ts);

    if pan_trajectory.is_empty() {
        return vec![
            TargetPoint {
                ts: segment.start_ts,
                rect: base_rect.clone(),
            },
            TargetPoint {
                ts: segment.end_ts,
                rect: base_rect.clone(),
            },
        ];
    }

    let (start_offset_x, start_offset_y) = pan_offset_at_ts(&pan_trajectory, segment.start_ts);
    let mut points = vec![TargetPoint {
        ts: segment.start_ts,
        rect: apply_pan_offset(base_rect, start_offset_x, start_offset_y),
    }];

    for keyframe in &pan_trajectory {
        if keyframe.ts < segment.start_ts || keyframe.ts > segment.end_ts {
            continue;
        }
        points.push(TargetPoint {
            ts: keyframe.ts,
            rect: apply_pan_offset(base_rect, keyframe.offset_x, keyframe.offset_y),
        });
    }

    let (end_offset_x, end_offset_y) = pan_offset_at_ts(&pan_trajectory, segment.end_ts);
    points.push(TargetPoint {
        ts: segment.end_ts,
        rect: apply_pan_offset(base_rect, end_offset_x, end_offset_y),
    });
    points
}

fn resolve_runtime_segment<'a>(
    segments: &'a [SegmentRuntime],
    ts: u64,
) -> Option<&'a SegmentRuntime> {
    segments
        .iter()
        .rev()
        .find(|segment| ts >= segment.start_ts && ts < segment.end_ts)
}

fn target_rect_at_ts(segment: &SegmentRuntime, ts: u64) -> NormalizedRect {
    if segment.target_points.is_empty() {
        return segment.base_rect.clone();
    }
    if ts <= segment.target_points[0].ts {
        return segment.target_points[0].rect.clone();
    }
    for point in segment.target_points.iter().rev() {
        if ts >= point.ts {
            return point.rect.clone();
        }
    }
    segment.target_points[0].rect.clone()
}

fn default_spring_params() -> SpringParams {
    SpringParams {
        mass: DEFAULT_SPRING_MASS,
        stiffness: DEFAULT_SPRING_STIFFNESS,
        damping: DEFAULT_SPRING_DAMPING,
    }
}

fn normalize_spring_params(spring: &CameraSpring) -> SpringParams {
    SpringParams {
        mass: spring.mass.max(0.001),
        stiffness: spring.stiffness.max(0.001),
        damping: spring.damping.max(0.0),
    }
}

fn evaluate_spring_axis(
    state: AxisSpringState,
    target: f64,
    spring: SpringParams,
    dt_seconds: f64,
) -> AxisSpringState {
    let dt = dt_seconds.max(0.0);
    if dt <= 0.0 {
        return state;
    }

    let mass = spring.mass.max(0.001);
    let stiffness = spring.stiffness.max(0.001);
    let damping = spring.damping.max(0.0);
    let y0 = state.value - target;
    let v0 = state.velocity;
    let alpha = damping / (2.0 * mass);
    let omega_sq = stiffness / mass;
    let discriminant = alpha * alpha - omega_sq;

    let (y, v) = if discriminant.abs() <= 1e-9 {
        let c2 = v0 + alpha * y0;
        let exp = (-alpha * dt).exp();
        let y = (y0 + c2 * dt) * exp;
        let v = (v0 - alpha * c2 * dt) * exp;
        (y, v)
    } else if discriminant > 0.0 {
        let sqrt_disc = discriminant.sqrt();
        let r1 = -alpha + sqrt_disc;
        let r2 = -alpha - sqrt_disc;
        let denom = (r1 - r2).abs().max(1e-9);
        let c1 = (v0 - r2 * y0) / denom;
        let c2 = y0 - c1;
        let exp1 = (r1 * dt).exp();
        let exp2 = (r2 * dt).exp();
        let y = c1 * exp1 + c2 * exp2;
        let v = c1 * r1 * exp1 + c2 * r2 * exp2;
        (y, v)
    } else {
        let beta = (omega_sq - alpha * alpha).max(1e-9).sqrt();
        let c1 = y0;
        let c2 = (v0 + alpha * y0) / beta;
        let exp = (-alpha * dt).exp();
        let cos = (beta * dt).cos();
        let sin = (beta * dt).sin();
        let y = exp * (c1 * cos + c2 * sin);
        let v = exp * ((-alpha) * (c1 * cos + c2 * sin) + (-c1 * beta * sin + c2 * beta * cos));
        (y, v)
    };

    AxisSpringState {
        value: target + y,
        velocity: v,
    }
}

fn build_camera_value_expr(
    states: &[CameraState],
    axis: impl Fn(&CameraState) -> AxisSpringSegment + Copy,
    default_value: f64,
    source_fps: f64,
) -> String {
    let mut ordered = states.to_vec();
    ordered.sort_by(|left, right| {
        left.start_frame
            .total_cmp(&right.start_frame)
            .then_with(|| left.end_frame.total_cmp(&right.end_frame))
    });
    let safe_fps = source_fps.max(1.0);

    if ordered.len() > MAX_CAMERA_STATES_FOR_ANALYTIC_EXPR {
        let sampled = sample_camera_value_points(&ordered, axis, default_value, safe_fps);
        let reduced = decimate_time_value_points(&sampled, MAX_CAMERA_POINTS_FOR_EXPR);
        let duration_ms = reduced.last().map(|(ts, _)| *ts).unwrap_or(0);
        return build_piecewise_track_expr(&reduced, duration_ms);
    }

    let default_expr = format_f64(default_value);
    let mut terms = Vec::with_capacity(ordered.len() + 1);
    terms.push(default_expr.clone());

    for state in ordered {
        let axis_state = axis(&state);
        let elapsed = format!(
            "max(0,(n-{start})/{fps})",
            start = format_f64(state.start_frame),
            fps = format_f64(safe_fps)
        );
        let value = spring_value_expr(&elapsed, axis_state, state.spring);

        // Build a flat sum of disjoint interval terms instead of deeply nested if-expressions.
        // Nested expressions can exceed FFmpeg parser depth on projects with many segments.
        terms.push(format!(
            "if(gte(n,{start})*lt(n,{end}),({value})-({default}),0)",
            start = format_f64(state.start_frame),
            end = format_f64(state.end_frame),
            value = value,
            default = default_expr
        ));
    }

    terms.join("+")
}

fn sample_camera_value_points(
    states: &[CameraState],
    axis: impl Fn(&CameraState) -> AxisSpringSegment + Copy,
    default_value: f64,
    source_fps: f64,
) -> Vec<(u64, f64)> {
    if states.is_empty() {
        return vec![(0, default_value)];
    }

    let safe_fps = source_fps.max(1.0);
    let max_frame = states
        .iter()
        .map(|state| state.end_frame)
        .fold(0.0, f64::max)
        .ceil()
        .max(1.0);
    let step_frames = (safe_fps / CAMERA_FALLBACK_SAMPLE_RATE_HZ.max(1.0)).max(1.0);

    let mut points: Vec<(u64, f64)> = Vec::new();
    let mut frame = 0.0;
    while frame <= max_frame {
        let ts_ms = ((frame / safe_fps) * 1000.0).round().max(0.0) as u64;
        let value = sample_camera_axis_value(states, frame, safe_fps, axis, default_value);
        points.push((ts_ms, value));
        frame += step_frames;
    }

    let last_ts_ms = ((max_frame / safe_fps) * 1000.0).round().max(0.0) as u64;
    if points
        .last()
        .map(|(ts, _)| *ts != last_ts_ms)
        .unwrap_or(true)
    {
        let value = sample_camera_axis_value(states, max_frame, safe_fps, axis, default_value);
        points.push((last_ts_ms, value));
    }

    points.sort_by_key(|(ts, _)| *ts);
    points.dedup_by(|left, right| left.0 == right.0);
    points
}

fn spring_value_expr(
    elapsed_expr: &str,
    axis_state: AxisSpringSegment,
    spring: SpringParams,
) -> String {
    let mass = spring.mass.max(0.001);
    let stiffness = spring.stiffness.max(0.001);
    let damping = spring.damping.max(0.0);
    let y0 = axis_state.start - axis_state.target;
    let v0 = axis_state.velocity;
    let alpha = damping / (2.0 * mass);
    let omega_sq = stiffness / mass;
    let discriminant = alpha * alpha - omega_sq;

    if discriminant.abs() <= 1e-9 {
        let c2 = v0 + alpha * y0;
        return format!(
            "{target}+(({y0})+({c2})*({t}))*exp(-{alpha}*({t}))",
            target = format_f64(axis_state.target),
            y0 = format_f64(y0),
            c2 = format_f64(c2),
            alpha = format_f64(alpha),
            t = elapsed_expr
        );
    }

    if discriminant > 0.0 {
        let sqrt_disc = discriminant.sqrt();
        let r1 = -alpha + sqrt_disc;
        let r2 = -alpha - sqrt_disc;
        let denom = (r1 - r2).abs().max(1e-9);
        let c1 = (v0 - r2 * y0) / denom;
        let c2 = y0 - c1;
        return format!(
            "{target}+({c1})*exp({r1}*({t}))+({c2})*exp({r2}*({t}))",
            target = format_f64(axis_state.target),
            c1 = format_f64(c1),
            c2 = format_f64(c2),
            r1 = format_f64(r1),
            r2 = format_f64(r2),
            t = elapsed_expr
        );
    }

    let beta = (omega_sq - alpha * alpha).max(1e-9).sqrt();
    let c1 = y0;
    let c2 = (v0 + alpha * y0) / beta;
    format!(
        "{target}+exp(-{alpha}*({t}))*(({c1})*cos({beta}*({t}))+({c2})*sin({beta}*({t})))",
        target = format_f64(axis_state.target),
        alpha = format_f64(alpha),
        c1 = format_f64(c1),
        c2 = format_f64(c2),
        beta = format_f64(beta),
        t = elapsed_expr
    )
}

fn rect_to_camera_values(
    rect: NormalizedRect,
    source_width: f64,
    source_height: f64,
) -> (f64, f64, f64) {
    let zoom = (1.0 / rect.width.max(rect.height).max(0.0001)).clamp(1.0, 20.0);
    let crop_w = (source_width / zoom).clamp(32.0, source_width);
    let crop_h = (source_height / zoom).clamp(32.0, source_height);

    let center_x = (rect.x + rect.width / 2.0) * source_width;
    let center_y = (rect.y + rect.height / 2.0) * source_height;
    let crop_x = (center_x - crop_w / 2.0).clamp(0.0, (source_width - crop_w).max(0.0));
    let crop_y = (center_y - crop_h / 2.0).clamp(0.0, (source_height - crop_h).max(0.0));

    let max_offset_x = (source_width * zoom - source_width).max(0.0);
    let max_offset_y = (source_height * zoom - source_height).max(0.0);
    let offset_x = (crop_x * zoom).clamp(0.0, max_offset_x);
    let offset_y = (crop_y * zoom).clamp(0.0, max_offset_y);

    (zoom, offset_x, offset_y)
}

fn normalize_segment_rect(rect: NormalizedRect) -> NormalizedRect {
    let width = rect.width.clamp(0.001, 1.0);
    let height = rect.height.clamp(0.001, 1.0);

    NormalizedRect {
        x: rect.x.clamp(0.0, 1.0 - width),
        y: rect.y.clamp(0.0, 1.0 - height),
        width,
        height,
    }
}

fn apply_pan_offset(base_rect: &NormalizedRect, offset_x: f64, offset_y: f64) -> NormalizedRect {
    let normalized = normalize_segment_rect(base_rect.clone());
    let x = (normalized.x + offset_x).clamp(0.0, 1.0 - normalized.width);
    let y = (normalized.y + offset_y).clamp(0.0, 1.0 - normalized.height);

    NormalizedRect {
        x,
        y,
        width: normalized.width,
        height: normalized.height,
    }
}

fn pan_offset_at_ts(pan_trajectory: &[PanKeyframe], ts: u64) -> (f64, f64) {
    if pan_trajectory.is_empty() {
        return (0.0, 0.0);
    }

    if ts <= pan_trajectory[0].ts {
        return (0.0, 0.0);
    }

    let last = pan_trajectory
        .last()
        .expect("pan trajectory has at least one keyframe");
    if ts >= last.ts {
        return (last.offset_x, last.offset_y);
    }

    for pair in pan_trajectory.windows(2) {
        let left = &pair[0];
        let right = &pair[1];
        if ts < left.ts || ts > right.ts {
            continue;
        }
        let span = right.ts.saturating_sub(left.ts);
        if span == 0 {
            return (right.offset_x, right.offset_y);
        }
        let t = (ts.saturating_sub(left.ts)) as f64 / span as f64;
        return (
            left.offset_x + (right.offset_x - left.offset_x) * t,
            left.offset_y + (right.offset_y - left.offset_y) * t,
        );
    }

    (last.offset_x, last.offset_y)
}

fn format_f64(value: f64) -> String {
    format!("{value:.4}")
}

fn build_cursor_overlay_plan(
    project: &Project,
    events: Option<&EventsFile>,
    camera_states: &[CameraState],
    source_duration_ms: u64,
    project_duration_ms: u64,
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
    render_fps: f64,
) -> Result<Option<CursorOverlayPlan>, String> {
    let Some(events_file) = events else {
        return Ok(None);
    };
    if events_file.events.is_empty() {
        return Ok(None);
    }

    let Some(cursor_asset) = resolve_cursor_asset_for_render()? else {
        return Ok(None);
    };

    let mut points = smooth_cursor_path(
        &events_file.events,
        project.settings.cursor.smoothing_factor,
    );
    if points.is_empty() {
        return Ok(None);
    }

    points.sort_by_key(|point| point.ts);
    let desired_samples = (((source_duration_ms as f64) / 100.0).ceil() as usize)
        .clamp(24, MAX_CURSOR_SAMPLES_FOR_EXPR)
        .max(2);
    let frame_count = desired_samples - 1;
    let frame_step_ms = (source_duration_ms as f64 / frame_count as f64).max(1.0);

    let raw_click_times: Vec<u64> = events_file
        .events
        .iter()
        .filter_map(|event| match event {
            InputEvent::Click { ts, .. } => {
                Some(apply_cursor_timing_offset_ms(
                    map_time_ms(*ts, project_duration_ms, source_duration_ms),
                    source_duration_ms,
                ))
            }
            _ => None,
        })
        .collect();
    let click_times = compact_click_times(
        &decimate_u64_points(&raw_click_times, MAX_CLICK_EVENTS_FOR_EXPR),
        MIN_CLICK_PULSE_GAP_MS,
    );

    let target_min_side = target_width.min(target_height).max(1) as f64;
    let cursor_height_px =
        (project.settings.cursor.size * target_min_side * CURSOR_SIZE_TO_FRAME_RATIO)
            .clamp(8.0, 280.0);

    let screen_w = events_file.screen_width.max(1) as f64;
    let screen_h = events_file.screen_height.max(1) as f64;
    let src_w = source_width as f64;
    let src_h = source_height as f64;
    let dst_w = target_width as f64;
    let dst_h = target_height as f64;
    let mut mapped_points: Vec<(u64, f64, f64)> = points
        .into_iter()
        .map(|point| {
            (
                apply_cursor_timing_offset_ms(
                    map_time_ms(point.ts, project_duration_ms, source_duration_ms),
                    source_duration_ms,
                ),
                (point.x / screen_w * src_w).clamp(0.0, src_w),
                (point.y / screen_h * src_h).clamp(0.0, src_h),
            )
        })
        .collect();
    mapped_points.sort_by_key(|point| point.0);
    mapped_points.dedup_by(|current, next| current.0 == next.0);

    if mapped_points.is_empty() {
        return Err("No mapped cursor points for export".to_string());
    }
    if mapped_points.len() == 1 {
        let only = mapped_points[0];
        mapped_points.push((source_duration_ms, only.1, only.2));
    }

    let mut sampled: Vec<(u64, f64, f64)> = Vec::with_capacity(frame_count + 1);
    for frame in 0..=frame_count {
        let frame_ms = ((frame as f64) * frame_step_ms)
            .round()
            .clamp(0.0, source_duration_ms as f64) as u64;
        let frame_no = frame as f64;
        let (src_x, src_y) = interpolate_cursor_position(&mapped_points, frame_ms);
        let zoom =
            sample_camera_axis_value(camera_states, frame_no, render_fps, |state| state.zoom, 1.0);
        let offset_x = sample_camera_axis_value(
            camera_states,
            frame_no,
            render_fps,
            |state| state.offset_x,
            0.0,
        );
        let offset_y = sample_camera_axis_value(
            camera_states,
            frame_no,
            render_fps,
            |state| state.offset_y,
            0.0,
        );

        let (x, y) = map_cursor_to_output_space(
            src_x, src_y, zoom, offset_x, offset_y, src_w, src_h, dst_w, dst_h,
        );
        sampled.push((frame_ms, x, y));
    }

    sampled.dedup_by(|left, right| {
        left.0 == right.0 && (left.1 - right.1).abs() < 0.1 && (left.2 - right.2).abs() < 0.1
    });
    let sampled = decimate_cursor_samples(&sampled, MAX_CURSOR_SAMPLES_FOR_EXPR);

    let x_points: Vec<(u64, f64)> = sampled.iter().map(|(ts, x, _)| (*ts, *x)).collect();
    let y_points: Vec<(u64, f64)> = sampled.iter().map(|(ts, _, y)| (*ts, *y)).collect();

    let x_track_expr = build_piecewise_track_expr(&x_points, source_duration_ms);
    let y_track_expr = build_piecewise_track_expr(&y_points, source_duration_ms);

    let base_cursor_scale = cursor_height_px / cursor_asset.height.max(1) as f64;
    let pulse_factor_expr = build_click_pulse_factor_expr(&click_times);
    let scale_expr = format!(
        "({base_scale})*({pulse_factor})",
        base_scale = format_f64(base_cursor_scale),
        pulse_factor = pulse_factor_expr
    );

    let cursor_width_expr = format!(
        "max(2,{asset_w}*({scale}))",
        asset_w = format_f64(cursor_asset.width as f64),
        scale = scale_expr
    );
    let cursor_height_expr = format!(
        "max(2,{asset_h}*({scale}))",
        asset_h = format_f64(cursor_asset.height as f64),
        scale = scale_expr
    );
    let overlay_x_expr = format!(
        "({x})-({hotspot_x})*({scale})",
        x = x_track_expr,
        hotspot_x = format_f64(cursor_asset.hotspot_x),
        scale = scale_expr
    );
    let overlay_y_expr = format!(
        "({y})-({hotspot_y})*({scale})",
        y = y_track_expr,
        hotspot_y = format_f64(cursor_asset.hotspot_y),
        scale = scale_expr
    );

    Ok(Some(CursorOverlayPlan {
        cursor_png_path: cursor_asset.png_path,
        filter_chain: format!(
            "[1:v]format=rgba,scale=w='{w}':h='{h}':eval=frame[cursor];\
             [framed][cursor]overlay=x='{x}':y='{y}':eval=frame:format=auto[vout]",
            w = cursor_width_expr,
            h = cursor_height_expr,
            x = overlay_x_expr,
            y = overlay_y_expr,
        ),
    }))
}

fn build_vector_cursor_ass_file(
    project: &Project,
    events_file: &EventsFile,
    camera_states: &[CameraState],
    source_duration_ms: u64,
    project_duration_ms: u64,
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
    render_fps: f64,
) -> Result<PathBuf, String> {
    let mut points = smooth_cursor_path(
        &events_file.events,
        project.settings.cursor.smoothing_factor,
    );
    if points.is_empty() {
        return Err("No cursor points available for export".to_string());
    }

    points.sort_by_key(|point| point.ts);

    let sample_fps = VECTOR_CURSOR_SAMPLE_FPS.clamp(8.0, 30.0);
    let frame_step_ms = (1000.0 / sample_fps).max(1.0);
    let frame_count = ((source_duration_ms as f64 / frame_step_ms).ceil() as usize).max(2);

    let screen_w = events_file.screen_width.max(1) as f64;
    let screen_h = events_file.screen_height.max(1) as f64;
    let src_w = source_width.max(1) as f64;
    let src_h = source_height.max(1) as f64;
    let dst_w = target_width.max(1) as f64;
    let dst_h = target_height.max(1) as f64;

    let mut mapped_points: Vec<(u64, f64, f64)> = points
        .into_iter()
        .map(|point| {
            (
                apply_cursor_timing_offset_ms(
                    map_time_ms(point.ts, project_duration_ms, source_duration_ms),
                    source_duration_ms,
                ),
                (point.x / screen_w * src_w).clamp(0.0, src_w),
                (point.y / screen_h * src_h).clamp(0.0, src_h),
            )
        })
        .collect();
    mapped_points.sort_by_key(|point| point.0);
    mapped_points.dedup_by(|left, right| left.0 == right.0);
    if mapped_points.is_empty() {
        return Err("No mapped cursor points for export".to_string());
    }
    if mapped_points.len() == 1 {
        let only = mapped_points[0];
        mapped_points.push((source_duration_ms, only.1, only.2));
    }

    let raw_click_times: Vec<u64> = events_file
        .events
        .iter()
        .filter_map(|event| match event {
            InputEvent::Click { ts, .. } => {
                Some(apply_cursor_timing_offset_ms(
                    map_time_ms(*ts, project_duration_ms, source_duration_ms),
                    source_duration_ms,
                ))
            }
            _ => None,
        })
        .collect();
    let click_times = compact_click_times(
        &decimate_u64_points(&raw_click_times, MAX_CLICK_EVENTS_FOR_EXPR),
        MIN_CLICK_PULSE_GAP_MS,
    );

    let mut sampled: Vec<(u64, i64, i64, f64)> = Vec::with_capacity(frame_count + 1);
    for frame in 0..=frame_count {
        let frame_ms = ((frame as f64) * frame_step_ms)
            .round()
            .clamp(0.0, source_duration_ms as f64) as u64;
        let frame_no = (frame_ms as f64 / 1000.0) * render_fps.max(1.0);
        let (src_x, src_y) = interpolate_cursor_position(&mapped_points, frame_ms);
        let zoom = sample_camera_axis_value(
            camera_states,
            frame_no,
            render_fps.max(1.0),
            |state| state.zoom,
            1.0,
        );
        let offset_x = sample_camera_axis_value(
            camera_states,
            frame_no,
            render_fps.max(1.0),
            |state| state.offset_x,
            0.0,
        );
        let offset_y = sample_camera_axis_value(
            camera_states,
            frame_no,
            render_fps.max(1.0),
            |state| state.offset_y,
            0.0,
        );
        let (x, y) = map_cursor_to_output_space(
            src_x, src_y, zoom, offset_x, offset_y, src_w, src_h, dst_w, dst_h,
        );
        let pulse_scale = sample_click_pulse_scale_scalar(&click_times, frame_ms);
        let combined_scale = (zoom.max(1.0) * pulse_scale).clamp(0.5, 4.0);
        sampled.push((frame_ms, x.round() as i64, y.round() as i64, combined_scale));
    }

    sampled.dedup_by(|left, right| {
        left.0 == right.0
            && left.1 == right.1
            && left.2 == right.2
            && (left.3 - right.3).abs() < 0.01
    });
    let sampled = decimate_cursor_samples_scaled(&sampled, 480);

    let target_min_side = target_width.min(target_height).max(1) as f64;
    let cursor_height_px =
        (project.settings.cursor.size * target_min_side * CURSOR_SIZE_TO_FRAME_RATIO)
            .clamp(8.0, 220.0);
    let cursor_scale_percent = (cursor_height_px / VECTOR_CURSOR_ASS_BASE_HEIGHT) * 100.0;
    let cursor_outline_px = (cursor_height_px * 0.08).clamp(1.0, 5.0);

    let ass_path =
        std::env::temp_dir().join(format!("nsc-vcursor-{}-{}.ass", project.id, now_ms()));
    let mut file = File::create(&ass_path)
        .map_err(|e| format!("Failed to create vector cursor ass file: {e}"))?;

    writeln!(file, "[Script Info]").map_err(|e| format!("Failed to write ass header: {e}"))?;
    writeln!(file, "ScriptType: v4.00+").map_err(|e| format!("Failed to write ass header: {e}"))?;
    writeln!(file, "PlayResX: {target_width}")
        .map_err(|e| format!("Failed to write ass header: {e}"))?;
    writeln!(file, "PlayResY: {target_height}")
        .map_err(|e| format!("Failed to write ass header: {e}"))?;
    writeln!(file).map_err(|e| format!("Failed to write ass header: {e}"))?;
    writeln!(file, "[V4+ Styles]").map_err(|e| format!("Failed to write ass styles: {e}"))?;
    writeln!(
        file,
        "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding"
    )
    .map_err(|e| format!("Failed to write ass styles: {e}"))?;
    writeln!(
        file,
        "Style: Cursor,Arial,12,&H00000000,&H00000000,&H00FFFFFF,&H00000000,0,0,0,0,100,100,0,0,1,2,0,7,0,0,0,1"
    )
    .map_err(|e| format!("Failed to write ass styles: {e}"))?;
    writeln!(file).map_err(|e| format!("Failed to write ass styles: {e}"))?;
    writeln!(file, "[Events]").map_err(|e| format!("Failed to write ass events: {e}"))?;
    writeln!(
        file,
        "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text"
    )
    .map_err(|e| format!("Failed to write ass events: {e}"))?;

    for pair in sampled.windows(2) {
        let (start_ms, x1, y1, start_scale) = pair[0];
        let (end_ms, x2, y2, _) = pair[1];
        if end_ms <= start_ms {
            continue;
        }
        let scale_percent = cursor_scale_percent * start_scale;
        let outline_px = cursor_outline_px * start_scale.clamp(0.75, 2.5);

        writeln!(
            file,
            "Dialogue: 0,{},{},Cursor,,0,0,0,,{{\\an7\\p1\\fscx{:.2}\\fscy{:.2}\\bord{:.2}\\shad0\\move({},{},{},{})}}{}",
            format_ass_time(start_ms),
            format_ass_time(end_ms),
            scale_percent,
            scale_percent,
            outline_px,
            x1,
            y1,
            x2,
            y2,
            VECTOR_CURSOR_ASS_PATH
        )
        .map_err(|e| format!("Failed to write ass cursor event: {e}"))?;
    }

    Ok(ass_path)
}

fn decimate_cursor_samples(points: &[(u64, f64, f64)], max_points: usize) -> Vec<(u64, f64, f64)> {
    if points.len() <= max_points || max_points < 2 {
        return points.to_vec();
    }

    let mut result = Vec::with_capacity(max_points);
    let last_index = points.len() - 1;
    let max_index = max_points - 1;
    let mut prev_idx = usize::MAX;

    for index in 0..max_points {
        let sample_idx = index * last_index / max_index;
        if sample_idx == prev_idx {
            continue;
        }
        result.push(points[sample_idx]);
        prev_idx = sample_idx;
    }

    if result.last().map(|point| point.0) != points.last().map(|point| point.0) {
        result.push(points[last_index]);
    }

    result
}

fn decimate_cursor_samples_scaled(
    points: &[(u64, i64, i64, f64)],
    max_points: usize,
) -> Vec<(u64, i64, i64, f64)> {
    if points.len() <= max_points || max_points < 2 {
        return points.to_vec();
    }

    let mut result = Vec::with_capacity(max_points);
    let last_index = points.len() - 1;
    let max_index = max_points - 1;
    let mut prev_idx = usize::MAX;

    for index in 0..max_points {
        let sample_idx = index * last_index / max_index;
        if sample_idx == prev_idx {
            continue;
        }
        result.push(points[sample_idx]);
        prev_idx = sample_idx;
    }

    if result.last().map(|point| point.0) != points.last().map(|point| point.0) {
        result.push(points[last_index]);
    }

    result
}

fn decimate_time_value_points(points: &[(u64, f64)], max_points: usize) -> Vec<(u64, f64)> {
    if points.len() <= max_points || max_points < 2 {
        return points.to_vec();
    }

    let mut result = Vec::with_capacity(max_points);
    let last_index = points.len() - 1;
    let max_index = max_points - 1;
    let mut prev_idx = usize::MAX;

    for index in 0..max_points {
        let sample_idx = index * last_index / max_index;
        if sample_idx == prev_idx {
            continue;
        }
        result.push(points[sample_idx]);
        prev_idx = sample_idx;
    }

    if result.last().map(|point| point.0) != points.last().map(|point| point.0) {
        result.push(points[last_index]);
    }

    result
}

fn decimate_u64_points(points: &[u64], max_points: usize) -> Vec<u64> {
    if points.len() <= max_points || max_points < 2 {
        return points.to_vec();
    }

    let mut result = Vec::with_capacity(max_points);
    let last_index = points.len() - 1;
    let max_index = max_points - 1;
    let mut prev_idx = usize::MAX;

    for index in 0..max_points {
        let sample_idx = index * last_index / max_index;
        if sample_idx == prev_idx {
            continue;
        }
        result.push(points[sample_idx]);
        prev_idx = sample_idx;
    }

    if result.last().copied() != points.last().copied() {
        result.push(points[last_index]);
    }

    result
}

fn compact_click_times(points: &[u64], min_gap_ms: u64) -> Vec<u64> {
    if points.is_empty() {
        return Vec::new();
    }

    let mut sorted = points.to_vec();
    sorted.sort_unstable();

    let mut result = Vec::with_capacity(sorted.len());
    let mut last_kept: Option<u64> = None;
    for ts in sorted {
        if let Some(last) = last_kept {
            if ts.saturating_sub(last) < min_gap_ms {
                continue;
            }
        }
        result.push(ts);
        last_kept = Some(ts);
    }
    result
}

fn build_piecewise_track_expr(points: &[(u64, f64)], duration_ms: u64) -> String {
    if points.is_empty() {
        return "0".to_string();
    }

    let mut normalized = points.to_vec();
    normalized.sort_by_key(|(ts, _)| *ts);
    normalized.dedup_by(|left, right| left.0 == right.0);

    if normalized[0].0 > 0 {
        normalized.insert(0, (0, normalized[0].1));
    }
    if normalized
        .last()
        .is_some_and(|(last_ts, _)| *last_ts < duration_ms)
    {
        let last = *normalized.last().expect("normalized has last");
        normalized.push((duration_ms, last.1));
    }

    let base = normalized[0].1;
    let mut terms = vec![format_f64(base)];

    for pair in normalized.windows(2) {
        let (start_ms, start_value) = pair[0];
        let (end_ms, end_value) = pair[1];
        if end_ms <= start_ms {
            continue;
        }

        let start_s = start_ms as f64 / 1000.0;
        let end_s = end_ms as f64 / 1000.0;
        let span_s = ((end_ms - start_ms) as f64 / 1000.0).max(0.0001);
        let delta = end_value - start_value;
        let interp_expr = format!(
            "({start}+((t-{start_t})/{span})*({delta}))",
            start = format_f64(start_value),
            start_t = format_f64(start_s),
            span = format_f64(span_s),
            delta = format_f64(delta)
        );
        terms.push(format!(
            "if(gte(t,{start})*lt(t,{end}),({interp})-({base}),0)",
            start = format_f64(start_s),
            end = format_f64(end_s),
            interp = interp_expr,
            base = format_f64(base)
        ));
    }

    if let Some((last_ts, last_value)) = normalized.last() {
        terms.push(format!(
            "if(gte(t,{start}),({last})-({base}),0)",
            start = format_f64(*last_ts as f64 / 1000.0),
            last = format_f64(*last_value),
            base = format_f64(base)
        ));
    }

    terms.join("+")
}

fn build_click_pulse_factor_expr(click_times_ms: &[u64]) -> String {
    if click_times_ms.is_empty() {
        return "1".to_string();
    }

    let mut terms = vec!["1".to_string()];
    let amp = 1.0 - CLICK_PULSE_MIN_SCALE;
    let down_s = CLICK_PULSE_DOWN_MS / 1000.0;
    let up_s = (CLICK_PULSE_TOTAL_MS - CLICK_PULSE_DOWN_MS).max(1.0) / 1000.0;

    for click_ms in click_times_ms {
        let click_s = *click_ms as f64 / 1000.0;
        let down_end_s = click_s + down_s;
        let up_end_s = click_s + (CLICK_PULSE_TOTAL_MS / 1000.0);

        let down_expr = format!(
            "1-({amp})*((t-{start})/{down})",
            amp = format_f64(amp),
            start = format_f64(click_s),
            down = format_f64(down_s)
        );
        let up_expr = format!(
            "{min}+({amp})*((t-{start})/{up})",
            min = format_f64(CLICK_PULSE_MIN_SCALE),
            amp = format_f64(amp),
            start = format_f64(down_end_s),
            up = format_f64(up_s)
        );

        terms.push(format!(
            "if(gte(t,{start})*lt(t,{end}),({value})-1,0)",
            start = format_f64(click_s),
            end = format_f64(down_end_s),
            value = down_expr
        ));
        terms.push(format!(
            "if(gte(t,{start})*lt(t,{end}),({value})-1,0)",
            start = format_f64(down_end_s),
            end = format_f64(up_end_s),
            value = up_expr
        ));
    }

    terms.join("+")
}

fn sample_click_pulse_scale_scalar(click_times_ms: &[u64], ts_ms: u64) -> f64 {
    if click_times_ms.is_empty() {
        return 1.0;
    }

    let idx = click_times_ms.partition_point(|&click_ts| click_ts <= ts_ms);
    if idx == 0 {
        return 1.0;
    }

    let click_ts = click_times_ms[idx - 1];
    let dt_ms = ts_ms.saturating_sub(click_ts) as f64;
    if dt_ms < 0.0 || dt_ms > CLICK_PULSE_TOTAL_MS {
        return 1.0;
    }

    if dt_ms <= CLICK_PULSE_DOWN_MS {
        let t = dt_ms / CLICK_PULSE_DOWN_MS.max(1.0);
        return 1.0 - (1.0 - CLICK_PULSE_MIN_SCALE) * t;
    }

    let up_duration = (CLICK_PULSE_TOTAL_MS - CLICK_PULSE_DOWN_MS).max(1.0);
    let t = (dt_ms - CLICK_PULSE_DOWN_MS) / up_duration;
    CLICK_PULSE_MIN_SCALE + (1.0 - CLICK_PULSE_MIN_SCALE) * t
}

fn update_status(state: &Arc<Mutex<ExportStatus>>, updater: impl FnOnce(&mut ExportStatus)) {
    if let Ok(mut status) = state.lock() {
        updater(&mut status);
    }
}

fn resolve_project_file(path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("Project path is empty".to_string());
    }

    let input = PathBuf::from(trimmed);
    if input
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    {
        Ok(input)
    } else {
        Ok(input.join("project.json"))
    }
}

fn load_project_file(path: &Path) -> Result<Project, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read project file {}: {e}", path.display()))?;
    let project: Project = serde_json::from_str(&raw)
        .map_err(|e| format!("Failed to parse project file {}: {e}", path.display()))?;

    if project.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "Unsupported project schemaVersion: expected {}, got {}",
            SCHEMA_VERSION, project.schema_version
        ));
    }

    Ok(project)
}

fn load_events_file(project_dir: &Path, events_path: &str) -> Result<EventsFile, String> {
    let path = resolve_media_path(project_dir, events_path)?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read events file {}: {e}", path.display()))?;
    let events: EventsFile = serde_json::from_str(&raw)
        .map_err(|e| format!("Failed to parse events file {}: {e}", path.display()))?;

    if events.schema_version != EVENTS_SCHEMA_VERSION {
        return Err(format!(
            "Unsupported events schemaVersion: expected {}, got {}",
            EVENTS_SCHEMA_VERSION, events.schema_version
        ));
    }

    Ok(events)
}

fn resolve_media_path(project_dir: &Path, raw_path: &str) -> Result<PathBuf, String> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return Err("Project videoPath is empty".to_string());
    }

    let candidate = PathBuf::from(trimmed);
    if candidate.is_absolute() {
        Ok(candidate)
    } else {
        Ok(project_dir.join(candidate))
    }
}

fn resolve_output_path(
    project_dir: &Path,
    project_id: &str,
    output_path: Option<String>,
) -> Result<PathBuf, String> {
    if let Some(raw) = output_path {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }

    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    Ok(project_dir.join(format!("export-{project_id}-{timestamp}.mp4")))
}

fn map_time_ms(ts: u64, from_duration_ms: u64, to_duration_ms: u64) -> u64 {
    if from_duration_ms == 0 || to_duration_ms == 0 {
        return 0;
    }
    let mapped = (ts as f64 / from_duration_ms as f64) * to_duration_ms as f64;
    mapped.round().clamp(0.0, to_duration_ms as f64) as u64
}

fn apply_cursor_timing_offset_ms(ts_ms: u64, duration_ms: u64) -> u64 {
    ts_ms
        .saturating_add(CURSOR_TIMING_OFFSET_MS)
        .min(duration_ms)
}

fn format_ass_time(ms: u64) -> String {
    let total_centis = ms / 10;
    let centis = total_centis % 100;
    let total_seconds = total_centis / 100;
    let seconds = total_seconds % 60;
    let total_minutes = total_seconds / 60;
    let minutes = total_minutes % 60;
    let hours = total_minutes / 60;
    format!("{hours}:{minutes:02}:{seconds:02}.{centis:02}")
}

fn escape_filter_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .replace(':', "\\:")
        .replace('\'', "\\'")
}

fn interpolate_cursor_position(points: &[(u64, f64, f64)], ts: u64) -> (f64, f64) {
    if points.is_empty() {
        return (0.0, 0.0);
    }
    if ts <= points[0].0 {
        return (points[0].1, points[0].2);
    }
    let last = points[points.len() - 1];
    if ts >= last.0 {
        return (last.1, last.2);
    }

    let mut low = 0usize;
    let mut high = points.len() - 1;
    while low <= high {
        let mid = (low + high) / 2;
        if points[mid].0 == ts {
            return (points[mid].1, points[mid].2);
        }
        if points[mid].0 < ts {
            low = mid + 1;
        } else if mid == 0 {
            break;
        } else {
            high = mid - 1;
        }
    }

    let next = points[low.min(points.len() - 1)];
    let prev = points[low.saturating_sub(1)];
    let span = next.0.saturating_sub(prev.0);
    if span == 0 {
        return (prev.1, prev.2);
    }
    let t = (ts.saturating_sub(prev.0)) as f64 / span as f64;
    (
        prev.1 + (next.1 - prev.1) * t,
        prev.2 + (next.2 - prev.2) * t,
    )
}

fn sample_camera_axis_value(
    states: &[CameraState],
    frame: f64,
    source_fps: f64,
    axis: impl Fn(&CameraState) -> AxisSpringSegment,
    default_value: f64,
) -> f64 {
    let safe_fps = source_fps.max(1.0);
    for state in states {
        if frame < state.start_frame || frame >= state.end_frame {
            continue;
        }
        let elapsed_seconds = ((frame - state.start_frame) / safe_fps).max(0.0);
        let axis_state = axis(state);
        let evaluated = evaluate_spring_axis(
            AxisSpringState {
                value: axis_state.start,
                velocity: axis_state.velocity,
            },
            axis_state.target,
            state.spring,
            elapsed_seconds,
        );
        return evaluated.value;
    }
    default_value
}

fn map_cursor_to_output_space(
    source_x: f64,
    source_y: f64,
    zoom: f64,
    offset_x: f64,
    offset_y: f64,
    source_width: f64,
    source_height: f64,
    target_width: f64,
    target_height: f64,
) -> (f64, f64) {
    let safe_zoom = zoom.max(1.0);
    let scaled_width = source_width * safe_zoom;
    let scaled_height = source_height * safe_zoom;
    let max_offset_x = (scaled_width - source_width).max(0.0);
    let max_offset_y = (scaled_height - source_height).max(0.0);
    let clamped_offset_x = offset_x.clamp(0.0, max_offset_x);
    let clamped_offset_y = offset_y.clamp(0.0, max_offset_y);

    let camera_x = (source_x * safe_zoom - clamped_offset_x).clamp(0.0, source_width);
    let camera_y = (source_y * safe_zoom - clamped_offset_y).clamp(0.0, source_height);

    let fit_scale = (target_width / source_width)
        .min(target_height / source_height)
        .max(0.0001);
    let fitted_width = source_width * fit_scale;
    let fitted_height = source_height * fit_scale;
    let pad_x = (target_width - fitted_width) * 0.5;
    let pad_y = (target_height - fitted_height) * 0.5;

    (
        (camera_x * fit_scale + pad_x).clamp(0.0, target_width),
        (camera_y * fit_scale + pad_y).clamp(0.0, target_height),
    )
}

fn probe_media_info(source_video: &Path) -> MediaProbe {
    let ffmpeg = find_ffmpeg_exe();
    let mut command = Command::new(ffmpeg);
    apply_no_window_flags(&mut command);

    let output = command
        .arg("-i")
        .arg(source_video)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .ok();

    let Some(output) = output else {
        return MediaProbe::default();
    };

    let text = String::from_utf8_lossy(&output.stderr);
    let mut probe = MediaProbe::default();

    for line in text.lines() {
        if probe.duration_ms.is_none() {
            probe.duration_ms = extract_ffmpeg_duration_ms(line);
        }
        if probe.width.is_none() || probe.height.is_none() {
            if let Some((w, h)) = extract_ffmpeg_video_size(line) {
                probe.width = Some(w);
                probe.height = Some(h);
            }
        }
        if probe.duration_ms.is_some() && probe.width.is_some() && probe.height.is_some() {
            break;
        }
    }

    probe
}

fn extract_ffmpeg_duration_ms(line: &str) -> Option<u64> {
    let marker = "Duration: ";
    let start = line.find(marker)? + marker.len();
    let value = line[start..].split(',').next()?.trim();
    parse_hhmmss_ms(value)
}

fn extract_ffmpeg_time_ms(line: &str) -> Option<u64> {
    let marker = "time=";
    let start = line.find(marker)? + marker.len();
    let value = line[start..].split_whitespace().next()?;
    parse_hhmmss_ms(value)
}

fn extract_ffmpeg_video_size(line: &str) -> Option<(u32, u32)> {
    if !line.contains(" Video: ") {
        return None;
    }

    for token in line.split(|c: char| c.is_whitespace() || c == ',' || c == '[' || c == ']') {
        let Some((raw_w, raw_h)) = token.split_once('x') else {
            continue;
        };

        let width_text = raw_w.trim_matches(|c: char| !c.is_ascii_digit());
        let height_text = raw_h.trim_matches(|c: char| !c.is_ascii_digit());
        if width_text.is_empty() || height_text.is_empty() {
            continue;
        }

        let width = match width_text.parse::<u32>() {
            Ok(value) => value,
            Err(_) => continue,
        };
        let height = match height_text.parse::<u32>() {
            Ok(value) => value,
            Err(_) => continue,
        };

        if width >= 64 && height >= 64 {
            return Some((width, height));
        }
    }

    None
}

#[cfg(test)]
fn extract_ffmpeg_fps(line: &str) -> Option<f64> {
    if !line.contains(" Video: ") || !line.contains(" fps") {
        return None;
    }

    for chunk in line.split(',') {
        let trimmed = chunk.trim();
        if let Some(value) = trimmed.strip_suffix(" fps") {
            if let Ok(parsed) = value.trim().parse::<f64>() {
                if (1.0..=240.0).contains(&parsed) {
                    return Some(parsed);
                }
            }
        }
    }

    None
}

fn parse_hhmmss_ms(value: &str) -> Option<u64> {
    let mut parts = value.split(':');
    let hours = parts.next()?.parse::<u64>().ok()?;
    let minutes = parts.next()?.parse::<u64>().ok()?;
    let sec_part = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    let mut sec_split = sec_part.split('.');
    let seconds = sec_split.next()?.parse::<u64>().ok()?;
    let frac = sec_split.next().unwrap_or("0");
    let frac_trimmed = &frac[..frac.len().min(3)];
    let millis = format!("{:0<3}", frac_trimmed).parse::<u64>().ok()?;

    Some(hours * 3_600_000 + minutes * 60_000 + seconds * 1_000 + millis)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::project::{
        Background, CameraSpring, CursorSettings, ExportSettings, NormalizedRect, ProjectSettings,
        Timeline, ZoomMode, ZoomSegment, ZoomTrigger,
    };

    fn sample_project() -> Project {
        Project {
            schema_version: SCHEMA_VERSION,
            id: "test-project".to_string(),
            name: "Test".to_string(),
            created_at: 0,
            video_path: "raw.mp4".to_string(),
            proxy_video_path: None,
            events_path: "events.json".to_string(),
            duration_ms: 10_000,
            video_width: 1920,
            video_height: 1080,
            timeline: Timeline {
                zoom_segments: vec![ZoomSegment {
                    id: "z1".to_string(),
                    start_ts: 1_000,
                    end_ts: 2_000,
                    initial_rect: NormalizedRect {
                        x: 0.4,
                        y: 0.3,
                        width: 0.2,
                        height: 0.2,
                    },
                    target_points: vec![],
                    spring: CameraSpring {
                        mass: 1.0,
                        stiffness: 170.0,
                        damping: 26.0,
                    },
                    pan_trajectory: vec![],
                    legacy_easing: None,
                    mode: ZoomMode::Fixed,
                    trigger: ZoomTrigger::AutoClick,
                    is_auto: true,
                }],
            },
            settings: ProjectSettings {
                cursor: CursorSettings::default(),
                background: Background::default(),
                export: ExportSettings::default(),
            },
        }
    }

    fn zoom_segment(id: &str, start_ts: u64, end_ts: u64, rect: NormalizedRect) -> ZoomSegment {
        ZoomSegment {
            id: id.to_string(),
            start_ts,
            end_ts,
            initial_rect: rect,
            target_points: vec![],
            spring: CameraSpring {
                mass: 1.0,
                stiffness: 170.0,
                damping: 26.0,
            },
            pan_trajectory: vec![],
            legacy_easing: None,
            mode: ZoomMode::Fixed,
            trigger: ZoomTrigger::AutoClick,
            is_auto: true,
        }
    }

    #[test]
    fn filter_graph_uses_dynamic_zoom_pipeline() {
        let project = sample_project();
        let (graph, cursor_file, temp_file) =
            build_export_filter_graph(&project, None, 1920, 1080, 30, 10_000, 1920, 1080)
                .expect("filter graph");

        assert!(cursor_file.is_none());
        assert!(temp_file.is_none());
        assert!(graph.contains("split=2[base][zoom]"));
        assert!(graph.contains("scale=w='iw*("));
        assert!(graph.contains("exp("));
        assert!(graph.contains("eval=frame"));
        assert!(graph.contains("[base][scaled]overlay=x='-max(0,min("));
        assert!(graph.contains("overlay_h-main_h"));
        assert!(graph.contains("fps=30"));
    }

    #[test]
    fn camera_returns_to_fullscreen_between_separated_segments() {
        let mut project = sample_project();
        project.timeline.zoom_segments = vec![
            zoom_segment(
                "z1",
                1_000,
                2_000,
                NormalizedRect {
                    x: 0.4,
                    y: 0.3,
                    width: 0.2,
                    height: 0.2,
                },
            ),
            zoom_segment(
                "z2",
                4_000,
                5_000,
                NormalizedRect {
                    x: 0.2,
                    y: 0.2,
                    width: 0.25,
                    height: 0.25,
                },
            ),
        ];

        let states = build_camera_states(&project, 10_000, 10_000, 1_920, 1_080, 30.0);
        let gap_state = states
            .iter()
            .find(|state| state.start_frame >= 60.0 - 0.01 && state.start_frame <= 60.0 + 0.01)
            .expect("expected camera state at first segment end");

        assert!((gap_state.zoom.target - 1.0).abs() < 0.0001);
        assert!(gap_state.offset_x.target.abs() < 0.0001);
        assert!(gap_state.offset_y.target.abs() < 0.0001);
    }

    #[test]
    fn ffmpeg_video_size_parser_handles_common_line() {
        let line = "  Stream #0:0: Video: h264, yuv420p(progressive), 1920x1080, 30 fps";
        assert_eq!(extract_ffmpeg_video_size(line), Some((1920, 1080)));
    }

    #[test]
    fn ffmpeg_fps_parser_handles_common_line() {
        let line = "  Stream #0:0: Video: h264, yuv420p(progressive), 1920x1080, 29.97 fps, 30 tbr";
        let fps = extract_ffmpeg_fps(line).expect("fps");
        assert!((fps - 29.97).abs() < 0.0001);
    }

    #[test]
    fn cursor_interpolation_is_linear_between_points() {
        let points = vec![(0, 0.0, 0.0), (100, 100.0, 50.0)];
        let (x, y) = interpolate_cursor_position(&points, 50);
        assert!((x - 50.0).abs() < 0.0001);
        assert!((y - 25.0).abs() < 0.0001);
    }
}

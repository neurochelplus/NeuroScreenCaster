use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use serde::Serialize;

use crate::algorithm::cursor_smoothing::smooth_cursor_path;
use crate::capture::recorder::find_ffmpeg_exe;
use crate::models::events::{EventsFile, SCHEMA_VERSION as EVENTS_SCHEMA_VERSION};
use crate::models::project::{NormalizedRect, PanKeyframe, Project, ZoomEasing, SCHEMA_VERSION};

const CAMERA_TRANSITION_MS: f64 = 180.0;

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

#[derive(Debug, Clone)]
struct CameraState {
    start_frame: f64,
    end_frame: f64,
    from_zoom: f64,
    to_zoom: f64,
    from_offset_x: f64,
    to_offset_x: f64,
    from_offset_y: f64,
    to_offset_y: f64,
    easing: ZoomEasing,
}

#[derive(Debug, Clone, Copy, Default)]
struct MediaProbe {
    duration_ms: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
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

    let (filter_graph, cursor_ass_file) = match filter_build {
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
        &output_video,
        &filter_graph,
        &codec,
        source_duration_ms,
    );

    if let Some(path) = cursor_ass_file {
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
    output_video: &Path,
    filter_graph: &str,
    codec: &str,
    source_duration_ms: u64,
) -> Result<(), String> {
    let ffmpeg = find_ffmpeg_exe();

    let mut command = Command::new(&ffmpeg);
    command
        .arg("-y")
        .arg("-i")
        .arg(source_video)
        .arg("-vf")
        .arg(filter_graph)
        .arg("-an");

    match codec {
        "h264" => {
            command
                .arg("-c:v")
                .arg("libx264")
                .arg("-preset")
                .arg("medium")
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
                .arg("medium")
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
        _ => return Err(format!("Unsupported codec: {codec}")),
    };

    let mut child = command
        .arg(output_video)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
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

    let exit_status = child
        .wait()
        .map_err(|e| format!("Failed to wait for FFmpeg export: {e}"))?;

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
        if stderr_excerpt.is_empty() {
            return Err(format!("FFmpeg export failed with status: {exit_status}"));
        }
        return Err(format!(
            "FFmpeg export failed with status: {exit_status}\n{}",
            stderr_excerpt.join("\n")
        ));
    }

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
) -> Result<(String, Option<PathBuf>), String> {
    let render_fps = target_fps as f64;
    let camera_states = build_camera_states(
        project,
        source_duration_ms,
        project.duration_ms.max(1),
        source_width.max(1),
        source_height.max(1),
        render_fps,
    );

    let zoom_expr = build_camera_value_expr(
        &camera_states,
        |state| state.from_zoom,
        |state| state.to_zoom,
        1.0,
    );
    let offset_x_expr = build_camera_value_expr(
        &camera_states,
        |state| state.from_offset_x,
        |state| state.to_offset_x,
        0.0,
    );
    let offset_y_expr = build_camera_value_expr(
        &camera_states,
        |state| state.from_offset_y,
        |state| state.to_offset_y,
        0.0,
    );

    let mut input_chain: Vec<String> = Vec::new();
    let mut cursor_ass_file = None;

    // Upsample to target FPS before camera transforms to match preview smoothness.
    input_chain.push(format!("fps={target_fps}"));

    if let Some(events_file) = events {
        if !events_file.events.is_empty() {
            let ass = build_cursor_ass_file(
                project,
                events_file,
                source_duration_ms,
                project.duration_ms.max(1),
                source_width.max(1),
                source_height.max(1),
                render_fps,
            )?;
            let escaped = escape_filter_path(&ass);
            input_chain.push(format!("subtitles=filename='{escaped}'"));
            cursor_ass_file = Some(ass);
        }
    }

    input_chain.push("split=2[base][zoom]".to_string());

    let graph = format!(
        "{input};\
         [zoom]scale=w='iw*({zoom})':h='ih*({zoom})':eval=frame[scaled];\
         [base][scaled]overlay=x='-max(0,min({x},overlay_w-main_w))':y='-max(0,min({y},overlay_h-main_h))':eval=frame[cam];\
         [cam]scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black",
        input = input_chain.join(","),
        zoom = zoom_expr,
        x = offset_x_expr,
        y = offset_y_expr,
        w = target_width,
        h = target_height
    );

    Ok((graph, cursor_ass_file))
}

fn build_camera_states(
    project: &Project,
    source_duration_ms: u64,
    project_duration_ms: u64,
    source_width: u32,
    source_height: u32,
    source_fps: f64,
) -> Vec<CameraState> {
    let mut segments = project.timeline.zoom_segments.clone();
    segments.sort_by_key(|segment| segment.start_ts);

    let sw = source_width as f64;
    let sh = source_height as f64;
    let transition_project_ms = CAMERA_TRANSITION_MS.round().max(1.0) as u64;
    let mut states = Vec::new();
    let default_camera = (1.0, 0.0, 0.0);

    for segment in segments {
        let start_ts = segment.start_ts.min(project_duration_ms);
        let end_ts = segment.end_ts.min(project_duration_ms);
        if end_ts <= start_ts {
            continue;
        }

        let entry_end_ts = (start_ts.saturating_add(transition_project_ms)).min(end_ts);
        let exit_start_ts = if end_ts > entry_end_ts {
            end_ts
                .saturating_sub(transition_project_ms)
                .max(entry_end_ts)
        } else {
            entry_end_ts
        };

        let mut pan_trajectory = segment.pan_trajectory.clone();
        pan_trajectory.sort_by_key(|keyframe| keyframe.ts);
        let base_rect = normalize_segment_rect(segment.initial_rect.clone());
        let rect_at_ts = |ts: u64| {
            let (offset_x, offset_y) = pan_offset_at_ts(&pan_trajectory, ts);
            apply_pan_offset(&base_rect, offset_x, offset_y)
        };

        let initial_camera = rect_to_camera_values(rect_at_ts(start_ts), sw, sh);
        push_camera_state(
            &mut states,
            start_ts,
            entry_end_ts,
            project_duration_ms,
            source_duration_ms,
            source_fps,
            default_camera,
            initial_camera,
            segment.easing.clone(),
        );

        if exit_start_ts > entry_end_ts {
            let mut anchors = vec![entry_end_ts];
            anchors.extend(
                pan_trajectory
                    .iter()
                    .filter(|keyframe| keyframe.ts > entry_end_ts && keyframe.ts < exit_start_ts)
                    .map(|keyframe| keyframe.ts),
            );
            anchors.push(exit_start_ts);
            anchors.sort_unstable();
            anchors.dedup();

            for pair in anchors.windows(2) {
                let from_ts = pair[0];
                let to_ts = pair[1];
                if to_ts <= from_ts {
                    continue;
                }

                let from_camera = rect_to_camera_values(rect_at_ts(from_ts), sw, sh);
                let to_camera = rect_to_camera_values(rect_at_ts(to_ts), sw, sh);
                push_camera_state(
                    &mut states,
                    from_ts,
                    to_ts,
                    project_duration_ms,
                    source_duration_ms,
                    source_fps,
                    from_camera,
                    to_camera,
                    segment.easing.clone(),
                );
            }
        }

        if end_ts > exit_start_ts {
            let exit_camera = rect_to_camera_values(rect_at_ts(exit_start_ts), sw, sh);
            push_camera_state(
                &mut states,
                exit_start_ts,
                end_ts,
                project_duration_ms,
                source_duration_ms,
                source_fps,
                exit_camera,
                default_camera,
                segment.easing,
            );
        }
    }

    states.sort_by(|left, right| {
        left.start_frame
            .total_cmp(&right.start_frame)
            .then_with(|| left.end_frame.total_cmp(&right.end_frame))
    });

    states
}

fn build_camera_value_expr(
    states: &[CameraState],
    from_value: impl Fn(&CameraState) -> f64,
    to_value: impl Fn(&CameraState) -> f64,
    default_value: f64,
) -> String {
    let mut expr = format_f64(default_value);
    let mut ordered = states.to_vec();
    ordered.sort_by(|left, right| {
        left.start_frame
            .total_cmp(&right.start_frame)
            .then_with(|| left.end_frame.total_cmp(&right.end_frame))
    });

    for state in ordered {
        let duration = (state.end_frame - state.start_frame).max(1.0);
        let progress = format!(
            "max(0,min(1,(n-{start})/{duration}))",
            start = format_f64(state.start_frame),
            duration = format_f64(duration)
        );
        let eased_progress = easing_progress_expr(&progress, &state.easing);
        let interpolated = format!(
            "{from}+({to}-{from})*({progress})",
            from = format_f64(from_value(&state)),
            to = format_f64(to_value(&state)),
            progress = eased_progress
        );

        expr = format!(
            "if(between(n,{start},{end}),{value},{rest})",
            start = format_f64(state.start_frame),
            end = format_f64(state.end_frame),
            value = interpolated,
            rest = expr
        );
    }

    expr
}

fn push_camera_state(
    states: &mut Vec<CameraState>,
    start_ts: u64,
    end_ts: u64,
    project_duration_ms: u64,
    source_duration_ms: u64,
    source_fps: f64,
    from: (f64, f64, f64),
    to: (f64, f64, f64),
    easing: ZoomEasing,
) {
    if end_ts <= start_ts {
        return;
    }

    let start_ms = map_time_ms(start_ts, project_duration_ms, source_duration_ms);
    let end_ms = map_time_ms(end_ts, project_duration_ms, source_duration_ms);
    if end_ms <= start_ms {
        return;
    }

    states.push(CameraState {
        start_frame: start_ms as f64 / 1000.0 * source_fps,
        end_frame: end_ms as f64 / 1000.0 * source_fps,
        from_zoom: from.0,
        to_zoom: to.0,
        from_offset_x: from.1,
        to_offset_x: to.1,
        from_offset_y: from.2,
        to_offset_y: to.2,
        easing,
    });
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

fn easing_progress_expr(t_expr: &str, easing: &ZoomEasing) -> String {
    match easing {
        ZoomEasing::Linear => t_expr.to_string(),
        ZoomEasing::EaseIn => format!("({t})*({t})", t = t_expr),
        ZoomEasing::EaseOut => format!("1-(1-({t}))*(1-({t}))", t = t_expr),
        ZoomEasing::EaseInOut => {
            format!(
                "if(lt({t},0.5),2*({t})*({t}),1-pow(-2*({t})+2,2)/2)",
                t = t_expr
            )
        }
    }
}

fn format_f64(value: f64) -> String {
    format!("{value:.4}")
}

fn build_cursor_ass_file(
    project: &Project,
    events_file: &EventsFile,
    source_duration_ms: u64,
    project_duration_ms: u64,
    source_width: u32,
    source_height: u32,
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
    let frame_step_ms = (1000.0 / render_fps.max(1.0)).clamp(1.0, 100.0);
    let frame_count = ((source_duration_ms as f64 / frame_step_ms).ceil() as usize).max(2);
    let cursor_font_size = (project.settings.cursor.size * 36.0)
        .clamp(8.0, 140.0)
        .round() as u32;
    let cursor_outline = (project.settings.cursor.size * 1.8).clamp(1.0, 8.0);
    let cursor_color = rgb_hex_to_ass_color(&project.settings.cursor.color)
        .unwrap_or_else(|| "&H00FFFFFF".to_string());

    let ass_path = std::env::temp_dir().join(format!("nsc-cursor-{}-{}.ass", project.id, now_ms()));
    let mut file = File::create(&ass_path)
        .map_err(|e| format!("Failed to create temporary cursor subtitle file: {e}"))?;

    writeln!(file, "[Script Info]").map_err(|e| format!("Failed to write ass header: {e}"))?;
    writeln!(file, "ScriptType: v4.00+").map_err(|e| format!("Failed to write ass header: {e}"))?;
    writeln!(file, "PlayResX: {source_width}")
        .map_err(|e| format!("Failed to write ass header: {e}"))?;
    writeln!(file, "PlayResY: {source_height}")
        .map_err(|e| format!("Failed to write ass header: {e}"))?;
    writeln!(file).map_err(|e| format!("Failed to write ass header: {e}"))?;
    writeln!(file, "[V4+ Styles]").map_err(|e| format!("Failed to write ass style: {e}"))?;
    writeln!(
        file,
        "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding"
    )
    .map_err(|e| format!("Failed to write ass style format: {e}"))?;
    writeln!(
        file,
        "Style: Cursor,Segoe UI,12,{cursor_color},&H00FFFFFF,&H00000000,&H00000000,-1,0,0,0,100,100,0,0,1,2,0,5,0,0,0,1"
    )
    .map_err(|e| format!("Failed to write ass style body: {e}"))?;
    writeln!(file).map_err(|e| format!("Failed to write ass style: {e}"))?;
    writeln!(file, "[Events]").map_err(|e| format!("Failed to write ass events: {e}"))?;
    writeln!(
        file,
        "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text"
    )
    .map_err(|e| format!("Failed to write ass events format: {e}"))?;

    let screen_w = events_file.screen_width.max(1) as f64;
    let screen_h = events_file.screen_height.max(1) as f64;
    let src_w = source_width as f64;
    let src_h = source_height as f64;
    let mut mapped_points: Vec<(u64, f64, f64)> = points
        .into_iter()
        .map(|point| {
            (
                map_time_ms(point.ts, project_duration_ms, source_duration_ms),
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

    let mut sampled: Vec<(u64, i64, i64)> = Vec::with_capacity(frame_count + 1);
    for frame in 0..=frame_count {
        let frame_ms = ((frame as f64) * frame_step_ms)
            .round()
            .clamp(0.0, source_duration_ms as f64) as u64;
        let (x, y) = interpolate_cursor_position(&mapped_points, frame_ms);
        sampled.push((frame_ms, x.round() as i64, y.round() as i64));
    }

    sampled.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1 && left.2 == right.2);

    for pair in sampled.windows(2) {
        let (start_ms, x1, y1) = pair[0];
        let (end_ms, x2, y2) = pair[1];
        if end_ms <= start_ms {
            continue;
        }

        writeln!(
            file,
            "Dialogue: 0,{},{},Cursor,,0,0,0,,{{\\an5\\fs{}\\bord{:.2}\\shad0\\move({},{},{},{})}}o",
            format_ass_time(start_ms),
            format_ass_time(end_ms),
            cursor_font_size,
            cursor_outline,
            x1,
            y1,
            x2,
            y2
        )
        .map_err(|e| format!("Failed to write ass cursor event: {e}"))?;
    }

    Ok(ass_path)
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

fn format_ass_time(ms: u64) -> String {
    let total_cs = ms / 10;
    let cs = total_cs % 100;
    let total_seconds = total_cs / 100;
    let seconds = total_seconds % 60;
    let total_minutes = total_seconds / 60;
    let minutes = total_minutes % 60;
    let hours = total_minutes / 60;
    format!("{hours}:{minutes:02}:{seconds:02}.{cs:02}")
}

fn rgb_hex_to_ass_color(hex: &str) -> Option<String> {
    let value = hex.trim().trim_start_matches('#');
    if value.len() != 6 {
        return None;
    }
    let rr = u8::from_str_radix(&value[0..2], 16).ok()?;
    let gg = u8::from_str_radix(&value[2..4], 16).ok()?;
    let bb = u8::from_str_radix(&value[4..6], 16).ok()?;
    Some(format!("&H00{bb:02X}{gg:02X}{rr:02X}"))
}

fn escape_filter_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .replace(':', "\\:")
        .replace('\'', "\\'")
}

fn probe_media_info(source_video: &Path) -> MediaProbe {
    let ffmpeg = find_ffmpeg_exe();
    let output = Command::new(ffmpeg)
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
        Background, CursorSettings, ExportSettings, NormalizedRect, ProjectSettings, Timeline,
        ZoomEasing, ZoomSegment,
    };

    fn sample_project() -> Project {
        Project {
            schema_version: SCHEMA_VERSION,
            id: "test-project".to_string(),
            name: "Test".to_string(),
            created_at: 0,
            video_path: "raw.mp4".to_string(),
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
                    pan_trajectory: vec![],
                    easing: ZoomEasing::EaseInOut,
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

    #[test]
    fn filter_graph_uses_dynamic_zoom_pipeline() {
        let project = sample_project();
        let (graph, cursor_file) =
            build_export_filter_graph(&project, None, 1920, 1080, 30, 10_000, 1920, 1080)
                .expect("filter graph");

        assert!(cursor_file.is_none());
        assert!(graph.contains("split=2[base][zoom]"));
        assert!(graph.contains("scale=w='iw*("));
        assert!(graph.contains("pow("));
        assert!(graph.contains("eval=frame"));
        assert!(graph.contains("[base][scaled]overlay=x='-max(0,min("));
        assert!(graph.contains("overlay_h-main_h"));
        assert!(graph.contains("fps=30"));
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

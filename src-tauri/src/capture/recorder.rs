//! Screen capture pipeline based on Windows Graphics Capture + Media Foundation encoder.
//!
//! This module keeps FFmpeg discovery helpers for export, but recording itself no longer streams
//! raw BGRA frames through a pipe to an external process.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Gdi::HMONITOR;
#[cfg(target_os = "windows")]
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_capture::{
    capture::{Context, GraphicsCaptureApiHandler},
    encoder::{
        AudioSettingsBuilder, ContainerSettingsBuilder, VideoEncoder, VideoSettingsBuilder,
        VideoSettingsSubType,
    },
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
};

/// Target FPS for capture/output.
pub const DEFAULT_TARGET_FPS: u32 = 60;
const HNS_PER_SECOND: i64 = 10_000_000;

#[derive(Clone, Debug)]
pub struct CaptureEncoderSettings {
    pub output_path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub target_fps: u32,
    pub quality: RecordingQuality,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingQuality {
    Low,
    Balanced,
    High,
}

impl RecordingQuality {
    fn bitrate_scale(self) -> f64 {
        match self {
            RecordingQuality::Low => 0.75,
            RecordingQuality::Balanced => 1.0,
            RecordingQuality::High => 1.35,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CaptureFlags {
    pub stop_flag: Arc<AtomicBool>,
    pub pause_flag: Arc<AtomicBool>,
    pub encoder: CaptureEncoderSettings,
}

#[derive(Clone)]
struct LatestFrame {
    pixels: Arc<[u8]>,
    sequence: u64,
}

#[derive(Default)]
struct FrameSlot {
    latest: Option<LatestFrame>,
    next_sequence: u64,
}

#[derive(Default)]
struct MuxerStats {
    encoded_frames: u64,
    duplicated_frames: u64,
}

pub struct ScreenRecorder {
    stop_flag: Arc<AtomicBool>,
    frame_slot: Arc<(Mutex<FrameSlot>, Condvar)>,
    muxer_thread: Option<JoinHandle<Result<MuxerStats, Box<dyn std::error::Error + Send + Sync>>>>,
    received_frames: u64,
}

impl ScreenRecorder {
    fn finish_encoder(&mut self) -> Result<MuxerStats, Box<dyn std::error::Error + Send + Sync>> {
        self.stop_flag.store(true, Ordering::Relaxed);
        let (_, cvar) = &*self.frame_slot;
        cvar.notify_all();

        if let Some(muxer_thread) = self.muxer_thread.take() {
            let stats = muxer_thread
                .join()
                .map_err(|_| std::io::Error::other("CFR muxer thread panicked"))??;
            return Ok(stats);
        }

        Ok(MuxerStats::default())
    }
}

fn run_cfr_muxer(
    mut encoder: VideoEncoder,
    stop_flag: Arc<AtomicBool>,
    pause_flag: Arc<AtomicBool>,
    frame_slot: Arc<(Mutex<FrameSlot>, Condvar)>,
    target_fps: u32,
) -> Result<MuxerStats, Box<dyn std::error::Error + Send + Sync>> {
    let safe_fps = target_fps.max(1) as u64;
    let frame_interval_hns = (HNS_PER_SECOND / safe_fps as i64).max(1);
    let frame_interval = Duration::from_nanos((1_000_000_000u64 / safe_fps).max(1));

    let (lock, cvar) = &*frame_slot;
    let mut stats = MuxerStats::default();
    let mut active_frame: Option<LatestFrame> = None;
    let mut last_sequence = 0u64;
    let mut frame_index = 0i64;
    let mut next_tick: Option<Instant> = None;
    let mut was_paused = false;

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        if pause_flag.load(Ordering::Relaxed) {
            was_paused = true;
            thread::sleep(Duration::from_millis(12));
            continue;
        }

        if was_paused {
            next_tick = Some(Instant::now());
            was_paused = false;
        }

        if active_frame.is_none() {
            let mut guard = lock
                .lock()
                .map_err(|_| std::io::Error::other("CFR frame slot lock poisoned"))?;
            if guard.latest.is_none() {
                let (next_guard, _) = cvar
                    .wait_timeout(guard, Duration::from_millis(50))
                    .map_err(|_| std::io::Error::other("CFR frame slot wait poisoned"))?;
                guard = next_guard;
            }
            if let Some(snapshot) = guard.latest.clone() {
                last_sequence = snapshot.sequence;
                active_frame = Some(snapshot);
                next_tick = Some(Instant::now());
            }
            continue;
        }

        let deadline = next_tick.unwrap_or_else(Instant::now);
        let now = Instant::now();
        if now < deadline {
            thread::sleep(deadline - now);
            continue;
        }

        {
            let guard = lock
                .lock()
                .map_err(|_| std::io::Error::other("CFR frame slot lock poisoned"))?;
            if let Some(snapshot) = guard.latest.clone() {
                if snapshot.sequence != last_sequence {
                    last_sequence = snapshot.sequence;
                    active_frame = Some(snapshot);
                } else if stats.encoded_frames > 0 {
                    stats.duplicated_frames = stats.duplicated_frames.saturating_add(1);
                }
            } else if stats.encoded_frames > 0 {
                stats.duplicated_frames = stats.duplicated_frames.saturating_add(1);
            }
        }

        if let Some(snapshot) = active_frame.as_ref() {
            let pts_hns = frame_index.saturating_mul(frame_interval_hns);
            encoder
                .send_frame_buffer(snapshot.pixels.as_ref(), pts_hns)
                .map_err(|err| Box::new(err) as Box<dyn std::error::Error + Send + Sync>)?;
            frame_index = frame_index.saturating_add(1);
            stats.encoded_frames = stats.encoded_frames.saturating_add(1);
        }

        let mut candidate = deadline + frame_interval;
        let now_after = Instant::now();
        while candidate <= now_after {
            candidate += frame_interval;
        }
        next_tick = Some(candidate);
    }

    encoder
        .finish()
        .map_err(|err| Box::new(err) as Box<dyn std::error::Error + Send + Sync>)?;
    Ok(stats)
}

fn normalize_frame_for_encoder(buffer: &[u8], width: usize, height: usize) -> Vec<u8> {
    let pixel_count = width.saturating_mul(height);
    let expected_len = pixel_count.saturating_mul(4);
    if pixel_count == 0 || buffer.len() < expected_len {
        return buffer.to_vec();
    }

    // `send_frame_buffer` expects bottom-to-top row order.
    // Convert from top-to-bottom buffer by flipping rows only.
    let row_bytes = width.saturating_mul(4);
    let mut normalized = vec![0u8; expected_len];
    for row in 0..height {
        let src_row = height - 1 - row;
        let src_start = src_row.saturating_mul(row_bytes);
        let dst_start = row.saturating_mul(row_bytes);
        normalized[dst_start..dst_start + row_bytes]
            .copy_from_slice(&buffer[src_start..src_start + row_bytes]);
    }
    normalized
}

impl GraphicsCaptureApiHandler for ScreenRecorder {
    type Flags = CaptureFlags;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let flags = ctx.flags;
        let target_fps = flags.encoder.target_fps.max(1);
        let bitrate = estimate_h264_bitrate(
            flags.encoder.width,
            flags.encoder.height,
            target_fps,
            flags.encoder.quality,
        );

        let video_settings = VideoSettingsBuilder::new(flags.encoder.width, flags.encoder.height)
            .sub_type(VideoSettingsSubType::H264)
            .frame_rate(target_fps)
            .bitrate(bitrate);

        let encoder = VideoEncoder::new(
            video_settings,
            AudioSettingsBuilder::default().disabled(true),
            ContainerSettingsBuilder::default(),
            &flags.encoder.output_path,
        )
        .map_err(|err| {
            format!(
                "Failed to initialize Media Foundation encoder at {}: {err}",
                flags.encoder.output_path.display()
            )
        })?;

        let frame_slot = Arc::new((Mutex::new(FrameSlot::default()), Condvar::new()));
        let muxer_stop_flag = flags.stop_flag.clone();
        let muxer_pause_flag = flags.pause_flag.clone();
        let muxer_slot = frame_slot.clone();
        let muxer_thread = thread::Builder::new()
            .name("nsc-cfr-muxer".to_string())
            .spawn(move || {
                run_cfr_muxer(
                    encoder,
                    muxer_stop_flag,
                    muxer_pause_flag,
                    muxer_slot,
                    target_fps,
                )
            })
            .map_err(|err| format!("Failed to spawn CFR muxer thread: {err}"))?;

        Ok(Self {
            stop_flag: flags.stop_flag,
            frame_slot,
            muxer_thread: Some(muxer_thread),
            received_frames: 0,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame<'_>,
        control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if self.stop_flag.load(Ordering::Relaxed) {
            control.stop();
            return Ok(());
        }

        let width = frame.width() as usize;
        let height = frame.height() as usize;
        let mut frame_buffer = frame
            .buffer()
            .map_err(|err| Box::new(err) as Box<dyn std::error::Error + Send + Sync>)?;
        let bytes = frame_buffer
            .as_nopadding_buffer()
            .map_err(|err| Box::new(err) as Box<dyn std::error::Error + Send + Sync>)?;
        let normalized = normalize_frame_for_encoder(bytes, width, height);
        let pixels: Arc<[u8]> = Arc::from(normalized);

        let (lock, cvar) = &*self.frame_slot;
        {
            let mut slot = lock
                .lock()
                .map_err(|_| std::io::Error::other("CFR frame slot lock poisoned"))?;
            slot.next_sequence = slot.next_sequence.saturating_add(1);
            slot.latest = Some(LatestFrame {
                pixels,
                sequence: slot.next_sequence,
            });
        }
        cvar.notify_all();
        self.received_frames = self.received_frames.saturating_add(1);

        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        let stats = self.finish_encoder()?;
        log::info!(
            "capture closed: received_frames={} encoded_frames={} duplicated_frames={}",
            self.received_frames,
            stats.encoded_frames,
            stats.duplicated_frames
        );
        Ok(())
    }
}

fn estimate_h264_bitrate(width: u32, height: u32, fps: u32, quality: RecordingQuality) -> u32 {
    // Bitrate heuristic tuned for screen content:
    // 1080p30 ~= 7 Mbps, 1440p60 ~= 20 Mbps, 2160p60 ~= 45 Mbps (clamped).
    let pixels_per_second = width as f64 * height as f64 * fps.max(1) as f64;
    let raw = (pixels_per_second * 0.11 * quality.bitrate_scale()).round() as u64;
    raw.clamp(3_000_000, 60_000_000) as u32
}

/// Returns monitor physical size by monitor index (0 = primary).
pub fn get_monitor_size(monitor_index: u32) -> Result<(u32, u32), String> {
    let monitors =
        Monitor::enumerate().map_err(|e| format!("Failed to enumerate monitors: {e}"))?;

    let monitor = monitors
        .into_iter()
        .nth(monitor_index as usize)
        .ok_or_else(|| format!("Monitor index {monitor_index} not found"))?;

    let width = monitor
        .width()
        .map_err(|e| format!("Failed to get monitor width: {e}"))?;
    let height = monitor
        .height()
        .map_err(|e| format!("Failed to get monitor height: {e}"))?;

    Ok((width, height))
}

/// Returns monitor scale factor (1.0 = 100%, 1.25 = 125%, etc).
pub fn get_monitor_scale_factor(monitor_index: u32) -> Result<f64, String> {
    #[cfg(target_os = "windows")]
    {
        let monitors =
            Monitor::enumerate().map_err(|e| format!("Failed to enumerate monitors: {e}"))?;

        let monitor = monitors
            .into_iter()
            .nth(monitor_index as usize)
            .ok_or_else(|| format!("Monitor index {monitor_index} not found"))?;

        let mut dpi_x: u32 = 0;
        let mut dpi_y: u32 = 0;

        unsafe {
            GetDpiForMonitor(
                HMONITOR(monitor.as_raw_hmonitor() as isize),
                MDT_EFFECTIVE_DPI,
                &mut dpi_x,
                &mut dpi_y,
            )
            .map_err(|e| format!("Failed to get monitor DPI: {e}"))?;
        }

        if dpi_x == 0 {
            return Ok(1.0);
        }

        let scale = (dpi_x as f64 / 96.0).clamp(0.5, 4.0);
        Ok(scale)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = monitor_index;
        Ok(1.0)
    }
}

/// Finds ffmpeg binary without requiring it in system PATH.
///
/// Search order:
/// 1. Dev build: `src-tauri/binaries/ffmpeg-x86_64-pc-windows-msvc.exe`
/// 2. Production: next to bundled app executable (`ffmpeg.exe`)
/// 3. Fallback: system PATH
pub fn find_ffmpeg_exe() -> std::path::PathBuf {
    #[cfg(debug_assertions)]
    {
        let dev = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join("ffmpeg-x86_64-pc-windows-msvc.exe");
        if dev.exists() {
            log::debug!("ffmpeg: using dev binary at {}", dev.display());
            return dev;
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("ffmpeg.exe");
            if candidate.exists() {
                log::debug!("ffmpeg: using bundled binary at {}", candidate.display());
                return candidate;
            }
        }
    }

    log::warn!("ffmpeg: bundled binary not found, falling back to system PATH");
    std::path::PathBuf::from("ffmpeg")
}

/// Configures external process launch so it does not spawn a visible console window on Windows.
pub fn apply_no_window_flags(command: &mut std::process::Command) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
}

/// Starts WGC capture on a dedicated thread.
pub fn start_capture(
    monitor_index: u32,
    stop_flag: Arc<AtomicBool>,
    pause_flag: Arc<AtomicBool>,
    output_path: PathBuf,
    width: u32,
    height: u32,
    target_fps: u32,
    quality: RecordingQuality,
) -> Result<std::thread::JoinHandle<Result<(), String>>, String> {
    let monitors =
        Monitor::enumerate().map_err(|e| format!("Failed to enumerate monitors: {e}"))?;

    let monitor = monitors
        .into_iter()
        .nth(monitor_index as usize)
        .ok_or_else(|| format!("Monitor index {monitor_index} not found"))?;

    let flags = CaptureFlags {
        stop_flag,
        pause_flag,
        encoder: CaptureEncoderSettings {
            output_path,
            width,
            height,
            target_fps: target_fps.max(1),
            quality,
        },
    };

    let safe_fps = target_fps.max(1);

    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Custom(Duration::from_secs_f64(1.0 / safe_fps as f64)),
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        flags,
    );

    let handle = std::thread::Builder::new()
        .name("nsc-capture".to_string())
        .spawn(move || {
            ScreenRecorder::start(settings).map_err(|e| format!("WGC capture failed: {e}"))
        })
        .map_err(|e| format!("Failed to spawn capture thread: {e}"))?;

    Ok(handle)
}

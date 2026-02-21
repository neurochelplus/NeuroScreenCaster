//! Screen capture pipeline based on Windows Graphics Capture + Media Foundation encoder.
//!
//! This module keeps FFmpeg discovery helpers for export, but recording itself no longer streams
//! raw BGRA frames through a pipe to an external process.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

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
pub const TARGET_FPS: u32 = 30;

#[derive(Clone, Debug)]
pub struct CaptureEncoderSettings {
    pub output_path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub target_fps: u32,
}

#[derive(Clone, Debug)]
pub struct CaptureFlags {
    pub stop_flag: Arc<AtomicBool>,
    pub encoder: CaptureEncoderSettings,
}

pub struct ScreenRecorder {
    stop_flag: Arc<AtomicBool>,
    encoder: Option<VideoEncoder>,
    encoded_frames: u64,
}

impl ScreenRecorder {
    fn finish_encoder(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(encoder) = self.encoder.take() {
            encoder
                .finish()
                .map_err(|err| Box::new(err) as Box<dyn std::error::Error + Send + Sync>)?;
        }
        Ok(())
    }
}

impl GraphicsCaptureApiHandler for ScreenRecorder {
    type Flags = CaptureFlags;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let flags = ctx.flags;
        let target_fps = flags.encoder.target_fps.max(1);
        let bitrate = estimate_h264_bitrate(flags.encoder.width, flags.encoder.height, target_fps);

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

        Ok(Self {
            stop_flag: flags.stop_flag,
            encoder: Some(encoder),
            encoded_frames: 0,
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

        if let Some(encoder) = self.encoder.as_mut() {
            if let Err(err) = encoder.send_frame(frame) {
                control.stop();
                return Err(Box::new(err));
            }
            self.encoded_frames = self.encoded_frames.saturating_add(1);
        }

        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        self.finish_encoder()?;
        log::info!("capture closed: encoded_frames={}", self.encoded_frames);
        Ok(())
    }
}

fn estimate_h264_bitrate(width: u32, height: u32, fps: u32) -> u32 {
    // Bitrate heuristic tuned for screen content:
    // 1080p30 ~= 7 Mbps, 1440p60 ~= 20 Mbps, 2160p60 ~= 45 Mbps (clamped).
    let pixels_per_second = width as f64 * height as f64 * fps.max(1) as f64;
    let raw = (pixels_per_second * 0.11).round() as u64;
    raw.clamp(4_000_000, 45_000_000) as u32
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

/// Starts WGC capture on a dedicated thread.
pub fn start_capture(
    monitor_index: u32,
    stop_flag: Arc<AtomicBool>,
    output_path: PathBuf,
    width: u32,
    height: u32,
) -> Result<std::thread::JoinHandle<Result<(), String>>, String> {
    let monitors =
        Monitor::enumerate().map_err(|e| format!("Failed to enumerate monitors: {e}"))?;

    let monitor = monitors
        .into_iter()
        .nth(monitor_index as usize)
        .ok_or_else(|| format!("Monitor index {monitor_index} not found"))?;

    let flags = CaptureFlags {
        stop_flag,
        encoder: CaptureEncoderSettings {
            output_path,
            width,
            height,
            target_fps: TARGET_FPS,
        },
    };

    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Custom(Duration::from_secs_f64(1.0 / TARGET_FPS as f64)),
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

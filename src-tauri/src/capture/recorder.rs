//! WGC-захват экрана и интеграция с FFmpeg.
//!
//! Захват работает через crate `windows-capture` v1.5+:
//! - `ScreenRecorder` реализует `GraphicsCaptureApiHandler`.
//! - Курсор отключён (`CursorCaptureSettings::WithoutCursor`).
//! - Кадры (BGRA8) пишутся в stdin FFmpeg, который кодирует H.264 MP4.
//! - Частота ограничена до `TARGET_FPS` пропуском «лишних» кадров.

use std::io::Write;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Gdi::HMONITOR;
#[cfg(target_os = "windows")]
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_capture::{
    capture::{Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
};

/// Целевая частота кадров выходного видео.
pub const TARGET_FPS: u32 = 30;

// ─── Обработчик кадров ───────────────────────────────────────────────────────

/// Принимает BGRA-кадры от WGC и записывает их в stdin FFmpeg.
pub struct ScreenRecorder {
    stop_flag: Arc<AtomicBool>,
    ffmpeg_stdin: ChildStdin,
    last_frame_time: Instant,
    frame_interval: Duration,
}

impl GraphicsCaptureApiHandler for ScreenRecorder {
    /// Flags = (stop_flag, ffmpeg_stdin).
    type Flags = (Arc<AtomicBool>, ChildStdin);
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let (stop_flag, stdin) = ctx.flags;
        Ok(Self {
            stop_flag,
            ffmpeg_stdin: stdin,
            // Устанавливаем прошлое время, чтобы первый кадр записался сразу.
            last_frame_time: Instant::now() - Duration::from_secs(1),
            frame_interval: Duration::from_secs_f64(1.0 / TARGET_FPS as f64),
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame<'_>,
        control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        // Проверяем флаг остановки (выставляется из stop_recording).
        if self.stop_flag.load(Ordering::Relaxed) {
            control.stop();
            return Ok(());
        }

        // Ограничение частоты до TARGET_FPS.
        let now = Instant::now();
        if now.duration_since(self.last_frame_time) < self.frame_interval {
            return Ok(());
        }
        self.last_frame_time = now;

        // Получаем BGRA-данные без row-padding.
        let mut buffer = frame.buffer()?;
        let raw = buffer.as_nopadding_buffer()?;

        // Пишем кадр в FFmpeg. При ошибке (напр. FFmpeg упал) — останавливаем захват.
        if let Err(e) = self.ffmpeg_stdin.write_all(raw) {
            log::error!("FFmpeg stdin write error: {e}");
            control.stop();
            return Err(Box::new(e));
        }

        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        // ffmpeg_stdin дропнется вместе с ScreenRecorder.
        // FFmpeg получит EOF на stdin и завершит кодирование.
        Ok(())
    }
}

// ─── Публичные функции ────────────────────────────────────────────────────────

/// Возвращает физическое разрешение монитора по индексу (0 = primary).
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

/// Находит ffmpeg-бинарник, не требуя его наличия в системном PATH.
///
/// Порядок поиска:
/// 1. В dev-сборке: `src-tauri/binaries/ffmpeg-x86_64-pc-windows-msvc.exe`
///    (путь вычислен на этапе компиляции через `CARGO_MANIFEST_DIR`).
/// 2. В production: рядом с исполняемым файлом приложения (Tauri sidecar).
/// 3. Fallback: системный PATH — если пользователь установил FFmpeg глобально.
pub fn find_ffmpeg_exe() -> std::path::PathBuf {
    // ── 1. Dev: compile-time path ──────────────────────────────────────────
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

    // ── 2. Production: next to the app exe (Tauri sidecar) ────────────────
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // Tauri strips the target-triple suffix when bundling.
            let candidate = dir.join("ffmpeg.exe");
            if candidate.exists() {
                log::debug!("ffmpeg: using bundled binary at {}", candidate.display());
                return candidate;
            }
        }
    }

    // ── 3. Fallback: system PATH ───────────────────────────────────────────
    log::warn!("ffmpeg: bundled binary not found, falling back to system PATH");
    std::path::PathBuf::from("ffmpeg")
}

/// Запускает FFmpeg: читает rawvideo (BGRA) из stdin, пишет H.264 MP4 в файл.
/// Возвращает `(Child, ChildStdin)` — процесс (без stdin) и его stdin.
pub fn spawn_ffmpeg(
    width: u32,
    height: u32,
    output_path: &Path,
) -> Result<(Child, ChildStdin), String> {
    let ffmpeg = find_ffmpeg_exe();
    let out = output_path
        .to_str()
        .ok_or("Output path contains non-UTF-8 characters")?;

    let mut child = Command::new(&ffmpeg)
        .args([
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "bgra",
            "-video_size",
            &format!("{width}x{height}"),
            "-r",
            &TARGET_FPS.to_string(),
            "-i",
            "pipe:0",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-crf",
            "18",
            "-pix_fmt",
            "yuv420p",
            out,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            format!(
                "Failed to spawn FFmpeg ({}): {e}. \
                 Run scripts/download-ffmpeg.ps1 to install the bundled binary.",
                ffmpeg.display()
            )
        })?;

    let stdin = child
        .stdin
        .take()
        .ok_or("Failed to acquire FFmpeg stdin pipe")?;

    Ok((child, stdin))
}

/// Запускает WGC-захват в отдельном потоке.
/// Поток завершается, когда `stop_flag` становится `true`.
pub fn start_capture(
    monitor_index: u32,
    stop_flag: Arc<AtomicBool>,
    ffmpeg_stdin: ChildStdin,
) -> Result<std::thread::JoinHandle<Result<(), String>>, String> {
    let monitors =
        Monitor::enumerate().map_err(|e| format!("Failed to enumerate monitors: {e}"))?;

    let monitor = monitors
        .into_iter()
        .nth(monitor_index as usize)
        .ok_or_else(|| format!("Monitor index {monitor_index} not found"))?;

    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        (stop_flag, ffmpeg_stdin),
    );

    let handle = std::thread::Builder::new()
        .name("nsc-capture".to_string())
        .spawn(move || {
            ScreenRecorder::start(settings).map_err(|e| format!("WGC capture failed: {e}"))
        })
        .map_err(|e| format!("Failed to spawn capture thread: {e}"))?;

    Ok(handle)
}

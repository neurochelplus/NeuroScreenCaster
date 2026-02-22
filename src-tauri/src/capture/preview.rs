//! Native monitor preview pipeline (Windows Graphics Capture -> JPEG data URL).
//!
//! Used by the Record screen to render a live preview without browser
//! `getDisplayMedia` permission prompts.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::{engine::general_purpose, Engine as _};
use serde::Serialize;
use tokio::sync::Mutex as AsyncMutex;
use windows_capture::{
    capture::{CaptureControl, Context, GraphicsCaptureApiHandler},
    encoder::ImageEncoder,
    frame::{Frame, ImageFormat},
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
};

const PREVIEW_TARGET_FPS: u32 = 12;
const PREVIEW_MIN_INTERVAL: Duration = Duration::from_millis(1000 / PREVIEW_TARGET_FPS as u64);
const PREVIEW_MAX_WIDTH: u32 = 1280;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativePreviewFrame {
    pub data_url: String,
    pub width: u32,
    pub height: u32,
    pub sequence: u64,
}

#[derive(Default)]
struct SharedPreviewFrame {
    latest: Option<NativePreviewFrame>,
}

struct PreviewCaptureFlags {
    shared: Arc<Mutex<SharedPreviewFrame>>,
    max_width: u32,
    min_interval: Duration,
}

struct PreviewCaptureHandler {
    shared: Arc<Mutex<SharedPreviewFrame>>,
    image_encoder: ImageEncoder,
    max_width: u32,
    min_interval: Duration,
    last_encoded_at: Option<Instant>,
    sequence: u64,
}

type PreviewCaptureControl = CaptureControl<PreviewCaptureHandler, String>;

fn downscale_bgra_for_preview<'a>(
    source: &'a [u8],
    width: u32,
    height: u32,
    max_width: u32,
) -> (Cow<'a, [u8]>, u32, u32) {
    if width == 0 || height == 0 || max_width == 0 || width <= max_width {
        return (Cow::Borrowed(source), width, height);
    }

    let expected_len = width as usize * height as usize * 4;
    if source.len() < expected_len {
        return (Cow::Borrowed(source), width, height);
    }

    let out_width = max_width;
    let out_height =
        ((height as u64 * max_width as u64) / width as u64).clamp(1, u32::MAX as u64) as u32;

    let mut downscaled = vec![0u8; out_width as usize * out_height as usize * 4];
    let src_width = width as usize;
    let dst_width = out_width as usize;

    for y in 0..out_height as usize {
        let src_y = (y as u64 * height as u64 / out_height as u64) as usize;
        for x in 0..out_width as usize {
            let src_x = (x as u64 * width as u64 / out_width as u64) as usize;

            let src_idx = (src_y * src_width + src_x) * 4;
            let dst_idx = (y * dst_width + x) * 4;
            downscaled[dst_idx..dst_idx + 4].copy_from_slice(&source[src_idx..src_idx + 4]);
        }
    }

    (Cow::Owned(downscaled), out_width, out_height)
}

impl GraphicsCaptureApiHandler for PreviewCaptureHandler {
    type Flags = PreviewCaptureFlags;
    type Error = String;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            shared: ctx.flags.shared,
            image_encoder: ImageEncoder::new(ImageFormat::Jpeg, ColorFormat::Bgra8),
            max_width: ctx.flags.max_width,
            min_interval: ctx.flags.min_interval,
            last_encoded_at: None,
            sequence: 0,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame<'_>,
        control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if self
            .last_encoded_at
            .is_some_and(|last| last.elapsed() < self.min_interval)
        {
            return Ok(());
        }

        let width = frame.width();
        let height = frame.height();

        let mut frame_buffer = frame
            .buffer()
            .map_err(|err| format!("Failed to map preview frame: {err}"))?;
        let bytes = frame_buffer
            .as_nopadding_buffer()
            .map_err(|err| format!("Failed to read preview frame bytes: {err}"))?;

        let (scaled, scaled_width, scaled_height) =
            downscale_bgra_for_preview(bytes, width, height, self.max_width);
        let jpeg = self
            .image_encoder
            .encode(scaled.as_ref(), scaled_width, scaled_height)
            .map_err(|err| format!("Failed to encode preview frame: {err}"))?;

        let data_url = format!(
            "data:image/jpeg;base64,{}",
            general_purpose::STANDARD.encode(jpeg)
        );

        self.sequence = self.sequence.saturating_add(1);
        let preview_frame = NativePreviewFrame {
            data_url,
            width: scaled_width,
            height: scaled_height,
            sequence: self.sequence,
        };

        if let Ok(mut shared) = self.shared.lock() {
            shared.latest = Some(preview_frame);
        }

        self.last_encoded_at = Some(Instant::now());

        if width == 0 || height == 0 {
            control.stop();
        }

        Ok(())
    }
}

struct PreviewSession {
    monitor_index: u32,
    control: PreviewCaptureControl,
    shared: Arc<Mutex<SharedPreviewFrame>>,
}

pub struct PreviewManager {
    session: Option<PreviewSession>,
}

impl PreviewManager {
    #[must_use]
    pub fn new() -> Self {
        Self { session: None }
    }

    pub fn start_session(&mut self, monitor_index: u32) -> Result<(), String> {
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.monitor_index == monitor_index)
        {
            return Ok(());
        }

        self.stop_session();

        let monitors =
            Monitor::enumerate().map_err(|err| format!("Failed to enumerate monitors: {err}"))?;
        let monitor = monitors
            .into_iter()
            .nth(monitor_index as usize)
            .ok_or_else(|| format!("Monitor index {monitor_index} not found"))?;

        let shared = Arc::new(Mutex::new(SharedPreviewFrame::default()));
        let flags = PreviewCaptureFlags {
            shared: shared.clone(),
            max_width: PREVIEW_MAX_WIDTH,
            min_interval: PREVIEW_MIN_INTERVAL,
        };

        let settings = Settings::new(
            monitor,
            CursorCaptureSettings::WithCursor,
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Custom(PREVIEW_MIN_INTERVAL),
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            flags,
        );

        let control = PreviewCaptureHandler::start_free_threaded(settings)
            .map_err(|err| format!("Failed to start native preview: {err}"))?;

        self.session = Some(PreviewSession {
            monitor_index,
            control,
            shared,
        });

        Ok(())
    }

    pub fn stop_session(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };

        if let Err(err) = session.control.stop() {
            log::warn!("native preview stop failed: {err}");
        }
    }

    #[must_use]
    pub fn latest_frame(&self) -> Option<NativePreviewFrame> {
        self.session
            .as_ref()
            .and_then(|session| session.shared.lock().ok())
            .and_then(|shared| shared.latest.clone())
    }
}

impl Drop for PreviewManager {
    fn drop(&mut self) {
        self.stop_session();
    }
}

pub struct NativePreviewState(pub Arc<AsyncMutex<PreviewManager>>);

impl NativePreviewState {
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(AsyncMutex::new(PreviewManager::new())))
    }
}

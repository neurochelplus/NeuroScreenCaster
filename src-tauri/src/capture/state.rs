//! Active screen-recording state.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::models::events::InputEvent;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AutoZoomTriggerMode {
    #[default]
    SingleClick,
    MultiClickWindow,
    CtrlClick,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RecordingAudioMode {
    #[default]
    NoAudio,
    SystemOnly,
    MicrophoneOnly,
    MicrophoneAndSystem,
}

pub enum AudioCaptureBackend {
    FfmpegChild(std::process::Child),
    NativeLoopback {
        stop_flag: Arc<AtomicBool>,
        join_handle: std::thread::JoinHandle<Result<(), String>>,
    },
}

pub struct AudioCaptureProcess {
    pub backend: AudioCaptureBackend,
    pub output_path: PathBuf,
}

pub struct AudioCaptureSession {
    pub system_capture: Option<AudioCaptureProcess>,
    pub microphone_capture: Option<AudioCaptureProcess>,
}

/// Data for one active recording session.
pub struct ActiveRecording {
    pub recording_id: String,
    /// Shared stop signal consumed by the capture callback.
    pub stop_flag: Arc<AtomicBool>,
    /// Shared pause signal consumed by the encoder/muxer path.
    pub pause_flag: Arc<AtomicBool>,
    /// WGC capture thread; exits once stop flag is observed.
    pub capture_thread: std::thread::JoinHandle<Result<(), String>>,
    /// Project directory: `{Videos}/NeuroScreenCaster/{recording_id}/`
    pub output_dir: PathBuf,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
    /// Unix timestamp in ms when recording started.
    pub start_ms: u64,
    /// Active pause start timestamp (absolute Unix ms); `None` when not paused.
    pub pause_started_at_ms: Option<u64>,
    /// Closed pause ranges (absolute Unix ms).
    pub pause_ranges_ms: Vec<(u64, u64)>,
    /// True when cursor should be visible in preview/export.
    pub cursor_visible: bool,
    /// Active hidden-cursor interval start (absolute Unix ms).
    pub cursor_hidden_started_at_ms: Option<u64>,
    /// Closed hidden-cursor intervals (absolute Unix ms).
    pub cursor_hidden_ranges_abs_ms: Vec<(u64, u64)>,
    /// Auto-zoom activation mode selected before recording start.
    pub auto_zoom_trigger_mode: AutoZoomTriggerMode,
    /// Audio capture mode selected before recording start.
    pub audio_mode: RecordingAudioMode,
    /// Selected microphone input device name (if required by mode).
    pub microphone_device: Option<String>,
    /// Optional live audio capture session.
    pub audio_capture_session: Option<AudioCaptureSession>,
    /// Telemetry processor thread (returns all collected events on join).
    pub telemetry_processor: std::thread::JoinHandle<Vec<InputEvent>>,
}

/// Tauri managed recorder state.
pub struct RecorderState(pub Arc<Mutex<Option<ActiveRecording>>>);

impl RecorderState {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(None)))
    }
}

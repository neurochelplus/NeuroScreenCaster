//! Active screen-recording state.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::models::events::InputEvent;

/// Data for one active recording session.
pub struct ActiveRecording {
    pub recording_id: String,
    /// Shared stop signal consumed by the capture callback.
    pub stop_flag: Arc<AtomicBool>,
    /// WGC capture thread; exits once stop flag is observed.
    pub capture_thread: std::thread::JoinHandle<Result<(), String>>,
    /// Project directory: `{Videos}/NeuroScreenCaster/{recording_id}/`
    pub output_dir: PathBuf,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
    /// Unix timestamp in ms when recording started.
    pub start_ms: u64,
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

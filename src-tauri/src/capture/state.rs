//! Состояние активной записи экрана.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::models::events::InputEvent;

/// Данные активной сессии записи.
pub struct ActiveRecording {
    pub recording_id: String,
    /// Флаг остановки, разделяемый с обработчиком кадров WGC.
    pub stop_flag: Arc<AtomicBool>,
    /// Поток WGC-захвата. Завершается, когда обработчик видит stop_flag.
    pub capture_thread: std::thread::JoinHandle<Result<(), String>>,
    /// Процесс FFmpeg (stdin уже изъят и передан в обработчик).
    pub ffmpeg_process: std::process::Child,
    /// Директория проекта: {Videos}/NeuroScreenCaster/{recording_id}/
    pub output_dir: PathBuf,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
    /// Unix timestamp (мс) начала записи — точка отсчёта телеметрии.
    pub start_ms: u64,
    /// Поток-процессор телеметрии (Этап 3). При join возвращает все события.
    pub telemetry_processor: std::thread::JoinHandle<Vec<InputEvent>>,
}

/// Tauri managed state рекордера.
pub struct RecorderState(pub Arc<Mutex<Option<ActiveRecording>>>);

impl RecorderState {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(None)))
    }
}

pub mod algorithm;
pub mod capture;
pub mod commands;
pub mod models;
pub mod telemetry;

use capture::preview::NativePreviewState;
use capture::state::RecorderState;
use commands::export::ExportState;
use telemetry::logger::{spawn_rdev_thread, TelemetryGlobal, TelemetryState};

pub fn run() {
    env_logger::init();

    let telemetry_global = TelemetryGlobal::new();
    spawn_rdev_thread(telemetry_global.clone());

    tauri::Builder::default()
        .manage(RecorderState::new())
        .manage(NativePreviewState::new())
        .manage(TelemetryState(telemetry_global))
        .manage(ExportState::default())
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            commands::capture::start_native_preview,
            commands::capture::get_native_preview_frame,
            commands::capture::stop_native_preview,
            commands::capture::is_ctrl_pressed,
            commands::capture::list_audio_input_devices,
            commands::capture::start_recording,
            commands::capture::stop_recording,
            commands::capture::pause_recording,
            commands::capture::resume_recording,
            commands::cursor::get_cursor_asset_info,
            commands::export::start_export,
            commands::export::cancel_export,
            commands::export::pick_export_folder,
            commands::export::get_export_status,
            commands::export::reset_export_status,
            commands::project::get_project,
            commands::project::get_events,
            commands::project::list_projects,
            commands::project::save_project,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

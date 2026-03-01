#[tauri::command]
pub fn exit_application(app: tauri::AppHandle) {
    app.exit(0);
}


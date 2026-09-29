#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use prodex_core::model::{Request, Response};
use std::path::PathBuf;
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_dialog::DialogExt;
mod terminal;

#[tauri::command]
async fn open_session(id: String, action: Option<String>) -> Result<(), String> {
    let response = request(Request::Status).await?;
    if !response.ok {
        return Err(response
            .error
            .unwrap_or_else(|| "Cannot read session status".into()));
    }
    let snapshot: prodex_core::model::Snapshot =
        serde_json::from_value(response.data).map_err(|error| error.to_string())?;
    let task = snapshot
        .tasks
        .iter()
        .find(|task| task.id == id)
        .ok_or("Session no longer available")?;
    let command = terminal::completion_command(task, action.as_deref().unwrap_or("terminal"))?;
    tauri::async_runtime::spawn_blocking(move || terminal::open(&command))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
fn copy_text(app: tauri::AppHandle, text: String) -> Result<(), String> {
    if text.len() > 65536 {
        return Err("Text is too large to copy".into());
    }
    app.clipboard()
        .write_text(text)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn pick_projects(window: tauri::WebviewWindow) -> Result<Vec<PathBuf>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let folders = window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title("Add projects")
            .blocking_pick_folders()
            .unwrap_or_default();
        let mut paths = Vec::new();
        for folder in folders {
            let path = folder
                .into_path()
                .map_err(|error| error.to_string())?
                .canonicalize()
                .map_err(|error| error.to_string())?;
            if !path.is_dir() {
                return Err("Select a project folder".into());
            }
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        Ok(paths)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn request(request: Request) -> Result<Response, String> {
    let state_dir = match std::env::var_os("PRODEX_STATE_DIR") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?)
            .join(".local/state/prodex"),
    };
    prodex_core::ipc::request(&state_dir, request)
        .await
        .map_err(|error| format!("Cannot reach Prodex service: {error:#}"))
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![
            request,
            pick_projects,
            copy_text,
            open_session
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Prodex desktop");
}

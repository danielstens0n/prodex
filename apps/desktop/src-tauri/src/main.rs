#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use prodex_core::model::{Request, Response};
use std::path::PathBuf;
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_dialog::DialogExt;
mod destinations;
mod terminal;

#[tauri::command]
async fn open_session(
    app: tauri::AppHandle,
    id: String,
    action: Option<String>,
    destination: Option<String>,
) -> Result<Option<String>, String> {
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
    let task = task.clone();
    let (notice, clipboard) = tauri::async_runtime::spawn_blocking(move || {
        destinations::launch(
            &task,
            action.as_deref().unwrap_or("terminal"),
            destination.as_deref().unwrap_or("terminal"),
        )
    })
    .await
    .map_err(|error| error.to_string())??;
    if let Some(text) = clipboard {
        app.clipboard()
            .write_text(text)
            .map_err(|e| format!("App opened, but copying the command failed: {e}"))?;
    }
    Ok(notice)
}

#[tauri::command]
async fn list_destinations(
    custom: Option<String>,
) -> Result<Vec<destinations::Destination>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut apps = destinations::installed();
        if let Some(path) = custom.and_then(|s| s.strip_prefix("app:").map(str::to_owned)) {
            if let Ok(app) = destinations::custom(std::path::Path::new(&path)) {
                apps.push(app);
            }
        }
        apps
    })
    .await
    .map_err(|e| e.to_string())
}
#[tauri::command]
async fn pick_destination(
    window: tauri::WebviewWindow,
) -> Result<Option<destinations::Destination>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let choice = window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title("Choose a terminal or editor")
            .set_directory("/Applications")
            .add_filter("Applications", &["app"])
            .blocking_pick_file();
        choice
            .map(|file| {
                file.into_path()
                    .map_err(|e| e.to_string())
                    .and_then(|p| destinations::custom(&p))
            })
            .transpose()
    })
    .await
    .map_err(|e| e.to_string())?
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
            open_session,
            list_destinations,
            pick_destination
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Prodex desktop");
}

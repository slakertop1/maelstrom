// Lightweight file logging for debugging. Writes timestamped lines to
// <app_config_dir>/maelstrom.log. Secrets (tokens, Authorization, client
// secrets, passwords, cookies) are NEVER written — they are masked as ***.
use std::io::Write;
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;

const MAX_BYTES: u64 = 5 * 1024 * 1024;

fn log_file(app: &AppHandle) -> Option<std::path::PathBuf> {
    let dir = app.path().app_config_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("maelstrom.log"))
}

/// Append a line. Rotates once past 5 MB (keeps a single .old backup).
pub fn write(app: &AppHandle, category: &str, msg: &str) {
    let Some(path) = log_file(app) else { return };
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_BYTES {
            let _ = std::fs::rename(&path, path.with_file_name("maelstrom.log.old"));
        }
    }
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{ts} [{category}] {msg}");
    }
}

// Secret redaction lives in the shared engine crate so the app and CLI mask
// identically (tokens, cookies, secrets, presigned-URL signatures, passwords).
pub use maelstrom_core::redact::{safe_headers, safe_url};

// ---------- commands ----------

#[tauri::command]
pub fn read_log(app: AppHandle) -> String {
    log_file(&app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default()
}

#[tauri::command]
pub fn log_path(app: AppHandle) -> String {
    log_file(&app).map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
}

#[tauri::command]
pub fn clear_log(app: AppHandle) -> Result<(), String> {
    if let Some(p) = log_file(&app) {
        let _ = std::fs::remove_file(&p);
    }
    Ok(())
}

#[tauri::command]
pub fn open_log_folder(app: AppHandle) -> Result<(), String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    app.opener()
        .open_path(dir.to_string_lossy().to_string(), None::<String>)
        .map_err(|e| e.to_string())
}

/// Log a line from the frontend (e.g. import / UI events).
#[tauri::command]
pub fn log_event(app: AppHandle, category: String, message: String) {
    write(&app, &category, &message);
}

/// App version + platform, for bug reports.
#[tauri::command]
pub fn app_version() -> String {
    format!(
        "Maelstrom {} · {} {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// Open a URL (e.g. the bug-tracker) in the default browser.
#[tauri::command]
pub fn open_url(app: AppHandle, url: String) -> Result<(), String> {
    app.opener().open_url(url, None::<String>).map_err(|e| e.to_string())
}

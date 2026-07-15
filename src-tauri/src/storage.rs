use std::io::Write;
use std::path::Path;
use tauri::{AppHandle, Manager};

fn read_non_empty(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// state.json / .bak can hold secrets (API keys, auth tokens, DB passwords
/// saved in requests). Lock them down to the owner only.
#[cfg(unix)]
fn restrict_perms(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(windows)]
fn restrict_perms(_path: &Path) {
    // TODO: restrict the Windows ACL to the current user (needs `icacls` or a
    // crate like `windows-acl`; default NTFS perms already limit access to the
    // owning account + admins under %APPDATA%, so this is a hardening gap, not
    // an open one). Out of scope for this point fix.
}

/// Atomically replace `file` with `data`: write a temp file, fsync it so the
/// bytes are actually on disk, keep the previous version as `.bak`, then
/// rename over the target. A crash mid-write (or a power loss right after)
/// can no longer destroy the existing state.
fn write_atomic(file: &Path, data: &str) -> Result<(), String> {
    let tmp = file.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    restrict_perms(&tmp);
    if file.exists() {
        let bak = file.with_extension("json.bak");
        if std::fs::copy(file, &bak).is_ok() {
            restrict_perms(&bak);
        }
    }
    std::fs::rename(&tmp, file).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn load_state(app: AppHandle) -> Result<String, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?;
    if let Some(s) = read_non_empty(&dir.join("state.json")) {
        return Ok(s);
    }
    // Main file missing or empty (e.g. interrupted write) — fall back to the backup.
    if let Some(s) = read_non_empty(&dir.join("state.json.bak")) {
        return Ok(s);
    }
    Ok("null".to_string())
}

/// Previous good copy of the state, for the frontend to fall back to when the
/// main file exists but fails to parse.
#[tauri::command]
pub fn load_state_backup(app: AppHandle) -> Result<String, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?;
    Ok(read_non_empty(&dir.join("state.json.bak")).unwrap_or_else(|| "null".to_string()))
}

#[tauri::command]
pub fn save_state(app: AppHandle, data: String) -> Result<(), String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    write_atomic(&dir.join("state.json"), &data)
}

// SECURITY NOTE: `path` is whatever the frontend passes, with no allow-list.
// That's intentional — these two commands back the save/export/import flow,
// where `path` comes from a native OS file dialog and is legitimately
// arbitrary (any directory the user picks). An allow-list here would break
// that flow. The actual control is *who* can invoke this Tauri command in the
// first place (capabilities/webview isolation so untrusted remote content
// can't call it) — that lives outside src-tauri/src/storage.rs, so it's not
// addressed by this point fix.
#[tauri::command]
pub fn write_text_file(path: String, contents: String) -> Result<(), String> {
    std::fs::write(path, contents).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn read_text_file(path: String) -> Result<String, String> {
    // Check the size before reading so a multi-gigabyte file picked by mistake
    // doesn't get loaded into memory.
    let meta = std::fs::metadata(&path).map_err(|e| format!("Не удалось прочитать файл: {e}"))?;
    if meta.len() > 40 * 1024 * 1024 {
        return Err("Файл слишком большой (>40 МБ)".to_string());
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("Не удалось прочитать файл: {e}"))?;
    String::from_utf8(bytes).map_err(|_| "Файл не в кодировке UTF-8".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_keeps_backup_and_replaces_target() {
        let dir = std::env::temp_dir().join(format!("maelstrom-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("state.json");

        write_atomic(&file, "v1").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "v1");

        write_atomic(&file, "v2").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "v2");
        assert_eq!(
            std::fs::read_to_string(dir.join("state.json.bak")).unwrap(),
            "v1"
        );
        assert!(!dir.join("state.json.tmp").exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_restricts_perms_to_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("maelstrom-test-perms-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("state.json");

        write_atomic(&file, "v1").unwrap();
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "state.json must not be group/world readable");

        write_atomic(&file, "v2").unwrap();
        let bak_mode = std::fs::metadata(dir.join("state.json.bak"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(bak_mode, 0o600, "state.json.bak must not be group/world readable");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

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

/// Create the temp file with owner-only permissions baked into the `open()`
/// call itself, so there is no window between file creation and chmod during
/// which a world/group-readable file with secrets (tokens, passwords, AWS
/// creds saved in requests) sits on disk.
///
/// `mode` is only honored by the kernel when `open()` actually creates a
/// fresh inode. `create(true)` without `O_EXCL` happily reopens/truncates a
/// file that already exists at this deterministic path (left over from a
/// prior crash between create and chmod, or planted by another local user
/// with write access to the directory) *without* ever applying `mode` to
/// it — so a stale wide-open file would silently receive the new secrets
/// while keeping its old permissions until the later `restrict_perms`
/// call. Use `create_new` (which forces `O_EXCL`) so `mode` is only ever
/// applied to a brand-new inode; if a stale file is in the way, remove it
/// and retry once so we still end up with a fresh 0600 file.
#[cfg(unix)]
fn create_tmp_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fn open_fresh(path: &Path) -> std::io::Result<std::fs::File> {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    }
    match open_fresh(path) {
        Ok(f) => Ok(f),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(path)?;
            open_fresh(path)
        }
        Err(e) => Err(e),
    }
}

#[cfg(windows)]
fn create_tmp_file(path: &Path) -> std::io::Result<std::fs::File> {
    // TODO: same ACL gap as restrict_perms above — no atomic owner-only
    // create on Windows via std, so this still opens with default perms.
    std::fs::File::create(path)
}

/// Atomically replace `file` with `data`: write a temp file, fsync it so the
/// bytes are actually on disk, keep the previous version as `.bak`, then
/// rename over the target. A crash mid-write (or a power loss right after)
/// can no longer destroy the existing state.
fn write_atomic(file: &Path, data: &str) -> Result<(), String> {
    let tmp = file.with_extension("json.tmp");
    {
        let mut f = create_tmp_file(&tmp).map_err(|e| e.to_string())?;
        f.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    // Belt-and-suspenders: on unix the file is already 0600 from the moment
    // it was created (no world/group-readable window), so this just forces
    // the exact mode regardless of umask; on Windows it remains the (currently
    // no-op) ACL TODO above.
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

    /// p1-TOCTOU regression test: the temp file must be born with 0600 — not
    /// created world/group-readable and chmod'd afterward, which would leave
    /// a window where secrets (tokens, passwords, AWS creds saved in
    /// requests) are readable by other local users. Checking the mode right
    /// after `create_tmp_file` returns, before any chmod call ever runs,
    /// proves there is no such window: permissions are set atomically by the
    /// `open()` call itself via `OpenOptionsExt::mode`.
    #[cfg(unix)]
    #[test]
    fn create_tmp_file_is_born_owner_only_with_no_toctou_window() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("maelstrom-test-toctou-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join("state.json.tmp");

        let f = create_tmp_file(&tmp).unwrap();
        let mode = f.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "temp file must be 0600 immediately on creation, before any later chmod"
        );
        drop(f);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// p1-TOCTOU regression, round 2: a stale/planted tmp file already
    /// sitting at the deterministic path (leftover from a prior crash
    /// between create and chmod, or planted by another local user with
    /// write access to the directory) must not be silently reused.
    /// `create(true)` without `O_EXCL` would reopen/truncate that existing
    /// inode without ever applying `mode`, so the new secrets would land
    /// in a file that keeps its old, possibly world/group-readable,
    /// permissions. Assert the file `create_tmp_file` hands back is a
    /// fresh, empty, strictly-0600 inode even when a wide-open file with
    /// content already existed at that path.
    #[cfg(unix)]
    #[test]
    fn create_tmp_file_replaces_preexisting_world_readable_file_with_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("maelstrom-test-stale-tmp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join("state.json.tmp");

        // Simulate a stale/planted tmp file with wide-open permissions and
        // leftover content from a previous (interrupted) write.
        std::fs::write(&tmp, b"leftover-secret").unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o666)).unwrap();

        let f = create_tmp_file(&tmp).unwrap();
        let mode = f.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "pre-existing tmp file must be replaced with a fresh 0600 inode, not reused"
        );
        assert_eq!(
            f.metadata().unwrap().len(),
            0,
            "pre-existing tmp file must be a fresh empty inode, not a reused/truncated one"
        );
        drop(f);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

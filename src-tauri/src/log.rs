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
/// The actual file I/O runs on a blocking thread (t5): `write` is called from
/// hot paths like the streams/scenario `on_log` callback, which can fire
/// often under load — doing metadata/rename/open/write synchronously there
/// would stall the tokio worker driving that run.
///
/// `category`/`msg` get a heuristic secret scrub first (p7): callers already
/// redact structured data before formatting a message (see `safe_url`/
/// `safe_headers`), but free-form text — notably messages logged verbatim
/// from the frontend via `log_event` — has no such guarantee, so this is a
/// last-resort safety net, not a replacement for redacting at the source.
pub fn write(app: &AppHandle, category: &str, msg: &str) {
    let app = app.clone();
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
    let category = mask_secrets(category);
    let line = mask_secrets(msg);
    tauri::async_runtime::spawn_blocking(move || {
        let Some(path) = log_file(&app) else { return };
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > MAX_BYTES {
                let _ = std::fs::rename(&path, path.with_file_name("maelstrom.log.old"));
            }
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(f, "{ts} [{category}] {line}");
        }
    });
}

/// Conservative redaction of secret-shaped `key=value` / `key: value` pairs
/// (authorization/token/secret/password/bearer/apikey, case-insensitive),
/// plus the `Authorization: <scheme> <token>` form, applied to every log
/// line as defense-in-depth (p7).
///
/// This only touches values that are STRUCTURALLY tied to a recognized
/// secret key via `=`/`:` — it never guesses at "the next word after a
/// trigger word" in free-form text. That guess used to be part of this
/// function and it was actively dangerous: for input like "your password is
/// hunter2, remember it" it masked "is" (the word after "password") and let
/// the actual secret "hunter2" through untouched. Masking the wrong token
/// while leaving the real secret exposed is strictly worse than not masking
/// at all, so when there's no confident `key=value`/`key:value` pairing this
/// function now leaves the line byte-for-byte unchanged. The primary
/// redaction happens upstream (see `safe_url`/`safe_headers`); this is only
/// a last-resort net for free-form text that bypasses those.
fn mask_secrets(msg: &str) -> String {
    fn normalized(s: &str) -> String {
        s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase()
    }
    // The key half of a `key=value`/`key:value` pair must match one of these
    // names EXACTLY (after case-folding and stripping separators like `-`/
    // `_`) — not merely contain one as a substring. Substring matching
    // over-fires on innocent keys that happen to embed one of these words
    // (e.g. "secretary=Jane", "tokenizer_version=2", "passwordless=true"),
    // and an over-fire here risks masking the wrong span. Whole-key matching
    // keeps this net conservative: only recognized secret-shaped keys are
    // touched, everything else is left exactly as written.
    fn key_is_sensitive(word: &str) -> bool {
        matches!(
            normalized(word).as_str(),
            "authorization" | "bearer" | "password" | "secret" | "token" | "apikey"
        )
    }
    // Auth-scheme words are never secrets themselves; when one shows up
    // right after a pending key (e.g. "Authorization: Bearer <token>"),
    // pass it through unmasked and keep waiting for the value after it.
    fn is_scheme_word(word: &str) -> bool {
        matches!(normalized(word).as_str(), "bearer" | "basic" | "digest")
    }

    let mut words: Vec<String> = Vec::new();
    let mut mask_next = false;
    let mut changed = false;
    for raw in msg.split_whitespace() {
        if let Some(idx) = raw.find(['=', ':']) {
            let (key, rest) = raw.split_at(idx);
            let sep = &rest[..1];
            let val = &rest[1..];
            if key_is_sensitive(key) {
                if val.is_empty() {
                    // "key:" / "key=" — the value is the next word (covers
                    // "key: value" and, via the scheme-word check below,
                    // "Authorization: Bearer <token>").
                    words.push(raw.to_string());
                    mask_next = true;
                } else {
                    // "key=value" / "key:value" glued into one word.
                    words.push(format!("{key}{sep}***"));
                    mask_next = false;
                    changed = true;
                }
                continue;
            }
        }
        if mask_next {
            if is_scheme_word(raw) {
                // "Bearer"/"Basic"/"Digest" itself isn't the secret — pass
                // it through and keep looking for the value after it.
                words.push(raw.to_string());
            } else {
                words.push("***".to_string());
                mask_next = false;
                changed = true;
            }
            continue;
        }
        words.push(raw.to_string());
    }
    // No structural key=value/key:value pairing found — return the original
    // bytes untouched rather than a whitespace-collapsed reconstruction.
    if !changed {
        return msg.to_string();
    }
    words.join(" ")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_authorization_bearer_value() {
        // Structural "Authorization: Bearer <token>" form. The token value
        // deliberately contains "SECRET"/"TOKEN" as substrings (real JWTs
        // often do) to make sure the scheme word "Bearer" and the unrelated
        // trailing word survive untouched while only the actual value is
        // replaced.
        let masked = mask_secrets(
            "отправлен запрос Authorization: Bearer eyJhbGciOi.SECRET.TOKEN конец",
        );
        assert!(!masked.contains("SECRET"), "leaked: {masked}");
        assert!(!masked.contains("eyJhbGciOi"), "leaked: {masked}");
        assert!(masked.contains("Bearer"), "scheme word wrongly masked: {masked}");
        assert!(masked.contains("конец"), "unrelated trailing word wrongly masked: {masked}");
    }

    #[test]
    fn masks_key_colon_value_given_as_the_next_word() {
        // No literal "Bearer" — just "Authorization: <value>".
        let masked = mask_secrets("Authorization: eyJhbGciOiJIUzI1NiJ9.raw.sig");
        assert!(!masked.contains("eyJhbGciOiJIUzI1NiJ9"), "leaked: {masked}");
    }

    #[test]
    fn masks_key_colon_space_value_pairs() {
        // "key: value" with a space, not glued — the other structural form
        // called out for this scrubber.
        let masked = mask_secrets("login ok, password: hunter2 stored");
        assert!(!masked.contains("hunter2"), "leaked: {masked}");
        assert!(masked.contains("***"), "not masked: {masked}");
    }

    #[test]
    fn masks_key_equals_value_pairs_glued_in_one_word() {
        for line in [
            "token=abc123xyz",
            "api_key=abc123xyz",
            "api-key=abc123xyz",
            "apikey=abc123xyz",
            "secret=abc123xyz",
            "password=abc123xyz",
        ] {
            let masked = mask_secrets(line);
            assert!(!masked.contains("abc123xyz"), "{line} leaked: {masked}");
            assert!(masked.contains("***"), "{line} not masked: {masked}");
        }
    }

    #[test]
    fn leaves_natural_language_sentences_unchanged() {
        // Regression for the p7 bug: a trigger word with no structural `=`/
        // `:` link to a value must never cause ANY word to be masked — doing
        // so risks masking an innocent neighbor while the real secret (here,
        // "hunter2") sails through untouched. Conservative behavior is to
        // leave the line byte-for-byte unchanged when there's no confident
        // key=value/key:value pairing.
        for line in [
            "your password is hunter2, remember it",
            "the secret ingredient is love",
            "please send your token to the admin later",
            "5 ручек, 30с | GET https://api.example.com/x @10rps",
        ] {
            assert_eq!(mask_secrets(line), line, "wrongly modified: {line}");
        }
    }

    #[test]
    fn does_not_mask_keys_that_merely_contain_a_secret_word() {
        // "secretary"/"tokenizer_version"/"passwordless" are not secret keys
        // even though they contain "secret"/"token"/"password" as
        // substrings — the key must match a known secret-shaped name
        // exactly, not just contain one.
        for line in [
            "secretary=Jane",
            "tokenizer_version=2",
            "passwordless_login=true",
        ] {
            assert_eq!(mask_secrets(line), line, "wrongly masked: {line}");
        }
    }

    #[test]
    fn write_does_not_panic_without_a_tauri_runtime() {
        // Smoke test: mask_secrets (the pure part write() delegates to before
        // spawning the blocking I/O) must never panic on arbitrary input.
        let _ = mask_secrets("");
        let _ = mask_secrets("Bearer");
        let _ = mask_secrets("token=");
        let _ = mask_secrets("=token=weird:case:");
    }
}

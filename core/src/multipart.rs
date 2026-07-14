// multipart/form-data bodies with file parts (images, jars, …). Files are read
// into memory once (via `prepare_parts`) and a fresh `Form` is built per request
// so the same upload can be replayed under load.
use crate::dynval::RequestCtx;
use crate::types::MultipartPart;
use reqwest::multipart::{Form, Part};

pub enum Prepared {
    Text(String),
    File {
        // `Bytes` so each request's form reuses the buffer via a refcount bump
        // instead of copying the whole file.
        bytes: bytes::Bytes,
        filename: String,
        mime: Option<String>,
    },
    /// A file drawn per request from a named pool (`{{$file.NAME}}`).
    Pool {
        pool: String,
        filename: Option<String>,
        mime: Option<String>,
    },
}

pub struct PreparedPart {
    pub name: String,
    pub data: Prepared,
}

/// If a file part's value is `{{$file.NAME}}`, return NAME.
fn parse_file_ref(value: &str) -> Option<String> {
    let v = value.trim();
    let inner = v.strip_prefix("{{$file.")?.strip_suffix("}}")?;
    let name = inner.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

pub(crate) fn basename(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// Guess a content type from the file extension when the user didn't set one.
pub(crate) fn guess_mime(filename: &str) -> Option<&'static str> {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "xml" => "application/xml",
        "csv" => "text/csv",
        "txt" => "text/plain",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "jar" => "application/java-archive",
        "war" => "application/java-archive",
        "bin" | "exe" => "application/octet-stream",
        _ => return None,
    })
}

/// Read file parts into memory. Called once before a run so files aren't re-read
/// on every request.
pub fn prepare_parts(parts: &[MultipartPart]) -> Result<Vec<PreparedPart>, String> {
    let mut out = Vec::new();
    for p in parts {
        if !p.enabled || p.name.trim().is_empty() {
            continue;
        }
        if p.kind == "file" {
            let path = p.value.trim();
            if path.is_empty() {
                return Err(format!("У файлового поля «{}» не указан путь", p.name));
            }
            // A pool reference is resolved per request, not read here.
            if let Some(pool) = parse_file_ref(path) {
                out.push(PreparedPart {
                    name: p.name.clone(),
                    data: Prepared::Pool {
                        pool,
                        filename: p.filename.clone().filter(|f| !f.trim().is_empty()),
                        mime: p.content_type.clone().filter(|m| !m.trim().is_empty()),
                    },
                });
                continue;
            }
            let bytes = std::fs::read(path)
                .map_err(|e| format!("Не удалось прочитать файл {path}: {e}"))?;
            let filename = p
                .filename
                .clone()
                .filter(|f| !f.trim().is_empty())
                .unwrap_or_else(|| basename(path));
            let mime = p
                .content_type
                .clone()
                .filter(|m| !m.trim().is_empty())
                .or_else(|| guess_mime(&filename).map(|s| s.to_string()));
            out.push(PreparedPart {
                name: p.name.clone(),
                data: Prepared::File { bytes: bytes::Bytes::from(bytes), filename, mime },
            });
        } else {
            out.push(PreparedPart {
                name: p.name.clone(),
                data: Prepared::Text(p.value.clone()),
            });
        }
    }
    Ok(out)
}

/// Build a fresh multipart Form from prepared parts (cheap byte copies). Text
/// parts get per-request dynamic expansion; file bytes are reused as-is.
pub fn form_from_prepared(prepared: &[PreparedPart], ctx: &RequestCtx) -> Form {
    let mut form = Form::new();
    for p in prepared {
        match &p.data {
            Prepared::Text(v) => {
                form = form.text(p.name.clone(), ctx.expand(v));
            }
            Prepared::File { bytes, filename, mime } => {
                form = form.part(p.name.clone(), file_part(bytes, filename, mime.as_deref()));
            }
            Prepared::Pool { pool, filename, mime } => {
                // Pick this request's file from the pool; skip the part if the
                // pool is empty/unknown.
                if let Some(f) = ctx.pick_file(pool) {
                    let name = filename.clone().unwrap_or_else(|| f.filename.clone());
                    let m = mime.clone().or_else(|| f.mime.clone());
                    form = form.part(p.name.clone(), file_part(&f.bytes, &name, m.as_deref()));
                }
            }
        }
    }
    form
}

/// Build a multipart file part, falling back to no explicit MIME if the string
/// is invalid (mime_str consumes the part, so it's rebuilt on failure). The
/// shared `Bytes` is reused (refcount bump) rather than copied per request.
fn file_part(bytes: &bytes::Bytes, filename: &str, mime: Option<&str>) -> Part {
    let len = bytes.len() as u64;
    let make = || {
        Part::stream_with_length(reqwest::Body::from(bytes.clone()), len)
            .file_name(filename.to_string())
    };
    match mime {
        Some(m) => make().mime_str(m).unwrap_or_else(|_| make()),
        None => make(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MultipartPart;

    fn part(name: &str, kind: &str, value: &str) -> MultipartPart {
        MultipartPart {
            name: name.into(),
            kind: kind.into(),
            value: value.into(),
            filename: None,
            content_type: None,
            enabled: true,
        }
    }

    #[test]
    fn mime_by_extension() {
        assert_eq!(guess_mime("a.png"), Some("image/png"));
        assert_eq!(guess_mime("x.JPG"), Some("image/jpeg"));
        assert_eq!(guess_mime("lib.jar"), Some("application/java-archive"));
        assert_eq!(guess_mime("data.csv"), Some("text/csv"));
        assert_eq!(guess_mime("noext"), None);
    }

    #[test]
    fn basename_strips_dirs() {
        assert_eq!(basename("C:/a/b/c.png"), "c.png");
        assert_eq!(basename("/tmp/x.jar"), "x.jar");
        assert_eq!(basename("plain"), "plain");
    }

    #[test]
    fn prepare_skips_disabled_and_unnamed() {
        let mut disabled = part("a", "text", "1");
        disabled.enabled = false;
        let parts = vec![disabled, part("", "text", "2"), part("keep", "text", "3")];
        let prepared = prepare_parts(&parts).unwrap();
        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].name, "keep");
    }

    #[test]
    fn missing_file_path_errors() {
        let parts = vec![part("f", "file", "")];
        assert!(prepare_parts(&parts).is_err());
    }

    #[test]
    fn parses_file_pool_reference() {
        assert_eq!(parse_file_ref("{{$file.imgs}}"), Some("imgs".into()));
        assert_eq!(parse_file_ref("  {{$file.pool1}} "), Some("pool1".into()));
        assert_eq!(parse_file_ref("/real/path.png"), None);
        assert_eq!(parse_file_ref("{{$file.}}"), None);
        assert_eq!(parse_file_ref("{{var}}"), None);
    }

    #[test]
    fn prepare_maps_pool_ref_without_reading_disk() {
        // A pool reference must NOT be read from disk here (no such file exists).
        let parts = vec![part("photo", "file", "{{$file.imgs}}")];
        let prepared = prepare_parts(&parts).unwrap();
        assert_eq!(prepared.len(), 1);
        match &prepared[0].data {
            Prepared::Pool { pool, .. } => assert_eq!(pool, "imgs"),
            _ => panic!("expected a pool part"),
        }
    }
}

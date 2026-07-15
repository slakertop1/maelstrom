//! Dynamic gRPC for Maelstrom: compile a `.proto` at runtime, introspect its
//! services/methods, and call them with JSON payloads — no codegen, no protoc.

use prost_reflect::{DescriptorPool, MethodDescriptor};
use std::path::{Path, PathBuf};

/// Cap on the entry `.proto` file's size, checked before it's handed to
/// protox. Real-world `.proto` files are a few KB to a few hundred KB; this
/// is a cheap guard against a pathological input being used to burn memory
/// or CPU during compilation.
const MAX_PROTO_SOURCE_BYTES: usize = 4 * 1024 * 1024;

/// A parsed `.proto` (plus its imports), ready to introspect and call.
pub struct Proto {
    pool: DescriptorPool,
}

/// One callable RPC method, in a UI-friendly shape.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MethodInfo {
    /// `package.Service` — the gRPC service name.
    pub service: String,
    /// Method name, e.g. `GetUser`.
    pub method: String,
    /// Full path used on the wire: `/package.Service/Method`.
    pub path: String,
    pub client_streaming: bool,
    pub server_streaming: bool,
    pub input_type: String,
    pub output_type: String,
}

impl Proto {
    /// Compile a `.proto` file from disk. Imports are resolved against the
    /// user-provided `include` dirs plus the file's own directory. Missing
    /// imports are auto-discovered: on an "import X not found" error we search
    /// the file's directory tree for a root dir that contains X, add it, and
    /// retry — so a repo that vendors googleapis under `external/` just works.
    pub fn from_file(path: &str, includes: &[String]) -> Result<Proto, String> {
        let mut dirs: Vec<PathBuf> = includes.iter().map(PathBuf::from).collect();
        let proto = Path::new(path);
        let search_root = proto.parent().map(|p| p.to_path_buf());
        if let Some(r) = &search_root {
            if !dirs.contains(r) {
                dirs.push(r.clone());
            }
        }

        // Guard against pathological/hostile input before it ever reaches
        // protox: cap the entry file's size, and reject import statements
        // that try to walk out of the include tree via ".." or an absolute
        // path (e.g. `import "../../../../etc/passwd";`). This only covers
        // the entry file's own imports — files pulled in later via
        // `find_include_root` are not re-scanned; full sandboxing of the
        // whole import graph is out of scope here.
        let source = std::fs::read_to_string(proto).map_err(|e| format!("Ошибка чтения .proto: {e}"))?;
        if source.len() > MAX_PROTO_SOURCE_BYTES {
            return Err(format!(
                "Ошибка .proto: файл слишком большой ({} байт, лимит {MAX_PROTO_SOURCE_BYTES} байт)",
                source.len()
            ));
        }
        check_import_paths(&source)?;

        // Retry, adding one discovered include root per round (bounded).
        for _ in 0..64 {
            // protox::compile on a hostile/malformed .proto has no size or
            // recursion guard of its own; catch a panic instead of taking
            // the whole process down with it.
            let compiled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                protox::compile([proto], dirs.iter().map(|d| d.as_path()))
            }));
            let compiled = match compiled {
                Ok(r) => r,
                Err(_) => {
                    return Err(
                        "Ошибка .proto: компилятор аварийно завершился на этом файле".to_string()
                    )
                }
            };
            match compiled {
                Ok(fds) => {
                    let pool = DescriptorPool::from_file_descriptor_set(fds)
                        .map_err(|e| format!("Дескрипторы: {e}"))?;
                    return Ok(Proto { pool });
                }
                Err(e) => {
                    let msg = e.to_string();
                    let Some(missing) = extract_missing_import(&msg) else {
                        return Err(format!("Ошибка .proto: {msg}"));
                    };
                    match search_root.as_deref().and_then(|sr| find_include_root(sr, &missing)) {
                        Some(dir) if !dirs.contains(&dir) => dirs.push(dir),
                        _ => {
                            return Err(format!(
                                "Ошибка .proto: не найден импорт «{missing}». Положите его в дерево рядом с .proto или укажите папку импортов."
                            ));
                        }
                    }
                }
            }
        }
        Err("Ошибка .proto: слишком много уровней импортов".to_string())
    }

    /// Compile a `.proto` from inline text (single file, no imports). Written to
    /// a temp file so protox does full linking/type resolution.
    pub fn from_source(name: &str, source: &str) -> Result<Proto, String> {
        let base = sanitize_proto_name(name);
        let dir = std::env::temp_dir().join(format!("maelstrom-proto-{}-{}", std::process::id(), base));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let file = dir.join(format!("{base}.proto"));
        std::fs::write(&file, source).map_err(|e| e.to_string())?;
        let result = Proto::from_file(&file.to_string_lossy(), &[]);
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    /// All callable methods across all services in the proto.
    pub fn methods(&self) -> Vec<MethodInfo> {
        self.pool
            .services()
            .flat_map(|svc| svc.methods().collect::<Vec<_>>())
            .map(|m| method_info(&m))
            .collect()
    }

    fn find_method(&self, service: &str, method: &str) -> Result<MethodDescriptor, String> {
        let svc = self
            .pool
            .services()
            .find(|s| s.full_name() == service || s.name() == service)
            .ok_or_else(|| format!("Сервис «{service}» не найден"))?;
        svc.methods()
            .collect::<Vec<_>>()
            .into_iter()
            .find(|m| m.name() == method)
            .ok_or_else(|| format!("Метод «{method}» не найден в «{service}»"))
    }
}

/// Derive a filesystem-safe file stem from a user-supplied proto name. Only
/// the final path segment is kept (so "/", "\" and ".." in `name` can't
/// steer the write outside the temp dir we create), and any character
/// outside `[A-Za-z0-9_-]` is replaced — the result never influences which
/// *directory* gets written to, only the file name inside a process-private
/// temp dir.
fn sanitize_proto_name(name: &str) -> String {
    let trimmed = name.trim_end_matches(".proto");
    let last = trimmed.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    let cleaned = cleaned.trim_matches('_').to_string();
    if cleaned.is_empty() {
        "proto".to_string()
    } else {
        cleaned
    }
}

/// Reject `.proto` import statements that try to escape the include tree via
/// a `..` path segment or an absolute path, e.g.
/// `import "../../../../etc/passwd";`. Best-effort static scan of the raw
/// import string literals, run before the file is ever handed to protox.
fn check_import_paths(source: &str) -> Result<(), String> {
    let mut rest = source;
    while let Some(pos) = rest.find("import") {
        rest = &rest[pos + "import".len()..];
        let after_kw = rest.trim_start();
        let after_modifier = after_kw
            .strip_prefix("public")
            .or_else(|| after_kw.strip_prefix("weak"))
            .map(str::trim_start)
            .unwrap_or(after_kw);
        let Some(quote) = after_modifier.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            continue;
        };
        let body = &after_modifier[1..];
        let Some(end) = body.find(quote) else { continue };
        let import_path = &body[..end];
        if is_unsafe_import_path(import_path) {
            return Err(format!(
                "Ошибка .proto: недопустимый импорт «{import_path}» — абсолютные пути и «..» запрещены"
            ));
        }
        rest = &body[end + 1..];
    }
    Ok(())
}

/// True if an import path is absolute (Unix or Windows-drive-letter style)
/// or contains a `..` segment.
fn is_unsafe_import_path(p: &str) -> bool {
    let normalized = p.replace('\\', "/");
    if normalized.starts_with('/') {
        return true;
    }
    if normalized.as_bytes().get(1) == Some(&b':') {
        return true; // e.g. "C:/secrets"
    }
    normalized.split('/').any(|seg| seg == "..")
}

/// Pull the missing import path out of a protox compile error, e.g.
/// `import 'google/api/http.proto' not found` → `google/api/http.proto`.
fn extract_missing_import(msg: &str) -> Option<String> {
    if !msg.contains("not found") && !msg.to_lowercase().contains("import") {
        return None;
    }
    for q in ['\'', '"'] {
        if let Some(a) = msg.find(q) {
            if let Some(len) = msg[a + 1..].find(q) {
                let s = &msg[a + 1..a + 1 + len];
                if s.ends_with(".proto") {
                    return Some(s.replace('\\', "/"));
                }
            }
        }
    }
    None
}

/// Search `search_root`'s tree for a directory `D` such that `D/<missing>`
/// exists — that `D` is the include root that resolves the import.
fn find_include_root(search_root: &Path, missing: &str) -> Option<PathBuf> {
    let mut budget = 20_000usize; // cap dirs/entries scanned
    let mut stack = vec![search_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if dir.join(missing).is_file() {
            return Some(dir);
        }
        if budget == 0 {
            break;
        }
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                if budget == 0 {
                    break;
                }
                budget -= 1;
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                }
            }
        }
    }
    None
}

fn method_info(m: &MethodDescriptor) -> MethodInfo {
    let svc = m.parent_service();
    MethodInfo {
        service: svc.full_name().to_string(),
        method: m.name().to_string(),
        path: format!("/{}/{}", svc.full_name(), m.name()),
        client_streaming: m.is_client_streaming(),
        server_streaming: m.is_server_streaming(),
        input_type: m.input().full_name().to_string(),
        output_type: m.output().full_name().to_string(),
    }
}

mod codec;
mod invoke;
pub use invoke::{
    grpc_load, json_to_message, message_to_json, CallResult, GrpcLoadResult, LoadCall,
};
pub use prost_reflect::{DynamicMessage, MessageDescriptor};

#[cfg(test)]
mod tests {
    use super::*;

    const PROTO: &str = r#"
        syntax = "proto3";
        package demo;
        message HelloRequest { string name = 1; }
        message HelloReply { string message = 1; }
        service Greeter {
            rpc SayHello (HelloRequest) returns (HelloReply);
            rpc SayHelloStream (HelloRequest) returns (stream HelloReply);
        }
    "#;

    #[test]
    fn parses_proto_and_lists_methods() {
        let proto = Proto::from_source("demo.proto", PROTO).unwrap();
        let mut methods = proto.methods();
        methods.sort_by(|a, b| a.method.cmp(&b.method));
        assert_eq!(methods.len(), 2);

        let hello = &methods[0];
        assert_eq!(hello.service, "demo.Greeter");
        assert_eq!(hello.method, "SayHello");
        assert_eq!(hello.path, "/demo.Greeter/SayHello");
        assert!(!hello.server_streaming);
        assert_eq!(hello.input_type, "demo.HelloRequest");

        let stream = &methods[1];
        assert_eq!(stream.method, "SayHelloStream");
        assert!(stream.server_streaming);
    }

    #[test]
    fn find_method_errors_are_clear() {
        let proto = Proto::from_source("demo.proto", PROTO).unwrap();
        assert!(proto.find_method("demo.Greeter", "Nope").unwrap_err().contains("не найден"));
        assert!(proto.find_method("Nope", "SayHello").unwrap_err().contains("не найден"));
    }

    #[test]
    fn bad_proto_reports_error() {
        assert!(Proto::from_source("x.proto", "this is not valid proto").is_err());
    }

    #[test]
    fn extracts_missing_import_from_error() {
        assert_eq!(
            extract_missing_import("proto:5:1: import 'google/api/http.proto' not found"),
            Some("google/api/http.proto".to_string())
        );
        assert_eq!(
            extract_missing_import("import \"model.proto\" not found"),
            Some("model.proto".to_string())
        );
        assert_eq!(extract_missing_import("some unrelated error"), None);
    }

    #[test]
    fn auto_resolves_nested_import_root() {
        // Layout mimics a repo vendoring deps: main.proto imports sub/leaf.proto
        // (found via the file's own dir), but leaf.proto imports "inner.proto"
        // which only resolves if <dir>/sub is ALSO an include root. The auto
        // resolver must discover and add it.
        let dir = std::env::temp_dir().join(format!("mgrpc-imp-{}", std::process::id()));
        let sub = dir.join("sub");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            dir.join("main.proto"),
            "syntax=\"proto3\"; package demo; import \"sub/leaf.proto\";\nmessage Main { demo.Leaf leaf = 1; }\nservice S { rpc Do(Main) returns (Main); }",
        )
        .unwrap();
        std::fs::write(
            sub.join("leaf.proto"),
            "syntax=\"proto3\"; package demo; import \"inner.proto\";\nmessage Leaf { demo.Inner inner = 1; }",
        )
        .unwrap();
        std::fs::write(
            sub.join("inner.proto"),
            "syntax=\"proto3\"; package demo; message Inner { string v = 1; }",
        )
        .unwrap();

        let proto = Proto::from_file(&dir.join("main.proto").to_string_lossy(), &[])
            .expect("auto-resolve nested import");
        assert!(proto.methods().iter().any(|m| m.method == "Do"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}

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
        // path (e.g. `import "../../../../etc/passwd";`). The size cap and
        // import-path check only cover the entry file's own text — files
        // pulled in later via `find_include_root` are not re-scanned for
        // those; full sandboxing of the whole import graph is out of scope
        // here. The nesting-depth check below is different: protox itself
        // opens and parses every transitively imported `.proto` it finds,
        // so that check is applied to the whole reachable set, not just
        // this file — see `check_include_dirs_nesting_depth`.
        let source = std::fs::read_to_string(proto).map_err(|e| format!("Ошибка чтения .proto: {e}"))?;
        if source.len() > MAX_PROTO_SOURCE_BYTES {
            return Err(format!(
                "Ошибка .proto: файл слишком большой ({} байт, лимит {MAX_PROTO_SOURCE_BYTES} байт)",
                source.len()
            ));
        }
        // Cheap pre-check BEFORE protox ever sees the source: a pathological
        // brace nesting depth can blow the stack in protox's recursive-descent
        // parser, which is a process abort (not a panic) — catch_unwind below
        // cannot catch that, so this has to run first. See check_nesting_depth.
        check_nesting_depth(&source)?;
        check_import_paths(&source)?;

        // Retry, adding one discovered include root per round (bounded).
        // `dirs` only grows across iterations, so track how much of it has
        // already been swept for nesting depth and only scan the new tail
        // each time, instead of re-walking everything on every retry.
        let mut nesting_checked_dirs = 0usize;
        for _ in 0..64 {
            // protox doesn't just parse `proto` — it opens and parses every
            // `.proto` it transitively imports from anywhere under `dirs`.
            // A deeply nested *imported* file can blow the parser's stack
            // exactly like a deeply nested entry file, so every include dir
            // has to be swept for depth before each compile attempt below,
            // including dirs added by the auto-discovery further down.
            if nesting_checked_dirs < dirs.len() {
                check_include_dirs_nesting_depth(&dirs[nesting_checked_dirs..])?;
                nesting_checked_dirs = dirs.len();
            }
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
///
/// The raw string between the quotes is decoded with [`decode_proto_escapes`]
/// before the safety check: protox's lexer (like protoc's) understands octal
/// (e.g. `\057`), hex (e.g. `\x2f`), and Unicode (`\uXXXX` / `\UXXXXXXXX`)
/// escapes inside string literals. All three encode arbitrary bytes/code
/// points without a literal `/` or `.` ever appearing in the raw source, so
/// an import string built entirely out of these escapes can still resolve
/// to a real traversal or absolute path at compile time. Checking the raw
/// text alone would miss that.
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
        let decoded = decode_proto_escapes(import_path);
        if is_unsafe_import_path(&decoded) {
            return Err(format!(
                "Ошибка .proto: недопустимый импорт «{import_path}» — абсолютные пути и «..» запрещены"
            ));
        }
        rest = &body[end + 1..];
    }
    Ok(())
}

/// Decode the proto string-literal escapes that can smuggle a path-traversal
/// payload past a naive raw-text scan: octal (`\NNN`, 1-3 octal digits) and
/// hex (`\xNN`/`\XNN`, 1-2 hex digits) byte escapes — e.g. `\057` and `\x2f`
/// both decode to `/` — plus Unicode scalar escapes, `\uXXXX` (exactly 4 hex
/// digits) and `\UXXXXXXXX` (exactly 8 hex digits), which protox's lexer
/// (like protoc's) also accepts and which decode to the UTF-8 encoding of
/// the given code point, so a `.` or `/` can be spelled as its Unicode
/// escape just as easily as its octal or hex one. Also unescapes the common
/// single-character C-style escapes (`\n \t \r \\ \'
/// \"`) so they aren't misread as literal backslash sequences. Not a full
/// proto string-literal parser (that's protox's job) — this only needs to
/// see through escapes well enough for the traversal/absolute-path check in
/// `is_unsafe_import_path` to run on the same bytes protox will actually
/// resolve the import against.
fn decode_proto_escapes(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        match bytes[i + 1] {
            b'x' | b'X' => {
                let mut j = i + 2;
                let mut val: u32 = 0;
                let mut digits = 0;
                while j < bytes.len() && digits < 2 && (bytes[j] as char).is_ascii_hexdigit() {
                    val = val * 16 + (bytes[j] as char).to_digit(16).unwrap();
                    j += 1;
                    digits += 1;
                }
                if digits == 0 {
                    out.push(bytes[i]);
                    i += 1;
                } else {
                    out.push(val as u8);
                    i = j;
                }
            }
            b'u' => match parse_unicode_escape(bytes, i + 2, 4) {
                Some((ch, digits)) => {
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    i += 2 + digits;
                }
                None => {
                    out.push(bytes[i]);
                    i += 1;
                }
            },
            b'U' => match parse_unicode_escape(bytes, i + 2, 8) {
                Some((ch, digits)) => {
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    i += 2 + digits;
                }
                None => {
                    out.push(bytes[i]);
                    i += 1;
                }
            },
            b'0'..=b'7' => {
                let mut j = i + 1;
                let mut val: u32 = 0;
                let mut digits = 0;
                while j < bytes.len() && digits < 3 && (b'0'..=b'7').contains(&bytes[j]) {
                    val = val * 8 + (bytes[j] - b'0') as u32;
                    j += 1;
                    digits += 1;
                }
                out.push(val as u8);
                i = j;
            }
            b'n' => {
                out.push(b'\n');
                i += 2;
            }
            b't' => {
                out.push(b'\t');
                i += 2;
            }
            b'r' => {
                out.push(b'\r');
                i += 2;
            }
            b'\\' | b'\'' | b'"' => {
                out.push(bytes[i + 1]);
                i += 2;
            }
            _ => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse exactly `n` ASCII hex digits starting at `bytes[start]` and decode
/// them as a Unicode scalar value, per proto's `\uXXXX` (`n == 4`) and
/// `\UXXXXXXXX` (`n == 8`) escapes. Unlike `\x`, protox's lexer requires the
/// exact digit count for these — not "up to `n`" — so this only matches
/// when all `n` digits are present and valid. Returns `None` (caller falls
/// back to treating the backslash as a literal character, same as every
/// other malformed-escape case in `decode_proto_escapes`) if there aren't
/// enough digits, they aren't all hex digits, or the resulting code point
/// isn't a valid Unicode scalar value (e.g. a lone surrogate half).
fn parse_unicode_escape(bytes: &[u8], start: usize, n: usize) -> Option<(char, usize)> {
    let digits = bytes.get(start..start + n)?;
    if !digits.iter().all(|b| (*b as char).is_ascii_hexdigit()) {
        return None;
    }
    let code = u32::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()?;
    char::from_u32(code).map(|ch| (ch, n))
}

/// Cap on `{ }` nesting depth (message/service/enum/oneof/extend blocks, or
/// anything else brace-delimited) in a `.proto`, checked with a cheap linear
/// scan before the file is handed to protox. Pathologically deep nesting can
/// blow the stack in protox's recursive-descent parser; a stack overflow in
/// Rust aborts the process instead of panicking, so `std::panic::catch_unwind`
/// around `protox::compile` (see `Proto::from_file`) can't protect against
/// this shape of input — it has to be rejected before compilation starts.
const MAX_BRACE_DEPTH: usize = 100;

/// Reject a `.proto` whose brace nesting exceeds [`MAX_BRACE_DEPTH`]. Braces
/// inside string/char literals and `//`/`/* */` comments don't count, so a
/// legitimate file with braces in an option string isn't misflagged.
fn check_nesting_depth(source: &str) -> Result<(), String> {
    let mut depth: usize = 0;
    let mut chars = source.chars().peekable();
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut string_quote: Option<char> = None;

    while let Some(c) = chars.next() {
        if in_line_comment {
            if c == '\n' {
                in_line_comment = false;
            }
            continue;
        }
        if in_block_comment {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block_comment = false;
            }
            continue;
        }
        if let Some(q) = string_quote {
            if c == '\\' {
                chars.next(); // skip whatever follows the escape, e.g. `\"`
            } else if c == q {
                string_quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => string_quote = Some(c),
            '/' if chars.peek() == Some(&'/') => {
                chars.next();
                in_line_comment = true;
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                in_block_comment = true;
            }
            '{' => {
                depth += 1;
                if depth > MAX_BRACE_DEPTH {
                    return Err(format!(
                        "Ошибка .proto: слишком глубокая вложенность блоков (> {MAX_BRACE_DEPTH} уровней)"
                    ));
                }
            }
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

/// Recursively sweep every `.proto` file reachable under `dirs` and apply
/// [`check_nesting_depth`] to each. `check_nesting_depth` on its own only
/// ever sees the entry file's source — but `protox::compile` resolves and
/// parses *every* file it transitively imports by searching these same
/// include directories, so a deeply nested file that is only ever reached
/// through an `import` statement (never checked otherwise) can still blow
/// the stack in protox's recursive-descent parser and abort the process.
/// This has to run — and reject — before `protox::compile` is invoked,
/// same as the entry-file check; `catch_unwind` cannot catch a stack
/// overflow. Best-effort and bounded (like `find_include_root`): stops
/// after a fixed number of directory entries so a pathologically large
/// include tree can't be used to make this scan itself expensive.
fn check_include_dirs_nesting_depth(dirs: &[PathBuf]) -> Result<(), String> {
    let mut budget = 20_000usize; // cap dirs/files scanned, mirrors find_include_root
    let mut stack: Vec<PathBuf> = dirs.to_vec();
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            if budget == 0 {
                return Ok(());
            }
            budget -= 1;
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|e| e.to_str()) == Some("proto") {
                if let Ok(contents) = std::fs::read_to_string(&p) {
                    check_nesting_depth(&contents)
                        .map_err(|e| format!("{e} (импортируемый файл «{}»)", p.display()))?;
                }
            }
        }
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
/// Re-exported so callers of [`invoke::Proto::build_call_with_tls`] /
/// `call_json_with_tls` (custom CA / mTLS for gRPC — see `invoke.rs`) don't
/// need a separate direct dependency on `maelstrom-core` just for this type.
pub use maelstrom_core::types::TlsConfig;

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

    // ---- g2: proto-escape-aware import safety check ----

    #[test]
    fn decode_proto_escapes_handles_octal_and_hex() {
        assert_eq!(decode_proto_escapes("\\057"), "/");
        assert_eq!(decode_proto_escapes("\\x2f"), "/");
        assert_eq!(decode_proto_escapes("\\X2F"), "/");
        assert_eq!(decode_proto_escapes("plain/path.proto"), "plain/path.proto");
        assert_eq!(decode_proto_escapes("\\056\\056"), "..");
    }

    #[test]
    fn decode_proto_escapes_handles_unicode_u_and_capital_u() {
        // The escape "u002e" (with a leading backslash) is U+002E ('.');
        // "U0000002f" (with a leading backslash) is U+002F ('/').
        assert_eq!(decode_proto_escapes("\\u002e\\u002e"), "..");
        assert_eq!(decode_proto_escapes("\\U0000002f"), "/");
        assert_eq!(decode_proto_escapes("\\u002F"), "/"); // hex digits case-insensitive
        // Malformed (too few hex digits): falls back to the literal text,
        // same as every other malformed-escape case in the function.
        assert_eq!(decode_proto_escapes("\\u2f"), "\\u2f");
        assert_eq!(decode_proto_escapes("\\U2f"), "\\U2f");
    }

    #[test]
    fn octal_escaped_traversal_import_is_rejected() {
        // \056 is octal for '.', so this decodes to "../../etc/passwd" — a
        // path-traversal payload smuggled past a raw-text scan that doesn't
        // understand proto string escapes.
        let src = r#"
            syntax = "proto3";
            package demo;
            import "\056\056/\056\056/etc/passwd";
            message M { string v = 1; }
        "#;
        let err = Proto::from_source("evil-octal.proto", src).err().unwrap();
        assert!(err.contains("недопустимый импорт"), "got {err}");
    }

    #[test]
    fn hex_escaped_absolute_import_is_rejected() {
        // \x2f is hex for '/', so this decodes to an absolute Unix path.
        let src = r#"
            syntax = "proto3";
            package demo;
            import "\x2fetc\x2fpasswd";
            message M { string v = 1; }
        "#;
        let err = Proto::from_source("evil-hex.proto", src).err().unwrap();
        assert!(err.contains("недопустимый импорт"), "got {err}");
    }

    #[test]
    fn plain_traversal_import_is_still_rejected() {
        // Regression: the escape-decoding change must not weaken the
        // existing raw-text ".." / absolute-path checks.
        let src = r#"
            syntax = "proto3";
            package demo;
            import "../../../../etc/passwd";
            message M { string v = 1; }
        "#;
        let err = Proto::from_source("evil-plain.proto", src).err().unwrap();
        assert!(err.contains("недопустимый импорт"), "got {err}");
    }

    #[test]
    fn unicode_u_escaped_traversal_import_is_rejected() {
        // Build the import path's escapes for '.' and '/' as 4-hex-digit
        // Unicode scalar escapes (computed, not typed literally, so the
        // intent can't be lost to a copy/paste slip): the resulting raw
        // .proto source contains no literal '.' or '/' at all, only
        // backslash-u-hexhexhexhex runs — exactly what protox's lexer
        // decodes, which a check that only handled octal/hex escapes would
        // miss.
        let esc = |c: char| format!("\\u{:04x}", c as u32);
        let dot = esc('.');
        let slash = esc('/');
        let payload = format!("{dot}{dot}{slash}{dot}{dot}{slash}etc{slash}passwd");
        assert_eq!(payload.matches('.').count(), 0, "payload must not contain a literal dot");
        assert_eq!(payload.matches('/').count(), 0, "payload must not contain a literal slash");
        let src = format!(
            "syntax = \"proto3\";\npackage demo;\nimport \"{payload}\";\nmessage M {{ string v = 1; }}\n"
        );
        let err = Proto::from_source("evil-unicode-u.proto", &src).err().unwrap();
        assert!(err.contains("недопустимый импорт"), "got {err}");
    }

    #[test]
    fn unicode_big_u_escaped_absolute_import_is_rejected() {
        // Same idea with the 8-hex-digit form: an absolute Unix path spelled
        // entirely with backslash-cap-U-hex8 escapes for '/'.
        let esc = |c: char| format!("\\U{:08x}", c as u32);
        let slash = esc('/');
        let payload = format!("{slash}etc{slash}passwd");
        assert_eq!(payload.matches('/').count(), 0, "payload must not contain a literal slash");
        let src = format!(
            "syntax = \"proto3\";\npackage demo;\nimport \"{payload}\";\nmessage M {{ string v = 1; }}\n"
        );
        let err = Proto::from_source("evil-unicode-bigu.proto", &src).err().unwrap();
        assert!(err.contains("недопустимый импорт"), "got {err}");
    }

    // ---- g5: brace-nesting depth guard ----

    #[test]
    fn deeply_nested_proto_is_rejected_before_compiling() {
        let mut src = String::from("syntax = \"proto3\"; package demo;\n");
        for i in 0..300 {
            src.push_str(&format!("message M{i} {{\n"));
        }
        src.push_str("message Leaf { string v = 1; }\n");
        for _ in 0..300 {
            src.push_str("}\n");
        }
        let err = Proto::from_source("deep.proto", &src).err().unwrap();
        assert!(err.contains("вложенность"), "got {err}");
    }

    #[test]
    fn deeply_nested_imported_file_is_rejected_before_compiling() {
        // Regression for g5: the depth guard used to run only on the entry
        // file's own source. protox itself opens and parses every file it
        // transitively imports, so a deeply nested file that's reachable
        // only via an `import` statement — never the entry file itself —
        // used to reach protox's recursive-descent parser unchecked. The
        // entry file here is shallow; only the *imported* file is deep.
        let dir = std::env::temp_dir().join(format!("mgrpc-deepimp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(
            dir.join("entry.proto"),
            "syntax = \"proto3\"; package demo; import \"deep.proto\";\nmessage Main { demo.Leaf leaf = 1; }\nservice S { rpc Do(Main) returns (Main); }\n",
        )
        .unwrap();

        let mut deep = String::from("syntax = \"proto3\"; package demo;\n");
        for i in 0..300 {
            deep.push_str(&format!("message M{i} {{\n"));
        }
        deep.push_str("message Leaf { string v = 1; }\n");
        for _ in 0..300 {
            deep.push_str("}\n");
        }
        std::fs::write(dir.join("deep.proto"), &deep).unwrap();

        let err = Proto::from_file(&dir.join("entry.proto").to_string_lossy(), &[])
            .err()
            .expect("deeply nested imported file must be rejected, not handed to protox");
        assert!(err.contains("вложенность"), "got {err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn moderately_nested_proto_still_compiles() {
        // Regression: legitimate (if unusual) nesting well under the limit
        // must not be rejected by the new depth guard.
        let mut src = String::from("syntax = \"proto3\"; package demo;\n");
        for i in 0..10 {
            src.push_str(&format!("message M{i} {{\n"));
        }
        src.push_str("message Leaf { string v = 1; }\n");
        for _ in 0..10 {
            src.push_str("}\n");
        }
        assert!(Proto::from_source("nested-ok.proto", &src).is_ok());
    }

    #[test]
    fn braces_inside_strings_and_comments_do_not_count_toward_depth() {
        // A field option string value containing braces, and comments
        // containing braces, must not trip the depth guard.
        let src = r#"
            syntax = "proto3";
            package demo;
            // a comment with { lots of { fake nesting } in it }
            /* another one { { { } } } */
            message M {
                string v = 1 [json_name = "{not real nesting {{{"];
            }
        "#;
        let res = Proto::from_source("braces-in-strings.proto", src);
        assert!(res.is_ok(), "{:?}", res.err());
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

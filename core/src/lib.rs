//! Maelstrom load-testing engine, independent of any UI. Used by the Tauri
//! desktop app and by the headless `maelstrom` CLI (pipelines / Kubernetes).
//!
//! Modules: scenario (multi-endpoint runner), oauth (token fetch + auto-refresh),
//! tls (mTLS/CA), multipart (file uploads), dynval (per-request generators and
//! dataset providers), histogram (stats), report (standalone HTML), types.

pub mod awssig;
pub mod dynval;
pub mod histogram;
pub mod multipart;
pub mod oauth;
pub mod redact;
pub mod report;
pub mod scenario;
pub mod tls;
pub mod types;
pub mod ws;

pub use types::*;

/// Format an error together with its full cause chain — reqwest's top-level
/// Display ("error sending request for url…") hides the actual reason
/// (DNS, TLS certificate, connection refused…).
pub fn error_chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(cause) = src {
        let text = cause.to_string();
        if !s.contains(&text) {
            s.push_str(": ");
            s.push_str(&text);
        }
        src = cause.source();
    }
    s
}

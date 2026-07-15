//! Secret redaction for logs, shared by the desktop app (file log) and the CLI
//! (stderr / --log-file). Tokens, Authorization, cookies, client secrets,
//! passwords and presigned-URL signatures are NEVER written — they become `***`.

/// Header names whose VALUE must never be logged.
pub fn is_secret_header(name: &str) -> bool {
    let n = name.to_lowercase();
    n == "authorization"
        || n == "cookie"
        || n == "set-cookie"
        || n == "proxy-authorization"
        || n.contains("token")
        || n.contains("api-key")
        || n.contains("apikey")
        || n.contains("secret")
        || n == "x-auth"
}

/// Render headers for logging with secret values masked.
pub fn safe_headers(headers: &[(String, String)]) -> String {
    if headers.is_empty() {
        return "—".into();
    }
    headers
        .iter()
        .map(|(k, v)| {
            let val = if is_secret_header(k) { "***" } else { v.as_str() };
            format!("{k}: {val}")
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// True for query-parameter keys that carry secrets (auth tokens, API keys,
/// and AWS/S3 presigned-URL signature fields). Compared after stripping
/// `-`/`_` and lowercasing, so `apikey`, `api_key`, `api-key` and
/// `x-api-key` are all caught the same way.
fn is_secret_query_key(key: &str) -> bool {
    let k = key.to_lowercase();
    let norm: String = k.chars().filter(|c| *c != '-' && *c != '_').collect();
    norm.contains("token")
        || norm.contains("secret")
        || norm.contains("password")
        || norm.contains("apikey")
        || k == "key"
        || k == "access_token"
        || k == "sig"
        || k == "signature"
        || k.starts_with("x-amz-")
        || k == "awsaccesskeyid"
        || k == "credential"
}

/// Mask secret query parameters and any password in the userinfo of a URL.
pub fn safe_url(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(mut u) => {
            if u.query().is_some() {
                let masked: Vec<(String, String)> = u
                    .query_pairs()
                    .map(|(k, v)| {
                        let hide = is_secret_query_key(&k);
                        (k.into_owned(), if hide { "***".into() } else { v.into_owned() })
                    })
                    .collect();
                let mut qs = u.query_pairs_mut();
                qs.clear();
                for (k, v) in masked {
                    qs.append_pair(&k, &v);
                }
                drop(qs);
            }
            // Hide any password in user:pass@host.
            if u.password().is_some() {
                let _ = u.set_password(Some("***"));
            }
            u.to_string()
        }
        Err(_) => {
            // Fail closed, not open: a string that's URL-shaped (has a
            // scheme separator, query, or userinfo) but doesn't parse might
            // still carry a secret we can't structurally locate — never echo
            // it raw. Plain non-URL text (e.g. a synthetic run label like
            // "5 ручек") has nothing to redact and passes through unchanged.
            if url.contains("://") || url.contains('?') || url.contains('@') {
                "<unparseable-url>".to_string()
            } else {
                url.to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_mask_secrets_keep_rest() {
        let h = vec![
            ("Authorization".into(), "Bearer eyJhbGciOi.SECRET.TOKEN".into()),
            ("X-Api-Key".into(), "sk_live_12345".into()),
            ("Cookie".into(), "session=abc".into()),
            ("Content-Type".into(), "application/json".into()),
            ("X-Trace".into(), "trace-123".into()),
        ];
        let s = safe_headers(&h);
        assert!(!s.contains("SECRET.TOKEN"), "token leaked: {s}");
        assert!(!s.contains("sk_live_12345"));
        assert!(!s.contains("session=abc"));
        assert!(s.contains("Authorization: ***"));
        assert!(s.contains("Content-Type: application/json"));
        assert!(s.contains("X-Trace: trace-123"));
    }

    #[test]
    fn url_masks_token_query_params() {
        let masked =
            safe_url("https://api.example.com/x?id=7&access_token=SECRET&key=SECRET2&q=hello");
        assert!(!masked.contains("SECRET"), "leaked: {masked}");
        assert!(masked.contains("id=7"));
        assert!(masked.contains("q=hello"));
    }

    #[test]
    fn url_masks_s3_presigned_signature() {
        let masked = safe_url(
            "https://bucket.s3.amazonaws.com/img.png?X-Amz-Credential=AKIA123&X-Amz-Signature=deadbeef&X-Amz-Expires=900",
        );
        assert!(!masked.contains("deadbeef"), "signature leaked: {masked}");
        assert!(!masked.contains("AKIA123"), "credential leaked: {masked}");
    }

    #[test]
    fn url_masks_password_in_userinfo() {
        let masked = safe_url("postgres://user:hunter2@db.host:5432/app");
        assert!(!masked.contains("hunter2"), "password leaked: {masked}");
        assert!(masked.contains("user:***@") || masked.contains("user:%2A%2A%2A@"));
    }

    #[test]
    fn url_without_query_is_unchanged() {
        assert_eq!(
            safe_url("https://api.example.com/v1/orders"),
            "https://api.example.com/v1/orders"
        );
    }

    #[test]
    fn url_unparsable_but_url_shaped_does_not_leak() {
        // An unencoded space breaks Url::parse, but the string still carries
        // a secret in its query string — the old fail-open fallback (return
        // the raw string) would have leaked it.
        let raw = "http://exa mple.com/x?token=SECRETVALUE";
        assert!(reqwest::Url::parse(raw).is_err(), "test setup: input must be unparsable");
        let masked = safe_url(raw);
        assert!(!masked.contains("SECRETVALUE"), "leaked: {masked}");
        assert_eq!(masked, "<unparseable-url>");
    }

    #[test]
    fn url_plain_non_url_text_passes_through() {
        // Synthetic run labels (scenario aggregate / stream group targets)
        // aren't URLs and must not be replaced by the placeholder.
        assert_eq!(safe_url("5 ручек"), "5 ручек");
    }

    #[test]
    fn secret_query_key_matches_all_api_key_variants() {
        for key in ["apikey", "api_key", "api-key", "x-api-key", "X-Api-Key", "X-API-KEY"] {
            assert!(is_secret_query_key(key), "should catch: {key}");
        }
    }

    #[test]
    fn url_masks_api_key_dash_and_underscore_variants() {
        for key in ["api_key", "api-key", "x-api-key"] {
            let url = format!("https://api.example.com/x?{key}=SECRET123&q=hello");
            let masked = safe_url(&url);
            assert!(!masked.contains("SECRET123"), "{key} leaked: {masked}");
            assert!(masked.contains("q=hello"));
        }
    }
}

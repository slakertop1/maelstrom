//! Minimal AWS Signature Version 4 signing for **GET** requests — lets a dataset
//! or file-pool URL source read objects from a PRIVATE S3 bucket using access
//! keys, not only public or presigned URLs.
//!
//! Scope is deliberately narrow: a GET with an empty body (what we send when
//! fetching a dataset CSV/JSON or a pooled file). We compute the SigV4 headers
//! and hand them back to the caller to attach to the reqwest request; the client
//! sets `Host` itself to match what we signed.
//!
//! Correctness is pinned in the tests against the worked example published in the
//! AWS "Signing AWS requests with Signature Version 4" documentation (the GET
//! `ListUsers` request), so the canonicalisation, signing-key derivation and
//! final signature are all validated against a known-good vector.

use crate::types::AwsAuth;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// SHA-256 of an empty payload — the body hash for every GET we sign.
const EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex(&h.finalize())
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts a key of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// AWS4 signing key: an HMAC chain over date → region → service → "aws4_request".
fn signing_key(secret: &str, date_stamp: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), date_stamp.as_bytes());
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, service.as_bytes());
    hmac(&k_service, b"aws4_request")
}

/// RFC 3986 encoding used by SigV4 for query keys/values: everything except the
/// unreserved set (A–Z a–z 0–9 `-` `_` `.` `~`) becomes `%XX`.
fn uri_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The `Host` header value the client will send — host, plus an explicit
/// non-default port (matching hyper/reqwest, so we sign what is actually sent).
fn host_header(url: &reqwest::Url) -> String {
    match url.port() {
        Some(p) => format!("{}:{}", url.host_str().unwrap_or(""), p),
        None => url.host_str().unwrap_or("").to_string(),
    }
}

/// Canonical URI = the request path. S3 uses single URI-encoding and `Url::path`
/// is already percent-encoded once, so it is used verbatim (root → "/").
fn canonical_uri(url: &reqwest::Url) -> String {
    let p = url.path();
    if p.is_empty() {
        "/".to_string()
    } else {
        p.to_string()
    }
}

/// Canonical query string = each key/value URI-encoded, sorted by key then value.
fn canonical_query(url: &reqwest::Url) -> String {
    let mut pairs: Vec<(String, String)> =
        url.query_pairs().map(|(k, v)| (uri_encode(&k), uri_encode(&v))).collect();
    pairs.sort();
    pairs.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
}

/// Build the canonical request and the `;`-joined signed-header names. `headers`
/// is sorted in place (SigV4 requires header names in ascending order).
fn canonical_request(
    method: &str,
    uri: &str,
    query: &str,
    headers: &mut [(String, String)],
    payload_hash: &str,
) -> (String, String) {
    headers.sort_by(|a, b| a.0.cmp(&b.0));
    let mut block = String::new();
    for (k, v) in headers.iter() {
        block.push_str(k);
        block.push(':');
        block.push_str(v.trim());
        block.push('\n');
    }
    let signed_names = headers.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let cr = format!("{method}\n{uri}\n{query}\n{block}\n{signed_names}\n{payload_hash}");
    (cr, signed_names)
}

fn string_to_sign(amz_date: &str, scope: &str, canonical_request: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    )
}

/// Compute the SigV4 headers to attach to a GET request so it authenticates to a
/// private S3 bucket. `now` is injected for deterministic testing. The returned
/// pairs are added to the request; the client fills in `Host` to match.
pub fn sign_get(url: &reqwest::Url, auth: &AwsAuth, now: DateTime<Utc>) -> Vec<(String, String)> {
    let service = auth.service.as_deref().filter(|s| !s.is_empty()).unwrap_or("s3");
    let region = auth.region.as_str();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();
    let payload_hash = EMPTY_PAYLOAD_SHA256;

    let mut signed = vec![
        ("host".to_string(), host_header(url)),
        ("x-amz-content-sha256".to_string(), payload_hash.to_string()),
        ("x-amz-date".to_string(), amz_date.clone()),
    ];
    let token = auth.session_token.as_deref().filter(|t| !t.is_empty());
    if let Some(tok) = token {
        signed.push(("x-amz-security-token".to_string(), tok.to_string()));
    }

    let uri = canonical_uri(url);
    let query = canonical_query(url);
    let (cr, signed_names) = canonical_request("GET", &uri, &query, &mut signed, payload_hash);

    let scope = format!("{date_stamp}/{region}/{service}/aws4_request");
    let sts = string_to_sign(&amz_date, &scope, &cr);
    let key = signing_key(&auth.secret_access_key, &date_stamp, region, service);
    let signature = hex(&hmac(&key, sts.as_bytes()));

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
        auth.access_key_id, scope, signed_names, signature
    );

    let mut out = vec![
        ("x-amz-date".to_string(), amz_date),
        ("x-amz-content-sha256".to_string(), payload_hash.to_string()),
        ("authorization".to_string(), authorization),
    ];
    if let Some(tok) = token {
        out.push(("x-amz-security-token".to_string(), tok.to_string()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixed credentials/date from the AWS SigV4 documentation worked example.
    const AKID: &str = "AKIDEXAMPLE";
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

    fn header(name: &str, value: &str) -> (String, String) {
        (name.to_string(), value.to_string())
    }

    #[test]
    fn empty_payload_hash_is_correct() {
        assert_eq!(sha256_hex(b""), EMPTY_PAYLOAD_SHA256);
    }

    // The GET ListUsers example from the AWS docs: canonical request must hash to
    // the documented value, and the full pipeline must reproduce the documented
    // signature. This pins canonicalisation, signing-key derivation and HMAC.
    #[test]
    fn matches_aws_docs_worked_example() {
        let mut headers = vec![
            header("content-type", "application/x-www-form-urlencoded; charset=utf-8"),
            header("host", "iam.amazonaws.com"),
            header("x-amz-date", "20150830T123600Z"),
        ];
        let (cr, signed_names) = canonical_request(
            "GET",
            "/",
            "Action=ListUsers&Version=2010-05-08",
            &mut headers,
            EMPTY_PAYLOAD_SHA256,
        );
        assert_eq!(signed_names, "content-type;host;x-amz-date");
        assert_eq!(
            sha256_hex(cr.as_bytes()),
            "f536975d06c0309214f805bb90ccff089219ecd68b2577efef23edd43b7e1a59",
            "canonical request hash mismatch"
        );

        let scope = "20150830/us-east-1/iam/aws4_request";
        let sts = string_to_sign("20150830T123600Z", scope, &cr);
        let key = signing_key(SECRET, "20150830", "us-east-1", "iam");
        let signature = hex(&hmac(&key, sts.as_bytes()));
        assert_eq!(
            signature,
            "5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7",
            "final signature mismatch"
        );
    }

    fn at(iso: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(iso).unwrap().with_timezone(&Utc)
    }

    fn find<'a>(hs: &'a [(String, String)], name: &str) -> Option<&'a str> {
        hs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    #[test]
    fn sign_get_produces_expected_headers() {
        let auth = AwsAuth {
            access_key_id: AKID.to_string(),
            secret_access_key: SECRET.to_string(),
            session_token: None,
            region: "us-east-1".to_string(),
            service: None, // defaults to s3
        };
        let url = reqwest::Url::parse("https://bucket.s3.amazonaws.com/clients.csv").unwrap();
        let hs = sign_get(&url, &auth, at("2015-08-30T12:36:00Z"));

        assert_eq!(find(&hs, "x-amz-date"), Some("20150830T123600Z"));
        assert_eq!(find(&hs, "x-amz-content-sha256"), Some(EMPTY_PAYLOAD_SHA256));
        let authz = find(&hs, "authorization").expect("authorization header");
        assert!(
            authz.starts_with(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/s3/aws4_request"
            ),
            "credential/scope wrong: {authz}"
        );
        assert!(
            authz.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date"),
            "signed headers wrong: {authz}"
        );
        assert!(authz.contains("Signature="), "no signature: {authz}");
        // No session token → no security-token header.
        assert!(find(&hs, "x-amz-security-token").is_none());
    }

    #[test]
    fn session_token_is_signed_and_attached() {
        let auth = AwsAuth {
            access_key_id: AKID.to_string(),
            secret_access_key: SECRET.to_string(),
            session_token: Some("FQoGZXIvYXdzEJr".to_string()),
            region: "eu-central-1".to_string(),
            service: None,
        };
        let url = reqwest::Url::parse("https://bucket.s3.eu-central-1.amazonaws.com/a.csv").unwrap();
        let hs = sign_get(&url, &auth, at("2015-08-30T12:36:00Z"));
        assert_eq!(find(&hs, "x-amz-security-token"), Some("FQoGZXIvYXdzEJr"));
        let authz = find(&hs, "authorization").unwrap();
        assert!(
            authz.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token"),
            "token must be in signed headers: {authz}"
        );
    }

    #[test]
    fn host_header_includes_only_non_default_port() {
        let default = reqwest::Url::parse("https://h.example.com/x").unwrap();
        assert_eq!(host_header(&default), "h.example.com");
        let custom = reqwest::Url::parse("https://h.example.com:9000/x").unwrap();
        assert_eq!(host_header(&custom), "h.example.com:9000");
    }

    #[test]
    fn canonical_query_encodes_and_sorts() {
        let url = reqwest::Url::parse("https://h/o?b=2&a=hello world&versionId=x/y").unwrap();
        // sorted by key; space → %20, '/' encoded in values
        assert_eq!(canonical_query(&url), "a=hello%20world&b=2&versionId=x%2Fy");
    }
}

//! Shared SQL layer for Maelstrom, used by the desktop app (queries, DB load
//! tests) and the CLI (DB-backed datasets in pipelines).
//!
//! The generic sqlx "Any" driver can't decode many real-world types (UUID,
//! TIMESTAMP, NUMERIC, JSONB, arrays…), so we connect with the native driver
//! for the URL scheme and stringify values by column type name, with a safe
//! fallback chain.

use futures_util::TryStreamExt;
use sqlx::mysql::{MySqlPoolOptions, MySqlRow};
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::sqlite::{SqlitePoolOptions, SqliteRow};
use sqlx::{Column, Row, TypeInfo, ValueRef};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Clone)]
pub enum Db {
    Pg(sqlx::PgPool),
    MySql(sqlx::MySqlPool),
    Sqlite(sqlx::SqlitePool),
}

pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub row_count: u64,
    pub truncated: bool,
}

impl Db {
    pub async fn connect(
        url: &str,
        max_connections: u32,
        acquire_timeout: Duration,
    ) -> Result<Db, String> {
        let u = url.trim();
        let scheme = u.split(':').next().unwrap_or("").to_ascii_lowercase();
        match scheme.as_str() {
            "postgres" | "postgresql" => PgPoolOptions::new()
                .max_connections(max_connections)
                .acquire_timeout(acquire_timeout)
                .connect(u)
                .await
                .map(Db::Pg)
                .map_err(|e| e.to_string()),
            "mysql" | "mariadb" => {
                let u = if scheme == "mariadb" {
                    format!("mysql{}", &u["mariadb".len()..])
                } else {
                    u.to_string()
                };
                MySqlPoolOptions::new()
                    .max_connections(max_connections)
                    .acquire_timeout(acquire_timeout)
                    .connect(&u)
                    .await
                    .map(Db::MySql)
                    .map_err(|e| e.to_string())
            }
            "sqlite" => SqlitePoolOptions::new()
                .max_connections(max_connections)
                .acquire_timeout(acquire_timeout)
                .connect(u)
                .await
                .map(Db::Sqlite)
                .map_err(|e| e.to_string()),
            other => Err(format!(
                "Неподдерживаемая СУБД «{other}». Поддерживаются postgres://, mysql:// (mariadb://), sqlite:"
            )),
        }
    }

    /// Fetch up to `limit` preview rows as a table of strings (for the app grid).
    /// Streams the *whole* result set so `row_count` reflects the real total
    /// (the UI shows it as-is), but only materializes the first `limit` rows
    /// into memory for the preview — a preview must not pull the whole result
    /// set into memory just to show the first page.
    pub async fn fetch(&self, query: &str, limit: usize) -> Result<Table, sqlx::Error> {
        match self {
            Db::Pg(p) => to_table_capped(sqlx::query(query).fetch(p), limit, pg_cell).await,
            Db::MySql(p) => to_table_capped(sqlx::query(query).fetch(p), limit, mysql_cell).await,
            Db::Sqlite(p) => to_table_capped(sqlx::query(query).fetch(p), limit, sqlite_cell).await,
        }
    }

    /// Stream rows as per-row maps (column → value), stopping at `cap` rows.
    /// Streaming keeps memory bounded for very large result sets (e.g. a million
    /// rows used as a data provider). Returns the rows and whether it was capped.
    /// Column names are read per row: SQLite runs multi-statement SQL in one call
    /// and may return rows with differing column sets.
    pub async fn fetch_maps_capped(
        &self,
        query: &str,
        cap: usize,
    ) -> Result<(Vec<HashMap<String, String>>, bool), sqlx::Error> {
        match self {
            Db::Pg(p) => collect_capped(sqlx::query(query).fetch(p), cap, pg_cell).await,
            Db::MySql(p) => collect_capped(sqlx::query(query).fetch(p), cap, mysql_cell).await,
            Db::Sqlite(p) => collect_capped(sqlx::query(query).fetch(p), cap, sqlite_cell).await,
        }
    }

    pub async fn execute(&self, query: &str) -> Result<u64, sqlx::Error> {
        match self {
            Db::Pg(p) => Ok(sqlx::query(query).execute(p).await?.rows_affected()),
            Db::MySql(p) => Ok(sqlx::query(query).execute(p).await?.rows_affected()),
            Db::Sqlite(p) => Ok(sqlx::query(query).execute(p).await?.rows_affected()),
        }
    }

    /// Run a query for the load test: only success/failure matters, rows discarded.
    /// Drains the whole stream (without materializing rows) instead of
    /// collecting a full `Vec` — a SELECT under load (many concurrent VUs)
    /// must not pull a full table into memory just to confirm it succeeded,
    /// but the latency measured around this call must reflect the time to
    /// receive the *entire* result set, and errors surfacing partway through
    /// the stream (e.g. a `statement_timeout`) must still be caught.
    pub async fn run_ok(&self, query: &str, is_select: bool) -> bool {
        if is_select {
            match self {
                Db::Pg(p) => select_ok(sqlx::query(query).fetch(p)).await,
                Db::MySql(p) => select_ok(sqlx::query(query).fetch(p)).await,
                Db::Sqlite(p) => select_ok(sqlx::query(query).fetch(p)).await,
            }
        } else {
            self.execute(query).await.is_ok()
        }
    }

    pub async fn close(&self) {
        match self {
            Db::Pg(p) => p.close().await,
            Db::MySql(p) => p.close().await,
            Db::Sqlite(p) => p.close().await,
        }
    }
}

/// Drain an entire SELECT stream to know whether it succeeded, without
/// materializing the result set — every row is fetched and dropped, but none
/// are collected into a `Vec`. This must read to the end (not stop at the
/// first row): the load test's latency measurement wraps this call, and an
/// error can surface partway through the stream (e.g. a `statement_timeout`
/// firing mid-transfer), not only on the first poll.
async fn select_ok<R, S>(mut stream: S) -> bool
where
    R: Row,
    S: futures_util::Stream<Item = Result<R, sqlx::Error>> + Unpin,
{
    loop {
        match stream.try_next().await {
            Ok(Some(_)) => {} // discard the row, keep draining
            Ok(None) => return true, // end of stream: fully drained, no error
            Err(_) => return false,
        }
    }
}

async fn collect_capped<R, S>(
    mut stream: S,
    cap: usize,
    cell: impl Fn(&R, usize) -> String,
) -> Result<(Vec<HashMap<String, String>>, bool), sqlx::Error>
where
    R: Row,
    S: futures_util::Stream<Item = Result<R, sqlx::Error>> + Unpin,
{
    let mut rows = Vec::new();
    let mut truncated = false;
    while let Some(row) = stream.try_next().await? {
        // Only mark truncated when a row genuinely exists beyond the cap — a
        // result of exactly `cap` rows is NOT truncated.
        if rows.len() >= cap {
            truncated = true;
            break;
        }
        let map = (0..row.columns().len())
            .map(|i| (row.columns()[i].name().to_string(), cell(&row, i)))
            .collect();
        rows.push(map);
    }
    Ok((rows, truncated))
}

async fn to_table_capped<R, S>(
    mut stream: S,
    limit: usize,
    cell: impl Fn(&R, usize) -> String,
) -> Result<Table, sqlx::Error>
where
    R: Row,
    S: futures_util::Stream<Item = Result<R, sqlx::Error>> + Unpin,
{
    let mut columns: Vec<String> = Vec::new();
    let mut rows = Vec::new();
    // Counts every row in the stream, not just the ones kept in `rows` — the
    // UI (DbResultView.tsx "rows: {row_count}", DatasetsModal) shows this as
    // the real total, which can be larger than the preview.
    let mut row_count: u64 = 0;
    while let Some(row) = stream.try_next().await? {
        // Columns come from the first row seen, even if `limit` is 0 — a
        // preview with an empty grid should still show headers.
        if columns.is_empty() {
            columns = row.columns().iter().map(|c| c.name().to_string()).collect();
        }
        row_count += 1;
        // Only the first `limit` rows are materialized into the preview —
        // everything past that is counted above and then dropped, keeping
        // memory bounded regardless of the real total.
        if rows.len() < limit {
            rows.push((0..row.columns().len()).map(|i| cell(&row, i)).collect());
        }
    }
    // Truncated iff more rows existed than fit in the preview — exactly
    // `limit` rows is NOT truncated (mirrors collect_capped).
    let truncated = row_count > limit as u64;
    Ok(Table {
        columns,
        rows,
        row_count,
        truncated,
    })
}

// ---------- value → string per driver ----------

macro_rules! try_display {
    ($row:expr, $i:expr, $($ty:ty),+) => {
        $(if let Ok(v) = $row.try_get::<$ty, _>($i) { return v.to_string(); })+
    };
}

macro_rules! try_array {
    ($row:expr, $i:expr, $($ty:ty),+) => {
        $(if let Ok(v) = $row.try_get::<Vec<$ty>, _>($i) {
            return format!("{{{}}}", v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", "));
        })+
    };
}

fn bin_label(n: usize) -> String {
    format!("<binary {n} байт>")
}

fn pg_cell(row: &PgRow, i: usize) -> String {
    if row.try_get_raw(i).map(|v| v.is_null()).unwrap_or(false) {
        return "NULL".to_string();
    }
    let type_name = row.columns()[i].type_info().name().to_uppercase();
    use sqlx::types::chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
    match type_name.as_str() {
        "TEXT" | "VARCHAR" | "CHAR" | "BPCHAR" | "NAME" | "CITEXT" | "UNKNOWN" => {
            try_display!(row, i, String)
        }
        "UUID" => try_display!(row, i, sqlx::types::Uuid),
        "BOOL" => try_display!(row, i, bool),
        "INT2" => try_display!(row, i, i16),
        "INT4" => try_display!(row, i, i32),
        "INT8" => try_display!(row, i, i64),
        "FLOAT4" => try_display!(row, i, f32),
        "FLOAT8" => try_display!(row, i, f64),
        "NUMERIC" => try_display!(row, i, sqlx::types::Decimal),
        "TIMESTAMPTZ" => {
            if let Ok(v) = row.try_get::<DateTime<Utc>, _>(i) {
                return v.to_rfc3339();
            }
        }
        "TIMESTAMP" => try_display!(row, i, NaiveDateTime),
        "DATE" => try_display!(row, i, NaiveDate),
        "TIME" => try_display!(row, i, NaiveTime),
        "JSON" | "JSONB" => try_display!(row, i, sqlx::types::JsonValue),
        "INET" | "CIDR" => try_display!(row, i, sqlx::types::ipnetwork::IpNetwork),
        "BYTEA" => {
            if let Ok(v) = row.try_get::<Vec<u8>, _>(i) {
                return bin_label(v.len());
            }
        }
        "TEXT[]" | "VARCHAR[]" | "NAME[]" => try_array!(row, i, String),
        "UUID[]" => try_array!(row, i, sqlx::types::Uuid),
        "INT2[]" => try_array!(row, i, i16),
        "INT4[]" => try_array!(row, i, i32),
        "INT8[]" => try_array!(row, i, i64),
        "FLOAT4[]" => try_array!(row, i, f32),
        "FLOAT8[]" => try_array!(row, i, f64),
        "BOOL[]" => try_array!(row, i, bool),
        _ => {}
    }
    try_display!(row, i, String, i64, i32, f64, bool, sqlx::types::Uuid, sqlx::types::Decimal, sqlx::types::JsonValue);
    if let Ok(v) = row.try_get::<Vec<u8>, _>(i) {
        return bin_label(v.len());
    }
    format!("<{}: приведите к тексту через ::text>", type_name.to_lowercase())
}

fn mysql_cell(row: &MySqlRow, i: usize) -> String {
    if row.try_get_raw(i).map(|v| v.is_null()).unwrap_or(false) {
        return "NULL".to_string();
    }
    let type_name = row.columns()[i].type_info().name().to_uppercase();
    use sqlx::types::chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
    match type_name.as_str() {
        "VARCHAR" | "CHAR" | "TEXT" | "TINYTEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM" | "SET" => {
            try_display!(row, i, String)
        }
        "BOOLEAN" => try_display!(row, i, bool),
        "TINYINT" => try_display!(row, i, i8),
        "SMALLINT" => try_display!(row, i, i16),
        "MEDIUMINT" | "INT" => try_display!(row, i, i32),
        "BIGINT" => try_display!(row, i, i64),
        "TINYINT UNSIGNED" => try_display!(row, i, u8),
        "SMALLINT UNSIGNED" => try_display!(row, i, u16),
        "MEDIUMINT UNSIGNED" | "INT UNSIGNED" => try_display!(row, i, u32),
        "BIGINT UNSIGNED" => try_display!(row, i, u64),
        "YEAR" => try_display!(row, i, u16),
        "FLOAT" => try_display!(row, i, f32),
        "DOUBLE" => try_display!(row, i, f64),
        "DECIMAL" => try_display!(row, i, sqlx::types::Decimal),
        "DATETIME" => try_display!(row, i, NaiveDateTime),
        "TIMESTAMP" => {
            if let Ok(v) = row.try_get::<DateTime<Utc>, _>(i) {
                return v.to_rfc3339();
            }
        }
        "DATE" => try_display!(row, i, NaiveDate),
        "TIME" => try_display!(row, i, NaiveTime),
        "JSON" => try_display!(row, i, sqlx::types::JsonValue),
        "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "BINARY" | "VARBINARY" => {
            if let Ok(v) = row.try_get::<Vec<u8>, _>(i) {
                return bin_label(v.len());
            }
        }
        _ => {}
    }
    try_display!(row, i, String, i64, u64, f64, bool, sqlx::types::Decimal, sqlx::types::JsonValue);
    if let Ok(v) = row.try_get::<Vec<u8>, _>(i) {
        return bin_label(v.len());
    }
    format!("<{}>", type_name.to_lowercase())
}

fn sqlite_cell(row: &SqliteRow, i: usize) -> String {
    if row.try_get_raw(i).map(|v| v.is_null()).unwrap_or(false) {
        return "NULL".to_string();
    }
    try_display!(row, i, String, i64, f64, bool);
    if let Ok(v) = row.try_get::<Vec<u8>, _>(i) {
        return bin_label(v.len());
    }
    "<?>".to_string()
}

// ---------- URL helpers ----------

/// Accept common JDBC-style URLs (`jdbc:postgresql://…`) by dropping the
/// `jdbc:` prefix so sqlx recognises the scheme.
pub fn normalize_db_url(url: &str) -> String {
    let u = url.trim();
    u.strip_prefix("jdbc:").unwrap_or(u).to_string()
}

fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Inject `user:password@` into a URL when credentials are given as separate
/// fields (like DBeaver). Leaves the URL alone if it already has credentials.
pub fn apply_credentials(url: &str, user: &str, pass: &str) -> String {
    if user.trim().is_empty() {
        return url.to_string();
    }
    if let Some(pos) = url.find("://") {
        let after = &url[pos + 3..];
        let authority_end = after.find('/').unwrap_or(after.len());
        if after[..authority_end].contains('@') {
            return url.to_string();
        }
        let creds = if pass.is_empty() {
            pct_encode(user)
        } else {
            format!("{}:{}", pct_encode(user), pct_encode(pass))
        };
        return format!("{}://{}@{}", &url[..pos], creds, after);
    }
    url.to_string()
}

/// Normalize a URL and merge separate user/password fields into it.
pub fn build_db_url(url: &str, user: &str, pass: &str) -> String {
    apply_credentials(&normalize_db_url(url), user, pass)
}

/// Hide the password in a connection URL before it lands in logs / reports.
/// Masks the whole userinfo segment (before `@`) — including a bare token
/// with no `user:pass` split (`scheme://TOKEN@host`), which must not leak.
pub fn mask_db_url(url: &str) -> String {
    if let Some(scheme_end) = url.find("://") {
        let rest = &url[scheme_end + 3..];
        if let Some(at) = rest.find('@') {
            let creds = &rest[..at];
            let scheme = &url[..scheme_end];
            let host = &rest[at + 1..];
            return match creds.find(':') {
                Some(colon) => format!("{scheme}://{}:***@{host}", &creds[..colon]),
                None => format!("{scheme}://***@{host}"),
            };
        }
    }
    url.to_string()
}

pub fn is_query(sql: &str) -> bool {
    let head = sql.trim_start().to_lowercase();
    ["select", "with", "show", "pragma", "explain", "describe", "values", "returning"]
        .iter()
        .any(|kw| head.starts_with(kw))
        || sql.to_lowercase().contains("returning")
}

// ---------- DB-backed datasets ----------

/// Default cap on rows pulled from a DB dataset — a guard so an accidental
/// unbounded SELECT can't exhaust memory. Large enough for a million-row pool.
pub const DB_DATASET_MAX_ROWS: usize = 1_000_000;

/// Resolve any `db`-sourced datasets into inline rows (runs the query via sqlx),
/// leaving other sources untouched, so the engine crate stays database-free.
/// Rows are streamed and capped at `cap` (use [`DB_DATASET_MAX_ROWS`]).
pub async fn resolve_db_datasets(
    specs: &[maelstrom_core::types::DatasetSpec],
    cap: usize,
) -> Result<(Vec<maelstrom_core::types::DatasetSpec>, Vec<String>), String> {
    let mut out = Vec::with_capacity(specs.len());
    // Non-fatal notes (e.g. a truncated result set) for the caller to surface —
    // in the GUI there is no stderr, so returning them keeps truncation visible.
    let mut warnings: Vec<String> = Vec::new();
    for spec in specs {
        if spec.source.kind != "db" {
            out.push(spec.clone());
            continue;
        }
        let url = normalize_db_url(spec.source.url.as_deref().unwrap_or(""));
        let query = spec.source.query.as_deref().unwrap_or("").trim();
        if url.is_empty() || query.is_empty() {
            return Err(format!(
                "Датасет «{}»: для источника БД нужны строка подключения и SQL",
                spec.name
            ));
        }
        let db = Db::connect(&url, 1, Duration::from_secs(30))
            .await
            .map_err(|e| format!("Датасет «{}»: подключение к БД: {e}", spec.name))?;
        // Close the pool on BOTH the success and error paths — an early `?`
        // return must not skip it and leave the pool to close only via Drop.
        let (rows, truncated) = match db.fetch_maps_capped(query, cap.max(1)).await {
            Ok(v) => v,
            Err(e) => {
                db.close().await;
                return Err(format!("Датасет «{}»: запрос: {e}", spec.name));
            }
        };
        db.close().await;
        if truncated {
            warnings.push(format!(
                "Датасет «{}»: получено {} строк (достигнут лимит {}), остальные отброшены — нагрузка пойдёт по усечённым данным",
                spec.name,
                rows.len(),
                cap
            ));
        }

        let mut resolved = spec.clone();
        resolved.source = maelstrom_core::types::DatasetSource {
            kind: "inline".to_string(),
            rows: Some(rows),
            path: None,
            url: None,
            format: None,
            query: None,
            aws: None,
        };
        out.push(resolved);
    }
    Ok((out, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_jdbc_prefix() {
        assert_eq!(normalize_db_url("jdbc:postgresql://host:5432/db"), "postgresql://host:5432/db");
        assert_eq!(normalize_db_url("  jdbc:mysql://h/db "), "mysql://h/db");
        assert_eq!(normalize_db_url("postgres://u:p@h:5432/db"), "postgres://u:p@h:5432/db");
    }

    #[test]
    fn merges_separate_credentials_incl_jdbc_and_encoding() {
        assert_eq!(
            build_db_url("jdbc:postgresql://h:5432/db", "user", "pass"),
            "postgresql://user:pass@h:5432/db"
        );
        assert_eq!(
            build_db_url("postgres://h/db", "u", "p@ss:w/rd"),
            "postgres://u:p%40ss%3Aw%2Frd@h/db"
        );
        assert_eq!(build_db_url("postgres://h/db", "", ""), "postgres://h/db");
        assert_eq!(build_db_url("postgres://a:b@h/db", "user", "pass"), "postgres://a:b@h/db");
    }

    #[test]
    fn masks_password_in_url() {
        assert_eq!(mask_db_url("postgres://u:secret@h:5432/db"), "postgres://u:***@h:5432/db");
        assert_eq!(mask_db_url("sqlite:///x.db"), "sqlite:///x.db");
    }

    #[test]
    fn masks_bare_token_without_colon_in_url() {
        // scheme://TOKEN@host (no `user:pass` split) must still be fully masked —
        // the token is a credential just as much as a password.
        assert_eq!(mask_db_url("postgres://TOKEN@h:5432/db"), "postgres://***@h:5432/db");
        assert_eq!(
            mask_db_url("mysql://eyJhbGciOiJIUzI1NiJ9@h/db"),
            "mysql://***@h/db"
        );
    }

    #[test]
    fn detects_read_queries() {
        assert!(is_query("SELECT 1"));
        assert!(is_query("  with x as (..) select"));
        assert!(is_query("INSERT ... RETURNING id"));
        assert!(!is_query("INSERT INTO t VALUES (1)"));
        assert!(!is_query("UPDATE t SET a=1"));
    }

    #[tokio::test]
    async fn unsupported_scheme_is_reported() {
        let err = Db::connect("oracle://h/db", 1, Duration::from_secs(1)).await.err().unwrap();
        assert!(err.contains("Поддерживаются"), "{err}");
    }

    #[tokio::test]
    async fn sqlite_fetch_decodes_common_types() {
        let db = Db::connect("sqlite::memory:", 1, Duration::from_secs(5)).await.unwrap();
        let table = db
            .fetch("SELECT 1 AS a, 'x' AS b, 2.5 AS c, NULL AS d, x'DEADBEEF' AS e", 10)
            .await
            .unwrap();
        assert_eq!(table.columns, vec!["a", "b", "c", "d", "e"]);
        assert_eq!(table.row_count, 1);
        assert_eq!(table.rows[0][0], "1");
        assert_eq!(table.rows[0][3], "NULL");
        assert!(table.rows[0][4].starts_with("<binary"));
        db.close().await;
    }

    #[tokio::test]
    async fn fetch_row_count_reflects_real_total_beyond_limit() {
        // Regression check for b2: to_table_capped must count every row in the
        // stream, not just the ones kept in the preview.
        let db = Db::connect("sqlite::memory:", 1, Duration::from_secs(5)).await.unwrap();
        db.execute("CREATE TABLE t (id INTEGER)").await.unwrap();
        db.execute("INSERT INTO t VALUES (1),(2),(3),(4),(5)").await.unwrap();
        let table = db.fetch("SELECT id FROM t ORDER BY id", 2).await.unwrap();
        // row_count must be the REAL total (5) — the UI (DbResultView.tsx
        // "rows: {row_count}", DatasetsModal) relies on this, not the
        // truncated preview length.
        assert_eq!(table.row_count, 5, "row_count must be the real total, not the preview length");
        assert_eq!(table.rows.len(), 2, "preview must still be capped at `limit`");
        assert!(table.truncated);
        db.close().await;
    }

    #[tokio::test]
    async fn select_ok_catches_error_after_first_row() {
        // Regression check for b1: select_ok must drain the whole stream, not
        // stop after the first poll — an error surfacing later (e.g. a
        // statement_timeout mid-transfer) must still fail the load test.
        let db = Db::connect("sqlite::memory:", 1, Duration::from_secs(5)).await.unwrap();
        db.execute("CREATE TABLE t (id INTEGER)").await.unwrap();
        db.execute("INSERT INTO t VALUES (1),(2),(3)").await.unwrap();
        let first_row: SqliteRow = match &db {
            Db::Sqlite(p) => sqlx::query("SELECT id FROM t ORDER BY id").fetch(p).try_next().await.unwrap().unwrap(),
            _ => unreachable!(),
        };
        // Synthetic stream: a real first row (Ok), then an error — simulating
        // a failure that only surfaces after streaming has begun.
        let synthetic = futures_util::stream::iter(vec![Ok(first_row), Err(sqlx::Error::RowNotFound)]);
        assert!(!select_ok(synthetic).await, "an error after the first row must still be caught");
        db.close().await;
    }

    #[tokio::test]
    async fn fetch_maps_capped_streams_and_caps() {
        let db = Db::connect("sqlite::memory:", 1, Duration::from_secs(5)).await.unwrap();
        db.execute("CREATE TABLE t (id INTEGER, name TEXT)").await.unwrap();
        db.execute("INSERT INTO t VALUES (1,'a'),(2,'b'),(3,'c'),(4,'d')").await.unwrap();
        // Cap at 2 → truncated.
        let (rows, truncated) = db.fetch_maps_capped("SELECT id, name FROM t ORDER BY id", 2).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(truncated);
        assert_eq!(rows[0].get("id").map(String::as_str), Some("1"));
        assert_eq!(rows[1].get("name").map(String::as_str), Some("b"));
        // Cap above total → all rows, not truncated.
        let (all, trunc2) = db.fetch_maps_capped("SELECT id FROM t", 100).await.unwrap();
        assert_eq!(all.len(), 4);
        assert!(!trunc2);
        // Cap EXACTLY equal to the row count is NOT truncation (regression 2.1).
        let (exact, trunc3) = db.fetch_maps_capped("SELECT id FROM t", 4).await.unwrap();
        assert_eq!(exact.len(), 4);
        assert!(!trunc3, "exactly-cap must not be flagged truncated");
        db.close().await;
    }

    #[tokio::test]
    async fn resolve_db_dataset_from_sqlite() {
        // Use a shared in-memory DB via a temp file so a fresh connection sees the data.
        let dir = std::env::temp_dir().join(format!("mdb-ds-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("d.db");
        let url = format!("sqlite://{}?mode=rwc", file.to_string_lossy().replace('\\', "/"));
        let db = Db::connect(&url, 1, Duration::from_secs(5)).await.unwrap();
        db.execute("CREATE TABLE users (id INTEGER, email TEXT)").await.unwrap();
        db.execute("INSERT INTO users VALUES (1,'a@x'),(2,'b@x')").await.unwrap();
        db.close().await;

        let spec = maelstrom_core::types::DatasetSpec {
            name: "users".into(),
            mode: "sequential".into(),
            source: maelstrom_core::types::DatasetSource {
                kind: "db".into(),
                rows: None,
                path: None,
                url: Some(url),
                format: None,
                query: Some("SELECT id, email FROM users ORDER BY id".into()),
                aws: None,
            },
        };
        let (resolved, warnings) = resolve_db_datasets(std::slice::from_ref(&spec), DB_DATASET_MAX_ROWS)
            .await
            .unwrap();
        assert!(warnings.is_empty(), "small result set shouldn't warn: {warnings:?}");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].source.kind, "inline");
        let rows = resolved[0].source.rows.as_ref().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get("email").map(String::as_str), Some("a@x"));

        std::fs::remove_dir_all(&dir).ok();
    }
}

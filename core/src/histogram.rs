use crate::types::{HistBucket, LoadTestResult, RunMeta, TimelinePoint};
use hdrhistogram::Histogram;
use std::collections::HashMap;

/// Build ~20 linear buckets between min and p99 plus a tail bucket, for the
/// response-time distribution chart.
pub fn build_histogram(hist: &Histogram<u64>) -> Vec<HistBucket> {
    if hist.is_empty() {
        return Vec::new();
    }
    let min = hist.min();
    let p99 = hist.value_at_quantile(0.99).max(min + 1);
    let n: u64 = 20;
    let width = ((p99 - min) / n).max(1);
    let max = hist.max();
    let tail_lo = min + n * width;

    // Bin each recorded value exactly once. (Using `count_between` over the
    // linear ranges double-counts when a histogram's internal bucket is wider
    // than our linear step — e.g. a near-constant distribution — because one
    // wide bucket overlaps several narrow ranges.)
    let mut counts = vec![0u64; n as usize + 1];
    for iv in hist.iter_recorded() {
        let val = iv.value_iterated_to();
        let idx = if val >= tail_lo { n } else { (val - min) / width };
        counts[(idx as usize).min(n as usize)] += iv.count_at_value();
    }

    let mut buckets = Vec::with_capacity(n as usize + 1);
    for i in 0..n {
        let lo = min + i * width;
        buckets.push(HistBucket {
            from_ms: lo as f64 / 1000.0,
            to_ms: (lo + width) as f64 / 1000.0, // half-open [lo, lo+width)
            count: counts[i as usize],
        });
    }
    if counts[n as usize] > 0 {
        buckets.push(HistBucket {
            from_ms: tail_lo as f64 / 1000.0,
            to_ms: max as f64 / 1000.0,
            count: counts[n as usize],
        });
    }
    buckets
}

/// Human label for a status code in a result's status breakdown. DB load runs
/// use synthetic statuses (0 = failure, 200 = success) that need different
/// wording than real HTTP codes. Shared so the app and the engine agree.
pub fn status_label(status: u16, is_db: bool) -> String {
    match status {
        0 if is_db => "Ошибка".to_string(),
        0 => "Сетевая ошибка".to_string(),
        200 if is_db => "Успех".to_string(),
        s => s.to_string(),
    }
}

/// Assemble a LoadTestResult from accumulated stats.
#[allow(clippy::too_many_arguments)]
pub fn finalize_result(
    hist: &Histogram<u64>,
    status_counts: HashMap<u16, u64>,
    total: u64,
    errors: u64,
    sum_us: u128,
    timeline: Vec<TimelinePoint>,
    meta: RunMeta,
    actual_duration_ms: f64,
    stopped_early: bool,
) -> LoadTestResult {
    let is_db = meta.kind == "SQL";
    let mut counts: Vec<(String, u64)> = status_counts
        .into_iter()
        .map(|(status, count)| (status_label(status, is_db), count))
        .collect();
    counts.sort_by_key(|&(_, c)| std::cmp::Reverse(c));

    LoadTestResult {
        // meta.target is a raw URL for HTTP/WS targets (may carry a
        // presigned-S3 signature, api_key or token in the query string) —
        // never store it unmasked in a result that ends up in the JSON/HTML
        // report artifacts.
        url: crate::redact::safe_url(&meta.target),
        method: meta.kind,
        vus: meta.vus,
        duration_secs: meta.duration_secs,
        rps_limit: meta.rps_limit,
        started_at: String::new(),
        actual_duration_ms,
        total_requests: total,
        errors,
        error_rate: if total > 0 { errors as f64 / total as f64 * 100.0 } else { 0.0 },
        rps_avg: if actual_duration_ms > 0.0 {
            total as f64 / (actual_duration_ms / 1000.0)
        } else {
            0.0
        },
        latency_min_ms: if total > 0 { hist.min() as f64 / 1000.0 } else { 0.0 },
        latency_max_ms: hist.max() as f64 / 1000.0,
        latency_avg_ms: if total > 0 { sum_us as f64 / total as f64 / 1000.0 } else { 0.0 },
        p50_ms: hist.value_at_quantile(0.5) as f64 / 1000.0,
        p75_ms: hist.value_at_quantile(0.75) as f64 / 1000.0,
        p90_ms: hist.value_at_quantile(0.9) as f64 / 1000.0,
        p95_ms: hist.value_at_quantile(0.95) as f64 / 1000.0,
        p99_ms: hist.value_at_quantile(0.99) as f64 / 1000.0,
        status_counts: counts,
        timeline,
        histogram: build_histogram(hist),
        stopped_early,
        dropped: 0, // set by the caller from the scheduler's shortfall counters
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RunMeta;
    use std::collections::HashMap;

    #[test]
    fn finalize_computes_rates_and_percentiles() {
        let mut hist = Histogram::<u64>::new(3).unwrap();
        // 100 samples of 10ms (10_000us), 1 sample of 200ms
        for _ in 0..100 {
            hist.record(10_000).unwrap();
        }
        hist.record(200_000).unwrap();
        let mut status = HashMap::new();
        status.insert(200u16, 95u64);
        status.insert(500u16, 6u64);
        let meta = RunMeta {
            target: "http://x".into(),
            kind: "GET".into(),
            vus: 4,
            duration_secs: 1,
            rps_limit: None,
        };
        let r = finalize_result(&hist, status, 101, 6, 101 * 10_000, vec![], meta, 1000.0, false);
        assert_eq!(r.total_requests, 101);
        assert_eq!(r.errors, 6);
        assert!((r.error_rate - (6.0 / 101.0 * 100.0)).abs() < 1e-6);
        assert!(r.rps_avg > 100.0 && r.rps_avg < 102.0); // 101 in 1s
        assert!(r.p50_ms >= 9.0 && r.p50_ms <= 11.0);
        assert!(r.latency_max_ms >= 199.0);
        // status_counts sorted by count desc
        assert_eq!(r.status_counts[0].0, "200");
    }

    #[test]
    fn histogram_buckets_nonempty_for_data() {
        let mut hist = Histogram::<u64>::new(3).unwrap();
        for i in 1..=1000 {
            hist.record(i * 100).unwrap();
        }
        let buckets = build_histogram(&hist);
        assert!(!buckets.is_empty());
        let total: u64 = buckets.iter().map(|b| b.count).sum();
        assert!(total > 0);
    }

    #[test]
    fn empty_histogram_is_safe() {
        let hist = Histogram::<u64>::new(3).unwrap();
        assert!(build_histogram(&hist).is_empty());
    }

    #[test]
    fn status_labels_differ_for_db_and_http() {
        // HTTP: 0 is a network error, real codes pass through verbatim.
        assert_eq!(status_label(0, false), "Сетевая ошибка");
        assert_eq!(status_label(200, false), "200");
        assert_eq!(status_label(503, false), "503");
        // DB: synthetic 0/200 become failure/success wording.
        assert_eq!(status_label(0, true), "Ошибка");
        assert_eq!(status_label(200, true), "Успех");
    }

    #[test]
    fn histogram_single_value_is_safe() {
        // All samples identical: min == p99, so the `p99.max(min+1)` and
        // `width.max(1)` guards must keep bucketing from dividing by zero.
        let mut hist = Histogram::<u64>::new(3).unwrap();
        for _ in 0..50 {
            hist.record(5_000).unwrap(); // 5ms each
        }
        let buckets = build_histogram(&hist);
        assert!(!buckets.is_empty());
        let total: u64 = buckets.iter().map(|b| b.count).sum();
        assert_eq!(total, 50, "every sample must land in a bucket");
    }

    #[test]
    fn finalize_masks_secret_query_in_url() {
        let mut hist = Histogram::<u64>::new(3).unwrap();
        hist.record(10_000).unwrap();
        let mut status = HashMap::new();
        status.insert(200u16, 1u64);
        let meta = RunMeta {
            target: "https://api.example.com/x?api_key=SECRET123&id=7".into(),
            kind: "GET".into(),
            vus: 1,
            duration_secs: 1,
            rps_limit: None,
        };
        let r = finalize_result(&hist, status, 1, 0, 10_000, vec![], meta, 1000.0, false);
        assert!(!r.url.contains("SECRET123"), "secret leaked into result url: {}", r.url);
        assert!(r.url.contains("id=7"));
    }

    #[test]
    fn histogram_has_tail_bucket_for_outlier() {
        // 99 fast samples and one far outlier → the distribution needs an extra
        // tail bucket that reaches up to max, carrying the outlier.
        let mut hist = Histogram::<u64>::new(3).unwrap();
        for _ in 0..99 {
            hist.record(10_000).unwrap(); // 10ms
        }
        hist.record(5_000_000).unwrap(); // 5s outlier
        let buckets = build_histogram(&hist);
        let last = buckets.last().expect("at least one bucket");
        assert!(last.count >= 1, "outlier must be counted in the tail bucket");
        assert!(
            (last.to_ms - hist.max() as f64 / 1000.0).abs() < 1e-6,
            "tail bucket must reach the max latency"
        );
    }
}

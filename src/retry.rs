use rand::Rng;
use std::time::{SystemTime, UNIX_EPOCH};

/// Compute backoff delay using Equal Jitter strategy.
///
/// `exp = min(base_delay_ms * 2^attempt, max_delay_ms)`
/// `delay = exp/2 + random(0, exp/2)`
///
/// Result range: `[exp/2, exp]`.
/// `attempt` is 0-based: 1st retry uses attempt=0 → exp = base_delay_ms.
pub fn compute_backoff_ms(base_delay_ms: u64, max_delay_ms: u64, attempt: u32) -> u64 {
    let exp = base_delay_ms
        .saturating_mul(2u64.saturating_pow(attempt))
        .min(max_delay_ms);
    let half = exp / 2;
    let jitter = rand::thread_rng().gen_range(0..=half);
    half + jitter
}

/// Parse Retry-After related headers into milliseconds.
///
/// Checks `Retry-After-ms` first (float milliseconds), then `Retry-After`
/// (float seconds or HTTP date).
pub fn parse_retry_after_ms(headers: &http::HeaderMap) -> Option<u64> {
    // Retry-After-ms header (non-standard but used by some APIs)
    if let Some(v) = headers.get("retry-after-ms") {
        if let Ok(s) = v.to_str() {
            if let Ok(ms) = s.trim().parse::<f64>() {
                return Some(ms as u64);
            }
        }
    }

    // Retry-After header (standard)
    if let Some(v) = headers.get("retry-after") {
        if let Ok(s) = v.to_str() {
            let s = s.trim();

            // Try float seconds
            if let Ok(secs) = s.parse::<f64>() {
                return Some((secs * 1000.0) as u64);
            }

            // Try HTTP date
            if let Ok(date) = httpdate::parse_http_date(s) {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let target = date
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                if target > now {
                    return Some(target - now);
                }
                return Some(0);
            }
        }
    }

    None
}

/// Check if an HTTP status code is retryable.
pub fn is_retryable_status(status: u16, retry_codes: &[u16]) -> bool {
    retry_codes.contains(&status)
}

/// Compute the actual delay: `min(retry_after_ms, backoff_ms)` if Retry-After
/// is present, otherwise `backoff_ms`.
///
/// This caps Retry-After at the backoff value to avoid being held hostage
/// by abnormally long Retry-After values.
pub fn compute_delay_ms(retry_after_ms: Option<u64>, backoff_ms: u64) -> u64 {
    match retry_after_ms {
        Some(ra) => ra.min(backoff_ms),
        None => backoff_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderMap;

    #[test]
    fn test_backoff_exponential_growth() {
        // With large max_delay, backoff should grow exponentially
        for attempt in 0..10 {
            let delay = compute_backoff_ms(1000, 60000, attempt);
            let exp = (1000u64 * 2u64.pow(attempt)).min(60000);
            assert!(
                delay >= exp / 2 && delay <= exp,
                "attempt {}: delay {} not in [{}, {}]",
                attempt,
                delay,
                exp / 2,
                exp
            );
        }
    }

    #[test]
    fn test_backoff_max_cap() {
        // Should be capped at max_delay_ms
        for _ in 0..100 {
            let delay = compute_backoff_ms(1000, 5000, 10);
            assert!(delay <= 5000, "delay {} exceeded max_delay 5000", delay);
        }
    }

    #[test]
    fn test_backoff_jitter_range() {
        // Run 1000 iterations, check all within [exp/2, exp]
        let base = 1000u64;
        let max = 60000u64;
        let attempt = 0u32;
        let exp = base.min(max);
        for _ in 0..1000 {
            let delay = compute_backoff_ms(base, max, attempt);
            assert!(
                delay >= exp / 2 && delay <= exp,
                "delay {} not in [{}, {}]",
                delay,
                exp / 2,
                exp
            );
        }
    }

    #[test]
    fn test_parse_retry_after_ms_header() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after-ms", "5000".parse().unwrap());
        assert_eq!(parse_retry_after_ms(&headers), Some(5000));
    }

    #[test]
    fn test_parse_retry_after_ms_header_float() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after-ms", "1500.5".parse().unwrap());
        assert_eq!(parse_retry_after_ms(&headers), Some(1500));
    }

    #[test]
    fn test_parse_retry_after_seconds() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "5".parse().unwrap());
        assert_eq!(parse_retry_after_ms(&headers), Some(5000));
    }

    #[test]
    fn test_parse_retry_after_http_date() {
        let mut headers = HeaderMap::new();
        // Use a date far in the future
        headers.insert(
            "retry-after",
            "Wed, 21 Oct 2099 07:28:00 GMT".parse().unwrap(),
        );
        let ms = parse_retry_after_ms(&headers);
        assert!(ms.is_some(), "should parse future date");
        assert!(ms.unwrap() > 0, "should be positive");
    }

    #[test]
    fn test_parse_retry_after_missing() {
        let headers = HeaderMap::new();
        assert_eq!(parse_retry_after_ms(&headers), None);
    }

    #[test]
    fn test_parse_retry_after_invalid() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "garbage".parse().unwrap());
        assert_eq!(parse_retry_after_ms(&headers), None);
    }

    #[test]
    fn test_is_retryable_status() {
        let codes = [429, 500, 502, 503, 504, 408, 529];
        assert!(is_retryable_status(429, &codes));
        assert!(is_retryable_status(529, &codes));
        assert!(!is_retryable_status(200, &codes));
        assert!(!is_retryable_status(400, &codes));
        assert!(!is_retryable_status(404, &codes));
    }

    #[test]
    fn test_compute_delay_min() {
        // Retry-After < backoff → use Retry-After
        assert_eq!(compute_delay_ms(Some(500), 1000), 500);
        // Retry-After > backoff → cap at backoff
        assert_eq!(compute_delay_ms(Some(120000), 1000), 1000);
        // Retry-After == backoff → either is fine
        assert_eq!(compute_delay_ms(Some(1000), 1000), 1000);
    }

    #[test]
    fn test_compute_delay_no_retry_after() {
        assert_eq!(compute_delay_ms(None, 1000), 1000);
    }
}

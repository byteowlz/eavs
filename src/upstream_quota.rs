//! Upstream rate limit quota tracking.
//!
//! Parses rate limit headers from upstream provider responses and tracks
//! remaining quotas per provider/account. This enables:
//! - Smart routing to less-loaded accounts (multi-account)
//! - Surfacing quota info in the admin API
//! - Proactive backoff before hitting hard limits

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Parsed rate limit info from a single response.
#[derive(Debug, Clone)]
pub struct UpstreamQuota {
    /// Requests per minute/day limit
    pub requests_limit: Option<u64>,
    /// Remaining requests in current window
    pub requests_remaining: Option<u64>,
    /// When the request limit resets (duration from now)
    pub requests_reset: Option<Duration>,

    /// Tokens per minute/day limit
    pub tokens_limit: Option<u64>,
    /// Remaining tokens in current window
    pub tokens_remaining: Option<u64>,
    /// When the token limit resets (duration from now)
    pub tokens_reset: Option<Duration>,

    /// When this snapshot was taken
    pub observed_at: Instant,
}

/// Key for tracking quotas: (provider_config_key, account_label)
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct QuotaKey {
    pub provider: String,
    pub account: String,
}

/// Stores the latest observed upstream quota for each provider/account.
#[derive(Debug, Clone)]
pub struct QuotaTracker {
    inner: Arc<RwLock<HashMap<QuotaKey, UpstreamQuota>>>,
}

impl QuotaTracker {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Update the quota snapshot for a provider/account.
    pub async fn update(&self, key: QuotaKey, quota: UpstreamQuota) {
        let mut map = self.inner.write().await;
        map.insert(key, quota);
    }

    /// Get the latest quota snapshot for a provider/account.
    // Tracker accessor exercised by unit tests; kept as part of the QuotaTracker API.
    #[allow(dead_code)]
    pub async fn get(&self, key: &QuotaKey) -> Option<UpstreamQuota> {
        let map = self.inner.read().await;
        map.get(key).cloned()
    }

    /// Get all current quota snapshots.
    pub async fn all(&self) -> HashMap<QuotaKey, UpstreamQuota> {
        let map = self.inner.read().await;
        map.clone()
    }

    /// Remove stale entries older than the given duration.
    // Maintenance helper exercised by unit tests; kept as part of the QuotaTracker API.
    #[allow(dead_code)]
    pub async fn cleanup(&self, max_age: Duration) {
        let mut map = self.inner.write().await;
        let now = Instant::now();
        map.retain(|_, v| now.duration_since(v.observed_at) < max_age);
    }
}

/// Parse rate limit headers from an HTTP response.
///
/// Supports:
/// - OpenAI: x-ratelimit-limit-requests, x-ratelimit-remaining-requests,
///   x-ratelimit-reset-requests, x-ratelimit-limit-tokens,
///   x-ratelimit-remaining-tokens, x-ratelimit-reset-tokens
/// - Anthropic: anthropic-ratelimit-requests-limit, anthropic-ratelimit-requests-remaining,
///   anthropic-ratelimit-requests-reset, anthropic-ratelimit-tokens-limit,
///   anthropic-ratelimit-tokens-remaining, anthropic-ratelimit-tokens-reset
/// - Google: No standard rate limit headers (uses HTTP 429 + Retry-After)
pub fn parse_quota_headers(headers: &http::HeaderMap) -> Option<UpstreamQuota> {
    // Try OpenAI format first
    let openai_req_limit = header_u64(headers, "x-ratelimit-limit-requests");
    let openai_req_remaining = header_u64(headers, "x-ratelimit-remaining-requests");
    let openai_req_reset = header_duration(headers, "x-ratelimit-reset-requests");
    let openai_tok_limit = header_u64(headers, "x-ratelimit-limit-tokens");
    let openai_tok_remaining = header_u64(headers, "x-ratelimit-remaining-tokens");
    let openai_tok_reset = header_duration(headers, "x-ratelimit-reset-tokens");

    if openai_req_limit.is_some() || openai_tok_limit.is_some() {
        return Some(UpstreamQuota {
            requests_limit: openai_req_limit,
            requests_remaining: openai_req_remaining,
            requests_reset: openai_req_reset,
            tokens_limit: openai_tok_limit,
            tokens_remaining: openai_tok_remaining,
            tokens_reset: openai_tok_reset,
            observed_at: Instant::now(),
        });
    }

    // Try Anthropic format
    let anth_req_limit = header_u64(headers, "anthropic-ratelimit-requests-limit");
    let anth_req_remaining = header_u64(headers, "anthropic-ratelimit-requests-remaining");
    let anth_req_reset = header_duration_rfc3339(headers, "anthropic-ratelimit-requests-reset");
    let anth_tok_limit = header_u64(headers, "anthropic-ratelimit-tokens-limit");
    let anth_tok_remaining = header_u64(headers, "anthropic-ratelimit-tokens-remaining");
    let anth_tok_reset = header_duration_rfc3339(headers, "anthropic-ratelimit-tokens-reset");

    if anth_req_limit.is_some() || anth_tok_limit.is_some() {
        return Some(UpstreamQuota {
            requests_limit: anth_req_limit,
            requests_remaining: anth_req_remaining,
            requests_reset: anth_req_reset,
            tokens_limit: anth_tok_limit,
            tokens_remaining: anth_tok_remaining,
            tokens_reset: anth_tok_reset,
            observed_at: Instant::now(),
        });
    }

    None
}

fn header_u64(headers: &http::HeaderMap, name: &str) -> Option<u64> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
}

/// Parse OpenAI-style duration strings like "6m0s", "2s", "1ms"
fn header_duration(headers: &http::HeaderMap, name: &str) -> Option<Duration> {
    let s = headers.get(name).and_then(|v| v.to_str().ok())?;
    parse_openai_duration(s)
}

fn parse_openai_duration(s: &str) -> Option<Duration> {
    // OpenAI duration format: "6m0s", "2s", "1h30m0s", "500ms"
    let s = s.trim();
    let mut total_ms: u64 = 0;
    let mut num_buf = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() || c == '.' {
            num_buf.push(c);
            i += 1;
        } else if c == 'm' && i + 1 < chars.len() && chars[i + 1] == 's' {
            // milliseconds
            let val: f64 = num_buf.parse().ok()?;
            num_buf.clear();
            total_ms += val as u64;
            i += 2;
        } else if c == 'm' {
            // minutes
            let val: f64 = num_buf.parse().ok()?;
            num_buf.clear();
            total_ms += (val * 60_000.0) as u64;
            i += 1;
        } else if c == 'h' {
            let val: f64 = num_buf.parse().ok()?;
            num_buf.clear();
            total_ms += (val * 3_600_000.0) as u64;
            i += 1;
        } else if c == 's' {
            let val: f64 = num_buf.parse().ok()?;
            num_buf.clear();
            total_ms += (val * 1_000.0) as u64;
            i += 1;
        } else {
            i += 1;
        }
    }

    if total_ms > 0 {
        Some(Duration::from_millis(total_ms))
    } else {
        None
    }
}

/// Parse Anthropic-style RFC3339 timestamp and convert to duration from now
fn header_duration_rfc3339(headers: &http::HeaderMap, name: &str) -> Option<Duration> {
    let s = headers.get(name).and_then(|v| v.to_str().ok())?;
    let reset_time = chrono::DateTime::parse_from_rfc3339(s).ok()?;
    let now = chrono::Utc::now();
    let diff = reset_time.signed_duration_since(now);
    if diff.num_milliseconds() > 0 {
        Some(Duration::from_millis(diff.num_milliseconds() as u64))
    } else {
        None
    }
}

/// Serializable snapshot of upstream quotas for API responses.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QuotaSnapshot {
    pub provider: String,
    pub account: String,
    pub requests_limit: Option<u64>,
    pub requests_remaining: Option<u64>,
    pub requests_reset_secs: Option<f64>,
    pub tokens_limit: Option<u64>,
    pub tokens_remaining: Option<u64>,
    pub tokens_reset_secs: Option<f64>,
    pub age_secs: f64,
}

impl QuotaSnapshot {
    pub fn from_quota(key: &QuotaKey, quota: &UpstreamQuota) -> Self {
        let age = Instant::now().duration_since(quota.observed_at);
        Self {
            provider: key.provider.clone(),
            account: key.account.clone(),
            requests_limit: quota.requests_limit,
            requests_remaining: quota.requests_remaining,
            requests_reset_secs: quota.requests_reset.map(|d| d.as_secs_f64()),
            tokens_limit: quota.tokens_limit,
            tokens_remaining: quota.tokens_remaining,
            tokens_reset_secs: quota.tokens_reset.map(|d| d.as_secs_f64()),
            age_secs: age.as_secs_f64(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderMap;

    #[test]
    fn test_parse_openai_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-limit-requests", "500".parse().unwrap());
        headers.insert("x-ratelimit-remaining-requests", "499".parse().unwrap());
        headers.insert("x-ratelimit-reset-requests", "6m0s".parse().unwrap());
        headers.insert("x-ratelimit-limit-tokens", "200000".parse().unwrap());
        headers.insert("x-ratelimit-remaining-tokens", "199500".parse().unwrap());
        headers.insert("x-ratelimit-reset-tokens", "2s".parse().unwrap());

        let quota = parse_quota_headers(&headers).unwrap();
        assert_eq!(quota.requests_limit, Some(500));
        assert_eq!(quota.requests_remaining, Some(499));
        assert!(quota.requests_reset.unwrap() >= Duration::from_secs(300));
        assert_eq!(quota.tokens_limit, Some(200000));
        assert_eq!(quota.tokens_remaining, Some(199500));
        assert!(quota.tokens_reset.unwrap() >= Duration::from_secs(1));
    }

    #[test]
    fn test_parse_anthropic_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("anthropic-ratelimit-requests-limit", "100".parse().unwrap());
        headers.insert(
            "anthropic-ratelimit-requests-remaining",
            "95".parse().unwrap(),
        );
        headers.insert("anthropic-ratelimit-tokens-limit", "80000".parse().unwrap());
        headers.insert(
            "anthropic-ratelimit-tokens-remaining",
            "75000".parse().unwrap(),
        );
        // Anthropic uses RFC3339 for resets - use a far-future time for test
        headers.insert(
            "anthropic-ratelimit-requests-reset",
            "2099-01-01T00:00:00Z".parse().unwrap(),
        );
        headers.insert(
            "anthropic-ratelimit-tokens-reset",
            "2099-01-01T00:00:00Z".parse().unwrap(),
        );

        let quota = parse_quota_headers(&headers).unwrap();
        assert_eq!(quota.requests_limit, Some(100));
        assert_eq!(quota.requests_remaining, Some(95));
        assert_eq!(quota.tokens_limit, Some(80000));
        assert_eq!(quota.tokens_remaining, Some(75000));
        assert!(quota.requests_reset.is_some());
    }

    #[test]
    fn test_parse_no_headers() {
        let headers = HeaderMap::new();
        assert!(parse_quota_headers(&headers).is_none());
    }

    #[test]
    fn test_parse_openai_duration() {
        assert_eq!(
            parse_openai_duration("6m0s"),
            Some(Duration::from_millis(360_000))
        );
        assert_eq!(
            parse_openai_duration("2s"),
            Some(Duration::from_millis(2_000))
        );
        assert_eq!(
            parse_openai_duration("1h30m0s"),
            Some(Duration::from_millis(5_400_000))
        );
        assert_eq!(
            parse_openai_duration("500ms"),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            parse_openai_duration("1m500ms"),
            Some(Duration::from_millis(60_500))
        );
    }

    #[tokio::test]
    async fn test_quota_tracker_update_and_get() {
        let tracker = QuotaTracker::new();
        let key = QuotaKey {
            provider: "openai".to_string(),
            account: "default".to_string(),
        };
        let quota = UpstreamQuota {
            requests_limit: Some(500),
            requests_remaining: Some(450),
            requests_reset: None,
            tokens_limit: None,
            tokens_remaining: None,
            tokens_reset: None,
            observed_at: Instant::now(),
        };

        tracker.update(key.clone(), quota).await;
        let fetched = tracker.get(&key).await.unwrap();
        assert_eq!(fetched.requests_limit, Some(500));
        assert_eq!(fetched.requests_remaining, Some(450));
    }

    #[tokio::test]
    async fn test_quota_tracker_cleanup() {
        let tracker = QuotaTracker::new();
        let key = QuotaKey {
            provider: "old".to_string(),
            account: "default".to_string(),
        };
        let old_quota = UpstreamQuota {
            requests_limit: Some(100),
            requests_remaining: None,
            requests_reset: None,
            tokens_limit: None,
            tokens_remaining: None,
            tokens_reset: None,
            observed_at: Instant::now() - Duration::from_secs(3600),
        };

        tracker.update(key.clone(), old_quota).await;
        assert!(tracker.get(&key).await.is_some());

        tracker.cleanup(Duration::from_secs(60)).await;
        assert!(tracker.get(&key).await.is_none());
    }
}

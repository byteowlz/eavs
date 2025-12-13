//! Rate limiting for virtual API keys.
//!
//! Uses the token bucket algorithm via the `governor` crate for efficient,
//! in-memory rate limiting.

use dashmap::DashMap;
use governor::{
    clock::DefaultClock,
    state::{InMemoryState, NotKeyed},
    Quota, RateLimiter as GovernorRateLimiter,
};
use std::num::NonZeroU32;
use std::sync::Arc;


/// Rate limiter for virtual API keys.
///
/// Supports per-key rate limiting for:
/// - Requests per minute (RPM)
/// - Tokens per minute (TPM) - approximate, based on estimates
/// - Requests per day (RPD)
pub struct RateLimiter {
    /// RPM limiters per key
    rpm_limiters: DashMap<String, Arc<GovernorRateLimiter<NotKeyed, InMemoryState, DefaultClock>>>,
    /// TPM limiters per key (uses token bucket with larger capacity)
    tpm_limiters: DashMap<String, Arc<TokenBucketLimiter>>,
    /// RPD limiters per key
    rpd_limiters: DashMap<String, Arc<DailyLimiter>>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            rpm_limiters: DashMap::new(),
            tpm_limiters: DashMap::new(),
            rpd_limiters: DashMap::new(),
        }
    }

    /// Check if a request is allowed for the given key.
    ///
    /// Returns `Ok(())` if allowed, `Err(RateLimitError)` if rate limited.
    pub fn check_request(
        &self,
        key_id: &str,
        rpm_limit: Option<u32>,
        rpd_limit: Option<u32>,
    ) -> Result<(), RateLimitError> {
        // Check RPM
        if let Some(limit) = rpm_limit {
            if limit > 0 {
                let limiter = self.get_or_create_rpm_limiter(key_id, limit);
                if limiter.check().is_err() {
                    return Err(RateLimitError::RpmExceeded {
                        limit,
                        reset_seconds: 60,
                    });
                }
            }
        }

        // Check RPD
        if let Some(limit) = rpd_limit {
            if limit > 0 {
                let limiter = self.get_or_create_rpd_limiter(key_id, limit);
                if !limiter.check() {
                    return Err(RateLimitError::RpdExceeded {
                        limit,
                        reset_seconds: limiter.seconds_until_reset(),
                    });
                }
            }
        }

        Ok(())
    }

    /// Check if token usage is allowed for the given key.
    ///
    /// Call this with estimated tokens before the request.
    pub fn check_tokens(
        &self,
        key_id: &str,
        tokens: u32,
        tpm_limit: Option<u32>,
    ) -> Result<(), RateLimitError> {
        if let Some(limit) = tpm_limit {
            if limit > 0 && tokens > 0 {
                let limiter = self.get_or_create_tpm_limiter(key_id, limit);
                if !limiter.check(tokens) {
                    return Err(RateLimitError::TpmExceeded {
                        limit,
                        reset_seconds: 60,
                    });
                }
            }
        }
        Ok(())
    }

    /// Record actual token usage after a request completes.
    ///
    /// This adjusts the token bucket for more accurate limiting.
    pub fn record_tokens(&self, key_id: &str, tokens: u32, tpm_limit: Option<u32>) {
        if let Some(limit) = tpm_limit {
            if limit > 0 && tokens > 0 {
                let limiter = self.get_or_create_tpm_limiter(key_id, limit);
                limiter.consume(tokens);
            }
        }
    }

    fn get_or_create_rpm_limiter(
        &self,
        key_id: &str,
        limit: u32,
    ) -> Arc<GovernorRateLimiter<NotKeyed, InMemoryState, DefaultClock>> {
        self.rpm_limiters
            .entry(key_id.to_string())
            .or_insert_with(|| {
                let quota = Quota::per_minute(NonZeroU32::new(limit).unwrap_or(NonZeroU32::MIN));
                Arc::new(GovernorRateLimiter::direct(quota))
            })
            .clone()
    }

    fn get_or_create_tpm_limiter(&self, key_id: &str, limit: u32) -> Arc<TokenBucketLimiter> {
        self.tpm_limiters
            .entry(key_id.to_string())
            .or_insert_with(|| Arc::new(TokenBucketLimiter::new(limit)))
            .clone()
    }

    fn get_or_create_rpd_limiter(&self, key_id: &str, limit: u32) -> Arc<DailyLimiter> {
        self.rpd_limiters
            .entry(key_id.to_string())
            .or_insert_with(|| Arc::new(DailyLimiter::new(limit)))
            .clone()
    }

    /// Clear rate limit state for a key (e.g., when key is deleted).
    pub fn clear_key(&self, key_id: &str) {
        self.rpm_limiters.remove(key_id);
        self.tpm_limiters.remove(key_id);
        self.rpd_limiters.remove(key_id);
    }

    /// Get current usage stats for a key.
    #[allow(dead_code)]
    pub fn get_usage(&self, key_id: &str) -> RateLimitUsage {
        RateLimitUsage {
            rpm_remaining: self
                .rpm_limiters
                .get(key_id)
                .map(|l| l.check().is_ok())
                .unwrap_or(true),
            tpm_remaining: self
                .tpm_limiters
                .get(key_id)
                .map(|l| l.available())
                .unwrap_or(u32::MAX),
            rpd_remaining: self
                .rpd_limiters
                .get(key_id)
                .map(|l| l.remaining())
                .unwrap_or(u32::MAX),
        }
    }
}

/// Simple token bucket for TPM limiting.
///
/// More flexible than governor for variable-size token consumption.
struct TokenBucketLimiter {
    limit: u32,
    tokens: std::sync::atomic::AtomicU32,
    last_refill: std::sync::atomic::AtomicU64,
}

impl TokenBucketLimiter {
    fn new(limit: u32) -> Self {
        Self {
            limit,
            tokens: std::sync::atomic::AtomicU32::new(limit),
            last_refill: std::sync::atomic::AtomicU64::new(Self::now_secs()),
        }
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn refill_if_needed(&self) {
        let now = Self::now_secs();
        let last = self
            .last_refill
            .load(std::sync::atomic::Ordering::Relaxed);

        // Refill every 60 seconds
        if now >= last + 60 {
            if self
                .last_refill
                .compare_exchange(
                    last,
                    now,
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::Relaxed,
                )
                .is_ok()
            {
                self.tokens
                    .store(self.limit, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    fn check(&self, tokens: u32) -> bool {
        self.refill_if_needed();
        self.tokens.load(std::sync::atomic::Ordering::Relaxed) >= tokens
    }

    fn consume(&self, tokens: u32) {
        self.refill_if_needed();
        self.tokens
            .fetch_sub(tokens.min(self.available()), std::sync::atomic::Ordering::Relaxed);
    }

    fn available(&self) -> u32 {
        self.refill_if_needed();
        self.tokens.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Daily request limiter.
struct DailyLimiter {
    limit: u32,
    count: std::sync::atomic::AtomicU32,
    day_start: std::sync::atomic::AtomicU64,
}

impl DailyLimiter {
    fn new(limit: u32) -> Self {
        Self {
            limit,
            count: std::sync::atomic::AtomicU32::new(0),
            day_start: std::sync::atomic::AtomicU64::new(Self::today_start()),
        }
    }

    fn today_start() -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Round down to start of day (UTC)
        now - (now % 86400)
    }

    fn reset_if_new_day(&self) {
        let today = Self::today_start();
        let stored = self.day_start.load(std::sync::atomic::Ordering::Relaxed);

        if today > stored {
            if self
                .day_start
                .compare_exchange(
                    stored,
                    today,
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::Relaxed,
                )
                .is_ok()
            {
                self.count
                    .store(0, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    fn check(&self) -> bool {
        self.reset_if_new_day();
        let current = self
            .count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if current >= self.limit {
            // Undo the increment
            self.count
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            false
        } else {
            true
        }
    }

    fn remaining(&self) -> u32 {
        self.reset_if_new_day();
        self.limit
            .saturating_sub(self.count.load(std::sync::atomic::Ordering::Relaxed))
    }

    fn seconds_until_reset(&self) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let day_start = self.day_start.load(std::sync::atomic::Ordering::Relaxed);
        let day_end = day_start + 86400;
        day_end.saturating_sub(now)
    }
}

/// Rate limit error with details.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum RateLimitError {
    RpmExceeded { limit: u32, reset_seconds: u64 },
    TpmExceeded { limit: u32, reset_seconds: u64 },
    RpdExceeded { limit: u32, reset_seconds: u64 },
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RpmExceeded { limit, .. } => {
                write!(f, "Rate limit exceeded: {} requests per minute", limit)
            }
            Self::TpmExceeded { limit, .. } => {
                write!(f, "Token limit exceeded: {} tokens per minute", limit)
            }
            Self::RpdExceeded { limit, .. } => {
                write!(f, "Daily limit exceeded: {} requests per day", limit)
            }
        }
    }
}

impl std::error::Error for RateLimitError {}

/// Current rate limit usage for a key.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct RateLimitUsage {
    pub rpm_remaining: bool,
    pub tpm_remaining: u32,
    pub rpd_remaining: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rpm_limiting() {
        let limiter = RateLimiter::new();
        let key = "test-key";

        // Should allow requests up to the limit
        for _ in 0..5 {
            assert!(limiter.check_request(key, Some(5), None).is_ok());
        }

        // Should reject after limit
        assert!(limiter.check_request(key, Some(5), None).is_err());
    }

    #[test]
    fn test_no_limit() {
        let limiter = RateLimiter::new();
        let key = "test-key";

        // No limit should always allow
        for _ in 0..100 {
            assert!(limiter.check_request(key, None, None).is_ok());
        }
    }

    #[test]
    fn test_tpm_limiting() {
        let limiter = RateLimiter::new();
        let key = "test-key";

        // Should allow tokens up to limit
        assert!(limiter.check_tokens(key, 500, Some(1000)).is_ok());
        limiter.record_tokens(key, 500, Some(1000));

        assert!(limiter.check_tokens(key, 400, Some(1000)).is_ok());
        limiter.record_tokens(key, 400, Some(1000));

        // Should reject when near/over limit
        assert!(limiter.check_tokens(key, 200, Some(1000)).is_err());
    }

    #[test]
    fn test_clear_key() {
        let limiter = RateLimiter::new();
        let key = "test-key";

        // Exhaust limit
        for _ in 0..5 {
            let _ = limiter.check_request(key, Some(5), None);
        }

        // Clear should reset
        limiter.clear_key(key);

        // Should allow again
        assert!(limiter.check_request(key, Some(5), None).is_ok());
    }
}

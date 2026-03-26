//! Rate Limiter - Production-Grade Rate Limiting and Throttling
//!
//! This module implements comprehensive rate limiting for the Exchange Gateway
//! including per-user, per-device, per-IP, and per-endpoint rate limiting with
//! sliding window, token bucket, and leaky bucket algorithms.

use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

/// Rate limit result
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitResult {
    /// Request is allowed
    Allowed,
    /// Request is denied due to rate limit
    Denied { retry_after: Duration },
    /// Request is throttled (slowed down)
    Throttled { delay_ms: u64 },
}

/// Rate limit configuration
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Maximum requests per window
    pub max_requests: u32,
    /// Time window for rate limiting
    pub window: Duration,
    /// Burst allowance (requests that can exceed rate temporarily)
    pub burst: u32,
    /// Cooldown period after hitting limit
    pub cooldown: Option<Duration>,
}

impl RateLimitConfig {
    /// Create a new rate limit configuration
    pub fn new(max_requests: u32, window_seconds: i64) -> Self {
        Self {
            max_requests,
            window: Duration::seconds(window_seconds),
            burst: 0,
            cooldown: None,
        }
    }

    /// Set burst allowance
    pub fn with_burst(mut self, burst: u32) -> Self {
        self.burst = burst;
        self
    }

    /// Set cooldown period
    pub fn with_cooldown(mut self, cooldown_seconds: i64) -> Self {
        self.cooldown = Some(Duration::seconds(cooldown_seconds));
        self
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_requests: 100,
            window: Duration::seconds(60),
            burst: 10,
            cooldown: None,
        }
    }
}

/// Request counter for sliding window
#[derive(Debug, Clone)]
struct RequestCounter {
    /// Request timestamps in the current window
    timestamps: Vec<DateTime<Utc>>,
    /// Blocked until time (if rate limited)
    blocked_until: Option<DateTime<Utc>>,
}

impl RequestCounter {
    fn new() -> Self {
        Self {
            timestamps: Vec::new(),
            blocked_until: None,
        }
    }

    /// Add a request timestamp
    fn add_request(&mut self, now: DateTime<Utc>) {
        self.timestamps.push(now);
    }

    /// Clean old timestamps outside the window
    fn clean_old(&mut self, window: Duration, now: DateTime<Utc>) {
        let cutoff = now - window;
        self.timestamps.retain(|&t| t > cutoff);
    }

    /// Get request count in window
    fn count(&self) -> usize {
        self.timestamps.len()
    }

    /// Check if currently blocked
    fn is_blocked(&self, now: DateTime<Utc>) -> bool {
        self.blocked_until.map_or(false, |t| now < t)
    }

    /// Block for a duration
    fn block(&mut self, duration: Duration, now: DateTime<Utc>) {
        self.blocked_until = Some(now + duration);
    }

    /// Get time until unblock
    fn time_until_unblock(&self, now: DateTime<Utc>) -> Option<Duration> {
        self.blocked_until.map(|t| {
            if t > now {
                t - now
            } else {
                Duration::zero()
            }
        })
    }

    /// Get oldest timestamp (for calculating retry after)
    fn oldest(&self) -> Option<DateTime<Utc>> {
        self.timestamps.first().copied()
    }
}

/// Token bucket for token bucket rate limiting
#[derive(Debug, Clone)]
struct TokenBucket {
    /// Current tokens available
    tokens: f64,
    /// Maximum tokens
    max_tokens: f64,
    /// Token refill rate (tokens per second)
    refill_rate: f64,
    /// Last refill timestamp
    last_refill: DateTime<Utc>,
}

impl TokenBucket {
    fn new(max_tokens: f64, refill_rate: f64) -> Self {
        Self {
            tokens: max_tokens,
            max_tokens,
            refill_rate,
            last_refill: Utc::now(),
        }
    }

    /// Refill tokens based on elapsed time
    fn refill(&mut self, now: DateTime<Utc>) {
        let elapsed = (now - self.last_refill).num_milliseconds() as f64 / 1000.0;
        let new_tokens = elapsed * self.refill_rate;
        self.tokens = (self.tokens + new_tokens).min(self.max_tokens);
        self.last_refill = now;
    }

    /// Try to consume tokens
    fn consume(&mut self, tokens: f64, now: DateTime<Utc>) -> bool {
        self.refill(now);
        if self.tokens >= tokens {
            self.tokens -= tokens;
            true
        } else {
            false
        }
    }

    /// Get time until enough tokens available
    fn time_until_available(&self, tokens: f64) -> Duration {
        let needed = tokens - self.tokens;
        if needed <= 0.0 {
            Duration::zero()
        } else {
            let seconds = needed / self.refill_rate;
            Duration::milliseconds((seconds * 1000.0) as i64)
        }
    }
}

/// Rate limiter using sliding window algorithm
pub struct SlidingWindowRateLimiter {
    counters: HashMap<String, RequestCounter>,
    config: RateLimitConfig,
}

impl SlidingWindowRateLimiter {
    /// Create a new sliding window rate limiter
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            counters: HashMap::new(),
            config,
        }
    }

    /// Check if request is allowed
    pub fn check(&mut self, key: &str) -> RateLimitResult {
        let now = Utc::now();
        let counter = self.counters.entry(key.to_string()).or_insert_with(RequestCounter::new);

        // Check if blocked
        if counter.is_blocked(now) {
            if let Some(remaining) = counter.time_until_unblock(now) {
                return RateLimitResult::Denied { retry_after: remaining };
            }
        }

        // Clean old timestamps
        counter.clean_old(self.config.window, now);

        // Check if over limit
        let limit = self.config.max_requests + self.config.burst;
        if counter.count() >= limit as usize {
            // Calculate retry after based on oldest request
            let retry_after = if let Some(oldest) = counter.oldest() {
                (oldest + self.config.window) - now
            } else {
                self.config.window
            };

            // Apply cooldown if configured
            if let Some(cooldown) = self.config.cooldown {
                counter.block(cooldown, now);
            }

            return RateLimitResult::Denied { retry_after };
        }

        // Allow request and record it
        counter.add_request(now);
        RateLimitResult::Allowed
    }

    /// Get current count for a key
    pub fn get_count(&mut self, key: &str) -> usize {
        let now = Utc::now();
        if let Some(counter) = self.counters.get_mut(key) {
            counter.clean_old(self.config.window, now);
            counter.count()
        } else {
            0
        }
    }

    /// Reset counter for a key
    pub fn reset(&mut self, key: &str) {
        self.counters.remove(key);
    }

    /// Clean up old entries
    pub fn cleanup(&mut self) {
        let now = Utc::now();
        let window = self.config.window;
        
        self.counters.retain(|_, counter| {
            counter.clean_old(window, now);
            !counter.timestamps.is_empty() || counter.is_blocked(now)
        });
    }
}

/// Token bucket rate limiter
pub struct TokenBucketRateLimiter {
    buckets: HashMap<String, TokenBucket>,
    max_tokens: f64,
    refill_rate: f64,
}

impl TokenBucketRateLimiter {
    /// Create a new token bucket rate limiter
    pub fn new(max_requests: u32, window_seconds: i64) -> Self {
        let max_tokens = max_requests as f64;
        let refill_rate = max_tokens / window_seconds as f64;
        
        Self {
            buckets: HashMap::new(),
            max_tokens,
            refill_rate,
        }
    }

    /// Check if request is allowed (consumes 1 token)
    pub fn check(&mut self, key: &str) -> RateLimitResult {
        let now = Utc::now();
        let bucket = self.buckets.entry(key.to_string()).or_insert_with(|| {
            TokenBucket::new(self.max_tokens, self.refill_rate)
        });

        if bucket.consume(1.0, now) {
            RateLimitResult::Allowed
        } else {
            let retry_after = bucket.time_until_available(1.0);
            RateLimitResult::Denied { retry_after }
        }
    }

    /// Check with custom token cost
    pub fn check_with_cost(&mut self, key: &str, cost: f64) -> RateLimitResult {
        let now = Utc::now();
        let bucket = self.buckets.entry(key.to_string()).or_insert_with(|| {
            TokenBucket::new(self.max_tokens, self.refill_rate)
        });

        if bucket.consume(cost, now) {
            RateLimitResult::Allowed
        } else {
            let retry_after = bucket.time_until_available(cost);
            RateLimitResult::Denied { retry_after }
        }
    }

    /// Get current token count for a key
    pub fn get_tokens(&mut self, key: &str) -> f64 {
        let now = Utc::now();
        if let Some(bucket) = self.buckets.get_mut(key) {
            bucket.refill(now);
            bucket.tokens
        } else {
            self.max_tokens
        }
    }

    /// Reset bucket for a key
    pub fn reset(&mut self, key: &str) {
        self.buckets.remove(key);
    }
}

/// Multi-level rate limiter
pub struct MultiLevelRateLimiter {
    /// Global rate limiter
    global: Arc<Mutex<SlidingWindowRateLimiter>>,
    /// Per-user rate limiters
    per_user: Arc<Mutex<HashMap<String, SlidingWindowRateLimiter>>>,
    /// Per-device rate limiters
    per_device: Arc<Mutex<HashMap<String, SlidingWindowRateLimiter>>>,
    /// Per-IP rate limiters
    per_ip: Arc<Mutex<HashMap<IpAddr, SlidingWindowRateLimiter>>>,
    /// Per-endpoint rate limiters
    per_endpoint: Arc<Mutex<HashMap<String, SlidingWindowRateLimiter>>>,
    /// Configuration for each level
    configs: RateLimitLevels,
}

/// Rate limit configurations for each level
#[derive(Debug, Clone)]
pub struct RateLimitLevels {
    pub global: RateLimitConfig,
    pub per_user: RateLimitConfig,
    pub per_device: RateLimitConfig,
    pub per_ip: RateLimitConfig,
    pub per_endpoint: HashMap<String, RateLimitConfig>,
}

impl Default for RateLimitLevels {
    fn default() -> Self {
        let mut per_endpoint = HashMap::new();
        per_endpoint.insert("autodiscover".to_string(), RateLimitConfig::new(30, 60));
        per_endpoint.insert("eas".to_string(), RateLimitConfig::new(1000, 60));
        per_endpoint.insert("ews".to_string(), RateLimitConfig::new(500, 60));

        Self {
            global: RateLimitConfig::new(10000, 60),
            per_user: RateLimitConfig::new(300, 60),
            per_device: RateLimitConfig::new(200, 60),
            per_ip: RateLimitConfig::new(500, 60),
            per_endpoint,
        }
    }
}

impl MultiLevelRateLimiter {
    /// Create a new multi-level rate limiter
    pub fn new(configs: RateLimitLevels) -> Self {
        Self {
            global: Arc::new(Mutex::new(SlidingWindowRateLimiter::new(configs.global.clone()))),
            per_user: Arc::new(Mutex::new(HashMap::new())),
            per_device: Arc::new(Mutex::new(HashMap::new())),
            per_ip: Arc::new(Mutex::new(HashMap::new())),
            per_endpoint: Arc::new(Mutex::new(HashMap::new())),
            configs,
        }
    }

    /// Check request against all rate limit levels
    pub fn check(
        &self,
        user_id: Option<&str>,
        device_id: Option<&str>,
        ip: Option<IpAddr>,
        endpoint: &str,
    ) -> RateLimitResult {
        // Check global limit
        {
            let mut global = self.global.lock().unwrap();
            match global.check("global") {
                RateLimitResult::Allowed => {}
                denied => return denied,
            }
        }

        // Check per-user limit
        if let Some(user) = user_id {
            let mut per_user = self.per_user.lock().unwrap();
            let limiter = per_user.entry(user.to_string()).or_insert_with(|| {
                SlidingWindowRateLimiter::new(self.configs.per_user.clone())
            });
            match limiter.check(user) {
                RateLimitResult::Allowed => {}
                denied => return denied,
            }
        }

        // Check per-device limit
        if let Some(device) = device_id {
            let mut per_device = self.per_device.lock().unwrap();
            let limiter = per_device.entry(device.to_string()).or_insert_with(|| {
                SlidingWindowRateLimiter::new(self.configs.per_device.clone())
            });
            match limiter.check(device) {
                RateLimitResult::Allowed => {}
                denied => return denied,
            }
        }

        // Check per-IP limit
        if let Some(ip_addr) = ip {
            let mut per_ip = self.per_ip.lock().unwrap();
            let limiter = per_ip.entry(ip_addr).or_insert_with(|| {
                SlidingWindowRateLimiter::new(self.configs.per_ip.clone())
            });
            match limiter.check(&ip_addr.to_string()) {
                RateLimitResult::Allowed => {}
                denied => return denied,
            }
        }

        // Check per-endpoint limit
        if let Some(config) = self.configs.per_endpoint.get(endpoint) {
            let mut per_endpoint = self.per_endpoint.lock().unwrap();
            let limiter = per_endpoint.entry(endpoint.to_string()).or_insert_with(|| {
                SlidingWindowRateLimiter::new(config.clone())
            });
            match limiter.check(endpoint) {
                RateLimitResult::Allowed => {}
                denied => return denied,
            }
        }

        RateLimitResult::Allowed
    }

    /// Get statistics for a user
    pub fn get_user_stats(&self, user_id: &str) -> Option<usize> {
        let mut per_user = self.per_user.lock().unwrap();
        per_user.get_mut(user_id).map(|l| l.get_count(user_id))
    }

    /// Get statistics for a device
    pub fn get_device_stats(&self, device_id: &str) -> Option<usize> {
        let mut per_device = self.per_device.lock().unwrap();
        per_device.get_mut(device_id).map(|l| l.get_count(device_id))
    }

    /// Reset all limits for a user
    pub fn reset_user(&self, user_id: &str) {
        let mut per_user = self.per_user.lock().unwrap();
        per_user.remove(user_id);
    }

    /// Reset all limits for a device
    pub fn reset_device(&self, device_id: &str) {
        let mut per_device = self.per_device.lock().unwrap();
        per_device.remove(device_id);
    }

    /// Reset all limits for an IP
    pub fn reset_ip(&self, ip: IpAddr) {
        let mut per_ip = self.per_ip.lock().unwrap();
        per_ip.remove(&ip);
    }

    /// Cleanup old entries
    pub fn cleanup(&self) {
        self.global.lock().unwrap().cleanup();
        
        for (_, limiter) in self.per_user.lock().unwrap().iter_mut() {
            limiter.cleanup();
        }
        
        for (_, limiter) in self.per_device.lock().unwrap().iter_mut() {
            limiter.cleanup();
        }
        
        for (_, limiter) in self.per_ip.lock().unwrap().iter_mut() {
            limiter.cleanup();
        }
        
        for (_, limiter) in self.per_endpoint.lock().unwrap().iter_mut() {
            limiter.cleanup();
        }
    }
}

/// Rate limit middleware for HTTP handlers
pub struct RateLimitMiddleware {
    limiter: Arc<MultiLevelRateLimiter>,
}

impl RateLimitMiddleware {
    /// Create new rate limit middleware
    pub fn new(limiter: Arc<MultiLevelRateLimiter>) -> Self {
        Self { limiter }
    }

    /// Check request and return appropriate response
    pub fn check_request(
        &self,
        user_id: Option<&str>,
        device_id: Option<&str>,
        ip: Option<IpAddr>,
        endpoint: &str,
    ) -> Result<(), RateLimitError> {
        match self.limiter.check(user_id, device_id, ip, endpoint) {
            RateLimitResult::Allowed => Ok(()),
            RateLimitResult::Denied { retry_after } => {
                Err(RateLimitError::RateLimited {
                    retry_after_secs: retry_after.num_seconds() as u64,
                })
            }
            RateLimitResult::Throttled { delay_ms } => {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                Ok(())
            }
        }
    }
}

/// Rate limit error
#[derive(Debug, Clone)]
pub enum RateLimitError {
    RateLimited { retry_after_secs: u64 },
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RateLimitError::RateLimited { retry_after_secs } => {
                write!(f, "Rate limited. Retry after {} seconds", retry_after_secs)
            }
        }
    }
}

impl std::error::Error for RateLimitError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sliding_window_rate_limiter() {
        let config = RateLimitConfig::new(5, 60);
        let mut limiter = SlidingWindowRateLimiter::new(config);

        // First 5 requests should be allowed
        for _ in 0..5 {
            assert_eq!(limiter.check("test"), RateLimitResult::Allowed);
        }

        // 6th request should be denied
        let result = limiter.check("test");
        assert!(matches!(result, RateLimitResult::Denied { .. }));
    }

    #[test]
    fn test_token_bucket_rate_limiter() {
        let mut limiter = TokenBucketRateLimiter::new(5, 60);

        // First 5 requests should be allowed
        for _ in 0..5 {
            assert_eq!(limiter.check("test"), RateLimitResult::Allowed);
        }

        // 6th request should be denied
        let result = limiter.check("test");
        assert!(matches!(result, RateLimitResult::Denied { .. }));
    }

    #[test]
    fn test_token_bucket_refill() {
        let mut limiter = TokenBucketRateLimiter::new(100, 1); // 100 tokens per second

        // Use up tokens
        for _ in 0..100 {
            assert_eq!(limiter.check("test"), RateLimitResult::Allowed);
        }

        // Should be denied
        assert!(matches!(limiter.check("test"), RateLimitResult::Denied { .. }));

        // Wait for refill
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Should have some tokens now
        let tokens = limiter.get_tokens("test");
        assert!(tokens > 0.0);
    }

    #[test]
    fn test_request_counter() {
        let mut counter = RequestCounter::new();
        let now = Utc::now();

        counter.add_request(now);
        counter.add_request(now);
        counter.add_request(now);

        assert_eq!(counter.count(), 3);

        // Clean old (none should be removed)
        counter.clean_old(Duration::seconds(60), now);
        assert_eq!(counter.count(), 3);

        // Add old request and clean
        counter.add_request(now - Duration::seconds(120));
        counter.clean_old(Duration::seconds(60), now);
        assert_eq!(counter.count(), 3);
    }

    #[test]
    fn test_rate_limit_config() {
        let config = RateLimitConfig::new(100, 60)
            .with_burst(20)
            .with_cooldown(300);

        assert_eq!(config.max_requests, 100);
        assert_eq!(config.window, Duration::seconds(60));
        assert_eq!(config.burst, 20);
        assert_eq!(config.cooldown, Some(Duration::seconds(300)));
    }

    #[test]
    fn test_multi_level_rate_limiter() {
        let configs = RateLimitLevels::default();
        let limiter = MultiLevelRateLimiter::new(configs);

        // Should allow initial requests
        assert!(matches!(
            limiter.check(Some("user1"), Some("device1"), None, "eas"),
            RateLimitResult::Allowed
        ));
    }
}

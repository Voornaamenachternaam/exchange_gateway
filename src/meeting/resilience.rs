// src/meeting/resilience.rs
//! Resilience patterns for external service calls.
//!
//! This module provides:
//! - Circuit breaker pattern for failing fast when services are unhealthy
//! - Rate limiting to prevent overwhelming external services
//! - Retry policies with exponential backoff

use anyhow::{Result, anyhow};
use std::sync::Arc;
use std::time::{Duration, Instant};
use parking_lot::RwLock;
use governor::{Quota, RateLimiter, GovernorConfigBuilder};
use governor::state::{InMemoryState, DirectMiddleware};
use std::num::NonZeroU32;

/// Circuit breaker states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Circuit is closed, requests flow through
    Closed,
    /// Circuit is open, requests fail fast
    Open,
    /// Circuit is half-open, testing if service recovered
    HalfOpen,
}

/// Configuration for circuit breaker
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of failures before opening circuit
    pub failure_threshold: u32,
    /// Time to wait before attempting to close circuit (from open state)
    pub reset_timeout: Duration,
    /// Number of successes in half-open state before closing
    pub success_threshold: u32,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            reset_timeout: Duration::from_secs(30),
            success_threshold: 2,
        }
    }
}

/// Circuit breaker for protecting against cascading failures
#[derive(Debug)]
pub struct CircuitBreaker {
    state: RwLock<CircuitBreakerInner>,
    config: CircuitBreakerConfig,
    name: String,
}

#[derive(Debug)]
struct CircuitBreakerInner {
    state: CircuitState,
    failure_count: u32,
    success_count: u32,
    last_failure: Option<Instant>,
    opened_at: Option<Instant>,
}

impl CircuitBreaker {
    /// Creates a new circuit breaker with the given name and configuration
    pub fn new(name: impl Into<String>, config: CircuitBreakerConfig) -> Self {
        Self {
            state: RwLock::new(CircuitBreakerInner {
                state: CircuitState::Closed,
                failure_count: 0,
                success_count: 0,
                last_failure: None,
                opened_at: None,
            }),
            config,
            name: name.into(),
        }
    }

    /// Creates a circuit breaker with default configuration
    pub fn with_name(name: impl Into<String>) -> Self {
        Self::new(name, CircuitBreakerConfig::default())
    }

    /// Check if requests are allowed through the circuit
    pub fn is_allowed(&self) -> bool {
        let mut inner = self.state.write();
        
        match inner.state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                // Check if we should transition to half-open
                if let Some(opened_at) = inner.opened_at {
                    if opened_at.elapsed() >= self.config.reset_timeout {
                        inner.state = CircuitState::HalfOpen;
                        inner.success_count = 0;
                        tracing::info!(
                            circuit_breaker = %self.name,
                            "Circuit breaker transitioning to half-open"
                        );
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => true,
        }
    }

    /// Record a successful request
    pub fn record_success(&self) {
        let mut inner = self.state.write();
        
        match inner.state {
            CircuitState::Closed => {
                inner.failure_count = 0;
            }
            CircuitState::HalfOpen => {
                inner.success_count += 1;
                if inner.success_count >= self.config.success_threshold {
                    inner.state = CircuitState::Closed;
                    inner.failure_count = 0;
                    inner.opened_at = None;
                    tracing::info!(
                        circuit_breaker = %self.name,
                        "Circuit breaker closed after {} successes",
                        inner.success_count
                    );
                }
            }
            CircuitState::Open => {
                // Shouldn't happen, but handle gracefully
            }
        }
    }

    /// Record a failed request
    pub fn record_failure(&self) {
        let mut inner = self.state.write();
        
        inner.failure_count += 1;
        inner.last_failure = Some(Instant::now());
        
        match inner.state {
            CircuitState::Closed => {
                if inner.failure_count >= self.config.failure_threshold {
                    inner.state = CircuitState::Open;
                    inner.opened_at = Some(Instant::now());
                    tracing::warn!(
                        circuit_breaker = %self.name,
                        failure_count = inner.failure_count,
                        "Circuit breaker opened after {} failures",
                        inner.failure_count
                    );
                }
            }
            CircuitState::HalfOpen => {
                // Single failure in half-open reopens the circuit
                inner.state = CircuitState::Open;
                inner.opened_at = Some(Instant::now());
                tracing::warn!(
                    circuit_breaker = %self.name,
                    "Circuit breaker reopened from half-open state"
                );
            }
            CircuitState::Open => {
                // Already open, just update last failure
            }
        }
    }

    /// Get current state for monitoring
    pub fn state(&self) -> CircuitState {
        self.state.read().state
    }

    /// Get the name of this circuit breaker
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Rate limiter wrapper using governor
pub struct RateLimitGuard {
    limiter: Arc<RateLimiter<InMemoryState, DirectMiddleware>>,
    name: String,
}

impl RateLimitGuard {
    /// Create a new rate limiter with requests per second limit
    pub fn new(name: impl Into<String>, requests_per_second: u32) -> Self {
        let quota = Quota::per_second(NonZeroU32::new(requests_per_second).unwrap_or(NonZeroU32::new(1).unwrap()));
        let limiter = RateLimiter::direct(quota);
        
        Self {
            limiter: Arc::new(limiter),
            name: name.into(),
        }
    }

    /// Create a rate limiter with custom quota
    pub fn with_quota(name: impl Into<String>, quota: Quota) -> Self {
        Self {
            limiter: Arc::new(RateLimiter::direct(quota)),
            name: name.into(),
        }
    }

    /// Check if a request is allowed, returns true if allowed
    pub fn check(&self) -> bool {
        match self.limiter.check() {
            Ok(_) => true,
            Err(_) => {
                tracing::debug!(
                    rate_limiter = %self.name,
                    "Rate limit exceeded"
                );
                false
            }
        }
    }

    /// Wait until a request is allowed (async)
    pub async fn wait(&self) {
        // governor doesn't have async wait, so we poll
        loop {
            if self.check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// Combined resilience handler that provides circuit breaker and rate limiting
pub struct ResilienceHandler {
    circuit_breaker: Arc<CircuitBreaker>,
    rate_limiter: Arc<RateLimitGuard>,
}

impl ResilienceHandler {
    /// Create a new resilience handler
    pub fn new(
        name: impl Into<String> + Clone,
        circuit_config: CircuitBreakerConfig,
        requests_per_second: u32,
    ) -> Self {
        let name_str = name.into();
        Self {
            circuit_breaker: Arc::new(CircuitBreaker::new(name_str.clone(), circuit_config)),
            rate_limiter: Arc::new(RateLimitGuard::new(name_str, requests_per_second)),
        }
    }

    /// Check if a request should be allowed
    pub fn should_allow(&self) -> Result<()> {
        if !self.circuit_breaker.is_allowed() {
            return Err(anyhow!(
                "Circuit breaker '{}' is open - service unavailable",
                self.circuit_breaker.name()
            ));
        }
        
        if !self.rate_limiter.check() {
            return Err(anyhow!(
                "Rate limit exceeded for '{}'",
                self.circuit_breaker.name()
            ));
        }
        
        Ok(())
    }

    /// Record a successful operation
    pub fn record_success(&self) {
        self.circuit_breaker.record_success();
    }

    /// Record a failed operation
    pub fn record_failure(&self) {
        self.circuit_breaker.record_failure();
    }

    /// Get the underlying circuit breaker for monitoring
    pub fn circuit_breaker(&self) -> &Arc<CircuitBreaker> {
        &self.circuit_breaker
    }

    /// Get the underlying rate limiter for monitoring
    pub fn rate_limiter(&self) -> &Arc<RateLimitGuard> {
        &self.rate_limiter
    }
}

/// Retry configuration for transient failures
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retries
    pub max_retries: u32,
    /// Initial delay before first retry
    pub initial_delay: Duration,
    /// Maximum delay between retries
    pub max_delay: Duration,
    /// Multiplier for exponential backoff
    pub backoff_multiplier: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(10),
            backoff_multiplier: 2.0,
        }
    }
}

impl RetryConfig {
    /// Calculate delay for a given retry attempt
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let delay_ms = self.initial_delay.as_millis() as f64
            * self.backoff_multiplier.powi(attempt as i32);
        
        let delay = Duration::from_millis(delay_ms as u64);
        delay.min(self.max_delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker_opens_after_threshold() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            ..Default::default()
        };
        let cb = CircuitBreaker::new("test", config);
        
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.is_allowed());
        
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);
        
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.is_allowed());
    }

    #[test]
    fn test_rate_limiter() {
        let limiter = RateLimitGuard::new("test", 10);
        
        // Should allow initial requests
        for _ in 0..5 {
            assert!(limiter.check());
        }
    }

    #[test]
    fn test_retry_backoff() {
        let config = RetryConfig {
            max_retries: 5,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(10),
            backoff_multiplier: 2.0,
        };
        
        assert_eq!(config.delay_for_attempt(0), Duration::from_millis(100));
        assert_eq!(config.delay_for_attempt(1), Duration::from_millis(200));
        assert_eq!(config.delay_for_attempt(2), Duration::from_millis(400));
    }
}
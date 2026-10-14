// src/auth.rs
use crate::caldav::CaldavClient;
use crate::config::Config;
use crate::jmap::JmapClient;
use dashmap::DashMap;
use moka::Expiry;
use moka::sync::Cache;
use secrecy::SecretString;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tracing::{debug, trace, warn};

/// Outcome of a credential verification against the Stalwart backend.
///
/// The three states are deliberately distinct so a *backend outage* is never
/// conflated with *wrong credentials*: returning `InvalidCredentials` to an
/// Outlook client makes it prompt the user for a password, so a transient
/// 5xx / transport failure (reported as `BackendUnavailable`) must instead
/// surface as a retryable 503 and be retried, not re-prompted. See AUDIT.md §17.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthOutcome {
    /// The credentials are valid.
    Verified,
    /// The backend authoritatively rejected the credentials (401/403).
    InvalidCredentials,
    /// The backend could not be reached or answered with a 5xx / transport
    /// error; the credential state is *unknown* and must not be cached.
    BackendUnavailable,
}

impl AuthOutcome {
    /// Whether the credentials were verified.
    pub fn is_verified(self) -> bool {
        matches!(self, Self::Verified)
    }

    /// Whether the failure was a backend outage rather than a credential
    /// rejection. Callers use this to choose a retryable 5xx response over a
    /// password-prompting 401.
    pub fn is_backend_unavailable(self) -> bool {
        matches!(self, Self::BackendUnavailable)
    }
}

/// Tri-state result of a single backend credential probe, before it is
/// collapsed into an [`AuthOutcome`] and cached. Distinguishes an authoritative
/// 401/403 rejection from a 5xx / transport failure so the latter is neither
/// cached nor allowed to trigger a CalDAV fallback storm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialCheck {
    /// The backend accepted the credentials.
    Valid,
    /// The backend rejected the credentials (HTTP 401 or 403).
    Invalid,
    /// The backend was unreachable or returned a non-auth error (5xx,
    /// transport failure, malformed response).
    BackendError(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cached {
    Valid,
    Invalid,
}

/// Per-entry expiry for the credential cache.
///
/// Positive (verified) results live for the configured `auth_cache_ttl_secs`.
/// Negative (rejected) results live for the much shorter
/// `auth_cache_negative_ttl_secs` plus a deterministic per-key jitter, so a
/// user who fixes a password (or an account that is re-enabled) is not locked
/// out for the full positive TTL, and a burst of concurrent failures for the
/// same key does not expire in lockstep.
struct AuthCacheExpiry {
    positive: Duration,
    negative_base: Duration,
    negative_jitter: Duration,
}

impl Expiry<String, Cached> for AuthCacheExpiry {
    fn expire_after_create(
        &self,
        key: &String,
        value: &Cached,
        _created_at: Instant,
    ) -> Option<Duration> {
        match value {
            Cached::Valid => Some(self.positive),
            Cached::Invalid => {
                let ttl = self
                    .negative_base
                    .saturating_add(jitter_for(key, self.negative_jitter));
                // A zero base means "do not cache negatives"; the caller skips
                // the insert in that case, but keep a minimal non-zero
                // lifetime here so an entry could never become immortal.
                Some(if ttl.is_zero() {
                    Duration::from_millis(1)
                } else {
                    ttl
                })
            }
        }
    }
}

#[derive(Clone)]
pub struct AuthVerifier {
    cache: Cache<String, Cached>,
    caldav_base: String,
    jmap_base: String,
    /// Shared JMAP client (with its own session-auth cache). When present,
    /// credential probes go through [`JmapClient::probe_auth`], which both
    /// short-circuits the per-request Basic-auth probe and collapses the
    /// cold-cache probe burst that fronts EAS/EWS (AUDIT.md §18). Standalone
    /// constructions (tests) leave this `None` and build a throwaway client.
    jmap_client: Option<Arc<JmapClient>>,
    /// In-flight probe dedup keyed by `username:hash(password)`. Concurrent
    /// verifications for the *same credential* await ONE backend probe instead
    /// of fanning out N probes, so a cold-cache burst from two clients plus
    /// Microsoft IPs costs a single round-trip per credential. Keying on the
    /// credential (not the user) keeps two different-password probes of the
    /// same user concurrent, so the §17 start-order race guard still sees them
    /// overlap.
    inflight: Arc<DashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Per-username timestamp of the last *cold* backend probe. Enforces
    /// `probe_interval` as a per-user rate cap on the SPOF (the backend auth
    /// probe) even when requests arrive sequentially rather than concurrently.
    probe_gate: Cache<String, Instant>,
    /// Minimum interval between cold backend probes for one user.
    probe_interval: Duration,
    /// Consecutive `BackendUnavailable` results per user (the circuit-breaker
    /// failure counter). Reset to zero on any non-outage result.
    failure_streak: Cache<String, u32>,
    /// Open-circuit deadline per user: while `Instant::now() < deadline`,
    /// further cold probes are refused without contacting the backend.
    breaker_until: Cache<String, Instant>,
    /// Consecutive backend failures required to open the breaker.
    failure_threshold: u32,
    /// Exponential-backoff cap for an open breaker.
    breaker_max_backoff: Duration,
    /// Configured negative-cache lifetime; zero disables negative caching.
    negative_ttl: Duration,
    /// Monotonic ticket source: every verification takes a ticket *before*
    /// probing the backend, so tickets order concurrent verifications by the
    /// instant they started rather than by the instant they finished.
    ticket: Arc<AtomicU64>,
    /// Highest ticket already committed to the cache, per user. A finishing
    /// verification whose ticket is below this value lost a race to a newer
    /// request and must not repopulate the cache. Bounded (TTL + capacity) so a
    /// long-running process cannot accumulate per-user entries without limit.
    committed_generation: Cache<String, u64>,
    /// Serializes the check-then-commit step of [`AuthVerifier::commit_result`]
    /// so two verifications cannot both observe "no newer commit" and then both
    /// write. Held only across pure in-memory cache edits (no `.await`).
    commit_lock: Arc<Mutex<()>>,
}

impl AuthVerifier {
    pub fn new(cfg: &Config) -> Self {
        // Standalone constructor (unit tests, MAPI test harnesses): no shared
        // JMAP client, so the probe path builds a throwaway one per call.
        // Production uses `new_from_topology` so the client's session-auth
        // cache and the app-wide HTTP pool are shared.
        Self::from_parts(cfg, None)
    }

    /// Production constructor: share the application's JMAP client so the
    /// cold-cache probe path reuses its session-auth cache and connection pool
    /// (AUDIT.md §18).
    pub fn new_from_topology(cfg: &Config, jmap_client: Option<Arc<JmapClient>>) -> Self {
        Self::from_parts(cfg, jmap_client)
    }

    /// The shared JMAP client used for credential probes, if one was injected.
    /// Exposed so the readiness probe can reuse its connection pool instead of
    /// building a throwaway client per check (AUDIT.md §20), and so `AppState`
    /// wiring can be asserted in tests (AUDIT.md §18: the shared client must
    /// exist whenever JMAP auth is in use, regardless of whether the email
    /// feature is enabled).
    pub fn shared_jmap_client(&self) -> Option<&Arc<JmapClient>> {
        self.jmap_client.as_ref()
    }

    fn from_parts(cfg: &Config, jmap_client: Option<Arc<JmapClient>>) -> Self {
        // Positive results keep the operator-configured TTL (floor 30s).
        let positive = Duration::from_secs(cfg.auth_cache_ttl_secs.max(30));
        // Negative results use a short, separately-configured TTL. The jitter
        // is 25% of that base TTL, added on top per key.
        let negative = Duration::from_secs(cfg.auth_cache_negative_ttl_secs);
        let negative_jitter = Duration::from_millis((negative.as_millis() / 4) as u64);
        let max_entries = cfg.auth_cache_max_entries.max(1);
        let cache = Cache::builder()
            .max_capacity(max_entries as u64)
            .expire_after(AuthCacheExpiry {
                positive,
                negative_base: negative,
                negative_jitter,
            })
            .build();
        Self {
            cache,
            caldav_base: cfg.caldav_base.clone(),
            jmap_base: cfg.jmap_base.clone(),
            jmap_client,
            inflight: Arc::new(DashMap::new()),
            probe_gate: Cache::builder()
                .max_capacity(max_entries as u64)
                .time_to_live(Duration::from_secs(3600))
                .build(),
            probe_interval: Duration::from_millis(cfg.auth_probe_min_interval_ms),
            failure_streak: Cache::builder()
                .max_capacity(max_entries as u64)
                .time_to_live(Duration::from_secs(3600))
                .build(),
            breaker_until: Cache::builder()
                .max_capacity(max_entries as u64)
                .time_to_live(Duration::from_secs(3600))
                .build(),
            failure_threshold: cfg.auth_backend_failure_threshold.max(1),
            // A zero cap is meaningful: it disables the breaker (see
            // `update_breaker`), so do NOT clamp it up to 1.
            breaker_max_backoff: Duration::from_secs(cfg.auth_backend_max_backoff_secs),
            negative_ttl: negative,
            ticket: Arc::new(AtomicU64::new(0)),
            committed_generation: Cache::builder()
                .max_capacity(max_entries as u64)
                .time_to_live(Duration::from_secs(3600))
                .build(),
            commit_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Boolean convenience wrapper over [`AuthVerifier::verify_outcome`].
    ///
    /// Kept for the surfaces that only need a yes/no gate (OAB, ECP, NSPI,
    /// MAPI logon). A `BackendUnavailable` outcome maps to `false` here,
    /// matching the historical behaviour; the EAS and EWS front doors use
    /// `verify_outcome` directly so they can answer a backend outage with a
    /// retryable 5xx instead of a password-prompting 401.
    pub async fn verify(&self, username: &str, password: &str) -> bool {
        self.verify_outcome(username, password).await.is_verified()
    }

    /// Verify credentials, distinguishing valid / rejected / backend-down.
    ///
    /// Positive and negative results are cached per `(username, password)`;
    /// backend outages are never cached. A successful verification evicts any
    /// other cached credential for the same username, so a changed Stalwart
    /// password stops working at the gateway as soon as the new password is
    /// first used rather than lingering for the positive TTL.
    ///
    /// Concurrent verifications for the same user are ordered by the instant
    /// they *started* (their ticket), not the instant they finished: a slower
    /// request that began before a newer one can never repopulate the cache
    /// after the newer request's result has been committed. This closes the
    /// window in which an in-flight check of an old password could insert a
    /// fresh positive entry after a new-password login had already evicted it.
    pub async fn verify_outcome(&self, username: &str, password: &str) -> AuthOutcome {
        if username.is_empty() || password.is_empty() {
            debug!(
                target: "auth",
                username = ?username,
                "Rejected empty credentials"
            );
            return AuthOutcome::InvalidCredentials;
        }
        let cache_key = format!("{}:{}", username, hash_password_fast(password));
        if let Some(cached) = self.cache.get(&cache_key) {
            trace!(
                target: "auth",
                username = %username,
                cache_hit = true,
                valid = cached == Cached::Valid,
                "Authentication cache lookup"
            );
            return match cached {
                Cached::Valid => AuthOutcome::Verified,
                Cached::Invalid => AuthOutcome::InvalidCredentials,
            };
        }
        trace!(
            target: "auth",
            username = %username,
            "Cache miss - verifying credentials"
        );

        let ticket = self.ticket.fetch_add(1, Ordering::SeqCst);

        let outcome = self.warm_verify(username, password).await;

        match outcome {
            AuthOutcome::Verified => {
                // Commit only if no newer verification for this user has
                // already been committed; otherwise this stale positive could
                // resurrect an old password that a newer login evicted.
                if self.commit_result(username, ticket, || {
                    self.invalidate_other_passwords_for_user(username, &cache_key);
                    self.cache.insert(cache_key, Cached::Valid);
                }) {
                    debug!(
                        target: "auth",
                        username = %username,
                        "Authentication result cached (valid)"
                    );
                } else {
                    debug!(
                        target: "auth",
                        username = %username,
                        "Skipped caching a superseded valid result"
                    );
                }
            }
            AuthOutcome::InvalidCredentials => {
                if self.negative_ttl.is_zero() {
                    debug!(
                        target: "auth",
                        username = %username,
                        "Negative caching disabled; rejection not cached"
                    );
                } else if self.commit_result(username, ticket, || {
                    self.cache.insert(cache_key, Cached::Invalid);
                }) {
                    debug!(
                        target: "auth",
                        username = %username,
                        negative_ttl_secs = self.negative_ttl.as_secs(),
                        "Authentication result cached (invalid)"
                    );
                } else {
                    debug!(
                        target: "auth",
                        username = %username,
                        "Skipped caching a superseded invalid result"
                    );
                }
            }
            AuthOutcome::BackendUnavailable => {
                // Never cache: a transient backend failure must not become a
                // sticky rejection that outlives the outage. The ordering slot
                // is still advanced so a still-running older probe cannot later
                // commit a stale result.
                self.record_committed_generation(username, ticket);
                warn!(
                    target: "auth",
                    username = %username,
                    "Authentication backend unavailable; result not cached"
                );
            }
        }
        outcome
    }

    /// Run `apply` and commit `ticket` as this user's newest finished
    /// verification, but only if no newer verification has already committed.
    /// Returns `false` when a newer verification won the race, in which case
    /// the result must be discarded (the newer one owns the cache state).
    fn commit_result<F: FnOnce()>(&self, username: &str, ticket: u64, apply: F) -> bool {
        // Serialize the whole check-and-apply under a per-verifier mutex. The
        // `apply` bodies are pure in-memory cache edits with no `.await`, so
        // this is a short, uncontended critical section.
        let _guard = self.commit_lock.lock().unwrap_or_else(|e| e.into_inner());
        if self
            .committed_generation
            .get(username)
            .is_some_and(|g| g > ticket)
        {
            return false;
        }
        apply();
        self.record_committed_generation_locked(username, ticket);
        true
    }

    /// Record `ticket` as the newest committed generation for `username`,
    /// without the commit mutex (for the non-caching backend-outage path).
    fn record_committed_generation(&self, username: &str, ticket: u64) {
        let _guard = self.commit_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.record_committed_generation_locked(username, ticket);
    }

    fn record_committed_generation_locked(&self, username: &str, ticket: u64) {
        if self
            .committed_generation
            .get(username)
            .is_none_or(|g| g < ticket)
        {
            self.committed_generation
                .insert(username.to_string(), ticket);
        }
    }

    /// Drop every cached credential (positive or negative) for `username`.
    ///
    /// Exposed for explicit invalidation; `verify_outcome` also calls the
    /// narrower per-key variant automatically after a successful login.
    pub fn invalidate_user(&self, username: &str) {
        let prefix = format!("{}:", username);
        let mut removed = 0u32;
        for entry in self.cache.iter() {
            let key = entry.0;
            if key.starts_with(&prefix) {
                self.cache.invalidate(key.as_str());
                removed += 1;
            }
        }
        // Explicit invalidation also clears the warm-path state so a credential
        // change or an operator reset immediately re-enables probing even if the
        // circuit had opened during an outage, and purges the shared client's
        // session-auth cache so a still-fresh positive there cannot shadow it.
        self.reset_warm_state(username);
        if let Some(client) = &self.jmap_client {
            client.invalidate_user_session_auth(username);
        }
        if removed > 0 {
            debug!(
                target: "auth",
                username = %username,
                evicted = removed,
                "Invalidated cached credentials for user"
            );
        }
    }

    /// Warm-path probe funnel over [`AuthVerifier::verify_backend`] (AUDIT.md §18).
    ///
    /// The per-request Basic-auth probe is the single point of failure for both
    /// clients; this method protects it in three layers before the backend is
    /// touched at all:
    ///
    /// 1. **Circuit breaker** — after `failure_threshold` consecutive backend
    ///    outages the per-user circuit opens for an exponentially growing (and
    ///    capped) backoff window. While open, probes are refused with
    ///    `BackendUnavailable` *without contacting the backend*, so a down
    ///    Stalwart is not hammered by every request from both clients.
    /// 2. **In-flight dedup** — concurrent cold probes for the *same
    ///    credential* collapse onto one backend round-trip (a cold-cache burst
    ///    costs one probe per credential, not one per request). Keying on the
    ///    credential rather than the user is deliberate: two probes for
    ///    *different* passwords of the same user must still run concurrently,
    ///    so the §17 start-order race guard keeps observing genuine overlap.
    /// 3. **Per-user probe rate cap** — even sequential cold probes are spaced
    ///    by at least `probe_interval`, so a flood of requests for a user whose
    ///    credential cache is continuously missing cannot pin the backend.
    ///
    /// The breaker and the rate cap are deliberately **per user**, not global.
    /// A global breaker (or a global rate gate) would be simpler, but a Stalwart
    /// outage is a *backend-wide* condition while a credential is a *per-user*
    /// one; coupling them would let one user's repeatedly-failing credential
    /// refuse a concurrently-succeeding user's valid login, which is a
    /// correctness regression (a legitimate client would be told "backend
    /// unavailable" even though the backend is up and its password is correct).
    /// Trade-off accepted: during a genuine backend outage the aggregate probe
    /// count is `users × failure_threshold` before each circuit opens, bounded
    /// by the concurrent-request ceiling and the in-flight dedup; the
    /// `auth_backend_unavailable_total` / `auth_circuit_open_total` counters give
    /// operators the signal to size `failure_threshold`/`backoff` for their user
    /// population (see AUDIT.md §18).
    ///
    /// Backend *responses* (including a fast rejection) are never gated by the
    /// rate cap: once a probe has run its result is cached, and the rate cap
    /// only throttles consecutive *cold* probes. A `Valid`/`Invalid` result
    /// resets the breaker; a `BackendUnavailable` advances it.
    async fn warm_verify(&self, username: &str, password: &str) -> AuthOutcome {
        // Layer 1: refuse fast while the breaker is open.
        if let Some(until) = self.breaker_until.get(username)
            && Instant::now() < until
        {
            trace!(target: "auth", username = %username, "Auth circuit open; skipping backend probe");
            crate::metrics::app_metrics()
                .backend
                .record_auth_circuit_open(self.backend_label());
            return AuthOutcome::BackendUnavailable;
        }

        let cache_key = format!("{}:{}", username, hash_password_fast(password));

        // Layer 2: serialise concurrent probes for this credential.
        let lock = self
            .inflight
            .entry(cache_key.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;

        // Between `verify_outcome`'s cache check and acquiring the lock a
        // concurrent winner may have cached the result for this exact
        // credential. Re-check under the lock to collapse the burst.
        if let Some(cached) = self.cache.get(&cache_key) {
            let outcome = match cached {
                Cached::Valid => AuthOutcome::Verified,
                Cached::Invalid => AuthOutcome::InvalidCredentials,
            };
            self.release_inflight(&cache_key, &_guard);
            return outcome;
        }

        // Layer 3: per-user probe rate cap.
        {
            let now = Instant::now();
            if let Some(last) = self.probe_gate.get(username)
                && now.duration_since(last) < self.probe_interval
            {
                let wait = self.probe_interval.saturating_sub(now.duration_since(last));
                tokio::time::sleep(wait).await;
                // The winner we were queued behind has almost certainly
                // committed by now; re-check so the cap acts as a dedup barrier
                // instead of forcing a redundant probe.
                if let Some(cached) = self.cache.get(&cache_key) {
                    let outcome = match cached {
                        Cached::Valid => AuthOutcome::Verified,
                        Cached::Invalid => AuthOutcome::InvalidCredentials,
                    };
                    self.release_inflight(&cache_key, &_guard);
                    return outcome;
                }
            }
            self.probe_gate.insert(username.to_string(), Instant::now());
        }

        let outcome = self.verify_backend(username, password).await;
        if outcome.is_backend_unavailable() {
            crate::metrics::app_metrics()
                .backend
                .record_auth_backend_unavailable(self.backend_label());
        }
        self.update_breaker(username, outcome);
        self.release_inflight(&cache_key, &_guard);
        outcome
    }

    /// Release this credential's in-flight dedup slot so the map cannot grow
    /// without bound. The slot is removed while the caller still holds the
    /// guard; a waiter that already resolved the same `Arc` keeps its own
    /// reference and, after acquiring, re-checks the credential cache (a hit
    /// for a `Valid`/`Invalid` result), so the removal cannot let a duplicate
    /// probe slip through for a cached outcome.
    fn release_inflight(&self, cache_key: &str, _guard: &tokio::sync::MutexGuard<'_, ()>) {
        self.inflight.remove(cache_key);
    }

    /// Label describing which backend arbitrates authentication, for the
    /// PII-free outage/circuit metrics.
    fn backend_label(&self) -> &'static str {
        if !self.jmap_base.is_empty() {
            "jmap"
        } else if !self.caldav_base.is_empty() {
            "caldav"
        } else {
            "none"
        }
    }

    /// Advance or reset the per-user circuit breaker from a probe result.
    fn update_breaker(&self, username: &str, outcome: AuthOutcome) {
        match outcome {
            AuthOutcome::BackendUnavailable => {
                let streak = self
                    .failure_streak
                    .get(username)
                    .unwrap_or(0)
                    .saturating_add(1);
                self.failure_streak.insert(username.to_string(), streak);
                if streak >= self.failure_threshold {
                    // Exponential backoff on the streak, capped. A test can set
                    // the cap to zero to disable the breaker entirely.
                    let backoff = self.breaker_max_backoff;
                    if !backoff.is_zero() {
                        let shift = streak.saturating_sub(self.failure_threshold).min(16);
                        let scaled = backoff.saturating_mul(1u32 << shift);
                        self.breaker_until
                            .insert(username.to_string(), Instant::now() + scaled);
                        warn!(
                            target: "auth",
                            username = %username,
                            streak,
                            backoff_secs = scaled.as_secs(),
                            "Auth circuit opened for user after consecutive backend failures"
                        );
                    }
                }
            }
            AuthOutcome::Verified | AuthOutcome::InvalidCredentials => {
                // Any authoritative answer proves the backend is reachable.
                self.failure_streak.invalidate(username);
                self.breaker_until.invalidate(username);
            }
        }
    }

    /// Whether the per-user auth circuit is currently open (backend probes are
    /// being refused without contacting Stalwart). Exposed for observability
    /// and tests.
    pub fn circuit_open(&self, username: &str) -> bool {
        self.breaker_until
            .get(username)
            .is_some_and(|until| Instant::now() < until)
    }

    /// Drop every piece of per-user warm-path state (breaker, streak, probe
    /// gate). Exposed so an operator action (e.g. a credential change) can
    /// immediately re-enable probing.
    pub fn reset_warm_state(&self, username: &str) {
        self.failure_streak.invalidate(username);
        self.breaker_until.invalidate(username);
        self.probe_gate.invalidate(username);
    }

    /// Probe the configured backends, collapsing their tri-state results into
    /// an [`AuthOutcome`].
    async fn verify_backend(&self, username: &str, password: &str) -> AuthOutcome {
        let jmap_configured = !self.jmap_base.is_empty();
        let caldav_configured = !self.caldav_base.is_empty();

        if jmap_configured {
            let secret = SecretString::from(password.to_string());
            // Prefer the shared application JMAP client so its session-auth
            // cache and connection pool serve every request; fall back to a
            // throwaway client for standalone (test) constructions.
            let check = match &self.jmap_client {
                Some(client) => client.probe_auth(username, &secret).await,
                None => match JmapClient::new(&self.jmap_base) {
                    Ok(client) => client.probe_auth(username, &secret).await,
                    Err(e) => CredentialCheck::BackendError(format!(
                        "Failed to create JMAP client: {}",
                        e
                    )),
                },
            };
            match check {
                CredentialCheck::Valid => {
                    debug!(target: "auth", username = %username, "Authenticated via JMAP");
                    return AuthOutcome::Verified;
                }
                CredentialCheck::Invalid => {
                    // JMAP authoritatively rejected the credentials. Only
                    // consult CalDAV to accommodate a deployment where JMAP is
                    // present but CalDAV is the real arbiter; a CalDAV backend
                    // error here must not undo JMAP's definitive negative.
                    debug!(
                        target: "auth",
                        username = %username,
                        "JMAP rejected credentials"
                    );
                    if caldav_configured {
                        return match self.check_caldav(username, password).await {
                            CredentialCheck::Valid => AuthOutcome::Verified,
                            CredentialCheck::Invalid | CredentialCheck::BackendError(_) => {
                                AuthOutcome::InvalidCredentials
                            }
                        };
                    }
                    return AuthOutcome::InvalidCredentials;
                }
                CredentialCheck::BackendError(e) => {
                    // A JMAP hiccup (5xx / transport) must NOT fall through to
                    // CalDAV: that would hammer CalDAV for every request and
                    // amplify a Stalwart outage into platform-wide load. Fail
                    // closed as backend-unavailable so the caller returns a
                    // retryable 5xx rather than a password-prompting 401.
                    warn!(
                        target: "auth",
                        username = %username,
                        error = %e,
                        "JMAP auth backend error; not falling back to CalDAV"
                    );
                    return AuthOutcome::BackendUnavailable;
                }
            }
        }

        if caldav_configured {
            return match self.check_caldav(username, password).await {
                CredentialCheck::Valid => AuthOutcome::Verified,
                CredentialCheck::Invalid => AuthOutcome::InvalidCredentials,
                CredentialCheck::BackendError(e) => {
                    warn!(
                        target: "auth",
                        username = %username,
                        error = %e,
                        "CalDAV auth backend error"
                    );
                    AuthOutcome::BackendUnavailable
                }
            };
        }

        warn!(
            target: "auth",
            username = %username,
            "No JMAP or CalDAV backend configured for authentication"
        );
        AuthOutcome::BackendUnavailable
    }

    /// Verify credentials via CalDAV PROPFIND, preserving the 401/5xx
    /// distinction.
    async fn check_caldav(&self, username: &str, password: &str) -> CredentialCheck {
        let caldav = match CaldavClient::new_from_base(&self.caldav_base) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    target: "auth",
                    username = %username,
                    error = %e,
                    "Failed to create CalDAV client for auth"
                );
                return CredentialCheck::BackendError(format!(
                    "Failed to create CalDAV client: {}",
                    e
                ));
            }
        };
        caldav.check_credentials(username, password).await
    }

    /// Evict every cached credential for `username` other than `keep_key`.
    ///
    /// Called only after a *successful* verification, so an unauthenticated
    /// caller can never use it to clear another user's cached session.
    fn invalidate_other_passwords_for_user(&self, username: &str, keep_key: &str) {
        let prefix = format!("{}:", username);
        let mut removed = 0u32;
        for entry in self.cache.iter() {
            let key = entry.0;
            if key.starts_with(&prefix) && key.as_str() != keep_key {
                self.cache.invalidate(key.as_str());
                removed += 1;
            }
        }
        if removed > 0 {
            debug!(
                target: "auth",
                username = %username,
                evicted = removed,
                "Evicted stale cached credentials after successful login"
            );
        }
    }
}

fn hash_password_fast(password: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(password.as_bytes());
    const_hex::encode(h.finalize())
}

/// Deterministic per-key jitter in `[0, span]`.
///
/// Deriving the offset from a hash of the cache key (which already includes a
/// password hash) spreads negative-entry expiry across keys without an RNG and
/// without exposing anything about the password.
fn jitter_for(key: &str, span: Duration) -> Duration {
    if span.is_zero() {
        return Duration::ZERO;
    }
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(key.as_bytes());
    let digest = h.finalize();
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&digest[..8]);
    let n = u64::from_le_bytes(buf);
    let max = span.as_millis().min(u128::from(u64::MAX)) as u64;
    if max == 0 {
        Duration::ZERO
    } else {
        // `max + 1` can overflow only if span is ~584 million years; guard it.
        Duration::from_millis(n % max.saturating_add(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};

    /// A stub backend that serves JMAP `GET /session` with a mutable status
    /// and CalDAV `PROPFIND` with a mutable status, counting each request.
    struct BackendStub {
        base: String,
        jmap_status: Arc<AtomicU16>,
        jmap_hits: Arc<AtomicU32>,
        caldav_status: Arc<AtomicU16>,
        caldav_hits: Arc<AtomicU32>,
        /// When armed, the JMAP `/session` handler blocks until `jmap_gate`
        /// is notified. Lets a test hold one in-flight verification open while
        /// a newer request for the same user completes, proving the ordering
        /// guard.
        jmap_hold: Arc<AtomicBool>,
        jmap_gate: Arc<tokio::sync::Notify>,
    }

    impl BackendStub {
        async fn spawn() -> Self {
            use axum::response::Response;
            use axum::routing::{any, get};

            #[derive(Clone)]
            struct Ctl {
                status: Arc<AtomicU16>,
                hits: Arc<AtomicU32>,
                hold: Option<Arc<AtomicBool>>,
                gate: Option<Arc<tokio::sync::Notify>>,
            }

            async fn handler(
                axum::Extension(ctl): axum::Extension<Ctl>,
                _method: axum::http::Method,
            ) -> Response {
                ctl.hits.fetch_add(1, Ordering::SeqCst);
                if let (Some(hold), Some(gate)) = (&ctl.hold, &ctl.gate)
                    && hold.load(Ordering::SeqCst)
                {
                    gate.notified().await;
                }
                let status = ctl.status.load(Ordering::SeqCst);
                Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "apiUrl": "http://stub/api",
                            "downloadUrl": "http://stub/d/{accountId}/{blobId}",
                            "uploadUrl": "http://stub/u/{accountId}",
                            "eventSourceUrl": "http://stub/e",
                            "state": "s",
                            "username": "u",
                            "accounts": {},
                            "primaryAccounts": {},
                            "capabilities": {}
                        }))
                        .unwrap(),
                    ))
                    .unwrap()
            }

            let jmap_status = Arc::new(AtomicU16::new(200));
            let jmap_hits = Arc::new(AtomicU32::new(0));
            let caldav_status = Arc::new(AtomicU16::new(200));
            let caldav_hits = Arc::new(AtomicU32::new(0));
            let jmap_hold = Arc::new(AtomicBool::new(false));
            let jmap_gate = Arc::new(tokio::sync::Notify::new());

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let base = format!("http://127.0.0.1:{port}");

            let jctl = Ctl {
                status: jmap_status.clone(),
                hits: jmap_hits.clone(),
                hold: Some(jmap_hold.clone()),
                gate: Some(jmap_gate.clone()),
            };
            let cctl = Ctl {
                status: caldav_status.clone(),
                hits: caldav_hits.clone(),
                hold: None,
                gate: None,
            };
            let app = axum::Router::new()
                .route("/session", get(handler).layer(axum::Extension(jctl)))
                .route("/cal/{user}/", any(handler).layer(axum::Extension(cctl)));
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });

            Self {
                base,
                jmap_status,
                jmap_hits,
                caldav_status,
                caldav_hits,
                jmap_hold,
                jmap_gate,
            }
        }

        fn cfg(&self, negative_ttl_secs: u64) -> Config {
            Config {
                jmap_base: self.base.clone(),
                caldav_base: self.base.clone(),
                auth_cache_negative_ttl_secs: negative_ttl_secs,
                auth_cache_ttl_secs: 300,
                // No inter-probe spacing so the existing timing tests stay fast
                // and deterministic; the §18 warm-path tests set it explicitly.
                auth_probe_min_interval_ms: 0,
                ..Default::default()
            }
        }

        fn cfg_jmap_only(&self, negative_ttl_secs: u64) -> Config {
            Config {
                jmap_base: self.base.clone(),
                caldav_base: String::new(),
                auth_cache_negative_ttl_secs: negative_ttl_secs,
                auth_cache_ttl_secs: 300,
                auth_probe_min_interval_ms: 0,
                ..Default::default()
            }
        }
    }

    #[tokio::test]
    async fn verify_outcome_valid_is_cached_for_positive_ttl() {
        let stub = BackendStub::spawn().await;
        let verifier = AuthVerifier::new(&stub.cfg_jmap_only(30));
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::Verified
        );
        assert_eq!(stub.jmap_hits.load(Ordering::SeqCst), 1);
        // Flip the backend to reject; the still-fresh positive entry must win.
        stub.jmap_status.store(401, Ordering::SeqCst);
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::Verified
        );
        assert_eq!(
            stub.jmap_hits.load(Ordering::SeqCst),
            1,
            "second call must be served from the positive cache"
        );
    }

    #[tokio::test]
    async fn verify_outcome_invalid_is_negatively_cached() {
        let stub = BackendStub::spawn().await;
        stub.jmap_status.store(401, Ordering::SeqCst);
        let verifier = AuthVerifier::new(&stub.cfg_jmap_only(30));
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::InvalidCredentials
        );
        assert_eq!(stub.jmap_hits.load(Ordering::SeqCst), 1);
        // Even if the password would now be accepted, the negative entry
        // short-circuits within its TTL.
        stub.jmap_status.store(200, Ordering::SeqCst);
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::InvalidCredentials
        );
        assert_eq!(stub.jmap_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn verify_outcome_backend_error_is_not_cached() {
        let stub = BackendStub::spawn().await;
        stub.jmap_status.store(503, Ordering::SeqCst);
        let verifier = AuthVerifier::new(&stub.cfg_jmap_only(30));
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::BackendUnavailable
        );
        // The backend recovers; because the outage was not cached, the very
        // next call succeeds.
        stub.jmap_status.store(200, Ordering::SeqCst);
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::Verified
        );
        assert_eq!(stub.jmap_hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn jmap_backend_error_does_not_fall_back_to_caldav() {
        let stub = BackendStub::spawn().await;
        stub.jmap_status.store(500, Ordering::SeqCst);
        let verifier = AuthVerifier::new(&stub.cfg_jmap_only(30));
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::BackendUnavailable
        );
        assert_eq!(
            stub.caldav_hits.load(Ordering::SeqCst),
            0,
            "a 5xx from JMAP must not hammer CalDAV (AUDIT.md §17)"
        );
    }

    #[tokio::test]
    async fn jmap_rejection_consults_caldav_for_arbitration() {
        let stub = BackendStub::spawn().await;
        stub.jmap_status.store(401, Ordering::SeqCst);
        stub.caldav_status.store(200, Ordering::SeqCst);
        let verifier = AuthVerifier::new(&stub.cfg(30));
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::Verified
        );
        assert_eq!(stub.caldav_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn jmap_rejection_with_caldav_backend_error_stays_invalid() {
        let stub = BackendStub::spawn().await;
        stub.jmap_status.store(401, Ordering::SeqCst);
        stub.caldav_status.store(503, Ordering::SeqCst);
        let verifier = AuthVerifier::new(&stub.cfg(30));
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::InvalidCredentials,
            "a CalDAV backend error must not undo JMAP's authoritative rejection"
        );
    }

    #[tokio::test]
    async fn caldav_only_backend_error_is_backend_unavailable() {
        let stub = BackendStub::spawn().await;
        stub.caldav_status.store(500, Ordering::SeqCst);
        let mut cfg = stub.cfg(30);
        cfg.jmap_base = String::new();
        let verifier = AuthVerifier::new(&cfg);
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::BackendUnavailable
        );
    }

    #[tokio::test]
    async fn caldav_only_rejection_is_invalid() {
        let stub = BackendStub::spawn().await;
        stub.caldav_status.store(401, Ordering::SeqCst);
        let mut cfg = stub.cfg(30);
        cfg.jmap_base = String::new();
        let verifier = AuthVerifier::new(&cfg);
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::InvalidCredentials
        );
    }

    #[tokio::test]
    async fn negative_caching_disabled_always_reprobes() {
        let stub = BackendStub::spawn().await;
        stub.jmap_status.store(401, Ordering::SeqCst);
        let verifier = AuthVerifier::new(&stub.cfg_jmap_only(0));
        for _ in 0..3 {
            assert_eq!(
                verifier.verify_outcome("u", "pw").await,
                AuthOutcome::InvalidCredentials
            );
        }
        assert_eq!(
            stub.jmap_hits.load(Ordering::SeqCst),
            3,
            "negative TTL 0 must disable negative caching"
        );
    }

    #[tokio::test]
    async fn successful_login_evicts_other_cached_credentials_for_user() {
        let stub = BackendStub::spawn().await;
        let verifier = AuthVerifier::new(&stub.cfg_jmap_only(300));

        // Cache "old" as valid.
        assert_eq!(
            verifier.verify_outcome("u", "old").await,
            AuthOutcome::Verified
        );
        let hits_after_old = stub.jmap_hits.load(Ordering::SeqCst);

        // A successful login with the new password must evict the old entry.
        assert_eq!(
            verifier.verify_outcome("u", "new").await,
            AuthOutcome::Verified
        );
        assert_eq!(
            stub.jmap_hits.load(Ordering::SeqCst),
            hits_after_old + 1,
            "the new password is probed once"
        );

        // Now the old password must be re-probed (proving its entry was
        // evicted) rather than served from cache.
        assert_eq!(
            verifier.verify_outcome("u", "old").await,
            AuthOutcome::Verified,
            "old password is re-probed after eviction"
        );
        assert_eq!(
            stub.jmap_hits.load(Ordering::SeqCst),
            hits_after_old + 2,
            "old entry was evicted and re-verified"
        );
    }

    #[tokio::test]
    async fn invalidate_user_clears_positive_and_negative_entries() {
        let stub = BackendStub::spawn().await;
        let verifier = AuthVerifier::new(&stub.cfg_jmap_only(300));

        assert_eq!(
            verifier.verify_outcome("u", "good").await,
            AuthOutcome::Verified
        );
        stub.jmap_status.store(401, Ordering::SeqCst);
        assert_eq!(
            verifier.verify_outcome("u", "bad").await,
            AuthOutcome::InvalidCredentials
        );
        let hits = stub.jmap_hits.load(Ordering::SeqCst);

        verifier.invalidate_user("u");

        // Both entries are gone: each credential is re-probed.
        assert_eq!(
            verifier.verify_outcome("u", "good").await,
            AuthOutcome::InvalidCredentials
        );
        assert_eq!(
            verifier.verify_outcome("u", "bad").await,
            AuthOutcome::InvalidCredentials
        );
        assert_eq!(stub.jmap_hits.load(Ordering::SeqCst), hits + 2);
    }

    #[tokio::test]
    async fn invalidate_user_does_not_touch_other_users() {
        let stub = BackendStub::spawn().await;
        let verifier = AuthVerifier::new(&stub.cfg_jmap_only(300));
        assert_eq!(
            verifier.verify_outcome("alice", "pw").await,
            AuthOutcome::Verified
        );
        let hits = stub.jmap_hits.load(Ordering::SeqCst);
        verifier.invalidate_user("bob");
        assert_eq!(
            verifier.verify_outcome("alice", "pw").await,
            AuthOutcome::Verified
        );
        assert_eq!(
            stub.jmap_hits.load(Ordering::SeqCst),
            hits,
            "alice's cached entry must survive bob's invalidation"
        );
    }

    #[tokio::test]
    async fn verify_bool_wrapper_treats_backend_outage_as_false() {
        let stub = BackendStub::spawn().await;
        stub.jmap_status.store(503, Ordering::SeqCst);
        let verifier = AuthVerifier::new(&stub.cfg_jmap_only(30));
        assert!(!verifier.verify("u", "pw").await);
    }

    #[tokio::test]
    async fn empty_credentials_are_rejected_without_a_probe() {
        let stub = BackendStub::spawn().await;
        let verifier = AuthVerifier::new(&stub.cfg_jmap_only(30));
        assert_eq!(
            verifier.verify_outcome("", "pw").await,
            AuthOutcome::InvalidCredentials
        );
        assert_eq!(
            verifier.verify_outcome("u", "").await,
            AuthOutcome::InvalidCredentials
        );
        assert_eq!(
            stub.jmap_hits.load(Ordering::SeqCst),
            0,
            "empty credentials must not reach the backend"
        );
    }

    #[tokio::test]
    async fn no_backend_configured_is_backend_unavailable() {
        let cfg = Config {
            jmap_base: String::new(),
            caldav_base: String::new(),
            ..Default::default()
        };
        let verifier = AuthVerifier::new(&cfg);
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::BackendUnavailable
        );
    }

    #[test]
    fn jitter_is_deterministic_and_bounded() {
        let span = Duration::from_secs(10);
        let a = jitter_for("user:abc", span);
        let b = jitter_for("user:abc", span);
        assert_eq!(a, b, "jitter must be deterministic per key");
        assert!(a <= span, "jitter must not exceed the span");
        assert!(a < span, "a hash modulo span is below the span");
        assert_eq!(jitter_for("anything", Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn jitter_spreads_distinct_keys() {
        let span = Duration::from_secs(10);
        let values: std::collections::HashSet<u64> = (0..32)
            .map(|i| jitter_for(&format!("user:{i}"), span).as_millis() as u64)
            .collect();
        assert!(
            values.len() > 1,
            "jitter must vary across keys, got a single value"
        );
    }

    #[tokio::test]
    async fn negative_entry_expires_within_its_short_ttl() {
        let stub = BackendStub::spawn().await;
        stub.jmap_status.store(401, Ordering::SeqCst);
        let verifier = AuthVerifier::new(&stub.cfg_jmap_only(1));
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::InvalidCredentials
        );
        assert_eq!(stub.jmap_hits.load(Ordering::SeqCst), 1);
        // Wait out the 1s negative TTL plus its jitter (≤0.25s).
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::InvalidCredentials
        );
        assert_eq!(
            stub.jmap_hits.load(Ordering::SeqCst),
            2,
            "the negative entry must expire and re-probe"
        );
    }

    /// A slower in-flight verification of an OLD password that *started*
    /// before a newer login must not repopulate the cache after the newer
    /// login has committed and evicted it.
    #[tokio::test]
    async fn slow_older_verification_cannot_repopulate_cache_after_newer_login() {
        let stub = BackendStub::spawn().await;
        let verifier = Arc::new(AuthVerifier::new(&stub.cfg_jmap_only(30)));

        // The FIRST (older) request blocks in the backend.
        stub.jmap_hold.store(true, Ordering::SeqCst);
        let old = {
            let verifier = Arc::clone(&verifier);
            tokio::spawn(async move { verifier.verify_outcome("u", "old-pw").await })
        };
        // Wait until it is actually in flight (its probe has started).
        while stub.jmap_hits.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }

        // The SECOND (newer) request starts after, but completes first.
        stub.jmap_hold.store(false, Ordering::SeqCst);
        let new = verifier.verify_outcome("u", "new-pw").await;
        assert_eq!(new, AuthOutcome::Verified);

        // Let the older request finish; it resumed AFTER the newer committed.
        // `notify_one` stores a permit if the waiter has not yet registered, so
        // this cannot be lost to a scheduling window.
        stub.jmap_gate.notify_one();
        let old_result = old.await.unwrap();
        assert_eq!(old_result, AuthOutcome::Verified);

        // The stale positive for the old password must NOT have been
        // re-inserted (the newer login evicted it), so the old password
        // re-probes and is rejected once the backend is set to reject.
        stub.jmap_status.store(401, Ordering::SeqCst);
        let old_hits_before = stub.jmap_hits.load(Ordering::SeqCst);
        assert_eq!(
            verifier.verify_outcome("u", "old-pw").await,
            AuthOutcome::InvalidCredentials,
            "the superseded old-password result must not be cached"
        );
        assert_eq!(
            stub.jmap_hits.load(Ordering::SeqCst),
            old_hits_before + 1,
            "old password must re-probe, proving nothing was cached for it"
        );

        // The newer, winning password is still cached (no extra probe).
        let hits = stub.jmap_hits.load(Ordering::SeqCst);
        assert_eq!(
            verifier.verify_outcome("u", "new-pw").await,
            AuthOutcome::Verified
        );
        assert_eq!(stub.jmap_hits.load(Ordering::SeqCst), hits);
    }

    // ---- AUDIT.md §18: warm-path funnel (dedup / rate cap / breaker) ----

    fn cfg_warm(base: &str, threshold: u32, backoff_secs: u64, interval_ms: u64) -> Config {
        Config {
            jmap_base: base.to_string(),
            caldav_base: String::new(),
            auth_cache_ttl_secs: 300,
            auth_cache_negative_ttl_secs: 0,
            auth_probe_min_interval_ms: interval_ms,
            auth_backend_failure_threshold: threshold,
            auth_backend_max_backoff_secs: backoff_secs,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn circuit_breaker_opens_after_threshold_and_skips_backend() {
        let stub = BackendStub::spawn().await;
        stub.jmap_status.store(500, Ordering::SeqCst);
        let verifier = AuthVerifier::new(&cfg_warm(&stub.base, 2, 60, 0));

        // Two consecutive outages reach the backend...
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::BackendUnavailable
        );
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::BackendUnavailable
        );
        assert!(
            verifier.circuit_open("u"),
            "breaker must open at the threshold"
        );
        let hits = stub.jmap_hits.load(Ordering::SeqCst);
        assert_eq!(hits, 2, "exactly the threshold reached the backend");

        // ...the next request is refused WITHOUT touching the backend.
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::BackendUnavailable
        );
        assert_eq!(
            stub.jmap_hits.load(Ordering::SeqCst),
            hits,
            "an open circuit must not probe the backend"
        );
    }

    #[tokio::test]
    async fn circuit_breaker_resets_on_authoritative_answer() {
        let stub = BackendStub::spawn().await;
        stub.jmap_status.store(503, Ordering::SeqCst);
        let verifier = AuthVerifier::new(&cfg_warm(&stub.base, 1, 60, 0));

        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::BackendUnavailable
        );
        assert!(verifier.circuit_open("u"));

        // An explicit reset re-enables probing; the backend is now reachable and
        // authoritatively rejects, which closes the circuit.
        verifier.reset_warm_state("u");
        assert!(!verifier.circuit_open("u"));
        stub.jmap_status.store(401, Ordering::SeqCst);
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::InvalidCredentials
        );
        assert!(!verifier.circuit_open("u"), "an answer closes the circuit");
    }

    #[tokio::test]
    async fn zero_backoff_disables_the_breaker() {
        let stub = BackendStub::spawn().await;
        stub.jmap_status.store(500, Ordering::SeqCst);
        // threshold 1, backoff cap 0 → the breaker never opens.
        let verifier = AuthVerifier::new(&cfg_warm(&stub.base, 1, 0, 0));

        for _ in 0..3 {
            assert_eq!(
                verifier.verify_outcome("u", "pw").await,
                AuthOutcome::BackendUnavailable
            );
        }
        assert!(
            !verifier.circuit_open("u"),
            "a zero cap disables the breaker"
        );
        assert_eq!(
            stub.jmap_hits.load(Ordering::SeqCst),
            3,
            "with the breaker disabled every probe still reaches the backend"
        );
    }

    #[tokio::test]
    async fn concurrent_same_credential_burst_probes_backend_once() {
        let stub = BackendStub::spawn().await;
        let verifier = Arc::new(AuthVerifier::new(&cfg_warm(&stub.base, 5, 60, 0)));

        // Hold the single probe open, launch a burst, then release it.
        stub.jmap_hold.store(true, Ordering::SeqCst);
        let mut handles = Vec::new();
        for _ in 0..5 {
            let verifier = Arc::clone(&verifier);
            handles.push(tokio::spawn(async move {
                verifier.verify_outcome("u", "pw").await
            }));
        }
        // Wait for the one probe to be in flight.
        while stub.jmap_hits.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        stub.jmap_gate.notify_waiters();
        for h in handles {
            assert_eq!(h.await.unwrap(), AuthOutcome::Verified);
        }
        assert_eq!(
            stub.jmap_hits.load(Ordering::SeqCst),
            1,
            "a concurrent burst for one credential must cost a single probe"
        );
    }

    #[tokio::test]
    async fn per_user_rate_cap_spaces_sequential_cold_probes() {
        let stub = BackendStub::spawn().await;
        // Authoritative rejections, negative caching disabled, and a 200ms cap:
        // every call is cold and must be spaced by the cap.
        stub.jmap_status.store(401, Ordering::SeqCst);
        let verifier = AuthVerifier::new(&cfg_warm(&stub.base, 5, 60, 200));

        let start = Instant::now();
        for _ in 0..3 {
            assert_eq!(
                verifier.verify_outcome("u", "pw").await,
                AuthOutcome::InvalidCredentials
            );
        }
        let elapsed = start.elapsed();
        assert_eq!(
            stub.jmap_hits.load(Ordering::SeqCst),
            3,
            "each cold probe reaches the backend exactly once"
        );
        assert!(
            elapsed >= Duration::from_millis(360),
            "the rate cap must space two 200ms waits between three probes, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn shared_jmap_client_session_cache_backs_the_verifier() {
        // The verifier consults the shared client's session-auth cache: two
        // verifications of the same credential cost a single backend probe.
        let stub = BackendStub::spawn().await;
        let client = Arc::new(JmapClient::new(&stub.base).unwrap());
        let verifier =
            AuthVerifier::new_from_topology(&cfg_warm(&stub.base, 5, 60, 0), Some(client));

        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::Verified
        );
        // Second verification is served from the verifier's own credential
        // cache (a third call cannot be reached without a fresh verifier), so
        // the backend is hit exactly once here.
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::Verified
        );
        assert_eq!(stub.jmap_hits.load(Ordering::SeqCst), 1);

        // An explicit invalidation must purge BOTH the verifier cache and the
        // shared client's session-auth cache, so the next verification really
        // re-probes the backend instead of being shadowed by a stale positive.
        verifier.invalidate_user("u");
        assert_eq!(
            verifier.verify_outcome("u", "pw").await,
            AuthOutcome::Verified
        );
        assert_eq!(
            stub.jmap_hits.load(Ordering::SeqCst),
            2,
            "invalidate_user must purge the shared session-auth cache too"
        );
    }
}

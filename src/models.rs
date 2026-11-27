// src/models.rs
use crate::attachment::AttachmentManager;
use crate::auth::AuthVerifier;
use crate::caldav::CaldavClient;
use crate::carddav::CarddavClient;
use crate::config::Config;
use crate::directory::{self, DirectoryLookup};
use crate::ews::EwsPushNotifier;
use crate::jmap::JmapClient;
use crate::mapi::handler::MapiState;
use crate::metrics::AppMetrics;
use crate::notifications::{PushNotifier, SubscriptionManager};
use crate::oof::{self, OofManager};
use crate::room::RoomManager;
use crate::smtp::SmtpClient;
use crate::storage::Storage;
use governor::{Quota, RateLimiter, clock::DefaultClock, state::keyed::DefaultKeyedStateStore};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Config,
    pub storage: Arc<Storage>,
    pub attachment_manager: Arc<AttachmentManager>,
    pub room_manager: Arc<RoomManager>,
    pub auth_verifier: Arc<AuthVerifier>,
    /// Directory service for GAL/ResolveNames (None if not configured)
    pub directory: Option<Arc<dyn DirectoryLookup>>,
    /// OOF manager for Out of Office settings (None if not configured)
    pub oof_manager: Option<Arc<dyn OofManager>>,
    /// Notification subscription manager for streaming notifications
    pub subscription_manager: Arc<SubscriptionManager>,
    /// SMTP client for sending email (None if email is disabled or SMTP not configured)
    pub smtp_client: Option<Arc<SmtpClient>>,
    /// JMAP client for reading/syncing email (None if email is disabled or JMAP not configured)
    pub jmap_client: Option<Arc<JmapClient>>,
    /// CardDAV client for contacts sync (None if CardDAV not configured)
    pub carddav_client: Option<Arc<CarddavClient>>,
    /// CalDAV client shared by every calendar/task/notes path: EAS and EWS
    /// calendar sync, the Tasks/Notes mirror, the MAPI calendar contents table
    /// and the readiness probe. Each of those used to build its own
    /// `CaldavClient` per request, so every calendar Sync paid a fresh TCP+TLS
    /// handshake to Stalwart and threw its connection pool away afterwards.
    /// `None` when the client could not be built at startup (see
    /// [`AppState::caldav`]).
    pub caldav_client: Option<Arc<CaldavClient>>,
    /// Why `caldav_client` is `None`. Only set when the startup construction
    /// failed, so the per-request call sites can log the original cause instead
    /// of a generic "unavailable".
    caldav_client_error: Option<Arc<str>>,
    /// Application metrics collector.
    pub metrics: Arc<AppMetrics>,
    /// Per-client rate limiter, keyed by the edge-reported client IP
    /// (`CF-Connecting-IP`, `Forwarded`, then first `X-Forwarded-For`),
    /// with a shared global fallback bucket. Keying is deliberate: a single
    /// global bucket would let the target clients throttle each other
    /// behind Cloudflare/Microsoft datacenter IPs, while a username key
    /// cannot be used here because the middleware runs pre-authentication
    /// and the raw Basic header is attacker-controlled. None if disabled.
    ///
    /// Stale keys are evicted by a periodic `retain_recent` sweep spawned at
    /// construction, so the keyed store cannot grow without bound.
    pub rate_limiter:
        Option<Arc<RateLimiter<String, DefaultKeyedStateStore<String>, DefaultClock>>>,
    /// MAPI/HTTP (MS-OXCMAPIHTTP) session state. None if `mapi_enabled` is false.
    pub mapi: Option<Arc<MapiState>>,
    /// Shared registry of live JMAP EventSource push monitors (RFC 8620 §7.3),
    /// deduplicated per mailbox. The MAPI notification path and the EAS Ping
    /// loop both register here so one SSE stream feeds every consumer.
    pub push_registry: Arc<crate::jmap_push::PushMonitorRegistry>,
    /// Runtime gate for JMAP calendar *writes*. The deploy-time capability
    /// verification (`verify_calendar_capability_at_startup`) clears this when
    /// the backend provably discards `iCalendar` payloads, so create/update
    /// traffic falls back to CalDAV instead of silently losing event data.
    pub jmap_calendar_write_enabled: Arc<std::sync::atomic::AtomicBool>,
    /// Per-(owner, folder) async locks serializing the EWS email delta for one
    /// folder from its replay-cache lookup through its replay-cache write. Two
    /// concurrent `SyncFolderItems` requests carrying the SAME incoming cursor
    /// would otherwise both miss the replay cache, both run the keyword scan,
    /// and the second would overwrite the first's cached response — dropping a
    /// flag-only `<t:Update>` if the first response was lost. The key is
    /// `(owner, folder)`, so the map is bounded by users × folders (the same
    /// order as the other per-user state); entries are left in place so the lock
    /// identity is stable across requests.
    pub ews_delta_locks: Arc<dashmap::DashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl AppState {
    pub fn new(cfg: Config, storage: Arc<Storage>) -> Self {
        let max_attachment_bytes = cfg.max_attachment_bytes();
        let attachment_manager = Arc::new(AttachmentManager::new(
            storage.clone(),
            max_attachment_bytes,
        ));
        let room_manager = Arc::new(RoomManager::new(storage.clone()));

        // JMAP client is constructed before the AuthVerifier so the verifier can
        // share it: credential probes then reuse the client's session-auth cache
        // and connection pool instead of building a throwaway client per probe
        // (AUDIT.md §18).
        //
        // The verifier authenticates through JMAP whenever `jmap_base` is set,
        // regardless of `email_enabled` (an operator can disable the email
        // feature while still authenticating users against Stalwart). Build the
        // shared client whenever `jmap_base` is configured; expose it as the
        // email client only when email is enabled so `can_read_email()` and the
        // FolderSync email-folder gate keep their configured semantics.
        //
        // The client's credential-probe cache TTLs are derived from the
        // auth-cache knobs so it can never extend the documented
        // `GATEWAY_AUTH_CACHE_TTL_SECS` contract, and its capacity is bounded by
        // `GATEWAY_AUTH_CACHE_MAX_ENTRIES` (AUDIT.md §18).
        let auth_jmap_client = if cfg.jmap_base.is_empty() {
            None
        } else {
            let positive_ttl = Duration::from_secs(cfg.auth_cache_ttl_secs.max(30));
            let negative_ttl = Duration::from_secs(cfg.auth_cache_negative_ttl_secs);
            let max_entries = cfg.auth_cache_max_entries.max(1) as u64;
            match JmapClient::with_session_auth(
                &cfg.jmap_base,
                positive_ttl,
                negative_ttl,
                max_entries,
            ) {
                Ok(c) => Some(Arc::new(c)),
                Err(e) => {
                    tracing::warn!(
                        target: "models",
                        jmap_base = %cfg.jmap_base,
                        error = %e,
                        "Failed to create JMAP client; email sync and JMAP auth will be unavailable"
                    );
                    None
                }
            }
        };
        let jmap_client = if cfg.email_enabled {
            auth_jmap_client.clone()
        } else {
            None
        };

        let auth_verifier = Arc::new(AuthVerifier::new_from_topology(
            &cfg,
            auth_jmap_client.clone(),
        ));

        // The directory service (GAL / ResolveNames / OAB / NSPI) reads the
        // Stalwart directory over JMAP (`urn:stalwart:jmap` `x:Account/*` /
        // `x:MailingList/*`) using the administrator account credentials. It is
        // configured when a JMAP endpoint and admin credentials are both present;
        // the deprecated REST admin API is no longer used.
        // The administrator credentials authenticate the server-wide directory
        // and Sieve (OOF) JMAP calls via HTTP Basic. Warn (but do not hard-fail)
        // when that endpoint is cleartext `http://`: the gateway commonly sits on
        // an internal Docker network adjacent to Stalwart where TLS terminates at
        // the edge, but over a non-trusted network this would transmit the admin
        // password unencrypted (CWE-319). Requiring HTTPS unconditionally would
        // break that internal-container topology.
        if !cfg.jmap_base.is_empty()
            && !cfg.admin_username.is_empty()
            && !cfg.admin_password.is_empty()
            && cfg.jmap_base.trim_start().starts_with("http://")
        {
            tracing::warn!(
                target: "directory",
                "JMAP admin credentials may be sent over a cleartext (http://) endpoint; \
                 `jmap_base` should use HTTPS unless Stalwart is on a trusted internal network"
            );
        }

        let directory = if !cfg.jmap_base.is_empty()
            && !cfg.admin_username.is_empty()
            && !cfg.admin_password.is_empty()
        {
            Some(directory::create_directory(
                Some(&cfg.jmap_base),
                Some(&cfg.admin_username),
                Some(&cfg.admin_password),
            ))
        } else {
            None
        };

        let oof_manager = if !cfg.jmap_base.is_empty()
            && !cfg.admin_username.is_empty()
            && !cfg.admin_password.is_empty()
        {
            Some(oof::create_oof_manager(
                Some(&cfg.jmap_base),
                Some(&cfg.admin_username),
                Some(&cfg.admin_password),
                &cfg.mail_domain,
            ))
        } else {
            None
        };

        // Push notifications deliver EWS `SendNotification` SOAP to a client
        // callback URL over HTTP. Build a dedicated client (30s timeout, system
        // TLS, redirects disabled) and install it as the subscription manager's
        // push transport. Disabling redirects is part of the SSRF defence: a
        // redirect target is never followed, so destination validation done at
        // Subscribe time cannot be bypassed. If the client cannot be built, push
        // is left disabled (no notifier) so Subscribe returns a clean
        // "unavailable" response rather than a subtly-broken timeout-less client.
        let subscription_manager = Arc::new(
            match reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
            {
                Ok(client) => {
                    let notifier: Arc<dyn PushNotifier> = Arc::new(EwsPushNotifier::new(client));
                    SubscriptionManager::new().with_push_notifier(notifier)
                }
                Err(e) => {
                    tracing::warn!(
                        target: "models",
                        error = %e,
                        "Failed to create push-notification HTTP client; push subscriptions will be disabled"
                    );
                    SubscriptionManager::new()
                }
            },
        );

        let smtp_client = if cfg.email_enabled && !cfg.smtp_host.is_empty() {
            Some(Arc::new(SmtpClient::new(&cfg.smtp_host, cfg.smtp_port)))
        } else {
            None
        };

        let carddav_client = if !cfg.carddav_base.is_empty() {
            match CarddavClient::new(&cfg) {
                Ok(c) => Some(Arc::new(c)),
                Err(e) => {
                    tracing::warn!(
                        target: "models",
                        carddav_base = %cfg.carddav_base,
                        error = %e,
                        "Failed to create CardDAV client; contacts sync will be unavailable"
                    );
                    None
                }
            }
        } else {
            None
        };

        // One CalDAV client for the whole process. Every calendar/task/notes
        // path used to construct its own, so each calendar Sync opened a fresh
        // TCP+TLS connection to Stalwart and dropped the pool afterwards. The
        // client is stateless apart from that pool (the base URL is fixed at
        // startup and credentials travel per request), so sharing it is safe.
        // It is built even when `caldav_base` is empty, because that is exactly
        // what the old per-request construction did: call sites that need to
        // distinguish "CalDAV not configured" still test `cfg.caldav_base`.
        let (caldav_client, caldav_client_error) = match CaldavClient::new(&cfg) {
            Ok(c) => (Some(Arc::new(c)), None),
            Err(e) => {
                tracing::warn!(
                    target: "models",
                    caldav_base = %cfg.caldav_base,
                    error = %e,
                    "Failed to create CalDAV client; calendar sync will be unavailable"
                );
                (None, Some(Arc::<str>::from(e.to_string())))
            }
        };

        if cfg.email_enabled {
            tracing::info!(
                target: "models",
                smtp_configured = smtp_client.is_some(),
                jmap_configured = jmap_client.is_some(),
                "Email subsystem initialized"
            );
        }

        tracing::info!(
            target: "models",
            directory_available = directory.as_ref().map(|d| d.is_available()).unwrap_or(false),
            "Directory service initialized"
        );

        let metrics = crate::metrics::app_metrics();

        // Initialize rate limiter if enabled
        let rate_limiter = if cfg.rate_limit_enabled {
            let rps = cfg.rate_limit_requests_per_minute as f64 / 60.0;
            let rps_u32 = rps.max(1.0).round() as u32;
            let burst = NonZeroU32::new(cfg.rate_limit_max_concurrent.max(1) as u32).unwrap();
            let quota = Quota::per_second(NonZeroU32::new(rps_u32).unwrap()).allow_burst(burst);
            let limiter = Arc::new(RateLimiter::keyed(quota));
            // The keyed store keeps an entry per distinct client-IP key.
            // Sweep stale entries periodically so rotating source IPs (e.g.
            // from an attacker burning through addresses) cannot grow the
            // map without bound. `retain_recent` drops exactly those keys
            // whose GCRA state is indistinguishable from a fresh bucket, so
            // evicting them cannot alter limiter decisions for future
            // requests; `shrink_to_fit` then returns their capacity.
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let limiter = limiter.clone();
                runtime.spawn(async move {
                    const SWEEP_INTERVAL: Duration = Duration::from_secs(15 * 60);
                    let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
                    ticker.tick().await; // skip the immediate first tick
                    loop {
                        ticker.tick().await;
                        limiter.retain_recent();
                        limiter.shrink_to_fit();
                    }
                });
            }
            Some(limiter)
        } else {
            None
        };

        // One shared JMAP push-monitor registry for every surface that wants
        // live new-mail events (MAPI RopRegisterNotification, EAS Ping), so a
        // single EventSource stream per mailbox feeds all of them.
        let push_registry = Arc::new(crate::jmap_push::PushMonitorRegistry::new());

        // MAPI/HTTP (MS-OXCMAPIHTTP) surface. Constructed only when
        // `mapi_enabled`; the session/logon runtime is in `crate::mapi`.
        // We clone the Config and the shared AuthVerifier — the Config is a
        // cheap Clone (mostly small Strings), and AuthVerifier is `Arc`-shared.
        // The same `Arc<SubscriptionManager>` the EWS path uses is wired in
        // so MAPI property-write ROPs publish `ItemModified` events to the
        // shared feed, closing the EWS-only notification gap (audit §2e).
        let mapi = if cfg.mapi_enabled {
            let mut mapi_state = MapiState::with_subscription_manager(
                cfg.clone(),
                auth_verifier.clone(),
                subscription_manager.clone(),
            )
            .with_attachment_manager(attachment_manager.clone())
            .with_push_registry(push_registry.clone())
            .with_storage(storage.clone());
            // Share the process-wide JMAP client with the MAPI surface.
            // `Execute` carries every MAPI operation, and it used to build a
            // fresh `JmapClient` per RPC, discarding the connection pool, the
            // per-username session cache and the credential-probe cache each
            // time. The ungated client is used (not `jmap_client`) because MAPI
            // only needs JMAP when `jmap_base` is configured, regardless of the
            // `email_enabled` feature flag — the same condition the old
            // per-request construction applied.
            if let Some(client) = &auth_jmap_client {
                mapi_state = mapi_state.with_jmap(client.clone());
            }
            // Share the CalDAV client with the MAPI calendar contents table.
            if let Some(client) = &caldav_client {
                mapi_state = mapi_state.with_caldav(client.clone());
            }
            // Wire the operator-configured directory so the NSPI address-book
            // surface (`/mapi/nspi`) serves a real GAL rather than the
            // caller-only minimal stub (audit gap §2d). When no JMAP-backed
            // directory is configured `directory` is `None` and the NSPI
            // dispatcher itself falls back to the authenticated-self stub.
            if let Some(dir) = &directory {
                mapi_state = mapi_state.with_directory(dir.clone());
            }
            Some(Arc::new(mapi_state))
        } else {
            None
        };

        Self {
            cfg,
            storage,
            attachment_manager,
            room_manager,
            auth_verifier,
            directory,
            oof_manager,
            subscription_manager,
            smtp_client,
            jmap_client,
            carddav_client,
            caldav_client,
            caldav_client_error,
            metrics,
            rate_limiter,
            mapi,
            push_registry,
            jmap_calendar_write_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            ews_delta_locks: Arc::new(dashmap::DashMap::new()),
        }
    }

    /// Whether event create/update traffic may use the JMAP calendar write
    /// path. False only after deploy-time verification proved the backend
    /// discards `iCalendar` payloads (see `GATEWAY_CALENDAR_CAPABILITY_CHECK`).
    pub fn jmap_calendar_writes_enabled(&self) -> bool {
        self.jmap_calendar_write_enabled
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Permanently disable JMAP calendar writes for this process lifetime.
    pub fn disable_jmap_calendar_writes(&self) {
        self.jmap_calendar_write_enabled
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn gateway_host(&self) -> &str {
        &self.cfg.gateway_host
    }

    pub fn caldav_base(&self) -> &str {
        &self.cfg.caldav_base
    }

    /// The process-wide CalDAV client, shared by every calendar/task/notes
    /// path so they reuse one connection pool and one TLS session cache.
    ///
    /// `Err` carries the reason it is unavailable (the startup construction
    /// failure), so call sites can log the cause instead of a generic message.
    /// Note the client exists even when `caldav_base` is empty — the previous
    /// per-request construction behaved the same way, and call sites that need
    /// to distinguish "not configured" test [`AppState::caldav_base`].
    pub fn caldav(&self) -> Result<&Arc<CaldavClient>, &str> {
        match (&self.caldav_client, &self.caldav_client_error) {
            (Some(client), _) => Ok(client),
            (None, Some(reason)) => Err(reason),
            (None, None) => Err("CalDAV client is not available"),
        }
    }

    /// Whether email functionality is available (enabled and configured)
    pub fn email_available(&self) -> bool {
        self.cfg.email_enabled && (self.smtp_client.is_some() || self.jmap_client.is_some())
    }

    /// Whether email sending is available (JMAP submission or SMTP)
    pub fn can_send_email(&self) -> bool {
        self.jmap_client.is_some() || self.smtp_client.is_some()
    }

    /// Whether email reading/syncing is available
    pub fn can_read_email(&self) -> bool {
        self.jmap_client.is_some()
    }

    /// Returns true if contacts can be synced. JMAP Contacts (RFC 9610) is the
    /// primary backend; CardDAV is the fallback, so either client being
    /// configured is sufficient. Tasks and Notes are gateway-local and
    /// therefore always available.
    pub fn can_read_contacts(&self) -> bool {
        self.jmap_client.is_some() || self.carddav_client.is_some()
    }
}

#[derive(Clone, Debug, Default)]
pub struct RequestContext {
    pub request_id: String,
    pub user_email: String,
    pub device_id: Option<String>,
    pub protocol_version: Option<String>,
}

impl RequestContext {
    pub fn new(request_id: String, user_email: String) -> Self {
        Self {
            request_id,
            user_email,
            device_id: None,
            protocol_version: None,
        }
    }

    pub fn with_device_id(mut self, device_id: impl Into<String>) -> Self {
        self.device_id = Some(device_id.into());
        self
    }

    pub fn with_protocol_version(mut self, version: impl Into<String>) -> Self {
        self.protocol_version = Some(version.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;

    async fn state_for(cfg: Config) -> Arc<AppState> {
        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .expect("in-memory storage");
        storage.init_schema().await.expect("schema init");
        Arc::new(AppState::new(cfg, Arc::new(storage)))
    }

    /// AUDIT.md §18: when `jmap_base` is set the verifier must authenticate
    /// through a *shared* JMAP client even if the email feature is disabled, so
    /// credential probes reuse one connection pool and one session-auth cache
    /// instead of building a throwaway client per probe. The email accessor
    /// must still honor `email_enabled`.
    #[tokio::test]
    async fn auth_verifier_shares_jmap_client_when_email_disabled() {
        let cfg = Config {
            jmap_base: "http://127.0.0.1:1".to_string(),
            email_enabled: false,
            mail_domain: "example.com".to_string(),
            hmac_secret: SecretString::from("a".repeat(32)),
            ..Default::default()
        };
        let state = state_for(cfg).await;
        assert!(
            state.auth_verifier.shared_jmap_client().is_some(),
            "verifier must share a JMAP client whenever jmap_base is set"
        );
        assert!(
            state.jmap_client.is_none(),
            "email accessor must stay None while email is disabled"
        );
        assert!(!state.can_read_email());
        assert!(!state.email_available());
    }

    /// With email enabled the shared client is additionally exposed as the
    /// email client, preserving the pre-§18 behavior.
    #[tokio::test]
    async fn auth_verifier_shares_jmap_client_when_email_enabled() {
        let cfg = Config {
            jmap_base: "http://127.0.0.1:1".to_string(),
            email_enabled: true,
            mail_domain: "example.com".to_string(),
            hmac_secret: SecretString::from("a".repeat(32)),
            ..Default::default()
        };
        let state = state_for(cfg).await;
        assert!(state.auth_verifier.shared_jmap_client().is_some());
        assert!(state.jmap_client.is_some());
        assert!(state.can_read_email());
    }

    /// P0-2: the CalDAV client is built ONCE for the whole process. Every
    /// calendar/task/notes path used to construct its own per request, so each
    /// calendar Sync opened a fresh TCP+TLS connection and dropped the pool
    /// afterwards. Two `AppState::caldav()` calls must hand back the SAME
    /// `Arc`, which is only possible if the client is a process-wide field.
    #[tokio::test]
    async fn caldav_client_is_shared_across_the_process() {
        let cfg = Config {
            caldav_base: "http://127.0.0.1:1/dav/cal".to_string(),
            mail_domain: "example.com".to_string(),
            hmac_secret: SecretString::from("a".repeat(32)),
            ..Default::default()
        };
        let state = state_for(cfg).await;
        let first = state.caldav().expect("caldav client must build");
        let second = state.caldav().expect("caldav client must build");
        assert!(
            Arc::ptr_eq(first, second),
            "every caller must share one CalDAV client (one connection pool)"
        );
        assert!(
            state.caldav_client.is_some(),
            "the shared client must be exposed on the state"
        );
    }

    /// P0-1/P0-2: the MAPI surface reuses the SAME clients `AppState` built —
    /// `Execute` used to build a throwaway `JmapClient` per RPC (dropping the
    /// connection pool, the session cache and the credential-probe cache), and
    /// the calendar contents table built a throwaway `CaldavClient`.
    #[tokio::test]
    async fn mapi_state_shares_the_process_wide_backend_clients() {
        let cfg = Config {
            jmap_base: "http://127.0.0.1:1".to_string(),
            caldav_base: "http://127.0.0.1:1/dav/cal".to_string(),
            email_enabled: false,
            mapi_enabled: true,
            mail_domain: "example.com".to_string(),
            hmac_secret: SecretString::from("a".repeat(32)),
            ..Default::default()
        };
        let state = state_for(cfg).await;
        let mapi = state.mapi.as_ref().expect("mapi state must be built");

        let shared_jmap = state
            .auth_verifier
            .shared_jmap_client()
            .expect("ungated JMAP client");
        let mapi_jmap = mapi.jmap.as_ref().expect("MAPI must carry a JMAP client");
        assert!(
            Arc::ptr_eq(mapi_jmap, shared_jmap),
            "MAPI must reuse the process-wide JMAP client, not build its own"
        );

        let shared_caldav = state.caldav().expect("caldav client must build");
        let mapi_caldav = mapi
            .caldav
            .as_ref()
            .expect("MAPI must carry the shared CalDAV client");
        assert!(
            Arc::ptr_eq(mapi_caldav, shared_caldav),
            "MAPI must reuse the process-wide CalDAV client"
        );
    }

    /// A deployment with no CalDAV configured still gets a client object (the
    /// previous per-request construction behaved the same way); call sites that
    /// need "not configured" test `caldav_base` explicitly.
    #[tokio::test]
    async fn caldav_client_exists_without_a_configured_base() {
        let cfg = Config {
            caldav_base: String::new(),
            mail_domain: "example.com".to_string(),
            hmac_secret: SecretString::from("a".repeat(32)),
            ..Default::default()
        };
        let state = state_for(cfg).await;
        assert!(state.caldav().is_ok());
    }

    /// P0-2, measured on the wire: a shared `CaldavClient` reuses ONE TCP
    /// connection for consecutive requests (its `reqwest::Client` pool is the
    /// only state it carries), while two separately-constructed clients open
    /// two. This is the cost the per-request construction paid on every
    /// calendar request: a fresh TCP (and, against an `https` backend, TLS)
    /// handshake to Stalwart plus a discarded pool.
    ///
    /// The stub counts accepted TCP connections, so the assertion is about
    /// actual sockets, not about request counts (which a fresh client would
    /// produce identically).
    #[tokio::test]
    async fn shared_caldav_client_reuses_one_tcp_connection() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        /// Minimal HTTP/1.1 keep-alive responder that counts accepted sockets.
        async fn counting_stub() -> (String, Arc<AtomicUsize>) {
            fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
                haystack.windows(needle.len()).position(|w| w == needle)
            }
            fn content_length(head: &str) -> usize {
                head.lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.trim()
                            .eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0)
            }

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let accepts = Arc::new(AtomicUsize::new(0));
            let counter = accepts.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((mut sock, _)) = listener.accept().await else {
                        return;
                    };
                    counter.fetch_add(1, Ordering::SeqCst);
                    tokio::spawn(async move {
                        let mut buf: Vec<u8> = Vec::new();
                        let mut chunk = [0u8; 4096];
                        loop {
                            let n = match sock.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => n,
                            };
                            buf.extend_from_slice(&chunk[..n]);
                            while let Some(end) = find(&buf, b"\r\n\r\n") {
                                let head = String::from_utf8_lossy(&buf[..end]).to_string();
                                let total = end + 4 + content_length(&head);
                                if buf.len() < total {
                                    break;
                                }
                                buf.drain(..total);
                                let resp = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
                                if sock.write_all(resp.as_bytes()).await.is_err() {
                                    return;
                                }
                            }
                        }
                    });
                }
            });
            (format!("http://{addr}/dav/cal"), accepts)
        }

        let (base, accepts) = counting_stub().await;
        let cfg = Config {
            caldav_base: base.clone(),
            mail_domain: "example.com".to_string(),
            hmac_secret: SecretString::from("a".repeat(32)),
            ..Default::default()
        };
        let state = state_for(cfg.clone()).await;

        let caldav = state.caldav().expect("caldav client must build");
        for _ in 0..2 {
            let _ = caldav.check_credentials("u@example.com", "pw").await;
        }
        assert_eq!(
            accepts.load(Ordering::SeqCst),
            1,
            "the shared client must reuse its pooled TCP connection"
        );

        // Control: two throwaway clients (the pre-fix shape) each open their
        // own connection, so the counter is genuinely observing sockets.
        for _ in 0..2 {
            let fresh = crate::caldav::CaldavClient::new(&cfg).unwrap();
            let _ = fresh.check_credentials("u@example.com", "pw").await;
        }
        assert_eq!(
            accepts.load(Ordering::SeqCst),
            3,
            "a per-request client opens a new connection per request"
        );
    }

    /// No `jmap_base` means no JMAP auth and no email client, regardless of the
    /// email flag.
    #[tokio::test]
    async fn no_shared_client_without_jmap_base() {
        let cfg = Config {
            jmap_base: String::new(),
            email_enabled: true,
            mail_domain: "example.com".to_string(),
            hmac_secret: SecretString::from("a".repeat(32)),
            ..Default::default()
        };
        let state = state_for(cfg).await;
        assert!(state.auth_verifier.shared_jmap_client().is_none());
        assert!(state.jmap_client.is_none());
        assert!(!state.can_read_email());
    }
}

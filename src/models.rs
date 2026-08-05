// src/models.rs
use crate::attachment::AttachmentManager;
use crate::auth::AuthVerifier;
use crate::carddav::CarddavClient;
use crate::config::Config;
use crate::directory::{self, DirectoryLookup};
use crate::jmap::JmapClient;
use crate::mapi::handler::MapiState;
use crate::metrics::AppMetrics;
use crate::notifications::SubscriptionManager;
use crate::oof::{self, OofManager};
use crate::room::RoomManager;
use crate::smtp::SmtpClient;
use crate::storage::Storage;
use governor::{
    Quota, RateLimiter,
    clock::DefaultClock,
    state::{InMemoryState, NotKeyed},
};
use std::num::NonZeroU32;
use std::sync::Arc;

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
    /// Application metrics collector.
    pub metrics: Arc<AppMetrics>,
    /// Global rate limiter to protect against floods. None if rate limiting is disabled.
    pub rate_limiter: Option<Arc<RateLimiter<NotKeyed, InMemoryState, DefaultClock>>>,
    /// MAPI/HTTP (MS-OXCMAPIHTTP) session state. None if `mapi_enabled` is false.
    pub mapi: Option<Arc<MapiState>>,
}

impl AppState {
    pub fn new(cfg: Config, storage: Arc<Storage>) -> Self {
        let max_attachment_bytes = cfg.max_attachment_bytes();
        let attachment_manager = Arc::new(AttachmentManager::new(
            storage.clone(),
            max_attachment_bytes,
        ));
        let room_manager = Arc::new(RoomManager::new(storage.clone()));
        let auth_verifier = Arc::new(AuthVerifier::new(&cfg));

        let directory = if !cfg.admin_base.is_empty() {
            Some(directory::create_directory(
                Some(&cfg.admin_base),
                if cfg.admin_username.is_empty() {
                    None
                } else {
                    Some(&cfg.admin_username)
                },
                if cfg.admin_password.is_empty() {
                    None
                } else {
                    Some(&cfg.admin_password)
                },
            ))
        } else {
            None
        };

        let oof_manager = if !cfg.admin_base.is_empty()
            && !cfg.admin_username.is_empty()
            && !cfg.admin_password.is_empty()
        {
            Some(oof::create_oof_manager(
                Some(&cfg.admin_base),
                Some(&cfg.admin_username),
                Some(&cfg.admin_password),
                &cfg.mail_domain,
            ))
        } else {
            None
        };

        let subscription_manager = Arc::new(SubscriptionManager::new());

        let smtp_client = if cfg.email_enabled && !cfg.smtp_host.is_empty() {
            Some(Arc::new(SmtpClient::new(&cfg.smtp_host, cfg.smtp_port)))
        } else {
            None
        };

        let jmap_client = if cfg.email_enabled && !cfg.jmap_base.is_empty() {
            match JmapClient::new(&cfg.jmap_base) {
                Ok(c) => Some(Arc::new(c)),
                Err(e) => {
                    tracing::warn!(
                        target: "models",
                        jmap_base = %cfg.jmap_base,
                        error = %e,
                        "Failed to create JMAP client; email sync will be unavailable"
                    );
                    None
                }
            }
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

        let metrics = Arc::new(AppMetrics::new());

        // Initialize rate limiter if enabled
        let rate_limiter = if cfg.rate_limit_enabled {
            let rps = cfg.rate_limit_requests_per_minute as f64 / 60.0;
            let rps_u32 = rps.max(1.0).round() as u32;
            let burst = NonZeroU32::new(cfg.rate_limit_max_concurrent.max(1) as u32).unwrap();
            let quota = Quota::per_second(NonZeroU32::new(rps_u32).unwrap()).allow_burst(burst);
            Some(Arc::new(RateLimiter::direct(quota)))
        } else {
            None
        };

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
            .with_attachment_manager(attachment_manager.clone());
            // Wire the operator-configured directory so the NSPI address-book
            // surface (`/mapi/nspi`) serves a real GAL rather than the
            // caller-only minimal stub (audit gap §2d). When no `admin_base`
            // is configured `directory` is `None` and the NSPI dispatcher
            // itself falls back to the authenticated-self stub.
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
            metrics,
            rate_limiter,
            mapi,
        }
    }

    pub fn gateway_host(&self) -> &str {
        &self.cfg.gateway_host
    }

    pub fn caldav_base(&self) -> &str {
        &self.cfg.caldav_base
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

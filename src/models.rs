// src/models.rs
use crate::attachment::AttachmentManager;
use crate::auth::AuthVerifier;
use crate::config::Config;
use crate::jmap::JmapClient;
use crate::room::RoomManager;
use crate::smtp::SmtpClient;
use crate::storage::Storage;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Config,
    pub storage: Arc<Storage>,
    pub attachment_manager: Arc<AttachmentManager>,
    pub room_manager: Arc<RoomManager>,
    pub auth_verifier: Arc<AuthVerifier>,
    /// SMTP client for sending email (None if email is disabled or SMTP not configured)
    pub smtp_client: Option<Arc<SmtpClient>>,
    /// JMAP client for reading/syncing email (None if email is disabled or JMAP not configured)
    pub jmap_client: Option<Arc<JmapClient>>,
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

        if cfg.email_enabled {
            tracing::info!(
                target: "models",
                smtp_configured = smtp_client.is_some(),
                jmap_configured = jmap_client.is_some(),
                "Email subsystem initialized"
            );
        }

        Self {
            cfg,
            storage,
            attachment_manager,
            room_manager,
            auth_verifier,
            smtp_client,
            jmap_client,
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
        self.cfg.email_enabled
            && (self.smtp_client.is_some() || self.jmap_client.is_some())
    }

    /// Whether email sending is available
    pub fn can_send_email(&self) -> bool {
        self.smtp_client.is_some()
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

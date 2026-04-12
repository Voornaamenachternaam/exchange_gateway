// src/models.rs
use crate::config::Config;
use crate::storage::Storage;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Config,
    pub storage: Arc<Storage>,
}

impl AppState {
    pub fn new(cfg: Config, storage: Arc<Storage>) -> Self {
        Self { cfg, storage }
    }

    pub fn gateway_host(&self) -> &str {
        &self.cfg.gateway_host
    }

    pub fn caldav_base(&self) -> &str {
        &self.cfg.caldav_base
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

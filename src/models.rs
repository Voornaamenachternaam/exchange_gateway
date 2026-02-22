// src/models.rs
use crate::config::Config;
use crate::storage::Storage;
use std::sync::Arc;

/// Application state passed to request handlers.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Config,
    pub storage: Arc<Storage>,
}

use crate::config::Config;
use crate::sync::Storage; // Cloudflare storage as Storage
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Config,
    pub storage: Arc<Storage>,
}

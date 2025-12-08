use std::sync::Arc;
use crate::config::Config;
use crate::storage::Storage;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Config,
    pub storage: Arc<Storage>,
}

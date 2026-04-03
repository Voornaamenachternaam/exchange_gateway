use crate::config::Config;
use crate::smtp::SmtpClient;
use crate::storage::Storage;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Config,
    pub storage: Arc<Storage>,
    pub smtp: Option<Arc<SmtpClient>>,
}

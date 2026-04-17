// src/lib.rs
pub mod autodiscover;
pub mod caldav;
pub mod calendar;
pub mod config;
pub mod eas;
pub mod error;
pub mod ews;
pub mod ews_folders;
pub mod ews_update;
pub mod ical_parser;
pub mod meeting;
pub mod models;
pub mod protocol_fixtures;
pub mod storage;
pub mod sync;
pub mod timezone;
pub mod util;
pub mod wbxml;

pub use autodiscover::AutodiscoverJsonParams;
pub use config::Config;
pub use error::{GatewayError, Result};
pub use models::AppState;
pub use storage::Storage;
pub use util::xml_escape;

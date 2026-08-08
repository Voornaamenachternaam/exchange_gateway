// src/lib.rs
pub mod attachment;
pub mod auth;
pub mod autodiscover;
pub mod caldav;
pub mod calendar;
pub mod carddav;
pub mod config;
pub mod contacts;
pub mod delegate_ews;
pub mod directory;
pub mod eas;
pub mod ecp;
pub mod email;
pub mod error;
pub mod ews;
pub mod ews_folders;
pub mod ews_update;
pub mod ical_parser;
pub mod jmap;
pub mod logging;
pub mod mapi;
pub mod meeting;
pub mod metrics;
pub mod models;
pub mod notifications;
pub mod oab;
pub mod oidc;
pub mod oof;
pub mod permission;
pub mod protocol_fixtures;
pub mod rate_limit;
pub mod room;
pub mod smtp;
pub mod storage;
pub mod sync;
pub mod timezone;
pub mod traits;
pub mod util;
pub mod vcard;
pub mod version;

pub mod validation;
pub mod wbxml;

pub use autodiscover::AutodiscoverJsonParams;
pub use config::Config;
pub use error::{GatewayError, Result};
pub use models::AppState;
pub use permission::{
    CalendarPermission, DelegateInfo, DelegateManager, DelegatePermission, PermissionCheck,
    PermissionEnforcement, PermissionLevel, PermissionRights,
};
pub use storage::{SafeDebug, Storage};
pub use util::xml_escape;

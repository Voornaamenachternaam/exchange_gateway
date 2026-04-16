// src/lib.rs
//! Exchange Gateway Library
//!
//! This crate provides the core protocol implementation for the Exchange Gateway,
//! which translates Outlook EWS and ActiveSync calendar operations to CalDAV.
//!
//! The library is organized into the following modules:
//!
//! - `autodiscover`: Outlook Autodiscover protocol handlers
//! - `caldav`: CalDAV client for communicating with Stalwart Mailserver
//! - `calendar`: Calendar item parsing and rendering (ICS, EWS XML, EAS XML)
//! - `config`: Configuration loading and validation
//! - `eas`: Exchange ActiveSync protocol handlers
//! - `error`: Error types for the gateway
//! - `ews`: Exchange Web Services protocol handlers
//! - `ews_folders`: EWS folder hierarchy handling
//! - `ews_update`: EWS item update operations
//! - `ical_parser`: Nom-based iCalendar parsing (RFC 5545)
//! - `models`: Shared data models and application state
//! - `protocol_fixtures`: Protocol response fixtures for testing
//! - `storage`: D1-backed persistence layer
//! - `sync`: Sync state management for EAS and EWS
//! - `timezone`: Timezone conversion utilities
//! - `util`: Common utility functions
//! - `wbxml`: WBXML encoding/decoding for ActiveSync

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
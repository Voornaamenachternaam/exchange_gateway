// src/error.rs
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("CalDAV request failed: {0}")]
    CalDav(#[from] reqwest::Error),

    #[error("XML parse error: {0}")]
    Xml(String),

    #[error("ICS parse error: {0}")]
    Ics(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("SMTP error: {0}")]
    Smtp(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("Authentication required")]
    Unauthenticated,

    #[error("Item not found: {0}")]
    NotFound(String),

    #[error("Conflict: {0}")]
    Conflict(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),
}

impl From<anyhow::Error> for GatewayError {
    fn from(e: anyhow::Error) -> Self {
        GatewayError::Storage(e.to_string())
    }
}

impl From<quick_xml::Error> for GatewayError {
    fn from(e: quick_xml::Error) -> Self {
        GatewayError::Xml(e.to_string())
    }
}

pub type Result<T, E = GatewayError> = std::result::Result<T, E>;

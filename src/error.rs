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

    #[error("Protocol error: {context}")]
    Protocol {
        context: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("Rate limited: retry after {retry_after_secs}s ({endpoint})")]
    RateLimited {
        endpoint: String,
        retry_after_secs: u64,
    },
}

impl GatewayError {
    pub fn protocol(context: impl Into<String>) -> Self {
        GatewayError::Protocol {
            context: context.into(),
            source: None,
        }
    }

    pub fn protocol_with_source(context: impl Into<String>, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        GatewayError::Protocol {
            context: context.into(),
            source: Some(Box::new(source)),
        }
    }

    pub fn rate_limited(endpoint: impl Into<String>, retry_after_secs: u64) -> Self {
        GatewayError::RateLimited {
            endpoint: endpoint.into(),
            retry_after_secs,
        }
    }
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

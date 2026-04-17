// src/error.rs
use thiserror::Error;

/// Gateway error types for protocol and application errors.
///
/// This enum is marked as `#[non_exhaustive]` to allow adding new error
/// variants without breaking changes to downstream code.
#[non_exhaustive]
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

    #[error("WBXML decode error: {0}")]
    WbxmlDecode(String),

    #[error("WBXML encode error: {0}")]
    WbxmlEncode(String),

    #[error("Timezone error: {0}")]
    Timezone(String),

    #[error("Sync state error: {0}")]
    SyncState(String),

    #[error("Calendar parse error: {0}")]
    CalendarParse(String),

    #[error("Invalid sync key")]
    InvalidSyncKey,

    #[error("Folder not found: {0}")]
    FolderNotFound(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Operation not supported: {0}")]
    NotSupported(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl GatewayError {
    pub fn protocol(context: impl Into<String>) -> Self {
        GatewayError::Protocol {
            context: context.into(),
            source: None,
        }
    }

    pub fn protocol_with_source(
        context: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        GatewayError::Protocol {
            context: context.into(),
            source: Some(source.into()),
        }
    }

    pub fn rate_limited(endpoint: impl Into<String>, retry_after_secs: u64) -> Self {
        GatewayError::RateLimited {
            endpoint: endpoint.into(),
            retry_after_secs,
        }
    }

    pub fn wbxml_decode(msg: impl Into<String>) -> Self {
        GatewayError::WbxmlDecode(msg.into())
    }

    pub fn wbxml_encode(msg: impl Into<String>) -> Self {
        GatewayError::WbxmlEncode(msg.into())
    }

    pub fn timezone(msg: impl Into<String>) -> Self {
        GatewayError::Timezone(msg.into())
    }

    pub fn sync_state(msg: impl Into<String>) -> Self {
        GatewayError::SyncState(msg.into())
    }

    pub fn calendar_parse(msg: impl Into<String>) -> Self {
        GatewayError::CalendarParse(msg.into())
    }

    pub fn not_supported(operation: impl Into<String>) -> Self {
        GatewayError::NotSupported(operation.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        GatewayError::Internal(msg.into())
    }

    pub fn status_code(&self) -> u16 {
        match self {
            GatewayError::CalDav(_) => 502,
            GatewayError::Xml(_) => 400,
            GatewayError::Ics(_) => 400,
            GatewayError::Storage(_) => 500,
            GatewayError::Config(_) => 500,
            GatewayError::Unauthenticated => 401,
            GatewayError::NotFound(_) => 404,
            GatewayError::FolderNotFound(_) => 404,
            GatewayError::Conflict(_) => 409,
            GatewayError::InvalidInput(_) => 400,
            GatewayError::Protocol { .. } => 500,
            GatewayError::RateLimited { .. } => 429,
            GatewayError::WbxmlDecode(_) => 400,
            GatewayError::WbxmlEncode(_) => 500,
            GatewayError::Timezone(_) => 400,
            GatewayError::SyncState(_) => 500,
            GatewayError::CalendarParse(_) => 400,
            GatewayError::InvalidSyncKey => 400,
            GatewayError::PermissionDenied(_) => 403,
            GatewayError::NotSupported(_) => 501,
            GatewayError::Internal(_) => 500,
        }
    }

    pub fn is_client_error(&self) -> bool {
        self.status_code() >= 400 && self.status_code() < 500
    }

    pub fn is_server_error(&self) -> bool {
        self.status_code() >= 500
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

impl From<std::io::Error> for GatewayError {
    fn from(e: std::io::Error) -> Self {
        GatewayError::Internal(e.to_string())
    }
}

impl From<base64::DecodeError> for GatewayError {
    fn from(e: base64::DecodeError) -> Self {
        GatewayError::CalendarParse(format!("Base64 decode error: {}", e))
    }
}

pub type Result<T, E = GatewayError> = std::result::Result<T, E>;

// src/traits.rs
//! Core traits for the Exchange Gateway.
//!
//! These traits leverage Rust 2024's native async trait support and
//! Return Position Impl Trait In Traits (RPITIT) for flexible, composable APIs.

use crate::error::Result;
use crate::calendar::CalendarItem;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Trait for calendar storage backends.
///
/// This trait uses Rust 2024's native async trait support, allowing
/// async methods in traits without the `#[async_trait]` macro.
///
/// # Example Implementation
///
/// ```rust
/// use exchange_gateway::traits::CalendarStore;
/// use exchange_gateway::calendar::CalendarItem;
/// use exchange_gateway::error::Result;
///
/// struct D1Store {
///     // connection details
/// }
///
/// impl CalendarStore for D1Store {
///     async fn get_item(&self, id: &str) -> Result<Option<CalendarItem>> {
///         // Implementation
///         todo!()
///     }
///     
///     async fn put_item(&self, item: &CalendarItem) -> Result<String> {
///         // Implementation
///         todo!()
///     }
/// }
/// ```
pub trait CalendarStore: Send + Sync {
    /// Retrieves a calendar item by its server ID.
    ///
    /// Returns `None` if the item doesn't exist.
    fn get_item(&self, id: &str) -> impl std::future::Future<Output = Result<Option<CalendarItem>>> + Send;
    
    /// Stores a calendar item and returns its server ID.
    fn put_item(&self, item: &CalendarItem) -> impl std::future::Future<Output = Result<String>> + Send;
    
    /// Deletes a calendar item by its server ID.
    fn delete_item(&self, id: &str) -> impl std::future::Future<Output = Result<()>> + Send;
    
    /// Lists items modified since a given timestamp.
    fn list_changes_since(
        &self,
        since: DateTime<Utc>,
        limit: usize,
    ) -> impl std::future::Future<Output = Result<Vec<(String, CalendarItem)>>> + Send;
}

/// Trait for sync state management.
///
/// Manages synchronization keys and tokens for EAS and EWS protocols.
pub trait SyncStateStore: Send + Sync {
    /// Gets the sync key for a collection.
    fn get_sync_key(
        &self,
        owner: &str,
        collection_id: &str,
    ) -> impl std::future::Future<Output = Result<Option<(String, Option<String>)>>> + Send;
    
    /// Sets the sync key for a collection.
    fn set_sync_key(
        &self,
        owner: &str,
        collection_id: &str,
        sync_key: &str,
        token: Option<&str>,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
}

/// Trait for XML rendering with lazy evaluation.
///
/// Uses RPITIT to return `impl Display` for efficient string building.
pub trait XmlRender {
    /// Renders the item as XML.
    ///
    /// Returns `impl Display` for lazy evaluation, avoiding unnecessary
    /// string allocations when the result is immediately written to a buffer.
    fn render_xml(&self) -> impl std::fmt::Display + '_;
}

/// Trait for WBXML encoding/decoding.
pub trait WbxmlCodec {
    /// Decodes WBXML to XML string.
    fn decode(&self, data: &[u8]) -> Result<String>;
    
    /// Encodes XML to WBXML bytes.
    fn encode(&self, xml: &str) -> Result<Vec<u8>>;
}

/// Trait for rate limiting.
///
/// Uses async methods to support both synchronous and asynchronous rate limiters.
pub trait RateLimiter: Send + Sync {
    /// Checks if a request should be throttled.
    ///
    /// Returns `true` if the request should be blocked due to rate limiting.
    fn check_rate_limit(&self, key: &str) -> impl std::future::Future<Output = bool> + Send;
    
    /// Records a request for rate limiting purposes.
    fn record_request(&self, key: &str) -> impl std::future::Future<Output = ()> + Send;
}

/// Trait for authentication and authorization.
pub trait Authenticator: Send + Sync {
    /// Validates credentials and returns the authenticated user.
    fn authenticate(
        &self,
        username: &str,
        password: &str,
    ) -> impl std::future::Future<Output = Result<AuthenticatedUser>> + Send;
}

/// Represents an authenticated user.
#[derive(Clone, Debug)]
pub struct AuthenticatedUser {
    pub username: String,
    pub email: String,
    pub permissions: Vec<Permission>,
}

/// User permissions for calendar operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Permission {
    Read,
    Write,
    Delete,
    Admin,
}

/// Trait for calendar event processors.
///
/// Uses RPITIT to return iterators without boxing.
pub trait EventProcessor {
    /// Processes calendar events, returning transformed items.
    ///
    /// Returns an `impl Iterator` for efficient lazy processing.
    fn process_events<'a, I>(
        &'a self,
        events: I,
    ) -> impl Iterator<Item = Result<CalendarItem>> + 'a
    where
        I: Iterator<Item = CalendarItem> + 'a;
}

#[cfg(test)]
mod tests {
    use super::*;
    
    // Test that traits are object-safe where needed
    fn _assert_calendar_store_send_sync<T: CalendarStore>() {}
    fn _assert_sync_state_send_sync<T: SyncStateStore>() {}
    fn _assert_rate_limiter_send_sync<T: RateLimiter>() {}
    
    #[test]
    fn test_traits_compile() {
        // Verify all traits compile correctly
        // This is a compile-time test
    }
}
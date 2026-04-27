// src/traits.rs

use std::future::Future;

use crate::calendar::CalendarItem;
use crate::error::Result;
use chrono::{DateTime, Utc};

pub trait CalendarStore: Send + Sync {
    fn get_item(&self, id: &str) -> impl Future<Output = Result<Option<CalendarItem>>> + Send;
    fn put_item(&self, item: &CalendarItem) -> impl Future<Output = Result<String>> + Send;
    fn delete_item(&self, id: &str) -> impl Future<Output = Result<()>> + Send;
    fn list_changes_since(
        &self,
        since: DateTime<Utc>,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<(String, CalendarItem)>>> + Send;
}

pub trait SyncStateStore: Send + Sync {
    fn get_sync_key(
        &self,
        owner: &str,
        collection_id: &str,
    ) -> impl Future<Output = Result<Option<(String, Option<String>)>>> + Send;
    fn set_sync_key(
        &self,
        owner: &str,
        collection_id: &str,
        sync_key: &str,
        token: Option<&str>,
    ) -> impl Future<Output = Result<()>> + Send;
}

pub trait XmlRender {
    fn render_xml(&self) -> impl std::fmt::Display + '_;
}

pub trait WbxmlCodec {
    fn decode(&self, data: &[u8]) -> Result<String>;
    fn encode(&self, xml: &str) -> Result<Vec<u8>>;
}

pub trait RateLimiter: Send + Sync {
    fn check_rate_limit(&self, key: &str) -> impl Future<Output = bool> + Send;
    fn record_request(&self, key: &str) -> impl Future<Output = ()> + Send;
}

pub trait Authenticator: Send + Sync {
    fn authenticate(
        &self,
        username: &str,
        password: &str,
    ) -> impl Future<Output = Result<AuthenticatedUser>> + Send;
}

#[derive(Clone, Debug)]
pub struct AuthenticatedUser {
    pub username: String,
    pub email: String,
    pub permissions: Vec<Permission>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Permission {
    Read,
    Write,
    Delete,
    Admin,
}

pub trait EventProcessor {
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

    fn _assert_calendar_store_send_sync<T: CalendarStore>() {}
    fn _assert_sync_state_send_sync<T: SyncStateStore>() {}
    fn _assert_rate_limiter_send_sync<T: RateLimiter>() {}

    #[test]
    fn test_traits_compile() {}
}

// src/traits.rs

use crate::error::Result;
use crate::calendar::CalendarItem;
use chrono::{DateTime, Utc};

pub trait CalendarStore: Send + Sync {
 async fn get_item(&self, id: &str) -> Result<Option<CalendarItem>>;
 async fn put_item(&self, item: &CalendarItem) -> Result<String>;
 async fn delete_item(&self, id: &str) -> Result<()>;
 async fn list_changes_since(
  &self,
  since: DateTime<Utc>,
  limit: usize,
 ) -> Result<Vec<(String, CalendarItem)>>;
}

pub trait SyncStateStore: Send + Sync {
 async fn get_sync_key(
  &self,
  owner: &str,
  collection_id: &str,
 ) -> Result<Option<(String, Option<String>)>>;
 async fn set_sync_key(
  &self,
  owner: &str,
  collection_id: &str,
  sync_key: &str,
  token: Option<&str>,
 ) -> Result<()>;
}

pub trait XmlRender {
 fn render_xml(&self) -> impl std::fmt::Display + '_;
}

pub trait WbxmlCodec {
 fn decode(&self, data: &[u8]) -> Result<String>;
 fn encode(&self, xml: &str) -> Result<Vec<u8>>;
}

pub trait RateLimiter: Send + Sync {
 async fn check_rate_limit(&self, key: &str) -> bool;
 async fn record_request(&self, key: &str);
}

pub trait Authenticator: Send + Sync {
 async fn authenticate(
  &self,
  username: &str,
  password: &str,
 ) -> Result<AuthenticatedUser>;
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

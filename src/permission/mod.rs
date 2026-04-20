// src/permission/mod.rs
pub mod types;
pub mod storage;
pub mod enforcement;
pub mod delegate;

pub use types::{CalendarPermission, PermissionLevel, PermissionRights, DelegateInfo, DelegatePermission};
pub use storage::PermissionStorage;
pub use enforcement::{PermissionEnforcement, PermissionCheck, PermissionContext};
pub use delegate::DelegateManager;
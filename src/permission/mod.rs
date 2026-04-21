// src/permission/mod.rs
pub mod delegate;
pub mod enforcement;
pub mod storage;
pub mod types;

pub use delegate::DelegateManager;
pub use enforcement::{PermissionCheck, PermissionContext, PermissionEnforcement};
pub use storage::PermissionStorage;
pub use types::{
    CalendarPermission, DelegateInfo, DelegatePermission, PermissionLevel, PermissionRights,
};

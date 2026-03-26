//! Device Management - EAS Device Partnership Lifecycle
//!
//! This module implements comprehensive device management for Exchange ActiveSync
//! including device registration, partnership lifecycle, quarantine, access control,
//! and device monitoring.

use crate::eas_provision::{DeviceInfo, PolicyData, RemoteWipeStatus};
use crate::eas_status::EasStatus;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;

/// Device identifier
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceId(pub String);

impl DeviceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for DeviceId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Device partnership state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PartnershipState {
    /// Device is new and pending approval
    Pending = 0,
    /// Device is quarantined awaiting admin approval
    Quarantined = 1,
    /// Device is approved and active
    Active = 2,
    /// Device is blocked
    Blocked = 3,
    /// Device partnership is suspended
    Suspended = 4,
    /// Device has been wiped
    Wiped = 5,
    /// Device partnership has been removed
    Removed = 6,
}

impl PartnershipState {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(PartnershipState::Pending),
            1 => Some(PartnershipState::Quarantined),
            2 => Some(PartnershipState::Active),
            3 => Some(PartnershipState::Blocked),
            4 => Some(PartnershipState::Suspended),
            5 => Some(PartnershipState::Wiped),
            6 => Some(PartnershipState::Removed),
            _ => None,
        }
    }

    pub fn as_u8(&self) -> u8 {
        *self as u8
    }

    pub fn is_active(&self) -> bool {
        matches!(self, PartnershipState::Active)
    }

    pub fn can_sync(&self) -> bool {
        matches!(self, PartnershipState::Active | PartnershipState::Pending)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            PartnershipState::Pending => "Pending",
            PartnershipState::Quarantined => "Quarantined",
            PartnershipState::Active => "Active",
            PartnershipState::Blocked => "Blocked",
            PartnershipState::Suspended => "Suspended",
            PartnershipState::Wiped => "Wiped",
            PartnershipState::Removed => "Removed",
        }
    }
}

/// Device access state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessState {
    /// Full access granted
    Allowed = 0,
    /// Access blocked
    Denied = 1,
    /// Access restricted (e.g., only email)
    Restricted = 2,
    /// Access expired
    Expired = 3,
}

/// Device record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRecord {
    pub device_id: DeviceId,
    pub user_id: String,
    pub device_type: String,
    pub device_info: DeviceInfo,
    pub state: PartnershipState,
    pub access_state: AccessState,
    pub created_at: DateTime<Utc>,
    pub first_sync_at: Option<DateTime<Utc>>,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub last_sync_result: Option<EasStatus>,
    pub sync_count: u64,
    pub policy_key: Option<String>,
    pub policy_applied_at: Option<DateTime<Utc>>,
    pub remote_wipe_status: RemoteWipeStatus,
    pub wipe_requested_at: Option<DateTime<Utc>>,
    pub wipe_completed_at: Option<DateTime<Utc>>,
    pub quarantine_reason: Option<String>,
    pub quarantined_at: Option<DateTime<Utc>>,
    pub approved_at: Option<DateTime<Utc>>,
    pub approved_by: Option<String>,
    pub blocked_at: Option<DateTime<Utc>>,
    pub blocked_reason: Option<String>,
    pub suspension_reason: Option<String>,
    pub suspended_at: Option<DateTime<Utc>>,
    pub access_rules: Vec<AccessRule>,
    pub sync_key: Option<String>,
    pub protocol_version: Option<String>,
    pub user_agent: Option<String>,
    pub ip_addresses: Vec<IpAddr>,
    pub last_ip_address: Option<IpAddr>,
    pub hardware_id: Option<String>,
    pub os_version: Option<String>,
    pub phone_number: Option<String>,
    pub carrier: Option<String>,
}

impl DeviceRecord {
    /// Create a new device record
    pub fn new(device_id: DeviceId, user_id: String, device_type: String) -> Self {
        Self {
            device_id,
            user_id,
            device_type,
            device_info: DeviceInfo::default(),
            state: PartnershipState::Pending,
            access_state: AccessState::Allowed,
            created_at: Utc::now(),
            first_sync_at: None,
            last_sync_at: None,
            last_sync_result: None,
            sync_count: 0,
            policy_key: None,
            policy_applied_at: None,
            remote_wipe_status: RemoteWipeStatus::None,
            wipe_requested_at: None,
            wipe_completed_at: None,
            quarantine_reason: None,
            quarantined_at: None,
            approved_at: None,
            approved_by: None,
            blocked_at: None,
            blocked_reason: None,
            suspension_reason: None,
            suspended_at: None,
            access_rules: Vec::new(),
            sync_key: None,
            protocol_version: None,
            user_agent: None,
            ip_addresses: Vec::new(),
            last_ip_address: None,
            hardware_id: None,
            os_version: None,
            phone_number: None,
            carrier: None,
        }
    }

    /// Record a successful sync
    pub fn record_sync(&mut self, status: EasStatus, ip: Option<IpAddr>) {
        let now = Utc::now();
        if self.first_sync_at.is_none() {
            self.first_sync_at = Some(now);
        }
        self.last_sync_at = Some(now);
        self.last_sync_result = Some(status);
        self.sync_count += 1;
        
        if let Some(ip) = ip {
            self.last_ip_address = Some(ip);
            if !self.ip_addresses.contains(&ip) {
                self.ip_addresses.push(ip);
            }
        }
    }

    /// Check if device is inactive
    pub fn is_inactive(&self, threshold_days: i64) -> bool {
        if let Some(last_sync) = self.last_sync_at {
            Utc::now() - last_sync > Duration::days(threshold_days)
        } else {
            // Never synced, check creation time
            Utc::now() - self.created_at > Duration::days(threshold_days)
        }
    }

    /// Get device age in days
    pub fn age_days(&self) -> i64 {
        (Utc::now() - self.created_at).num_days()
    }

    /// Get days since last sync
    pub fn days_since_sync(&self) -> Option<i64> {
        self.last_sync_at.map(|t| (Utc::now() - t).num_days())
    }

    /// Check if device can be approved
    pub fn can_approve(&self) -> bool {
        matches!(self.state, PartnershipState::Pending | PartnershipState::Quarantined)
    }

    /// Check if device can be blocked
    pub fn can_block(&self) -> bool {
        !matches!(self.state, PartnershipState::Blocked | PartnershipState::Removed)
    }

    /// Check if device can be wiped
    pub fn can_wipe(&self) -> bool {
        matches!(self.state, PartnershipState::Active | PartnershipState::Suspended)
    }

    /// Update device info
    pub fn update_info(&mut self, info: DeviceInfo) {
        self.device_info = info;
    }

    /// Set policy key
    pub fn set_policy_key(&mut self, key: String) {
        self.policy_key = Some(key);
    }
}

/// Access rule for device
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessRule {
    pub rule_type: AccessRuleType,
    pub condition: AccessCondition,
    pub action: AccessAction,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Access rule type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccessRuleType {
    Allow,
    Deny,
    Restrict,
    Quarantine,
}

/// Access condition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AccessCondition {
    DeviceType(String),
    UserAgent(String),
    IpRange(IpRange),
    ProtocolVersion(String),
    OsVersion(String),
    Always,
}

/// IP range condition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpRange {
    pub start: IpAddr,
    pub end: IpAddr,
}

/// Access action
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AccessAction {
    GrantFullAccess,
    DenyAccess,
    AllowEmailOnly,
    AllowCalendarOnly,
    AllowContactsOnly,
    QuarantineDevice(String),
    RequireApproval,
}

/// Device quarantine decision
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineDecision {
    AutoApprove,
    AutoQuarantine,
    RequireAdminApproval,
}

/// Device manager
pub struct DeviceManager {
    devices: HashMap<DeviceId, DeviceRecord>,
    user_devices: HashMap<String, Vec<DeviceId>>,
    quarantine_policy: QuarantinePolicy,
    auto_approve_patterns: Vec<String>,
    auto_quarantine_patterns: Vec<String>,
    max_devices_per_user: usize,
    inactive_threshold_days: i64,
}

/// Quarantine policy
#[derive(Debug, Clone)]
pub struct QuarantinePolicy {
    pub enabled: bool,
    pub quarantine_unknown_devices: bool,
    pub quarantine_outdated_os: bool,
    pub min_os_version: Option<String>,
    pub require_encryption: bool,
    pub auto_approve_after_hours: Option<i64>,
}

impl Default for QuarantinePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            quarantine_unknown_devices: true,
            quarantine_outdated_os: false,
            min_os_version: None,
            require_encryption: false,
            auto_approve_after_hours: None,
        }
    }
}

impl DeviceManager {
    /// Create a new device manager
    pub fn new() -> Self {
        Self {
            devices: HashMap::new(),
            user_devices: HashMap::new(),
            quarantine_policy: QuarantinePolicy::default(),
            auto_approve_patterns: vec![
                "Outlook".to_string(),
                "Microsoft".to_string(),
                "Apple".to_string(),
            ],
            auto_quarantine_patterns: vec![
                "unknown".to_string(),
                "test".to_string(),
            ],
            max_devices_per_user: 10,
            inactive_threshold_days: 30,
        }
    }

    /// Register a new device
    pub fn register_device(
        &mut self,
        device_id: DeviceId,
        user_id: String,
        device_type: String,
        device_info: Option<DeviceInfo>,
    ) -> Result<&DeviceRecord, DeviceRegistrationError> {
        // Check if device already exists
        if let Some(existing) = self.devices.get(&device_id) {
            if existing.state == PartnershipState::Active {
                return Err(DeviceRegistrationError::DeviceAlreadyRegistered);
            }
        }

        // Check device limit per user
        let user_device_count = self.user_devices
            .get(&user_id)
            .map(|v| v.len())
            .unwrap_or(0);
        
        if user_device_count >= self.max_devices_per_user {
            return Err(DeviceRegistrationError::DeviceLimitExceeded);
        }

        // Create device record
        let mut record = DeviceRecord::new(device_id.clone(), user_id.clone(), device_type.clone());
        
        if let Some(info) = device_info {
            record.update_info(info);
        }

        // Determine initial state based on policy
        let decision = self.evaluate_quarantine(&record);
        record.state = match decision {
            QuarantineDecision::AutoApprove => PartnershipState::Active,
            QuarantineDecision::AutoQuarantine => {
                record.quarantine_reason = Some("Auto-quarantined by policy".to_string());
                record.quarantined_at = Some(Utc::now());
                PartnershipState::Quarantined
            }
            QuarantineDecision::RequireAdminApproval => {
                record.quarantine_reason = Some("Awaiting admin approval".to_string());
                record.quarantined_at = Some(Utc::now());
                PartnershipState::Quarantined
            }
        };

        // Store device
        self.devices.insert(device_id.clone(), record);
        
        // Update user index
        self.user_devices
            .entry(user_id)
            .or_default()
            .push(device_id);

        Ok(self.devices.get(&device_id).unwrap())
    }

    /// Evaluate quarantine policy for a device
    fn evaluate_quarantine(&self, record: &DeviceRecord) -> QuarantineDecision {
        if !self.quarantine_policy.enabled {
            return QuarantineDecision::AutoApprove;
        }

        // Check auto-quarantine patterns
        let user_agent = record.user_agent.as_deref()
            .or(record.device_info.user_agent.as_deref())
            .unwrap_or("");
        
        for pattern in &self.auto_quarantine_patterns {
            if user_agent.to_lowercase().contains(&pattern.to_lowercase()) {
                return QuarantineDecision::AutoQuarantine;
            }
        }

        // Check auto-approve patterns
        for pattern in &self.auto_approve_patterns {
            if user_agent.to_lowercase().contains(&pattern.to_lowercase()) {
                return QuarantineDecision::AutoApprove;
            }
        }

        // Check OS version requirement
        if self.quarantine_policy.quarantine_outdated_os {
            if let Some(ref min_version) = self.quarantine_policy.min_os_version {
                let os_version = record.os_version.as_deref()
                    .or(record.device_info.os.as_deref())
                    .unwrap_or("");
                if !self.version_meets_minimum(os_version, min_version) {
                    return QuarantineDecision::AutoQuarantine;
                }
            }
        }

        // Default: require admin approval for unknown devices
        if self.quarantine_policy.quarantine_unknown_devices {
            QuarantineDecision::RequireAdminApproval
        } else {
            QuarantineDecision::AutoApprove
        }
    }

    /// Check if version meets minimum requirement
    fn version_meets_minimum(&self, version: &str, minimum: &str) -> bool {
        let version_parts: Vec<u32> = version.split('.')
            .filter_map(|p| p.parse().ok())
            .collect();
        let minimum_parts: Vec<u32> = minimum.split('.')
            .filter_map(|p| p.parse().ok())
            .collect();

        for (v, m) in version_parts.iter().zip(minimum_parts.iter()) {
            if v < m {
                return false;
            }
            if v > m {
                return true;
            }
        }

        version_parts.len() >= minimum_parts.len()
    }

    /// Get device by ID
    pub fn get_device(&self, device_id: &DeviceId) -> Option<&DeviceRecord> {
        self.devices.get(device_id)
    }

    /// Get mutable device reference
    pub fn get_device_mut(&mut self, device_id: &DeviceId) -> Option<&mut DeviceRecord> {
        self.devices.get_mut(device_id)
    }

    /// Get all devices for a user
    pub fn get_user_devices(&self, user_id: &str) -> Vec<&DeviceRecord> {
        self.user_devices
            .get(user_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.devices.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Approve a quarantined device
    pub fn approve_device(
        &mut self,
        device_id: &DeviceId,
        approved_by: String,
    ) -> Result<(), DeviceManagementError> {
        let device = self.devices.get_mut(device_id)
            .ok_or(DeviceManagementError::DeviceNotFound)?;

        if !device.can_approve() {
            return Err(DeviceManagementError::InvalidStateTransition);
        }

        device.state = PartnershipState::Active;
        device.approved_at = Some(Utc::now());
        device.approved_by = Some(approved_by);
        device.quarantine_reason = None;

        Ok(())
    }

    /// Block a device
    pub fn block_device(
        &mut self,
        device_id: &DeviceId,
        reason: String,
    ) -> Result<(), DeviceManagementError> {
        let device = self.devices.get_mut(device_id)
            .ok_or(DeviceManagementError::DeviceNotFound)?;

        if !device.can_block() {
            return Err(DeviceManagementError::InvalidStateTransition);
        }

        device.state = PartnershipState::Blocked;
        device.blocked_at = Some(Utc::now());
        device.blocked_reason = Some(reason);

        Ok(())
    }

    /// Unblock a device
    pub fn unblock_device(&mut self, device_id: &DeviceId) -> Result<(), DeviceManagementError> {
        let device = self.devices.get_mut(device_id)
            .ok_or(DeviceManagementError::DeviceNotFound)?;

        if device.state != PartnershipState::Blocked {
            return Err(DeviceManagementError::InvalidStateTransition);
        }

        device.state = PartnershipState::Active;
        device.blocked_at = None;
        device.blocked_reason = None;

        Ok(())
    }

    /// Suspend a device
    pub fn suspend_device(
        &mut self,
        device_id: &DeviceId,
        reason: String,
    ) -> Result<(), DeviceManagementError> {
        let device = self.devices.get_mut(device_id)
            .ok_or(DeviceManagementError::DeviceNotFound)?;

        if device.state != PartnershipState::Active {
            return Err(DeviceManagementError::InvalidStateTransition);
        }

        device.state = PartnershipState::Suspended;
        device.suspended_at = Some(Utc::now());
        device.suspension_reason = Some(reason);

        Ok(())
    }

    /// Resume a suspended device
    pub fn resume_device(&mut self, device_id: &DeviceId) -> Result<(), DeviceManagementError> {
        let device = self.devices.get_mut(device_id)
            .ok_or(DeviceManagementError::DeviceNotFound)?;

        if device.state != PartnershipState::Suspended {
            return Err(DeviceManagementError::InvalidStateTransition);
        }

        device.state = PartnershipState::Active;
        device.suspended_at = None;
        device.suspension_reason = None;

        Ok(())
    }

    /// Remove a device partnership
    pub fn remove_device(&mut self, device_id: &DeviceId) -> Result<(), DeviceManagementError> {
        let device = self.devices.get(device_id)
            .ok_or(DeviceManagementError::DeviceNotFound)?;

        let user_id = device.user_id.clone();
        
        self.devices.remove(device_id);
        
        if let Some(devices) = self.user_devices.get_mut(&user_id) {
            devices.retain(|id| id != device_id);
        }

        Ok(())
    }

    /// Request remote wipe
    pub fn request_wipe(&mut self, device_id: &DeviceId) -> Result<(), DeviceManagementError> {
        let device = self.devices.get_mut(device_id)
            .ok_or(DeviceManagementError::DeviceNotFound)?;

        if !device.can_wipe() {
            return Err(DeviceManagementError::InvalidStateTransition);
        }

        device.remote_wipe_status = RemoteWipeStatus::Pending;
        device.wipe_requested_at = Some(Utc::now());

        Ok(())
    }

    /// Get device statistics
    pub fn get_statistics(&self) -> DeviceStatistics {
        let mut stats = DeviceStatistics::default();

        for device in self.devices.values() {
            stats.total_devices += 1;
            
            match device.state {
                PartnershipState::Active => stats.active_devices += 1,
                PartnershipState::Quarantined => stats.quarantined_devices += 1,
                PartnershipState::Blocked => stats.blocked_devices += 1,
                PartnershipState::Suspended => stats.suspended_devices += 1,
                PartnershipState::Wiped => stats.wiped_devices += 1,
                _ => {}
            }

            if device.is_inactive(self.inactive_threshold_days) {
                stats.inactive_devices += 1;
            }
        }

        stats
    }

    /// Get inactive devices
    pub fn get_inactive_devices(&self, threshold_days: i64) -> Vec<&DeviceRecord> {
        self.devices
            .values()
            .filter(|d| d.is_inactive(threshold_days))
            .collect()
    }

    /// Clean up inactive devices
    pub fn cleanup_inactive_devices(&mut self, threshold_days: i64) -> usize {
        let to_remove: Vec<DeviceId> = self.devices
            .values()
            .filter(|d| d.is_inactive(threshold_days) && d.state != PartnershipState::Active)
            .map(|d| d.device_id.clone())
            .collect();

        let count = to_remove.len();
        for device_id in to_remove {
            let _ = self.remove_device(&device_id);
        }

        count
    }

    /// Set quarantine policy
    pub fn set_quarantine_policy(&mut self, policy: QuarantinePolicy) {
        self.quarantine_policy = policy;
    }

    /// Set max devices per user
    pub fn set_max_devices_per_user(&mut self, max: usize) {
        self.max_devices_per_user = max;
    }

    /// Set inactive threshold
    pub fn set_inactive_threshold(&mut self, days: i64) {
        self.inactive_threshold_days = days;
    }
}

impl Default for DeviceManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Device registration error
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceRegistrationError {
    DeviceAlreadyRegistered,
    DeviceLimitExceeded,
    InvalidDeviceType,
    QuarantineRequired,
}

impl std::fmt::Display for DeviceRegistrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceRegistrationError::DeviceAlreadyRegistered => write!(f, "Device already registered"),
            DeviceRegistrationError::DeviceLimitExceeded => write!(f, "Device limit exceeded for user"),
            DeviceRegistrationError::InvalidDeviceType => write!(f, "Invalid device type"),
            DeviceRegistrationError::QuarantineRequired => write!(f, "Device requires quarantine approval"),
        }
    }
}

impl std::error::Error for DeviceRegistrationError {}

/// Device management error
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceManagementError {
    DeviceNotFound,
    InvalidStateTransition,
    PermissionDenied,
    OperationNotAllowed,
}

impl std::fmt::Display for DeviceManagementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceManagementError::DeviceNotFound => write!(f, "Device not found"),
            DeviceManagementError::InvalidStateTransition => write!(f, "Invalid state transition"),
            DeviceManagementError::PermissionDenied => write!(f, "Permission denied"),
            DeviceManagementError::OperationNotAllowed => write!(f, "Operation not allowed"),
        }
    }
}

impl std::error::Error for DeviceManagementError {}

/// Device statistics
#[derive(Debug, Clone, Default)]
pub struct DeviceStatistics {
    pub total_devices: u64,
    pub active_devices: u64,
    pub quarantined_devices: u64,
    pub blocked_devices: u64,
    pub suspended_devices: u64,
    pub wiped_devices: u64,
    pub inactive_devices: u64,
}

/// Device access controller
pub struct DeviceAccessController;

impl DeviceAccessController {
    /// Check if device can access EAS
    pub fn can_access_eas(device: &DeviceRecord) -> bool {
        device.state.can_sync() && matches!(device.access_state, AccessState::Allowed | AccessState::Restricted)
    }

    /// Check if device can access specific folder type
    pub fn can_access_folder(device: &DeviceRecord, folder_type: FolderType) -> bool {
        if !Self::can_access_eas(device) {
            return false;
        }

        match device.access_state {
            AccessState::Allowed => true,
            AccessState::Restricted => match folder_type {
                FolderType::Email => true,
                FolderType::Calendar => true,
                FolderType::Contacts => true,
                _ => false,
            },
            _ => false,
        }
    }

    /// Get access denial reason
    pub fn get_denial_reason(device: &DeviceRecord) -> Option<String> {
        match device.state {
            PartnershipState::Quarantined => {
                Some(format!("Device is quarantined: {}", 
                    device.quarantine_reason.as_deref().unwrap_or("Unknown reason")))
            }
            PartnershipState::Blocked => {
                Some(format!("Device is blocked: {}",
                    device.blocked_reason.as_deref().unwrap_or("Unknown reason")))
            }
            PartnershipState::Suspended => {
                Some(format!("Device is suspended: {}",
                    device.suspension_reason.as_deref().unwrap_or("Unknown reason")))
            }
            PartnershipState::Wiped => {
                Some("Device has been wiped".to_string())
            }
            PartnershipState::Removed => {
                Some("Device partnership has been removed".to_string())
            }
            _ => match device.access_state {
                AccessState::Denied => Some("Access denied by policy".to_string()),
                AccessState::Expired => Some("Device access has expired".to_string()),
                _ => None,
            }
        }
    }
}

/// Folder type for access control
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderType {
    Email,
    Calendar,
    Contacts,
    Tasks,
    Notes,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_registration() {
        let mut manager = DeviceManager::new();
        
        let result = manager.register_device(
            DeviceId::new("device1"),
            "user1".to_string(),
            "Android".to_string(),
            None,
        );
        
        assert!(result.is_ok());
        assert_eq!(manager.get_user_devices("user1").len(), 1);
    }

    #[test]
    fn test_device_limit() {
        let mut manager = DeviceManager::new();
        manager.set_max_devices_per_user(2);
        
        assert!(manager.register_device(DeviceId::new("d1"), "u1".to_string(), "Android".to_string(), None).is_ok());
        assert!(manager.register_device(DeviceId::new("d2"), "u1".to_string(), "iOS".to_string(), None).is_ok());
        
        let result = manager.register_device(DeviceId::new("d3"), "u1".to_string(), "Windows".to_string(), None);
        assert!(matches!(result, Err(DeviceRegistrationError::DeviceLimitExceeded)));
    }

    #[test]
    fn test_device_lifecycle() {
        let mut manager = DeviceManager::new();
        let device_id = DeviceId::new("device1");
        
        // Register
        manager.register_device(device_id.clone(), "user1".to_string(), "Android".to_string(), None).unwrap();
        
        // Block
        assert!(manager.block_device(&device_id, "Security concern".to_string()).is_ok());
        let device = manager.get_device(&device_id).unwrap();
        assert_eq!(device.state, PartnershipState::Blocked);
        
        // Unblock
        assert!(manager.unblock_device(&device_id).is_ok());
        let device = manager.get_device(&device_id).unwrap();
        assert_eq!(device.state, PartnershipState::Active);
        
        // Suspend
        assert!(manager.suspend_device(&device_id, "Maintenance".to_string()).is_ok());
        let device = manager.get_device(&device_id).unwrap();
        assert_eq!(device.state, PartnershipState::Suspended);
        
        // Resume
        assert!(manager.resume_device(&device_id).is_ok());
        let device = manager.get_device(&device_id).unwrap();
        assert_eq!(device.state, PartnershipState::Active);
    }

    #[test]
    fn test_device_access_control() {
        let mut device = DeviceRecord::new(DeviceId::new("d1"), "u1".to_string(), "Android".to_string());
        device.state = PartnershipState::Active;
        
        assert!(DeviceAccessController::can_access_eas(&device));
        
        device.state = PartnershipState::Blocked;
        assert!(!DeviceAccessController::can_access_eas(&device));
    }

    #[test]
    fn test_inactive_detection() {
        let mut device = DeviceRecord::new(DeviceId::new("d1"), "u1".to_string(), "Android".to_string());
        
        // New device is not inactive
        assert!(!device.is_inactive(30));
        
        // Record sync
        device.record_sync(EasStatus::Success, None);
        assert!(!device.is_inactive(30));
    }

    #[test]
    fn test_version_comparison() {
        let manager = DeviceManager::new();
        
        assert!(manager.version_meets_minimum("14.0", "13.0"));
        assert!(manager.version_meets_minimum("14.1", "14.0"));
        assert!(manager.version_meets_minimum("14.0.1", "14.0"));
        assert!(!manager.version_meets_minimum("13.0", "14.0"));
        assert!(manager.version_meets_minimum("14.0", "14.0"));
    }

    #[test]
    fn test_statistics() {
        let mut manager = DeviceManager::new();
        
        manager.register_device(DeviceId::new("d1"), "u1".to_string(), "Android".to_string(), None).unwrap();
        manager.register_device(DeviceId::new("d2"), "u1".to_string(), "iOS".to_string(), None).unwrap();
        
        let stats = manager.get_statistics();
        assert_eq!(stats.total_devices, 2);
        assert_eq!(stats.active_devices, 2);
    }
}

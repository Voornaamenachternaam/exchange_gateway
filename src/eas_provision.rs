//! EAS Provision Command - Device Provisioning and Policy Enforcement
//!
//! This module implements the Exchange ActiveSync Provision command (MS-ASPROV)
//! for device provisioning, policy enforcement, and remote wipe capabilities.
//! Supports protocol versions 12.0 through 16.1.

use crate::eas_status::EasStatus;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Provision command request
#[derive(Debug, Clone, Default)]
pub struct ProvisionRequest {
    pub device_id: String,
    pub device_type: String,
    pub policy_key: Option<String>,
    pub policy_status: Option<u8>,
    pub remote_wipe_ack: Option<bool>,
}

/// Provision command response
#[derive(Debug, Clone)]
pub struct ProvisionResponse {
    pub status: EasStatus,
    pub policy_key: Option<String>,
    pub policy_data: Option<PolicyData>,
    pub remote_wipe: Option<RemoteWipeStatus>,
}

/// Policy data structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyData {
    pub policy_type: PolicyType,
    pub policy_id: String,
    pub policy_name: String,
    pub description: Option<String>,
    pub requirements: PolicyRequirements,
    pub settings: PolicySettings,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: u32,
}

/// Policy type enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyType {
    /// Enterprise policy (EAS protocol)
    Enterprise = 0,
    /// Device policy (device-level enforcement)
    Device = 1,
}

impl PolicyType {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(PolicyType::Enterprise),
            1 => Some(PolicyType::Device),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            PolicyType::Enterprise => "EASPROV",
            PolicyType::Device => "MS-EAS-Provisioning-WBXML",
        }
    }
}

/// Policy requirements (what the device must support)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PolicyRequirements {
    pub require_signed_smime_messages: bool,
    pub require_encrypted_smime_messages: bool,
    pub require_signed_smime_algorithm: Option<u8>,
    pub require_encryption_smime_algorithm: Option<u8>,
    pub allow_smime_encryption_algorithm_negotiation: bool,
    pub allow_smime_soft_certs: bool,
    pub allow_browser: bool,
    pub allow_consumer_email: bool,
    pub allow_remote_desktop: bool,
    pub allow_internet_sharing: bool,
    pub allow_bluetooth: Option<u8>, // 0=Disable, 1=HandsFreeOnly, 2=Allow
    pub allow_camera: bool,
    pub allow_desktop_sync: bool,
    pub allow_irda: bool,
    pub allow_pop_imap_email: bool,
    pub allow_storage_card: bool,
    pub allow_text_messaging: bool,
    pub allow_wifi: bool,
    pub allow_unsigned_applications: bool,
    pub allow_unsigned_installation_packages: bool,
}

/// Policy settings (enforced limits)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PolicySettings {
    /// Maximum number of failed password attempts before wipe
    pub max_device_password_failed_attempts: Option<u32>,
    /// Maximum inactivity time before password required (minutes)
    pub max_inactivity_time_device_lock: Option<u32>,
    /// Minimum password length
    pub min_device_password_length: Option<u32>,
    /// Password complexity: 0=None, 1=Alphanumeric, 2=Numeric
    pub password_complexity: Option<u8>,
    /// Require alphanumeric password
    pub require_alphanumeric_password: bool,
    /// Require device encryption
    pub require_device_encryption: bool,
    /// Require encrypted backups
    pub require_encrypted_backup: bool,
    /// Require manual sync when roaming
    pub require_manual_sync_when_roaming: bool,
    /// Require storage card encryption
    pub require_storage_card_encryption: bool,
    /// Minimum password complex characters
    pub min_device_password_complex_characters: Option<u32>,
    /// Maximum attachment size (bytes)
    pub max_attachment_size: Option<u64>,
    /// Maximum calendar age filter (days)
    pub max_calendar_age_filter: Option<u32>,
    /// Maximum email age filter (days)
    pub max_email_age_filter: Option<u32>,
    /// Maximum email body truncation size (bytes)
    pub max_email_body_truncation_size: Option<u32>,
    /// Maximum HTML email body truncation size (bytes)
    pub max_email_html_body_truncation_size: Option<u32>,
    /// Maximum number of email folders
    pub max_email_folders: Option<u32>,
    /// Device password expiration (days)
    pub device_password_expiration: Option<u32>,
    /// Device password history count
    pub device_password_history: Option<u32>,
    /// Approved application list
    pub approved_application_list: Vec<String>,
    /// Unapproved inrom list
    pub unapproved_inrom_application_list: Vec<String>,
}

/// Remote wipe status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteWipeStatus {
    /// No wipe requested
    None = 0,
    /// Wipe requested, awaiting acknowledgment
    Pending = 1,
    /// Wipe acknowledged by device
    Acknowledged = 2,
    /// Wipe completed
    Completed = 3,
}

impl RemoteWipeStatus {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(RemoteWipeStatus::None),
            1 => Some(RemoteWipeStatus::Pending),
            2 => Some(RemoteWipeStatus::Acknowledged),
            3 => Some(RemoteWipeStatus::Completed),
            _ => None,
        }
    }

    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// Device policy state
#[derive(Debug, Clone)]
pub struct DevicePolicyState {
    pub device_id: String,
    pub user_id: String,
    pub policy_key: String,
    pub policy_status: PolicyAckStatus,
    pub policy_applied_at: Option<DateTime<Utc>>,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub remote_wipe_status: RemoteWipeStatus,
    pub device_info: DeviceInfo,
    pub compliance_status: ComplianceStatus,
}

/// Policy acknowledgment status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyAckStatus {
    /// Policy not yet acknowledged
    NotAcknowledged = 0,
    /// Policy acknowledged successfully
    Acknowledged = 1,
    /// Policy rejected by device
    Rejected = 2,
    /// Policy partially acknowledged
    Partial = 3,
}

impl PolicyAckStatus {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(PolicyAckStatus::NotAcknowledged),
            1 => Some(PolicyAckStatus::Acknowledged),
            2 => Some(PolicyAckStatus::Rejected),
            3 => Some(PolicyAckStatus::Partial),
            _ => None,
        }
    }

    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// Device information
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub model: Option<String>,
    pub imei: Option<String>,
    pub friendly_name: Option<String>,
    pub os: Option<String>,
    pub os_language: Option<String>,
    pub phone_number: Option<String>,
    pub mobile_operator: Option<String>,
    pub user_agent: Option<String>,
}

/// Device compliance status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComplianceStatus {
    /// Device is compliant with policy
    Compliant = 0,
    /// Device is not compliant
    NonCompliant = 1,
    /// Compliance unknown
    Unknown = 2,
    /// Compliance check pending
    Pending = 3,
}

impl ComplianceStatus {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(ComplianceStatus::Compliant),
            1 => Some(ComplianceStatus::NonCompliant),
            2 => Some(ComplianceStatus::Unknown),
            3 => Some(ComplianceStatus::Pending),
            _ => None,
        }
    }

    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// Policy engine for managing device policies
pub struct PolicyEngine {
    default_policy: PolicyData,
    device_states: HashMap<(String, String), DevicePolicyState>,
}

impl PolicyEngine {
    pub fn process_provision_request(&mut self, request: &ProvisionRequest, user_id: &str) -> ProvisionResponse {
        let key = (user_id.to_string(), request.device_id.clone());
        // ... updated logic to use `key` for lookups and insertions ...
        todo!()
    }
}

impl PolicyEngine {
    /// Create a new policy engine with default policy
    pub fn new() -> Self {
        Self {
            default_policy: Self::create_default_policy(),
            device_states: HashMap::new(),
        }
    }

    /// Create the default enterprise policy
    fn create_default_policy() -> PolicyData {
        PolicyData {
            policy_type: PolicyType::Enterprise,
            policy_id: "default-enterprise-policy".to_string(),
            policy_name: "Default Enterprise Policy".to_string(),
            description: Some("Default policy for enterprise devices".to_string()),
            requirements: PolicyRequirements {
                require_signed_smime_messages: false,
                require_encrypted_smime_messages: false,
                require_signed_smime_algorithm: None,
                require_encryption_smime_algorithm: None,
                allow_smime_encryption_algorithm_negotiation: true,
                allow_smime_soft_certs: true,
                allow_browser: true,
                allow_consumer_email: true,
                allow_remote_desktop: true,
                allow_internet_sharing: true,
                allow_bluetooth: Some(2), // Allow
                allow_camera: true,
                allow_desktop_sync: true,
                allow_irda: true,
                allow_pop_imap_email: true,
                allow_storage_card: true,
                allow_text_messaging: true,
                allow_wifi: true,
                allow_unsigned_applications: true,
                allow_unsigned_installation_packages: true,
            },
            settings: PolicySettings {
                max_device_password_failed_attempts: Some(8),
                max_inactivity_time_device_lock: Some(15),
                min_device_password_length: Some(4),
                password_complexity: Some(0),
                require_alphanumeric_password: false,
                require_device_encryption: false,
                require_encrypted_backup: false,
                require_manual_sync_when_roaming: false,
                require_storage_card_encryption: false,
                min_device_password_complex_characters: None,
                max_attachment_size: Some(10 * 1024 * 1024), // 10MB
                max_calendar_age_filter: None,
                max_email_age_filter: None,
                max_email_body_truncation_size: None,
                max_email_html_body_truncation_size: None,
                max_email_folders: None,
                device_password_expiration: None,
                device_password_history: None,
                approved_application_list: Vec::new(),
                unapproved_inrom_application_list: Vec::new(),
            },
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version: 1,
        }
    }

    /// Process a provision request
    pub fn process_provision_request(&mut self, request: &ProvisionRequest, user_id: &str) -> ProvisionResponse {
        // Check for remote wipe acknowledgment
        if let Some(true) = request.remote_wipe_ack {
            if let Some(state) = self.device_states.get_mut(&request.device_id) {
                state.remote_wipe_status = RemoteWipeStatus::Acknowledged;
                return ProvisionResponse {
                    status: EasStatus::RemoteWipeRequested,
                    policy_key: None,
                    policy_data: None,
                    remote_wipe: Some(RemoteWipeStatus::Acknowledged),
                };
            }
        }

        // Check if device has an existing policy key
        if let Some(ref policy_key) = request.policy_key {
            if let Some(state) = self.device_states.get(&request.device_id) {
                if state.policy_key == *policy_key {
                    // Policy acknowledged
                    if let Some(status) = request.policy_status {
                        let ack_status = PolicyAckStatus::from_u8(status)
                            .unwrap_or(PolicyAckStatus::NotAcknowledged);
                        
                        return self.handle_policy_acknowledgment(
                            &request.device_id,
                            ack_status,
                        );
                    }

                    // Return current policy state
                    return ProvisionResponse {
                        status: EasStatus::Success,
                        policy_key: Some(state.policy_key.clone()),
                        policy_data: None,
                        remote_wipe: Some(state.remote_wipe_status),
                    };
                }
            }
        }

        // New device or policy refresh needed
        let policy_key = self.generate_policy_key();
        let state = DevicePolicyState {
            device_id: request.device_id.clone(),
            user_id: user_id.to_string(),
            policy_key: policy_key.clone(),
            policy_status: PolicyAckStatus::NotAcknowledged,
            policy_applied_at: None,
            last_sync_at: None,
            remote_wipe_status: RemoteWipeStatus::None,
            device_info: DeviceInfo::default(),
            compliance_status: ComplianceStatus::Pending,
        };

        self.device_states.insert(request.device_id.clone(), state);

        ProvisionResponse {
            status: EasStatus::Success,
            policy_key: Some(policy_key),
            policy_data: Some(self.default_policy.clone()),
            remote_wipe: Some(RemoteWipeStatus::None),
        }
    }

    /// Handle policy acknowledgment from device
    fn handle_policy_acknowledgment(
        &mut self,
        device_id: &str,
        status: PolicyAckStatus,
    ) -> ProvisionResponse {
        if let Some(state) = self.device_states.get_mut(device_id) {
            state.policy_status = status;
            
            match status {
                PolicyAckStatus::Acknowledged => {
                    state.policy_applied_at = Some(Utc::now());
                    state.compliance_status = ComplianceStatus::Compliant;
                    ProvisionResponse {
                        status: EasStatus::Success,
                        policy_key: Some(state.policy_key.clone()),
                        policy_data: None,
                        remote_wipe: Some(state.remote_wipe_status),
                    }
                }
                PolicyAckStatus::Partial => {
                    state.compliance_status = ComplianceStatus::NonCompliant;
                    ProvisionResponse {
                        status: EasStatus::PartialSuccess,
                        policy_key: Some(state.policy_key.clone()),
                        policy_data: Some(self.create_partial_policy()),
                        remote_wipe: Some(state.remote_wipe_status),
                    }
                }
                PolicyAckStatus::Rejected => {
                    state.compliance_status = ComplianceStatus::NonCompliant;
                    ProvisionResponse {
                        status: EasStatus::PolicyRefreshRequired,
                        policy_key: None,
                        policy_data: Some(self.create_fallback_policy()),
                        remote_wipe: Some(state.remote_wipe_status),
                    }
                }
                _ => ProvisionResponse {
                    status: EasStatus::Success,
                    policy_key: Some(state.policy_key.clone()),
                    policy_data: None,
                    remote_wipe: Some(state.remote_wipe_status),
                },
            }
        } else {
            ProvisionResponse {
                status: EasStatus::InvalidPolicyKey,
                policy_key: None,
                policy_data: None,
                remote_wipe: None,
            }
        }
    }

    /// Create a partial policy for partial acknowledgment
    fn create_partial_policy(&self) -> PolicyData {
        let mut policy = self.default_policy.clone();
        policy.policy_name = "Partial Enterprise Policy".to_string();
        policy.description = Some("Reduced policy for devices with partial support".to_string());
        
        // Reduce requirements for partial compliance
        policy.settings.require_device_encryption = false;
        policy.settings.require_storage_card_encryption = false;
        policy.settings.min_device_password_length = Some(4);
        policy.settings.password_complexity = Some(0);
        
        policy
    }

    /// Create a fallback policy for rejected policies
    fn create_fallback_policy(&self) -> PolicyData {
        let mut policy = self.default_policy.clone();
        policy.policy_name = "Fallback Policy".to_string();
        policy.description = Some("Minimal policy for devices that cannot apply full policy".to_string());
        
        // Minimal requirements
        policy.requirements = PolicyRequirements::default();
        policy.settings = PolicySettings::default();
        policy.settings.min_device_password_length = Some(4);
        
        policy
    }

    /// Request remote wipe for a device
    pub fn request_remote_wipe(&mut self, device_id: &str) -> Result<(), String> {
        if let Some(state) = self.device_states.get_mut(device_id) {
            state.remote_wipe_status = RemoteWipeStatus::Pending;
            Ok(())
        } else {
            Err("Device not found".to_string())
        }
    }

    /// Cancel remote wipe request
    pub fn cancel_remote_wipe(&mut self, device_id: &str) -> Result<(), String> {
        if let Some(state) = self.device_states.get_mut(device_id) {
            if state.remote_wipe_status == RemoteWipeStatus::Pending {
                state.remote_wipe_status = RemoteWipeStatus::None;
                Ok(())
            } else {
                Err("Wipe already in progress or completed".to_string())
            }
        } else {
            Err("Device not found".to_string())
        }
    }

    /// Complete remote wipe
    pub fn complete_remote_wipe(&mut self, device_id: &str) -> Result<(), String> {
        if let Some(state) = self.device_states.get_mut(device_id) {
            state.remote_wipe_status = RemoteWipeStatus::Completed;
            Ok(())
        } else {
            Err("Device not found".to_string())
        }
    }

    /// Get device policy state
    pub fn get_device_state(&self, device_id: &str) -> Option<&DevicePolicyState> {
        self.device_states.get(device_id)
    }

    /// Update device information
    pub fn update_device_info(&mut self, device_id: &str, info: DeviceInfo) -> Result<(), String> {
        if let Some(state) = self.device_states.get_mut(device_id) {
            state.device_info = info;
            Ok(())
        } else {
            Err("Device not found".to_string())
        }
    }

    /// Check device compliance
    pub fn check_compliance(&self, device_id: &str) -> ComplianceStatus {
        if let Some(state) = self.device_states.get(device_id) {
            state.compliance_status
        } else {
            ComplianceStatus::Unknown
        }
    }

    /// Get all devices for a user
    pub fn get_user_devices(&self, user_id: &str) -> Vec<&DevicePolicyState> {
        self.device_states
            .values()
            .filter(|s| s.user_id == user_id)
            .collect()
    }

    /// Generate a new policy key
    fn generate_policy_key(&self) -> String {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let key: u64 = rng.gen();
        format!("{:016X}", key)
    }

    /// Get policy as XML for EAS response
    pub fn policy_to_xml(&self, policy: &PolicyData) -> String {
        use quick_xml::events::{BytesStart, BytesText, Event};
        use quick_xml::Writer;
        use std::io::Cursor;

        let mut buffer = Cursor::new(Vec::new());
        let mut writer = Writer::new_with_indent(&mut buffer, b' ', 4);

        let _ = writer.create_element("Provision").write_inner_content(|writer| {
            writer.create_element("Policies").write_inner_content(|writer| {
                writer.create_element("Policy").write_inner_content(|writer| {
                    writer.create_element("PolicyType").write_text_content(BytesText::new(policy.policy_type.as_str())).unwrap();
                    writer.create_element("PolicyKey").write_text_content(BytesText::new(&policy.policy_id)).unwrap();
                    writer.create_element("Status").write_text_content(BytesText::new("1")).unwrap();
                    writer.create_element("Data").write_inner_content(|writer| {
                        writer.create_element("EASProvisionDoc").write_inner_content(|writer| {
                            if let Some(max_attempts) = policy.settings.max_device_password_failed_attempts {
                                writer.create_element("DevicePasswordEnabled").write_text_content(BytesText::new("1")).unwrap();
                                writer.create_element("MaxDevicePasswordFailedAttempts").write_text_content(BytesText::new(&max_attempts.to_string())).unwrap();
                            }
                            if let Some(max_inactivity) = policy.settings.max_inactivity_time_device_lock {
                                writer.create_element("MaxInactivityTimeDeviceLock").write_text_content(BytesText::new(&max_inactivity.to_string())).unwrap();
                            }
                            if let Some(min_length) = policy.settings.min_device_password_length {
                                writer.create_element("MinDevicePasswordLength").write_text_content(BytesText::new(&min_length.to_string())).unwrap();
                            }
                            if let Some(complexity) = policy.settings.password_complexity {
                                writer.create_element("PasswordComplexity").write_text_content(BytesText::new(&complexity.to_string())).unwrap();
                            }
                            if policy.settings.require_alphanumeric_password {
                                writer.create_element("RequireAlphanumericDevicePassword").write_text_content(BytesText::new("1")).unwrap();
                            }
                            if policy.settings.require_device_encryption {
                                writer.create_element("RequireDeviceEncryption").write_text_content(BytesText::new("1")).unwrap();
                            }
                            if policy.settings.require_storage_card_encryption {
                                writer.create_element("RequireStorageCardEncryption").write_text_content(BytesText::new("1")).unwrap();
                            }
                            if !policy.requirements.allow_camera {
                                writer.create_element("AllowCamera").write_text_content(BytesText::new("0")).unwrap();
                            }
                            if !policy.requirements.allow_wifi {
                                writer.create_element("AllowWifi").write_text_content(BytesText::new("0")).unwrap();
                            }
                            if !policy.requirements.allow_text_messaging {
                                writer.create_element("AllowTextMessaging").write_text_content(BytesText::new("0")).unwrap();
                            }
                            if !policy.requirements.allow_pop_imap_email {
                                writer.create_element("AllowPOPIMAPEmail").write_text_content(BytesText::new("0")).unwrap();
                            }
                            if !policy.requirements.allow_browser {
                                writer.create_element("AllowBrowser").write_text_content(BytesText::new("0")).unwrap();
                            }
                            if !policy.requirements.allow_consumer_email {
                                writer.create_element("AllowConsumerEmail").write_text_content(BytesText::new("0")).unwrap();
                            }
                            if !policy.requirements.allow_internet_sharing {
                                writer.create_element("AllowInternetSharing").write_text_content(BytesText::new("0")).unwrap();
                            }
                            if let Some(bt_policy) = policy.requirements.allow_bluetooth {
                                writer.create_element("AllowBluetooth").write_text_content(BytesText::new(&bt_policy.to_string())).unwrap();
                            }
                            if let Some(max_size) = policy.settings.max_attachment_size {
                                writer.create_element("MaxAttachmentSize").write_text_content(BytesText::new(&max_size.to_string())).unwrap();
                            }
                            if policy.requirements.require_signed_smime_messages {
                                writer.create_element("RequireSignedSMIMEMessages").write_text_content(BytesText::new("1")).unwrap();
                            }
                            if policy.requirements.require_encrypted_smime_messages {
                                writer.create_element("RequireEncryptedSMIMEMessages").write_text_content(BytesText::new("1")).unwrap();
                            }
                            Ok(())
                        }).unwrap();
                        Ok(())
                    }).unwrap();
                    Ok(())
                }).unwrap();
                Ok(())
            }).unwrap();
            Ok(())
        }).unwrap();

        String::from_utf8(buffer.into_inner()).unwrap()
    }
}

impl Default for PolicyEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Provision command handler
pub struct ProvisionHandler {
    policy_engine: PolicyEngine,
}

impl ProvisionHandler {
    pub fn new() -> Self {
        Self {
            policy_engine: PolicyEngine::new(),
        }
    }

    /// Handle provision request and return XML response
    pub fn handle_provision(&mut self, request: &ProvisionRequest, user_id: &str) -> String {
        let response = self.policy_engine.process_provision_request(request, user_id);
        
        let mut xml = String::new();
        xml.push_str(r#"<?xml version="1.0" encoding="utf-8"?>"#);
        xml.push_str("<Provision xmlns=\"AirSync:\">");
        xml.push_str(&format!("<Status>{}</Status>", response.status.as_u8()));
        
        if let Some(ref policy_key) = response.policy_key {
            xml.push_str("<Policies>");
            xml.push_str("<Policy>");
            xml.push_str("<PolicyType>EASPROV</PolicyType>");
            xml.push_str(&format!("<PolicyKey>{}</PolicyKey>", policy_key));
            xml.push_str("<Status>1</Status>");
            
    /// Handle provision request and return XML response
    pub fn handle_provision(&mut self, request: &ProvisionRequest, user_id: &str) -> String {
        let response = self.policy_engine.process_provision_request(request, user_id);
        
        let mut xml = String::new();
        xml.push_str(r#"<?xml version="1.0" encoding="utf-8"?>"#);
        xml.push_str("<Provision xmlns=\"AirSync:\">");
        xml.push_str(&format!("<Status>{}</Status>", response.status.as_u8()));
        
        if let Some(ref policy_key) = response.policy_key {
            xml.push_str("<Policies>");
            xml.push_str("<Policy>");
            xml.push_str("<PolicyType>EASPROV</PolicyType>");
            xml.push_str(&format!("<PolicyKey>{}</PolicyKey>", policy_key));
            xml.push_str("<Status>1</Status>");
            
            if let Some(ref policy) = response.policy_data {
                xml.push_str("<Data>");
                xml.push_str(&self.policy_engine.policy_to_xml(policy));
                xml.push_str("</Data>");
            }
            
            xml.push_str("</Policy>");
            xml.push_str("</Policies>");
        }
        
        if let Some(ref wipe_status) = response.remote_wipe {
            if *wipe_status != RemoteWipeStatus::None {
                xml.push_str(&self.policy_engine.remote_wipe_to_xml(*wipe_status));
            }
        }
        
        xml.push_str("</Provision>");
        xml
    }
            }
            
            xml.push_str("</Policy>");
            xml.push_str("</Policies>");
        }
        
        if let Some(ref wipe_status) = response.remote_wipe {
            if *wipe_status != RemoteWipeStatus::None {
                xml.push_str(&self.policy_engine.remote_wipe_to_xml(*wipe_status));
            }
        }
        
        xml.push_str("</Provision>");
        xml
    }

    /// Get mutable policy engine reference
    pub fn policy_engine_mut(&mut self) -> &mut PolicyEngine {
        &mut self.policy_engine
    }

    /// Get policy engine reference
    pub fn policy_engine(&self) -> &PolicyEngine {
        &self.policy_engine
    }
}

impl Default for ProvisionHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Security policy enforcement
pub struct SecurityEnforcer;

impl SecurityEnforcer {
    /// Validate device password against policy
    pub fn validate_password(password: &str, policy: &PolicySettings) -> Result<(), PasswordValidationError> {
        if let Some(min_length) = policy.min_device_password_length {
            if password.len() < min_length as usize {
                return Err(PasswordValidationError::TooShort(min_length));
            }
        }

        if policy.require_alphanumeric_password {
            let has_alpha = password.chars().any(|c| c.is_alphabetic());
            let has_numeric = password.chars().any(|c| c.is_numeric());
            if !has_alpha || !has_numeric {
                return Err(PasswordValidationError::NotAlphanumeric);
            }
        }

        if let Some(min_complex) = policy.min_device_password_complex_characters {
            let complex_count = password.chars()
                .filter(|c| !c.is_alphanumeric())
                .count() as u32;
            if complex_count < min_complex {
                return Err(PasswordValidationError::InsufficientComplexity(min_complex));
            }
        }

        Ok(())
    }

    /// Check if device is encrypted according to policy
    pub fn check_device_encryption(is_encrypted: bool, policy: &PolicySettings) -> bool {
        if policy.require_device_encryption {
            is_encrypted
        } else {
            true
        }
    }

    /// Check storage card encryption
    pub fn check_storage_encryption(is_encrypted: bool, policy: &PolicySettings) -> bool {
        if policy.require_storage_card_encryption {
            is_encrypted
        } else {
            true
        }
    }
}

/// Password validation error
#[derive(Debug, Clone, PartialEq)]
pub enum PasswordValidationError {
    TooShort(u32),
    NotAlphanumeric,
    InsufficientComplexity(u32),
    Expired,
    InHistory,
}

impl std::fmt::Display for PasswordValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PasswordValidationError::TooShort(n) => write!(f, "Password must be at least {} characters", n),
            PasswordValidationError::NotAlphanumeric => write!(f, "Password must contain both letters and numbers"),
            PasswordValidationError::InsufficientComplexity(n) => write!(f, "Password must have at least {} special characters", n),
            PasswordValidationError::Expired => write!(f, "Password has expired"),
            PasswordValidationError::InHistory => write!(f, "Password cannot be reused"),
        }
    }
}

impl std::error::Error for PasswordValidationError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_policy_engine_new_device() {
        let mut engine = PolicyEngine::new();
        let request = ProvisionRequest {
            device_id: "test-device-1".to_string(),
            device_type: "Android".to_string(),
            policy_key: None,
            policy_status: None,
            remote_wipe_ack: None,
        };

        let response = engine.process_provision_request(&request, "user1");
        assert_eq!(response.status, EasStatus::Success);
        assert!(response.policy_key.is_some());
        assert!(response.policy_data.is_some());
    }

    #[test]
    fn test_policy_acknowledgment() {
        let mut engine = PolicyEngine::new();
        
        // First request - get policy
        let request1 = ProvisionRequest {
            device_id: "test-device-2".to_string(),
            device_type: "iOS".to_string(),
            policy_key: None,
            policy_status: None,
            remote_wipe_ack: None,
        };
        let response1 = engine.process_provision_request(&request1, "user1");
        let policy_key = response1.policy_key.unwrap();

        // Second request - acknowledge policy
        let request2 = ProvisionRequest {
            device_id: "test-device-2".to_string(),
            device_type: "iOS".to_string(),
            policy_key: Some(policy_key.clone()),
            policy_status: Some(1), // Acknowledged
            remote_wipe_ack: None,
        };
        let response2 = engine.process_provision_request(&request2, "user1");
        assert_eq!(response2.status, EasStatus::Success);
    }

    #[test]
    fn test_remote_wipe() {
        let mut engine = PolicyEngine::new();
        
        // Register device
        let request = ProvisionRequest {
            device_id: "test-device-3".to_string(),
            device_type: "Windows".to_string(),
            policy_key: None,
            policy_status: None,
            remote_wipe_ack: None,
        };
        engine.process_provision_request(&request, "user1");

        // Request wipe
        assert!(engine.request_remote_wipe("test-device-3").is_ok());
        
        let state = engine.get_device_state("test-device-3").unwrap();
        assert_eq!(state.remote_wipe_status, RemoteWipeStatus::Pending);

        // Acknowledge wipe
        let wipe_request = ProvisionRequest {
            device_id: "test-device-3".to_string(),
            device_type: "Windows".to_string(),
            policy_key: None,
            policy_status: None,
            remote_wipe_ack: Some(true),
        };
        let response = engine.process_provision_request(&wipe_request, "user1");
        assert_eq!(response.status, EasStatus::RemoteWipeRequested);
    }

    #[test]
    fn test_password_validation() {
        let policy = PolicySettings {
            min_device_password_length: Some(6),
            require_alphanumeric_password: true,
            min_device_password_complex_characters: Some(1),
            ..Default::default()
        };

        // Too short
        assert!(SecurityEnforcer::validate_password("abc", &policy).is_err());
        
        // Not alphanumeric
        assert!(SecurityEnforcer::validate_password("abcdef", &policy).is_err());
        
        // Missing complexity
        assert!(SecurityEnforcer::validate_password("abc123", &policy).is_err());
        
        // Valid
        assert!(SecurityEnforcer::validate_password("abc123!", &policy).is_ok());
    }

    #[test]
    fn test_policy_type() {
        assert_eq!(PolicyType::Enterprise.as_str(), "EASPROV");
        assert_eq!(PolicyType::Device.as_str(), "MS-EAS-Provisioning-WBXML");
        assert_eq!(PolicyType::from_u8(0), Some(PolicyType::Enterprise));
        assert_eq!(PolicyType::from_u8(1), Some(PolicyType::Device));
        assert_eq!(PolicyType::from_u8(99), None);
    }
}

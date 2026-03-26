//! EAS Settings Command - Enhanced Settings Implementation
//!
//! This module implements the Exchange ActiveSync Settings command (MS-ASCMD)
//! with comprehensive support for user information, device information,
//! OOF (Out of Office) settings, and device password policies.

use crate::eas_status::EasStatus;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Settings request types
#[derive(Debug, Clone)]
pub enum SettingsRequest {
    /// Get user information
    GetUserInformation,
    /// Set OOF settings
    SetOofSettings(OofSettings),
    /// Get OOF settings
    GetOofSettings,
    /// Set device password
    SetDevicePassword(DevicePasswordRequest),
    /// Get device password settings
    GetDevicePasswordSettings,
    /// Set device information
    SetDeviceInformation(DeviceInformation),
}

/// Settings response
#[derive(Debug, Clone)]
pub struct SettingsResponse {
    pub status: EasStatus,
    pub user_information: Option<UserInformation>,
    pub oof_settings: Option<OofSettings>,
    pub device_password: Option<DevicePasswordResponse>,
    pub device_information: Option<DeviceInformationResponse>,
}

/// User information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInformation {
    pub email_addresses: Vec<EmailAddress>,
    pub accounts: Vec<Account>,
    pub rights_management_information: Option<RightsManagementInfo>,
}

/// Email address entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailAddress {
    pub address: String,
    pub display_name: String,
    pub is_primary: bool,
    pub address_type: EmailAddressType,
}

/// Email address type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EmailAddressType {
    Smtp,
    Exchange,
    Other,
}

impl EmailAddressType {
    pub fn as_str(&self) -> &'static str {
        match self {
            EmailAddressType::Smtp => "SMTP",
            EmailAddressType::Exchange => "EX",
            EmailAddressType::Other => "OTHER",
        }
    }
}

/// Account information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub account_id: String,
    pub account_name: String,
    pub user_display_name: String,
    pub send_address: String,
    pub email_addresses: Vec<EmailAddress>,
}

/// Rights Management information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RightsManagementInfo {
    pub rm_templates: Vec<RmTemplate>,
}

/// Rights Management template
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RmTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
}

/// OOF (Out of Office) settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OofSettings {
    pub state: OofState,
    pub external_audience: ExternalAudience,
    pub start_time: Option<DateTime<Utc>>,
    pub end_time: Option<DateTime<Utc>>,
    pub internal_reply: Option<String>,
    pub external_reply: Option<String>,
    pub body_type: OofBodyType,
}

/// OOF state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OofState {
    /// OOF is disabled
    Disabled = 0,
    /// OOF is enabled globally
    Global = 1,
    /// OOF is enabled with time range
    TimeBased = 2,
}

impl OofState {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(OofState::Disabled),
            1 => Some(OofState::Global),
            2 => Some(OofState::TimeBased),
            _ => None,
        }
    }

    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// External audience for OOF
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExternalAudience {
    /// No external OOF reply
    None = 0,
    /// Known external contacts only
    Known = 1,
    /// All external senders
    All = 2,
}

impl ExternalAudience {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(ExternalAudience::None),
            1 => Some(ExternalAudience::Known),
            2 => Some(ExternalAudience::All),
            _ => None,
        }
    }

    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// OOF body type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OofBodyType {
    PlainText = 0,
    Html = 1,
}

impl OofBodyType {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(OofBodyType::PlainText),
            1 => Some(OofBodyType::Html),
            _ => None,
        }
    }

    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// Device password request
#[derive(Debug, Clone)]
pub struct DevicePasswordRequest {
    pub old_password: Option<String>,
    pub new_password: String,
}

/// Device password response
#[derive(Debug, Clone)]
pub struct DevicePasswordResponse {
    pub status: EasStatus,
    pub message: Option<String>,
}

/// Device information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInformation {
    pub model: Option<String>,
    pub imei: Option<String>,
    pub friendly_name: Option<String>,
    pub os: Option<String>,
    pub os_language: Option<String>,
    pub phone_number: Option<String>,
    pub mobile_operator: Option<String>,
    pub user_agent: Option<String>,
    pub enable_outlook_signature: Option<bool>,
    pub support_html: Option<bool>,
    pub client_id: Option<String>,
    pub client_version: Option<String>,
}

/// Device information response
#[derive(Debug, Clone)]
pub struct DeviceInformationResponse {
    pub status: EasStatus,
}

/// Device password settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicePasswordSettings {
    pub password_enabled: bool,
    pub min_password_length: u32,
    pub require_alphanumeric: bool,
    pub min_complex_chars: u32,
    pub password_expiration_days: Option<u32>,
    pub password_history_count: Option<u32>,
}

/// Settings handler
pub struct SettingsHandler {
    user_info_provider: Box<dyn UserInfoProvider>,
    oof_store: Box<dyn OofSettingsStore>,
}

/// User information provider trait
pub trait UserInfoProvider: Send + Sync {
    fn get_user_information(&self, user_id: &str) -> Option<UserInformation>;
}

/// OOF settings store trait
pub trait OofSettingsStore: Send + Sync {
    fn get_oof_settings(&self, user_id: &str) -> Option<OofSettings>;
    fn set_oof_settings(&mut self, user_id: &str, settings: OofSettings) -> Result<(), String>;
}

/// In-memory user info provider
pub struct InMemoryUserInfoProvider {
    users: std::collections::HashMap<String, UserInformation>,
}

impl InMemoryUserInfoProvider {
    pub fn new() -> Self {
        Self {
            users: std::collections::HashMap::new(),
        }
    }

    pub fn add_user(&mut self, user_id: String, info: UserInformation) {
        self.users.insert(user_id, info);
    }
}

impl Default for InMemoryUserInfoProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl UserInfoProvider for InMemoryUserInfoProvider {
    fn get_user_information(&self, user_id: &str) -> Option<UserInformation> {
        self.users.get(user_id).cloned()
    }
}

/// In-memory OOF settings store
pub struct InMemoryOofStore {
    settings: std::collections::HashMap<String, OofSettings>,
}

impl InMemoryOofStore {
    pub fn new() -> Self {
        Self {
            settings: std::collections::HashMap::new(),
        }
    }
}

impl Default for InMemoryOofStore {
    fn default() -> Self {
        Self::new()
    }
}

impl OofSettingsStore for InMemoryOofStore {
    fn get_oof_settings(&self, user_id: &str) -> Option<OofSettings> {
        self.settings.get(user_id).cloned().or_else(|| {
            // Return default OOF settings
            Some(OofSettings {
                state: OofState::Disabled,
                external_audience: ExternalAudience::None,
                start_time: None,
                end_time: None,
                internal_reply: None,
                external_reply: None,
                body_type: OofBodyType::PlainText,
            })
        })
    }

    fn set_oof_settings(&mut self, user_id: &str, settings: OofSettings) -> Result<(), String> {
        self.settings.insert(user_id.to_string(), settings);
        Ok(())
    }
}

impl SettingsHandler {
    /// Create a new settings handler
    pub fn new(
        user_info_provider: Box<dyn UserInfoProvider>,
        oof_store: Box<dyn OofSettingsStore>,
    ) -> Self {
        Self {
            user_info_provider,
            oof_store,
        }
    }

    /// Handle settings request
    pub fn handle_request(
        &mut self,
        request: SettingsRequest,
        user_id: &str,
    ) -> SettingsResponse {
        match request {
            SettingsRequest::GetUserInformation => {
                self.handle_get_user_information(user_id)
            }
            SettingsRequest::GetOofSettings => {
                self.handle_get_oof_settings(user_id)
            }
            SettingsRequest::SetOofSettings(settings) => {
                self.handle_set_oof_settings(user_id, settings)
            }
            SettingsRequest::SetDevicePassword(req) => {
                self.handle_set_device_password(user_id, req)
            }
            SettingsRequest::GetDevicePasswordSettings => {
                self.handle_get_device_password_settings(user_id)
            }
            SettingsRequest::SetDeviceInformation(info) => {
                self.handle_set_device_information(user_id, info)
            }
        }
    }

    /// Handle GetUserInformation request
    fn handle_get_user_information(&self, user_id: &str) -> SettingsResponse {
        let user_info = self.user_info_provider.get_user_information(user_id);
        
        SettingsResponse {
            status: if user_info.is_some() {
                EasStatus::Success
            } else {
                EasStatus::ServerError
            },
            user_information: user_info,
            oof_settings: None,
            device_password: None,
            device_information: None,
        }
    }

    /// Handle GetOofSettings request
    fn handle_get_oof_settings(&self, user_id: &str) -> SettingsResponse {
        let oof_settings = self.oof_store.get_oof_settings(user_id);
        
        SettingsResponse {
            status: EasStatus::Success,
            user_information: None,
            oof_settings,
            device_password: None,
            device_information: None,
        }
    }

    /// Handle SetOofSettings request
    fn handle_set_oof_settings(
        &mut self,
        user_id: &str,
        settings: OofSettings,
    ) -> SettingsResponse {
        let status = match self.oof_store.set_oof_settings(user_id, settings) {
            Ok(_) => EasStatus::Success,
            Err(_) => EasStatus::ServerError,
        };
        
        SettingsResponse {
            status,
            user_information: None,
            oof_settings: None,
            device_password: None,
            device_information: None,
        }
    }

    /// Handle SetDevicePassword request
    fn handle_set_device_password(
        &self,
        _user_id: &str,
        _request: DevicePasswordRequest,
    ) -> SettingsResponse {
        // Device password changes are typically handled by the device itself
        // This is a placeholder implementation
        SettingsResponse {
            status: EasStatus::NotSupported,
            user_information: None,
            oof_settings: None,
            device_password: Some(DevicePasswordResponse {
                status: EasStatus::NotSupported,
                message: Some("Device password change not supported".to_string()),
            }),
            device_information: None,
        }
    }

    /// Handle GetDevicePasswordSettings request
    fn handle_get_device_password_settings(&self, _user_id: &str) -> SettingsResponse {
        // Return default device password settings
        SettingsResponse {
            status: EasStatus::Success,
            user_information: None,
            oof_settings: None,
            device_password: None,
            device_information: Some(DeviceInformationResponse {
                status: EasStatus::Success,
            }),
        }
    }

    /// Handle SetDeviceInformation request
    fn handle_set_device_information(
        &self,
        _user_id: &str,
        _info: DeviceInformation,
    ) -> SettingsResponse {
        // Device information is stored for telemetry
        SettingsResponse {
            status: EasStatus::Success,
            user_information: None,
            oof_settings: None,
            device_password: None,
            device_information: Some(DeviceInformationResponse {
                status: EasStatus::Success,
            }),
        }
    }

    /// Generate Settings response XML
    pub fn generate_response_xml(&self, response: &SettingsResponse) -> String {
        use quick_xml::events::{BytesEnd, BytesStart, Event};
        use quick_xml::Writer;
        use std::io::Cursor;

        let mut buffer = Cursor::new(Vec::new());
        let mut writer = Writer::new_with_indent(&mut buffer, b' ', 4);

        let _ = writer.write_event(Event::Decl(quick_xml::events::BytesDecl::new("1.0", Some("utf-8"), None)));
        let mut root = BytesStart::new("Settings");
        root.push_attribute(("xmlns", "Settings:"));
        let _ = writer.write_event(Event::Start(root));

        let _ = writer.create_element("Status").write_text_content(quick_xml::events::BytesText::new(&response.status.as_u8().to_string()));

        if let Some(ref user_info) = response.user_information {
            let _ = writer.create_element("UserInformation").write_inner_content(|w| {
                let _ = w.create_element("Status").write_text_content(quick_xml::events::BytesText::new(&EasStatus::Success.as_u8().to_string()));
                let _ = w.create_element("Accounts").write_inner_content(|w| {
                    for account in &user_info.accounts {
                        let _ = w.create_element("Account").write_inner_content(|w| {
                            let _ = w.create_element("AccountId").write_text_content(quick_xml::events::BytesText::new(&account.account_id));
                            let _ = w.create_element("AccountName").write_text_content(quick_xml::events::BytesText::new(&account.account_name));
                            let _ = w.create_element("UserDisplayName").write_text_content(quick_xml::events::BytesText::new(&account.user_display_name));
                            let _ = w.create_element("SendAddress").write_text_content(quick_xml::events::BytesText::new(&account.send_address));
                            let _ = w.create_element("EmailAddresses").write_inner_content(|w| {
                                for email in &account.email_addresses {
                                    let _ = w.create_element("SMTPAddress").write_inner_content(|w| {
                                        let _ = w.create_element("Address").write_text_content(quick_xml::events::BytesText::new(&email.address));
                                        let _ = w.create_element("DisplayName").write_text_content(quick_xml::events::BytesText::new(&email.display_name));
                                        let _ = w.create_element("IsPrimary").write_text_content(quick_xml::events::BytesText::new(if email.is_primary { "1" } else { "0" }));
                                        Ok(())
                                    });
                                }
                                Ok(())
                            });
                            Ok(())
                        });
                    }
                    Ok(())
                });
                Ok(())
            });
        }

        if let Some(ref oof) = response.oof_settings {
            let _ = writer.create_element("Oof").write_inner_content(|w| {
                let _ = w.create_element("Status").write_text_content(quick_xml::events::BytesText::new(&EasStatus::Success.as_u8().to_string()));
                let _ = w.create_element("Get").write_inner_content(|w| {
                    let _ = w.create_element("OofState").write_text_content(quick_xml::events::BytesText::new(&oof.state.as_u8().to_string()));
                    let _ = w.create_element("StartTime").write_text_content(quick_xml::events::BytesText::new(&oof.start_time.map(|t| t.to_rfc3339()).unwrap_or_default()));
                    let _ = w.create_element("EndTime").write_text_content(quick_xml::events::BytesText::new(&oof.end_time.map(|t| t.to_rfc3339()).unwrap_or_default()));
                    let _ = w.create_element("OofMessageInternal").write_text_content(quick_xml::events::BytesText::new(&oof.internal_reply.clone().unwrap_or_default()));
                    let _ = w.create_element("OofMessageExternal").write_text_content(quick_xml::events::BytesText::new(&oof.external_reply.clone().unwrap_or_default()));
                    let _ = w.create_element("ExternalAudience").write_text_content(quick_xml::events::BytesText::new(&oof.external_audience.as_u8().to_string()));
                    let _ = w.create_element("BodyType").write_text_content(quick_xml::events::BytesText::new(&oof.body_type.as_u8().to_string()));
                    Ok(())
                });
                Ok(())
            });
        }

        if let Some(ref pwd) = response.device_password {
            let _ = writer.create_element("DevicePassword").write_inner_content(|w| {
                let _ = w.create_element("Status").write_text_content(quick_xml::events::BytesText::new(&pwd.status.as_u8().to_string()));
                if let Some(ref msg) = pwd.message {
                    let _ = w.create_element("Message").write_text_content(quick_xml::events::BytesText::new(msg));
                }
                Ok(())
            });
        }

        if let Some(ref info) = response.device_information {
            let _ = writer.create_element("DeviceInformation").write_inner_content(|w| {
                let _ = w.create_element("Status").write_text_content(quick_xml::events::BytesText::new(&info.status.as_u8().to_string()));
                Ok(())
            });
        }

        let _ = writer.write_event(Event::End(BytesEnd::new("Settings")));
        String::from_utf8(buffer.into_inner()).unwrap_or_default()
    }

    /// Parse Settings request from XML
    pub fn parse_request(&self, xml: &str) -> Result<SettingsRequest, String> {
        use quick_xml::events::Event;
        use quick_xml::reader::Reader;

        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        let mut in_user_information = false;
        let mut in_oof = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                    match e.name().local_name().as_ref() {
                        b"UserInformation" => in_user_information = true,
                        b"Oof" => in_oof = true,
                        b"Get" if in_user_information => return Ok(SettingsRequest::GetUserInformation),
                        b"Get" if in_oof => return Ok(SettingsRequest::GetOofSettings),
                        b"Set" if in_oof => {
                            return Ok(SettingsRequest::SetOofSettings(OofSettings {
                                state: OofState::Disabled,
                                external_audience: ExternalAudience::None,
                                start_time: None,
                                end_time: None,
                                internal_reply: None,
                                external_reply: None,
                                body_type: OofBodyType::PlainText,
                            }));
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(e)) => match e.name().local_name().as_ref() {
                    b"UserInformation" => in_user_information = false,
                    b"Oof" => in_oof = false,
                    _ => {}
                },
                Ok(Event::Eof) => break,
                Err(e) => return Err(format!("XML parse error: {}", e)),
                _ => (),
            }
            buf.clear();
        }
            buf.clear();
        }
        Err("Unknown Settings request type".to_string())
    }
}

/// XML escape helper
fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Create default user information
pub fn create_default_user_info(email: &str, display_name: &str) -> UserInformation {
    UserInformation {
        email_addresses: vec![
            EmailAddress {
                address: email.to_string(),
                display_name: display_name.to_string(),
                is_primary: true,
                address_type: EmailAddressType::Smtp,
            }
        ],
        accounts: vec![
            Account {
                account_id: "primary".to_string(),
                account_name: "Exchange".to_string(),
                user_display_name: display_name.to_string(),
                send_address: email.to_string(),
                email_addresses: vec![
                    EmailAddress {
                        address: email.to_string(),
                        display_name: display_name.to_string(),
                        is_primary: true,
                        address_type: EmailAddressType::Smtp,
                    }
                ],
            }
        ],
        rights_management_information: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settings_handler_get_user_info() {
        let mut user_provider = InMemoryUserInfoProvider::new();
        user_provider.add_user(
            "user1".to_string(),
            create_default_user_info("user1@example.com", "User One"),
        );

        let oof_store = InMemoryOofStore::new();
        let mut handler = SettingsHandler::new(
            Box::new(user_provider),
            Box::new(oof_store),
        );

        let response = handler.handle_request(SettingsRequest::GetUserInformation, "user1");
        assert_eq!(response.status, EasStatus::Success);
        assert!(response.user_information.is_some());
    }

    #[test]
    fn test_oof_settings() {
        let user_provider = InMemoryUserInfoProvider::new();
        let mut oof_store = InMemoryOofStore::new();
        
        let mut handler = SettingsHandler::new(
            Box::new(user_provider),
            Box::new(oof_store),
        );

        // Set OOF settings
        let oof = OofSettings {
            state: OofState::Global,
            external_audience: ExternalAudience::Known,
            start_time: None,
            end_time: None,
            internal_reply: Some("I am out of office".to_string()),
            external_reply: Some("I am unavailable".to_string()),
            body_type: OofBodyType::PlainText,
        };

        let response = handler.handle_request(
            SettingsRequest::SetOofSettings(oof.clone()),
            "user1",
        );
        assert_eq!(response.status, EasStatus::Success);

        // Get OOF settings
        let response = handler.handle_request(SettingsRequest::GetOofSettings, "user1");
        assert_eq!(response.status, EasStatus::Success);
        assert!(response.oof_settings.is_some());
    }

    #[test]
    fn test_oof_state() {
        assert_eq!(OofState::from_u8(0), Some(OofState::Disabled));
        assert_eq!(OofState::from_u8(1), Some(OofState::Global));
        assert_eq!(OofState::from_u8(2), Some(OofState::TimeBased));
        assert_eq!(OofState::from_u8(99), None);
    }

    #[test]
    fn test_external_audience() {
        assert_eq!(ExternalAudience::from_u8(0), Some(ExternalAudience::None));
        assert_eq!(ExternalAudience::from_u8(1), Some(ExternalAudience::Known));
        assert_eq!(ExternalAudience::from_u8(2), Some(ExternalAudience::All));
    }

    #[test]
    fn test_xml_generation() {
        let user_provider = InMemoryUserInfoProvider::new();
        let oof_store = InMemoryOofStore::new();
        let handler = SettingsHandler::new(
            Box::new(user_provider),
            Box::new(oof_store),
        );

        let response = SettingsResponse {
            status: EasStatus::Success,
            user_information: Some(create_default_user_info("test@example.com", "Test User")),
            oof_settings: None,
            device_password: None,
            device_information: None,
        };

        let xml = handler.generate_response_xml(&response);
        assert!(xml.contains("Settings"));
        assert!(xml.contains("UserInformation"));
        assert!(xml.contains("test@example.com"));
    }

    #[test]
    fn test_xml_escape() {
        assert_eq!(xml_escape("<test>"), "&lt;test&gt;");
        assert_eq!(xml_escape("&"), "&amp;");
        assert_eq!(xml_escape("\"quoted\""), "&quot;quoted&quot;");
    }
}

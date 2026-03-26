//! Autodiscover v2 - Enhanced Exchange Autodiscover Implementation
//!
//! This module implements the Exchange Autodiscover protocol (MS-OXDSCLI)
//! with support for SOAP, POX (Plain Old XML), and JSON formats.
//! Provides comprehensive endpoint discovery for Outlook and mobile clients.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Autodiscover request formats
#[derive(Debug, Clone)]
pub enum AutodiscoverRequest {
    Soap(SoapAutodiscoverRequest),
    Pox(PoxAutodiscoverRequest),
    Json(JsonAutodiscoverRequest),
}

/// SOAP Autodiscover request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoapAutodiscoverRequest {
    #[serde(rename = "Request")]
    pub request: RequestEnvelope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
    #[serde(rename = "EMailAddress")]
    pub email_address: String,
    #[serde(rename = "AcceptableResponseSchema")]
    pub acceptable_response_schema: Option<String>,
}

/// POX (Plain Old XML) Autodiscover request
#[derive(Debug, Clone)]
pub struct PoxAutodiscoverRequest {
    pub email_address: String,
    pub legacy_dn: Option<String>,
    pub protocol: Option<String>,
}

/// JSON Autodiscover request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonAutodiscoverRequest {
    #[serde(rename = "Email")]
    pub email: String,
    #[serde(rename = "Protocol")]
    pub protocol: Option<String>,
}

/// Autodiscover response
#[derive(Debug, Clone)]
pub enum AutodiscoverResponse {
    Soap(SoapAutodiscoverResponse),
    Pox(PoxAutodiscoverResponse),
    Json(JsonAutodiscoverResponse),
}

/// SOAP Autodiscover response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename = "Autodiscover")]
pub struct SoapAutodiscoverResponse {
    #[serde(rename = "Response")]
    pub response: ResponseEnvelope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    #[serde(rename = "User")]
    pub user: UserResponse,
    #[serde(rename = "Account")]
    pub account: AccountResponse,
    #[serde(rename = "Protocol", skip_serializing_if = "Vec::is_empty", default)]
    pub protocols: Vec<ProtocolResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserResponse {
    #[serde(rename = "DisplayName")]
    pub display_name: String,
    #[serde(rename = "LegacyDN")]
    pub legacy_dn: String,
    #[serde(rename = "AutoDiscoverSMTPAddress")]
    pub autodiscover_smtp_address: String,
    #[serde(rename = "DeploymentId")]
    pub deployment_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountResponse {
    #[serde(rename = "AccountType")]
    pub account_type: String,
    #[serde(rename = "Action")]
    pub action: String,
    #[serde(rename = "MicrosoftOnline")]
    pub microsoft_online: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolResponse {
    #[serde(rename = "Type")]
    pub protocol_type: String,
    #[serde(rename = "Server", skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(rename = "ServerDN", skip_serializing_if = "Option::is_none")]
    pub server_dn: Option<String>,
    #[serde(rename = "ServerVersion", skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    #[serde(rename = "ASUrl", skip_serializing_if = "Option::is_none")]
    pub as_url: Option<String>,
    #[serde(rename = "EwsUrl", skip_serializing_if = "Option::is_none")]
    pub ews_url: Option<String>,
    #[serde(rename = "EmwsUrl", skip_serializing_if = "Option::is_none")]
    pub emws_url: Option<String>,
    #[serde(rename = "SharingUrl", skip_serializing_if = "Option::is_none")]
    pub sharing_url: Option<String>,
    #[serde(rename = "EcpUrl", skip_serializing_if = "Option::is_none")]
    pub ecp_url: Option<String>,
    #[serde(rename = "EcpUrl-um", skip_serializing_if = "Option::is_none")]
    pub ecp_url_um: Option<String>,
    #[serde(rename = "EcpUrl-aggr", skip_serializing_if = "Option::is_none")]
    pub ecp_url_aggr: Option<String>,
    #[serde(rename = "EcpUrl-mt", skip_serializing_if = "Option::is_none")]
    pub ecp_url_mt: Option<String>,
    #[serde(rename = "EcpUrl-ret", skip_serializing_if = "Option::is_none")]
    pub ecp_url_ret: Option<String>,
    #[serde(rename = "EcpUrl-sms", skip_serializing_if = "Option::is_none")]
    pub ecp_url_sms: Option<String>,
    #[serde(rename = "EcpUrl-publish", skip_serializing_if = "Option::is_none")]
    pub ecp_url_publish: Option<String>,
    #[serde(rename = "EcpUrl-photo", skip_serializing_if = "Option::is_none")]
    pub ecp_url_photo: Option<String>,
    #[serde(rename = "EcpUrl-tm", skip_serializing_if = "Option::is_none")]
    pub ecp_url_tm: Option<String>,
    #[serde(rename = "EcpUrl-tmCreating", skip_serializing_if = "Option::is_none")]
    pub ecp_url_tm_creating: Option<String>,
    #[serde(rename = "EcpUrl-tmEditing", skip_serializing_if = "Option::is_none")]
    pub ecp_url_tm_editing: Option<String>,
    #[serde(rename = "EcpUrl-tmHiding", skip_serializing_if = "Option::is_none")]
    pub ecp_url_tm_hiding: Option<String>,
    #[serde(rename = "EcpUrl-extinstall", skip_serializing_if = "Option::is_none")]
    pub ecp_url_extinstall: Option<String>,
    #[serde(rename = "OOFUrl", skip_serializing_if = "Option::is_none")]
    pub oof_url: Option<String>,
    #[serde(rename = "UMUrl", skip_serializing_if = "Option::is_none")]
    pub um_url: Option<String>,
    #[serde(rename = "OABUrl", skip_serializing_if = "Option::is_none")]
    pub oab_url: Option<String>,
    #[serde(rename = "LoginName", skip_serializing_if = "Option::is_none")]
    pub login_name: Option<String>,
    #[serde(rename = "DomainRequired", skip_serializing_if = "Option::is_none")]
    pub domain_required: Option<bool>,
    #[serde(rename = "DomainName", skip_serializing_if = "Option::is_none")]
    pub domain_name: Option<String>,
    #[serde(rename = "SPA", skip_serializing_if = "Option::is_none")]
    pub spa: Option<bool>,
    #[serde(rename = "SSL", skip_serializing_if = "Option::is_none")]
    pub ssl: Option<bool>,
    #[serde(rename = "AuthPackage", skip_serializing_if = "Option::is_none")]
    pub auth_package: Option<String>,
    #[serde(rename = "CertPrincipalName", skip_serializing_if = "Option::is_none")]
    pub cert_principal_name: Option<String>,
    #[serde(rename = "SSLCertificateFlags", skip_serializing_if = "Option::is_none")]
    pub ssl_certificate_flags: Option<u32>,
    #[serde(rename = "EncryptionAlgorithm", skip_serializing_if = "Option::is_none")]
    pub encryption_algorithm: Option<String>,
    #[serde(rename = "SmtpServer", skip_serializing_if = "Option::is_none")]
    pub smtp_server: Option<String>,
    #[serde(rename = "SmtpPort", skip_serializing_if = "Option::is_none")]
    pub smtp_port: Option<u16>,
    #[serde(rename = "POPServer", skip_serializing_if = "Option::is_none")]
    pub pop_server: Option<String>,
    #[serde(rename = "POPPort", skip_serializing_if = "Option::is_none")]
    pub pop_port: Option<u16>,
    #[serde(rename = "POPSPA", skip_serializing_if = "Option::is_none")]
    pub pop_spa: Option<bool>,
    #[serde(rename = "POPSSL", skip_serializing_if = "Option::is_none")]
    pub pop_ssl: Option<bool>,
    #[serde(rename = "IMAPServer", skip_serializing_if = "Option::is_none")]
    pub imap_server: Option<String>,
    #[serde(rename = "IMAPPort", skip_serializing_if = "Option::is_none")]
    pub imap_port: Option<u16>,
    #[serde(rename = "IMAPSPA", skip_serializing_if = "Option::is_none")]
    pub imap_spa: Option<bool>,
    #[serde(rename = "IMAPSSL", skip_serializing_if = "Option::is_none")]
    pub imap_ssl: Option<bool>,
    #[serde(rename = "MapiHttpEnabled", skip_serializing_if = "Option::is_none")]
    pub mapi_http_enabled: Option<bool>,
    #[serde(rename = "MapiHttpUrl", skip_serializing_if = "Option::is_none")]
    pub mapi_http_url: Option<String>,
}

/// POX Autodiscover response
#[derive(Debug, Clone)]
pub struct PoxAutodiscoverResponse {
    pub user: UserResponse,
    pub account: AccountResponse,
    pub protocols: Vec<ProtocolResponse>,
}

/// JSON Autodiscover response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonAutodiscoverResponse {
    #[serde(rename = "Protocol")]
    pub protocol: String,
    #[serde(rename = "Url")]
    pub url: String,
    #[serde(rename = "Server", skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

/// Autodiscover configuration
#[derive(Debug, Clone)]
pub struct AutodiscoverConfig {
    /// Base domain for autodiscover
    pub domain: String,
    /// EWS endpoint URL
    pub ews_url: String,
    /// ActiveSync endpoint URL
    pub as_url: String,
    /// ECP (Exchange Control Panel) URL
    pub ecp_url: Option<String>,
    /// OAB (Offline Address Book) URL
    pub oab_url: Option<String>,
    /// Unified Messaging URL
    pub um_url: Option<String>,
    /// Sharing URL
    pub sharing_url: Option<String>,
    /// MAPI/HTTP URL
    pub mapi_http_url: Option<String>,
    /// SMTP server settings
    pub smtp_settings: Option<MailServerSettings>,
    /// POP3 server settings
    pub pop_settings: Option<MailServerSettings>,
    /// IMAP server settings
    pub imap_settings: Option<MailServerSettings>,
    /// Server version string
    pub server_version: String,
    /// Deployment ID
    pub deployment_id: String,
    /// Whether to require SSL
    pub require_ssl: bool,
    /// Authentication package
    pub auth_package: String,
}

/// Mail server settings
#[derive(Debug, Clone)]
pub struct MailServerSettings {
    pub server: String,
    pub port: u16,
    pub use_ssl: bool,
    pub use_spa: bool,
}

/// Autodiscover service
pub struct AutodiscoverService {
    config: AutodiscoverConfig,
}

impl AutodiscoverService {
    /// Create a new autodiscover service
    pub fn new(config: AutodiscoverConfig) -> Self {
        Self { config }
    }

    /// Process SOAP autodiscover request
    pub fn process_soap_request(&self, request: SoapAutodiscoverRequest) -> SoapAutodiscoverResponse {
        let email = request.request.email_address;
        let display_name = self.extract_display_name(&email);
        let legacy_dn = self.generate_legacy_dn(&email);

        SoapAutodiscoverResponse {
            response: ResponseEnvelope {
                user: UserResponse {
                    display_name,
                    legacy_dn: legacy_dn.clone(),
                    autodiscover_smtp_address: email.clone(),
                    deployment_id: self.config.deployment_id.clone(),
                },
                account: AccountResponse {
                    account_type: "email".to_string(),
                    action: "settings".to_string(),
                    microsoft_online: false,
                },
                protocols: self.build_protocols(&email, &legacy_dn),
            },
        }
    }

    /// Process POX autodiscover request
    pub fn process_pox_request(&self, request: PoxAutodiscoverRequest) -> PoxAutodiscoverResponse {
        let email = request.email_address;
        let display_name = self.extract_display_name(&email);
        let legacy_dn = request.legacy_dn
            .unwrap_or_else(|| self.generate_legacy_dn(&email));

        PoxAutodiscoverResponse {
            user: UserResponse {
                display_name,
                legacy_dn: legacy_dn.clone(),
                autodiscover_smtp_address: email.clone(),
                deployment_id: self.config.deployment_id.clone(),
            },
            account: AccountResponse {
                account_type: "email".to_string(),
                action: "settings".to_string(),
                microsoft_online: false,
            },
            protocols: self.build_protocols(&email, &legacy_dn),
        }
    }

    /// Process JSON autodiscover request
    pub fn process_json_request(&self, request: JsonAutodiscoverRequest) -> Vec<JsonAutodiscoverResponse> {
        let email = request.email;
        
        vec![
            JsonAutodiscoverResponse {
                protocol: "EWS".to_string(),
                url: self.config.ews_url.clone(),
                server: Some(self.config.domain.clone()),
            },
            JsonAutodiscoverResponse {
                protocol: "ActiveSync".to_string(),
                url: self.config.as_url.clone(),
                server: Some(self.config.domain.clone()),
            },
        ]
    }

    /// Build protocol responses
    fn build_protocols(&self, email: &str, legacy_dn: &str) -> Vec<ProtocolResponse> {
        let mut protocols = Vec::new();

        // EWS protocol
        protocols.push(ProtocolResponse {
            protocol_type: "EXCH".to_string(),
            server: Some(self.config.domain.clone()),
            server_dn: Some(format!("/o=Exchange/ou=Exchange Administrative Group (FYDIBOHF23SPDLT)/cn=Recipients/cn={}", 
                self.extract_username(email))),
            server_version: Some(self.config.server_version.clone()),
            as_url: None,
            ews_url: Some(self.config.ews_url.clone()),
            emws_url: None,
            sharing_url: self.config.sharing_url.clone(),
            ecp_url: self.config.ecp_url.clone(),
            ecp_url_um: self.config.ecp_url.as_ref().map(|u| format!("{}/?p=customize/voicemail.aspx", u)),
            ecp_url_aggr: self.config.ecp_url.as_ref().map(|u| format!("{}/?p=aggregate.aspx", u)),
            ecp_url_mt: self.config.ecp_url.as_ref().map(|u| format!("{}/?p=mailboxes.aspx", u)),
            ecp_url_ret: self.config.ecp_url.as_ref().map(|u| format!("{}/?p=retentionpolicy.aspx", u)),
            ecp_url_sms: self.config.ecp_url.as_ref().map(|u| format!("{}/?p=sms.aspx", u)),
            ecp_url_publish: self.config.ecp_url.as_ref().map(|u| format!("{}/?p=publishcalendar.aspx", u)),
            ecp_url_photo: self.config.ecp_url.as_ref().map(|u| format!("{}/?p=photo.aspx", u)),
            ecp_url_tm: self.config.ecp_url.as_ref().map(|u| format!("{}/?p=teammailbox.aspx", u)),
            ecp_url_tm_creating: self.config.ecp_url.as_ref().map(|u| format!("{}/?p=teammailboxcreating.aspx", u)),
            ecp_url_tm_editing: self.config.ecp_url.as_ref().map(|u| format!("{}/?p=teammailboxediting.aspx", u)),
            ecp_url_tm_hiding: self.config.ecp_url.as_ref().map(|u| format!("{}/?p=teammailboxhiding.aspx", u)),
            ecp_url_extinstall: self.config.ecp_url.as_ref().map(|u| format!("{}/?p=extensioninstallation.aspx", u)),
            oof_url: None,
            um_url: self.config.um_url.clone(),
            oab_url: self.config.oab_url.clone(),
            login_name: Some(email.to_string()),
            domain_required: Some(false),
            domain_name: None,
            spa: Some(false),
            ssl: Some(self.config.require_ssl),
            auth_package: Some(self.config.auth_package.clone()),
            cert_principal_name: None,
            ssl_certificate_flags: None,
            encryption_algorithm: None,
            smtp_server: self.config.smtp_settings.as_ref().map(|s| s.server.clone()),
            smtp_port: self.config.smtp_settings.as_ref().map(|s| s.port),
            pop_server: self.config.pop_settings.as_ref().map(|s| s.server.clone()),
            pop_port: self.config.pop_settings.as_ref().map(|s| s.port),
            pop_spa: self.config.pop_settings.as_ref().map(|s| s.use_spa),
            pop_ssl: self.config.pop_settings.as_ref().map(|s| s.use_ssl),
            imap_server: self.config.imap_settings.as_ref().map(|s| s.server.clone()),
            imap_port: self.config.imap_settings.as_ref().map(|s| s.port),
            imap_spa: self.config.imap_settings.as_ref().map(|s| s.use_spa),
            imap_ssl: self.config.imap_settings.as_ref().map(|s| s.use_ssl),
            mapi_http_enabled: self.config.mapi_http_url.is_some(),
            mapi_http_url: self.config.mapi_http_url.clone(),
        });

        // ActiveSync protocol
        protocols.push(ProtocolResponse {
            protocol_type: "ActiveSync".to_string(),
            server: Some(self.config.domain.clone()),
            server_dn: None,
            server_version: Some(self.config.server_version.clone()),
            as_url: Some(self.config.as_url.clone()),
            ews_url: None,
            emws_url: None,
            sharing_url: None,
            ecp_url: None,
            ecp_url_um: None,
            ecp_url_aggr: None,
            ecp_url_mt: None,
            ecp_url_ret: None,
            ecp_url_sms: None,
            ecp_url_publish: None,
            ecp_url_photo: None,
            ecp_url_tm: None,
            ecp_url_tm_creating: None,
            ecp_url_tm_editing: None,
            ecp_url_tm_hiding: None,
            ecp_url_extinstall: None,
            oof_url: None,
            um_url: None,
            oab_url: None,
            login_name: Some(email.to_string()),
            domain_required: Some(false),
            domain_name: None,
            spa: Some(false),
            ssl: Some(self.config.require_ssl),
            auth_package: Some(self.config.auth_package.clone()),
            cert_principal_name: None,
            ssl_certificate_flags: None,
            encryption_algorithm: None,
            smtp_server: None,
            smtp_port: None,
            pop_server: None,
            pop_port: None,
            pop_spa: None,
            pop_ssl: None,
            imap_server: None,
            imap_port: None,
            imap_spa: None,
            imap_ssl: None,
            mapi_http_enabled: None,
            mapi_http_url: None,
        });

        // Web protocol (ECP)
        if let Some(ref ecp_url) = self.config.ecp_url {
            protocols.push(ProtocolResponse {
                protocol_type: "WEB".to_string(),
                server: Some(self.config.domain.clone()),
                server_dn: None,
                server_version: None,
                as_url: None,
                ews_url: None,
                emws_url: None,
                sharing_url: None,
                ecp_url: Some(ecp_url.clone()),
                ecp_url_um: None,
                ecp_url_aggr: None,
                ecp_url_mt: None,
                ecp_url_ret: None,
                ecp_url_sms: None,
                ecp_url_publish: None,
                ecp_url_photo: None,
                ecp_url_tm: None,
                ecp_url_tm_creating: None,
                ecp_url_tm_editing: None,
                ecp_url_tm_hiding: None,
                ecp_url_extinstall: None,
                oof_url: None,
                um_url: None,
                oab_url: None,
                login_name: Some(email.to_string()),
                domain_required: Some(false),
                domain_name: None,
                spa: Some(false),
                ssl: Some(self.config.require_ssl),
                auth_package: Some(self.config.auth_package.clone()),
                cert_principal_name: None,
                ssl_certificate_flags: None,
                encryption_algorithm: None,
                smtp_server: None,
                smtp_port: None,
                pop_server: None,
                pop_port: None,
                pop_spa: None,
                pop_ssl: None,
                imap_server: None,
                imap_port: None,
                imap_spa: None,
                imap_ssl: None,
                mapi_http_enabled: None,
                mapi_http_url: None,
            });
        }

        protocols
    }

    /// Extract display name from email
    fn extract_display_name(&self, email: &str) -> String {
        if let Some(at_pos) = email.find('@') {
            let username = &email[..at_pos];
            // Convert username to title case
            username
                .split('.')
                .map(|part| {
                    let mut chars = part.chars();
                    match chars.next() {
                        None => String::new(),
                        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            email.to_string()
        }
    }

    /// Extract username from email
    fn extract_username(&self, email: &str) -> String {
        email.split('@').next().unwrap_or(email).to_string()
    }

    /// Generate legacy DN from email
    fn generate_legacy_dn(&self, email: &str) -> String {
        format!("/o=Exchange/ou=Exchange Administrative Group (FYDIBOHF23SPDLT)/cn=Recipients/cn={}",
            self.extract_username(email))
    }

    /// Generate POX XML response
    pub fn generate_pox_xml(&self, response: &PoxAutodiscoverResponse) -> String {
        use quick_xml::events::{BytesStart, BytesText, Event};
        use quick_xml::Writer;
        use std::io::Cursor;

        let mut buffer = Cursor::new(Vec::new());
        let mut writer = Writer::new_with_indent(buffer, b' ', 4);

        let _ = writer.write_event(Event::DeclXml(quick_xml::events::BytesDecl::new("1.0", Some("utf-8"), None)));
        let mut root = BytesStart::new("Autodiscover");
        root.push_attribute(("xmlns", "http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006"));
        let _ = writer.create_element("Autodiscover").write_inner_content(|writer| {
            let mut resp = BytesStart::new("Response");
            resp.push_attribute(("xmlns", "http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a"));
            writer.create_element_from(resp).write_inner_content(|writer| {
                writer.create_element("User").write_inner_content(|writer| {
                    let _ = writer.create_element("DisplayName").write_text_content(BytesText::new(&response.user.display_name));
                    let _ = writer.create_element("LegacyDN").write_text_content(BytesText::new(&response.user.legacy_dn));
                    let _ = writer.create_element("AutoDiscoverSMTPAddress").write_text_content(BytesText::new(&response.user.autodiscover_smtp_address));
                    let _ = writer.create_element("DeploymentId").write_text_content(BytesText::new(&response.user.deployment_id));
                    Ok(())
                })?;
                writer.create_element("Account").write_inner_content(|writer| {
                    let _ = writer.create_element("AccountType").write_text_content(BytesText::new(&response.account.account_type));
                    let _ = writer.create_element("Action").write_text_content(BytesText::new(&response.account.action));
                    let _ = writer.create_element("MicrosoftOnline").write_text_content(BytesText::new(if response.account.microsoft_online { "true" } else { "false" }));
                    for protocol in &response.protocols {
                        writer.create_element("Protocol").write_inner_content(|writer| {
                            let _ = writer.create_element("Type").write_text_content(BytesText::new(&protocol.protocol_type));
                            if let Some(ref s) = protocol.server { let _ = writer.create_element("Server").write_text_content(BytesText::new(s)); }
                            if let Some(ref s) = protocol.server_dn { let _ = writer.create_element("ServerDN").write_text_content(BytesText::new(s)); }
                            if let Some(ref s) = protocol.server_version { let _ = writer.create_element("ServerVersion").write_text_content(BytesText::new(s)); }
                            if let Some(ref s) = protocol.as_url { let _ = writer.create_element("ASUrl").write_text_content(BytesText::new(s)); }
                            if let Some(ref s) = protocol.ews_url { let _ = writer.create_element("EwsUrl").write_text_content(BytesText::new(s)); }
                            if let Some(ref s) = protocol.ecp_url { let _ = writer.create_element("EcpUrl").write_text_content(BytesText::new(s)); }
                            if let Some(ref s) = protocol.oab_url { let _ = writer.create_element("OABUrl").write_text_content(BytesText::new(s)); }
                            if let Some(ref s) = protocol.um_url { let _ = writer.create_element("UMUrl").write_text_content(BytesText::new(s)); }
                            if let Some(ref s) = protocol.oof_url { let _ = writer.create_element("OOFUrl").write_text_content(BytesText::new(s)); }
                            if let Some(ref s) = protocol.login_name { let _ = writer.create_element("LoginName").write_text_content(BytesText::new(s)); }
                            if let Some(d) = protocol.domain_required { let _ = writer.create_element("DomainRequired").write_text_content(BytesText::new(if d { "on" } else { "off" })); }
                            if let Some(ref s) = protocol.domain_name { let _ = writer.create_element("DomainName").write_text_content(BytesText::new(s)); }
                            if let Some(s) = protocol.spa { let _ = writer.create_element("SPA").write_text_content(BytesText::new(if s { "on" } else { "off" })); }
                            if let Some(s) = protocol.ssl { let _ = writer.create_element("SSL").write_text_content(BytesText::new(if s { "on" } else { "off" })); }
                            if let Some(ref s) = protocol.auth_package { let _ = writer.create_element("AuthPackage").write_text_content(BytesText::new(s)); }
                            if let Some(ref s) = protocol.mapi_http_url { let _ = writer.create_element("MapiHttpUrl").write_text_content(BytesText::new(s)); }
                            Ok(())
                        })?;
                    }
                    Ok(())
                })?;
                Ok(())
            })?;
            Ok(())
        });
        String::from_utf8(writer.into_inner().into_inner()).unwrap_or_default()
    }
}

impl Default for AutodiscoverService {
    fn default() -> Self {
        Self::new(AutodiscoverConfig::default())
    }
}

impl Default for AutodiscoverConfig {
    fn default() -> Self {
        Self {
            domain: "mail.example.com".to_string(),
            ews_url: "https://mail.example.com/EWS/Exchange.asmx".to_string(),
            as_url: "https://mail.example.com/Microsoft-Server-ActiveSync".to_string(),
            ecp_url: Some("https://mail.example.com/ecp".to_string()),
            oab_url: Some("https://mail.example.com/OAB".to_string()),
            um_url: None,
            sharing_url: None,
            mapi_http_url: None,
            smtp_settings: None,
            pop_settings: None,
            imap_settings: None,
            server_version: "15.1.2507.16".to_string(),
            deployment_id: "12345678-1234-1234-1234-123456789012".to_string(),
            require_ssl: true,
            auth_package: "basic".to_string(),
        }
    }
}

/// Autodiscover endpoint builder
pub struct AutodiscoverEndpointBuilder {
    base_domain: String,
}

impl AutodiscoverEndpointBuilder {
    pub fn new(base_domain: impl Into<String>) -> Self {
        Self {
            base_domain: base_domain.into(),
        }
    }

    /// Build common autodiscover endpoints
    pub fn build_endpoints(&self) -> Vec<String> {
        vec![
            format!("https://autodiscover.{}/autodiscover/autodiscover.xml", self.base_domain),
            format!("https://{}/autodiscover/autodiscover.xml", self.base_domain),
            format!("https://mail.{}/autodiscover/autodiscover.xml", self.base_domain),
        ]
    }

    /// Build autodiscover v2 (JSON) endpoints
    pub fn build_v2_endpoints(&self) -> Vec<String> {
        vec![
            format!("https://autodiscover.{}/autodiscover/autodiscover.json", self.base_domain),
            format!("https://{}/autodiscover/autodiscover.json", self.base_domain),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_autodiscover_service_soap() {
        let config = AutodiscoverConfig::default();
        let service = AutodiscoverService::new(config);
        
        let request = SoapAutodiscoverRequest {
            request: RequestEnvelope {
                email_address: "user@example.com".to_string(),
                acceptable_response_schema: None,
            },
        };
        
        let response = service.process_soap_request(request);
        assert_eq!(response.response.user.autodiscover_smtp_address, "user@example.com");
        assert!(!response.response.protocols.is_empty());
    }

    #[test]
    fn test_autodiscover_service_pox() {
        let config = AutodiscoverConfig::default();
        let service = AutodiscoverService::new(config);
        
        let request = PoxAutodiscoverRequest {
            email_address: "john.doe@example.com".to_string(),
            legacy_dn: None,
            protocol: None,
        };
        
        let response = service.process_pox_request(request);
        assert_eq!(response.user.autodiscover_smtp_address, "john.doe@example.com");
        assert_eq!(response.user.display_name, "John Doe");
    }

    #[test]
    fn test_autodiscover_service_json() {
        let config = AutodiscoverConfig::default();
        let service = AutodiscoverService::new(config);
        
        let request = JsonAutodiscoverRequest {
            email: "user@example.com".to_string(),
            protocol: None,
        };
        
        let response = service.process_json_request(request);
        assert_eq!(response.len(), 2);
        assert!(response.iter().any(|r| r.protocol == "EWS"));
        assert!(response.iter().any(|r| r.protocol == "ActiveSync"));
    }

    #[test]
    fn test_display_name_extraction() {
        let config = AutodiscoverConfig::default();
        let service = AutodiscoverService::new(config);
        
        assert_eq!(service.extract_display_name("john.doe@example.com"), "John Doe");
        assert_eq!(service.extract_display_name("jane@example.com"), "Jane");
        assert_eq!(service.extract_display_name("user@example.com"), "User");
    }

    #[test]
    fn test_endpoint_builder() {
        let builder = AutodiscoverEndpointBuilder::new("example.com");
        let endpoints = builder.build_endpoints();
        
        assert!(endpoints.iter().any(|e| e.contains("autodiscover.example.com")));
        assert!(endpoints.iter().any(|e| e.contains("example.com/autodiscover")));
    }

    #[test]
    fn test_pox_xml_generation() {
        let config = AutodiscoverConfig::default();
        let service = AutodiscoverService::new(config);
        
        let request = PoxAutodiscoverRequest {
            email_address: "test@example.com".to_string(),
            legacy_dn: None,
            protocol: None,
        };
        
        let response = service.process_pox_request(request);
        let xml = service.generate_pox_xml(&response);
        
        assert!(xml.contains("Autodiscover"));
        assert!(xml.contains("test@example.com"));
        assert!(xml.contains("Protocol"));
    }
}

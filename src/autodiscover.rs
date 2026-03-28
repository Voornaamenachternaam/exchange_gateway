// src/autodiscover.rs

use crate::config::Config;
use axum::{
    extract::Query,
    http::StatusCode,
    response::Response,
};
use serde::Deserialize;
use std::collections::HashMap;

/// Autodiscover request handler type — returned from each sub-handler.
pub type AdResponse = (StatusCode, Vec<(&'static str, &'static str)>, String);

/// Query parameters for Autodiscover JSON endpoint
/// Used by Outlook for Windows and mobile clients
#[derive(Debug, Deserialize, Default)]
pub struct AutodiscoverJsonParams {
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub redirecturl: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn no_cache_headers_xml() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Content-Type", "application/xml; charset=utf-8"),
        ("Cache-Control", "private, no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        ("X-Frame-Options", "DENY"),
        ("Content-Security-Policy", "default-src 'none'; frame-ancestors 'none'; sandbox"),
    ]
}

fn no_cache_headers_json() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Content-Type", "application/json; charset=utf-8"),
        ("Cache-Control", "private, no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        ("X-Frame-Options", "DENY"),
        ("Content-Security-Policy", "default-src 'none'; frame-ancestors 'none'; sandbox"),
    ]
}

fn no_cache_headers_soap() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Content-Type", "application/soap+xml; charset=utf-8"),
        ("Cache-Control", "private, no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        ("X-Frame-Options", "DENY"),
        ("Content-Security-Policy", "default-src 'none'; frame-ancestors 'none'; sandbox"),
    ]
}

/// Extract the email address from an Autodiscover v1 XML request body.
/// Public alias used by main.rs autodiscover route handler.
pub fn extract_email_from_body_xml(body: &str) -> Option<String> {
    extract_email_from_v1_xml(body)
}

fn extract_email_from_v1_xml(body: &str) -> Option<String> {
    // <EMailAddress>user@example.com</EMailAddress>
    let start = body.find("<EMailAddress>").map(|i| i + "<EMailAddress>".len())?;
    let end = body[start..].find("</EMailAddress>").map(|i| start + i)?;
    let email = body[start..end].trim().to_string();
    if email.contains('@') {
        Some(email)
    } else {
        None
    }
}

/// Extract the email address from an Autodiscover v1 SOAP request body.
fn extract_email_from_soap(body: &str) -> Option<String> {
    // <a:EMailAddress>user@example.com</a:EMailAddress>
    // or <Mailbox>user@example.com</Mailbox>
    for (open, close) in [
        ("<EMailAddress>", "</EMailAddress>"),
        ("<a:EMailAddress>", "</a:EMailAddress>"),
        ("<Mailbox>", "</Mailbox>"),
    ] {
        if let Some(start) = body.find(open).map(|i| i + open.len()) {
            if let Some(end) = body[start..].find(close).map(|i| start + i) {
                let email = body[start..end].trim().to_string();
                if email.contains('@') {
                    return Some(email);
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Autodiscover v1 XML
// ---------------------------------------------------------------------------

/// Respond to a POST /autodiscover/autodiscover.xml request.
///
/// Returns the full Exchange Autodiscover response with EXCH, EXPR, and
/// MobileSync protocol blocks. This is the primary format used by
/// Outlook for Windows desktop auto-configuration.
pub fn handle_autodiscover_xml(host: &str, body: &str, email: &str) -> AdResponse {
    let email_escaped = xml_escape(email);
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">
  <Response xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a">
    <User>
      <DisplayName>Stalwart Mail</DisplayName>
      <EMailAddress>{email}</EMailAddress>
      <DeploymentId>00000000-0000-0000-0000-000000000000</DeploymentId>
    </User>
    <Account>
      <AccountType>email</AccountType>
      <Action>settings</Action>
      <Protocol>
        <Type>EXCH</Type>
        <Server>{host}</Server>
        <ServerDN>/o=ExchangeLabs/ou=Exchange Administrative Group/cn=Configuration/cn=Servers/cn={host}</ServerDN>
        <ServerVersion>15.20.0.0</ServerVersion>
        <MdbDN>/o=ExchangeLabs/ou=Exchange Administrative Group/cn=Configuration/cn=Servers/cn={host}/cn=Microsoft Private MDB</MdbDN>
        <ASUrl>https://{host}/Microsoft-Server-ActiveSync</ASUrl>
        <EwsUrl>https://{host}/EWS/Exchange.asmx</EwsUrl>
        <EmwsUrl>https://{host}/EWS/Exchange.asmx</EmwsUrl>
        <EcpUrl>https://{host}/EWS/Exchange.asmx</EcpUrl>
        <OABUrl>https://{host}/EWS/Exchange.asmx</OABUrl>
        <OOFUrl>https://{host}/EWS/Exchange.asmx</OOFUrl>
        <UMUrl>https://{host}/EWS/Exchange.asmx</UMUrl>
        <EwsPartnerUrl>https://{host}/EWS/Exchange.asmx</EwsPartnerUrl>
        <LoginName>{email}</LoginName>
        <DomainRequired>off</DomainRequired>
        <SPA>off</SPA>
        <AuthPackage>Basic</AuthPackage>
        <CertPrincipalName>None</CertPrincipalName>
        <SSL>on</SSL>
        <AuthRequired>on</AuthRequired>
      </Protocol>
      <Protocol>
        <Type>EXPR</Type>
        <Server>{host}</Server>
        <SSL>on</SSL>
        <SPA>off</SPA>
        <CertPrincipalName>None</CertPrincipalName>
        <AuthPackage>Basic</AuthPackage>
        <ServerExclusiveConnect>off</ServerExclusiveConnect>
        <TTL>1</TTL>
        <ASUrl>https://{host}/Microsoft-Server-ActiveSync</ASUrl>
        <EwsUrl>https://{host}/EWS/Exchange.asmx</EwsUrl>
        <EmwsUrl>https://{host}/EWS/Exchange.asmx</EmwsUrl>
        <EcpUrl>https://{host}/EWS/Exchange.asmx</EcpUrl>
        <OABUrl>https://{host}/EWS/Exchange.asmx</OABUrl>
        <OOFUrl>https://{host}/EWS/Exchange.asmx</OOFUrl>
        <EwsPartnerUrl>https://{host}/EWS/Exchange.asmx</EwsPartnerUrl>
      </Protocol>
      <Protocol>
        <Type>MobileSync</Type>
        <Server>{host}</Server>
        <n>Exchange Gateway</n>
        <Url>https://{host}/Microsoft-Server-ActiveSync</Url>
        <LoginName>{email}</LoginName>
        <DomainRequired>off</DomainRequired>
        <SSL>on</SSL>
        <AuthPackage>Basic</AuthPackage>
        <ASUrl>https://{host}/Microsoft-Server-ActiveSync</ASUrl>
      </Protocol>
    </Account>
  </Response>
</Autodiscover>"#,
        host = xml_escape(host),
        email = email_escaped,
    );
    (StatusCode::OK, no_cache_headers_xml(), xml)
}

// ---------------------------------------------------------------------------
// Autodiscover v1 SOAP
// ---------------------------------------------------------------------------

/// Respond to a POST /autodiscover/autodiscover.svc request.
///
/// Returns a GetUserSettingsResponseMessage with all settings that Outlook
/// commonly requests in the SOAP GetUserSettings call.
pub fn handle_autodiscover_soap(host: &str, body: &str) -> AdResponse {
    let email = extract_email_from_soap(body).unwrap_or_default();
    let email_escaped = xml_escape(&email);
    let host_escaped = xml_escape(host);

    let settings = format!(
        r#"<a:UserSetting><a:Name>UserDisplayName</a:Name><a:Value>Stalwart Mail</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>UserDN</a:Name><a:Value>{email}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>AutoDiscoverSMTPAddress</a:Name><a:Value>{email}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalRpcClientServer</a:Name><a:Value>{host}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalEwsUrl</a:Name><a:Value>https://{host}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalEwsUrl</a:Name><a:Value>https://{host}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalEmwsUrl</a:Name><a:Value>https://{host}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalEmwsUrl</a:Name><a:Value>https://{host}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalEcpUrl</a:Name><a:Value>https://{host}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalEcpUrl</a:Name><a:Value>https://{host}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalOABUrl</a:Name><a:Value>https://{host}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalOABUrl</a:Name><a:Value>https://{host}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>MobileSyncServer</a:Name><a:Value>{host}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalMobileSyncUrl</a:Name><a:Value>https://{host}/Microsoft-Server-ActiveSync</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalMobileSyncUrl</a:Name><a:Value>https://{host}/Microsoft-Server-ActiveSync</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalEwsVersion</a:Name><a:Value>Exchange2016</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalEwsVersion</a:Name><a:Value>Exchange2016</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>EwsSupportedSchemas</a:Name><a:Value>Exchange2007,Exchange2007_SP1,Exchange2010,Exchange2010_SP1,Exchange2010_SP2,Exchange2013,Exchange2013_SP1,Exchange2016</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>PublicFolderServer</a:Name><a:Value>{host}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ActiveDirectoryServer</a:Name><a:Value>{host}</a:Value></a:UserSetting>"#,
        email = email_escaped,
        host = host_escaped
    );

    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"
            xmlns:a="http://schemas.microsoft.com/exchange/2010/Autodiscover">
  <s:Header>
    <a:ServerVersionInfo MajorVersion="15" MinorVersion="20" MajorBuildNumber="0"
                         MinorBuildNumber="0" Version="Exchange2016" />
  </s:Header>
  <s:Body>
    <a:GetUserSettingsResponseMessage xmlns:a="http://schemas.microsoft.com/exchange/2010/Autodiscover">
      <a:Response xmlns:a="http://schemas.microsoft.com/exchange/2010/Autodiscover">
        <a:ErrorCode>NoError</a:ErrorCode>
        <a:ErrorMessage />
        <a:UserResponses>
          <a:UserResponse>
            <a:ErrorCode>NoError</a:ErrorCode>
            <a:ErrorMessage />
            <a:RedirectTarget />
            <a:UserSettingErrors />
            <a:UserSettings>
              {settings}
            </a:UserSettings>
          </a:UserResponse>
        </a:UserResponses>
      </a:Response>
    </a:GetUserSettingsResponseMessage>
  </s:Body>
</s:Envelope>"#,
        settings = settings
    );
    (StatusCode::OK, no_cache_headers_soap(), xml)
}

// ---------------------------------------------------------------------------
// Autodiscover v2 JSON (Outlook 2013+, Office 365, new Outlook)
// ---------------------------------------------------------------------------

/// Respond to a GET /autodiscover/autodiscover.json request.
///
/// This is the modern Autodiscover endpoint used by:
/// - Outlook for Windows (new Outlook)
/// - Outlook for iOS/Android
/// - Office 365 hybrid configurations
///
/// Returns JSON with protocol configuration for EWS and ActiveSync.
pub fn handle_autodiscover_json(
    host: &str,
    protocol: Option<&str>,
    email: Option<&str>,
) -> AdResponse {
    let email = email.unwrap_or_default();
    
    // Validate email format
    if !email.contains('@') {
        let error = serde_json::json!({
            "error": {
                "code": "InvalidRequest",
                "message": "Email address is required"
            }
        });
        return (
            StatusCode::BAD_REQUEST,
            no_cache_headers_json(),
            error.to_string(),
        );
    }

    // Build the JSON response following Outlook Autodiscover JSON schema
    let json = serde_json::json!({
        "Protocol": "HTTP",
        "Url": format!("https://{}", host),
        "AuthenticationDisplayName": "Basic Authentication",
        "AuthenticationMethod": "Basic",
        "EmailAddress": email,
        "ExchangeServer": "Stalwart Mail",
        "ServerExclusiveConnect": "off",
        "ServerVersion": "Exchange2016",
        "PublicFolderServer": host,
        "ActiveDirectoryServer": host,
        "Capabilities": {
            "Account": {
                "Type": "EmailAddress",
                "Discovered": true
            },
            "Calendar": {
                "EmailAddress": email,
                "PrimarySmtpAddress": email,
                "AllMailboxesInSync": true
            },
            "Contacts": {
                "EmailAddress": email,
                "PrimarySmtpAddress": email,
                "AllMailboxesInSync": true
            },
            "Tasks": {
                "EmailAddress": email,
                "PrimarySmtpAddress": email,
                "AllMailboxesInSync": true
            },
            "Journal": {
                "EmailAddress": email,
                "PrimarySmtpAddress": email
            },
            "EwsAvailability": true,
            "EwsGetUserAvailability": true,
            "EwsFindFoldersInRoot": true,
            "SyncCalendarWithMobile": true,
            "SyncContactsWithMobile": true,
            "SyncTasksWithMobile": true,
            "FullMemberSync": true,
            "GroupingExclusions": [],
            "MailboxSearch": true,
            "MailboxSortByLastAccessTime": false,
            "OrganizationHierarchy": false,
            "PremiumClient": true,
            "SearchFoldersEnabled": true,
            "ShowGALAsSearchResult": true,
            "UMEnabled": false,
            "VirtualDirectories": {
                "OWA": {
                    "Internal": format!("https://{}/owa", host),
                    "External": format!("https://{}/owa", host)
                },
                "EWS": {
                    "Internal": format!("https://{}/EWS/Exchange.asmx", host),
                    "External": format!("https://{}/EWS/Exchange.asmx", host)
                },
                "Autodiscover": {
                    "Internal": format!("https://{}/Autodiscover/Autodiscover.svc", host),
                    "External": format!("https://{}/Autodiscover/Autodiscover.svc", host)
                },
                "MAPI": {
                    "Internal": format!("https://{}/mapi", host),
                    "External": format!("https://{}/mapi", host)
                }
            }
        },
        "Policies": [
            {
                "Name": "Individual",
                "PolicyState": "Enabled",
                "PolicyType": "Individual"
            }
        ],
        "UserDisplayName": email.split('@').next().unwrap_or(email),
        "UserLegacyDN": format!("/o=ExchangeLabs/ou=Exchange Administrative Group/cn=Configuration/cn=Servers/cn={}/cn=Mailbox GUID", host),
        "UserPrincipalName": email,
        "ExternalEwsUrl": format!("https://{}/EWS/Exchange.asmx", host),
        "InternalEwsUrl": format!("https://{}/EWS/Exchange.asmx", host),
        "ExternalOwaUrl": format!("https://{}/owa", host),
        "InternalOwaUrl": format!("https://{}/owa", host),
        "ExternalRpcHttpUrl": format!("https://{}/rpc", host),
        "InternalRpcHttpUrl": format!("https://{}/rpc", host),
        "ExternalMapiHttpUrl": format!("https://{}/mapi", host),
        "InternalMapiHttpUrl": format!("https://{}/mapi", host),
        "ExternalEcpUrl": format!("https://{}/ecp", host),
        "InternalEcpUrl": format!("https://{}/ecp", host),
        "ExternalEwsVersion": "Exchange2016",
        "InternalEwsVersion": "Exchange2016",
        "MobileSyncMailboxGuid": generate_mailbox_guid(email),
        "PreferredDomain": email.split('@').last().unwrap_or("")
    });

    (StatusCode::OK, no_cache_headers_json(), json.to_string())
}

/// Generate a consistent mailbox GUID from email address
/// This ensures the same mailbox gets the same GUID across requests
fn generate_mailbox_guid(email: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    
    let mut hasher = DefaultHasher::new();
    email.hash(&mut hasher);
    let hash = hasher.finish();
    
    // Format as GUID-like string: xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        hash,
        (hash >> 32) & 0xFFFF,
        (hash >> 48) & 0xFFFF,
        (hash >> 16) & 0xFFFF,
        hash & 0xFFFFFFFFFFFF
    )
}




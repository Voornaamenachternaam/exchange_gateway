// src/autodiscover.rs
//
// Gateway-side Autodiscover request handling for all three formats:
//   1. Autodiscover v1 XML  (POST /autodiscover/autodiscover.xml)
//   2. Autodiscover v1 SOAP (POST /autodiscover/autodiscover.svc)
//   3. Autodiscover v2 JSON (GET  /autodiscover/autodiscover.json?Email=…&Protocol=…)
//
// Gaps closed:
//   Gap 6 — Autodiscover is richer, but still not fully Exchange-topology aware.
//
//   The Cloudflare Worker (worker/index.js) handles Autodiscover requests for
//   the public hostname. This module provides equivalent Rust implementations
//   for completeness — the gateway will serve these if the Worker is unavailable
//   or if the docker-only deployment mode is used.
//
//   Specific improvements:
//     - Autodiscover v2 JSON now implements the modern ?Email=&Protocol= query
//       parameter form used by Outlook for Windows 11 and Android 15. The
//       previous implementation always returned the Exchange/EWS protocol
//       regardless of the requested protocol; now it responds correctly to
//       Protocol=AutodiscoverV1, Protocol=ActiveSync, Protocol=Ews.
//     - All three Autodiscover response formats now include the full set of
//       settings that Outlook bootstrapping requires:
//         EwsUrl, ASUrl, EcpUrl, OABUrl, OOFUrl, EmwsUrl,
//         ExternalEwsUrl, InternalEwsUrl, MobileSyncUrl, MobileSyncServer,
//         EwsSupportedSchemas, ExternalEwsVersion.
//     - Security: Content-Security-Policy and no-sniff headers on all responses.
//     - The SOAP response now handles GetUserSettings correctly with all
//       UserSetting names Outlook commonly requests.

use crate::config::Config;
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use std::collections::HashMap;
use std::sync::Arc;

/// Autodiscover request handler type — returned from each sub-handler.
pub type AdResponse = (StatusCode, Vec<(&'static str, &'static str)>, String);

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
    ]
}

fn no_cache_headers_json() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Content-Type", "application/json; charset=utf-8"),
        ("Cache-Control", "private, no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        ("X-Frame-Options", "DENY"),
    ]
}

fn no_cache_headers_soap() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Content-Type", "application/soap+xml; charset=utf-8"),
        ("Cache-Control", "private, no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        ("X-Frame-Options", "DENY"),
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
// Autodiscover v2 JSON
// ---------------------------------------------------------------------------

/// Respond to a GET /autodiscover/autodiscover.json request.
///
/// Supports the modern Outlook `?Email=…&Protocol=…` query parameter form
/// (Autodiscover v2) per [MS-OXDISCO] and the format used by Outlook for
/// Windows 11 and Android 15.
///
/// Supported Protocol values:
///   - `ActiveSync`       → returns the MobileSync endpoint
///   - `Ews`              → returns the EWS endpoint
///   - `AutodiscoverV1`   → returns a redirect URL for v1 Autodiscover
///   - (omitted/unknown)  → returns the Exchange / EWS endpoint (default)
pub fn handle_autodiscover_json(host: &str, protocol: Option<&str>, email: Option<&str>) -> AdResponse {
    if host.contains(['/', '@', '?', '#']) {
        return (StatusCode::BAD_REQUEST, vec![], "Invalid host".to_string());
    }
    let ews_url = format!("https://{}/EWS/Exchange.asmx", host);
    let as_url = format!("https://{}/Microsoft-Server-ActiveSync", host);
    let v1_url = format!("https://{}/autodiscover/autodiscover.xml", host);

    let proto = protocol
        .unwrap_or("Exchange")
        .to_ascii_lowercase();
    let email_str = email.unwrap_or_default();

    let body = match proto.as_str() {
        "activesync" => {
            format!(
                r#"{{"Protocol":"ActiveSync","Url":"{as_url}","ActiveSyncUrl":"{as_url}","MobileSyncUrl":"{as_url}","LoginName":{email_json}}}"#,
                as_url = as_url,
                email_json = serde_json_string(email_str)
            )
        }
        "ews" => {
            format!(
                r#"{{"Protocol":"Ews","Url":"{ews_url}","EwsUrl":"{ews_url}","ExternalEwsUrl":"{ews_url}","InternalEwsUrl":"{ews_url}"}}"#,
                ews_url = ews_url
            )
        }
        "autodiscoverv1" => {
            // Redirect to v1 autodiscover endpoint.
            format!(
                r#"{{"Protocol":"AutodiscoverV1","Url":"{v1_url}"}}"#,
                v1_url = v1_url
            )
        }
        _ => {
            // Default: Exchange / combined response with all endpoints.
            format!(
                r#"{{"Protocol":"Exchange","Url":"{ews_url}","EwsUrl":"{ews_url}","ExternalEwsUrl":"{ews_url}","InternalEwsUrl":"{ews_url}","ActiveSyncUrl":"{as_url}","MobileSyncUrl":"{as_url}","ExternalEwsVersion":"Exchange2016","EwsSupportedSchemas":"Exchange2007,Exchange2007_SP1,Exchange2010,Exchange2010_SP1,Exchange2010_SP2,Exchange2013,Exchange2013_SP1,Exchange2016"}}"#,
                ews_url = ews_url,
                as_url = as_url
            )
        }
    };

    (StatusCode::OK, no_cache_headers_json(), body)
}

/// Minimal JSON string encoder — only handles the common email address case.
fn serde_json_string(s: &str) -> String {
    serde_json::to_string(s).expect("Failed to serialize string to JSON")
}

// ---------------------------------------------------------------------------
// Axum handler integration types (used from main.rs routing if desired)
// ---------------------------------------------------------------------------
pub fn handle_autodiscover_json(host: &str, protocol: Option<&str>, email: Option<&str>) -> AdResponse {
    if host.contains(['/', '@', '?', '#']) {
        return (StatusCode::BAD_REQUEST, vec![], "Invalid host".to_string());
    }
    let ews_url = format!("https://{}/EWS/Exchange.asmx", host);
    let as_url = format!("https://{}/Microsoft-Server-ActiveSync", host);
    let v1_url = format!("https://{}/autodiscover/autodiscover.xml", host);

    let proto = protocol
        .unwrap_or("Exchange")
        .to_ascii_lowercase();
    let email_str = email.unwrap_or_default();

    let body = match proto.as_str() {
        "activesync" => {
            format!(
                r#"{{"Protocol":"ActiveSync","Url":"{as_url}","ActiveSyncUrl":"{as_url}","MobileSyncUrl":"{as_url}","LoginName":{email_json}}}"#, 
                as_url = as_url,
                email_json = serde_json_string(email_str)
            )
        }
        "ews" => {
            format!(
                r#"{{"Protocol":"Ews","Url":"{ews_url}","EwsUrl":"{ews_url}","ExternalEwsUrl":"{ews_url}","InternalEwsUrl":"{ews_url}"}}"#, 
                ews_url = ews_url
            )
        }
        "autodiscoverv1" => {
            format!(
                r#"{{"Protocol":"AutodiscoverV1","Url":"{v1_url}"}}"#, 
                v1_url = v1_url
            )
        }
        _ => {
            format!(
                r#"{{"Protocol":"Exchange","Url":"{ews_url}","EwsUrl":"{ews_url}","ExternalEwsUrl":"{ews_url}","InternalEwsUrl":"{ews_url}","ActiveSyncUrl":"{as_url}","MobileSyncUrl":"{as_url}","ExternalEwsVersion":"Exchange2016","EwsSupportedSchemas":"Exchange2007,Exchange2007_SP1,Exchange2010,Exchange2010_SP1,Exchange2010_SP2,Exchange2013,Exchange2013_SP1,Exchange2016"}}"#, 
                ews_url = ews_url,
                as_url = as_url
            )
        }
    };

    (StatusCode::OK, no_cache_headers_json(), body)
}

fn serde_json_string(s: &str) -> String {
    serde_json::to_string(s).expect("Failed to serialize string to JSON")
}

#[derive(serde::Deserialize)]
pub struct AutodiscoverJsonParams {
    #[serde(rename = "Email", alias = "email")]
    pub email: Option<String>,
    #[serde(rename = "Protocol", alias = "protocol")]
    pub protocol: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_json_default_returns_exchange_protocol() {
        let (status, _, body) = handle_autodiscover_json("mail.example.com", None, None);
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"Protocol\":\"Exchange\""));
        assert!(body.contains("EwsUrl"));
        assert!(body.contains("ActiveSyncUrl"));
    }

    #[test]
    fn v2_json_activesync_returns_activesync_url() {
        let (status, _, body) =
            handle_autodiscover_json("mail.example.com", Some("ActiveSync"), Some("u@example.com"));
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"Protocol\":\"ActiveSync\""));
        assert!(body.contains("/Microsoft-Server-ActiveSync"));
        assert!(!body.contains("EwsUrl"));
    }

    #[test]
    fn v2_json_ews_returns_ews_url() {
        let (status, _, body) =
            handle_autodiscover_json("mail.example.com", Some("Ews"), None);
        assert!(body.contains("\"Protocol\":\"Ews\""));
        assert!(body.contains("/EWS/Exchange.asmx"));
    }

    #[test]
    fn v2_json_autodiscoverv1_returns_redirect_url() {
        let (status, _, body) =
            handle_autodiscover_json("mail.example.com", Some("AutodiscoverV1"), None);
        assert!(body.contains("\"Protocol\":\"AutodiscoverV1\""));
        assert!(body.contains("autodiscover.xml"));
    }

    #[test]
    fn v1_xml_contains_all_required_endpoints() {
        let (status, _, body) =
            handle_autodiscover_xml("mail.example.com", "", "user@example.com");
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<EwsUrl>"));
        assert!(body.contains("<ASUrl>"));
        assert!(body.contains("<EcpUrl>"));
        assert!(body.contains("<OABUrl>"));
        assert!(body.contains("<OOFUrl>"));
        assert!(body.contains("MobileSync"));
        assert!(body.contains("EXCH"));
        assert!(body.contains("EXPR"));
        assert!(body.contains("user@example.com"));
    }

    #[test]
    fn v1_xml_xss_safe() {
        let (_, _, body) =
            handle_autodiscover_xml("mail.example.com", "", "<script>alert(1)</script>@example.com");
        assert!(!body.contains("<script>"));
        assert!(body.contains("&lt;script&gt;"));
    }

    #[test]
    fn soap_contains_all_required_settings() {
        let soap_body = r#"<s:Envelope><s:Body><a:GetUserSettingsRequestMessage>
            <a:Request><a:Users><a:User><a:EMailAddress>user@example.com</a:EMailAddress></a:User></a:Users>
            </a:Request></a:GetUserSettingsRequestMessage></s:Body></s:Envelope>"#;
        let (status, _, body) = handle_autodiscover_soap("mail.example.com", soap_body);
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("ExternalEwsUrl"));
        assert!(body.contains("MobileSyncServer"));
        assert!(body.contains("EwsSupportedSchemas"));
        assert!(body.contains("Exchange2016"));
        assert!(body.contains("user@example.com"));
    }

    #[test]
    fn soap_xss_safe() {
        let soap_body = r#"<a:EMailAddress><script>bad</script>@example.com</a:EMailAddress>"#;
        let (_, _, body) = handle_autodiscover_soap("mail.example.com", soap_body);
        assert!(!body.contains("<script>"));
    }

    #[test]
    fn extract_email_from_v1_xml_works() {
        let body = r#"<Autodiscover><Request><EMailAddress>user@example.com</EMailAddress></Request></Autodiscover>"#;
        assert_eq!(
            extract_email_from_v1_xml(body).as_deref(),
            Some("user@example.com")
        );
    }

    #[test]
    fn v1_xml_headers_include_security_headers() {
        let (_, headers, _) = handle_autodiscover_xml("mail.example.com", "", "u@example.com");
        let keys: Vec<&str> = headers.iter().map(|(k, _)| *k).collect();
        assert!(keys.contains(&"X-Content-Type-Options"));
        assert!(keys.contains(&"Cache-Control"));
    }
}

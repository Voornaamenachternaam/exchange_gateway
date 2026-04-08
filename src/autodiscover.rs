// src/autodiscover.rs
use axum::http::StatusCode;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct AutodiscoverJsonParams {
    #[serde(rename = "Protocol")]
    pub protocol: Option<String>,
    #[serde(rename = "Email")]
    pub email: Option<String>,
}

pub type AdResponse = (StatusCode, Vec<(&'static str, &'static str)>, String);

pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn no_cache_headers_xml() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Content-Type", "application/xml; charset=utf-8"),
        ("Cache-Control", "private, no-store, no-cache, max-age=0"),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        ("X-Frame-Options", "DENY"),
        (
            "Content-Security-Policy",
            "default-src 'none'; frame-ancestors 'none'; sandbox",
        ),
        ("X-XSS-Protection", "1; mode=block"),
    ]
}

fn no_cache_headers_json() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Content-Type", "application/json; charset=utf-8"),
        ("Cache-Control", "private, no-store, no-cache, max-age=0"),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        ("X-Frame-Options", "DENY"),
        (
            "Content-Security-Policy",
            "default-src 'none'; frame-ancestors 'none'; sandbox",
        ),
        ("X-XSS-Protection", "1; mode=block"),
    ]
}

fn no_cache_headers_soap() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Content-Type", "application/soap+xml; charset=utf-8"),
        ("Cache-Control", "private, no-store, no-cache, max-age=0"),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        ("X-Frame-Options", "DENY"),
        (
            "Content-Security-Policy",
            "default-src 'none'; frame-ancestors 'none'; sandbox",
        ),
        ("X-XSS-Protection", "1; mode=block"),
    ]
}

pub fn extract_email_from_body_xml(body: &str) -> Option<String> {
    extract_email_from_v1_xml(body)
}

fn extract_email_from_v1_xml(body: &str) -> Option<String> {
    let start = body
        .find("<EMailAddress>")
        .map(|i| i + "<EMailAddress>".len())?;
    let end = body[start..].find("</EMailAddress>").map(|i| start + i)?;
    let email = body[start..end].trim().to_string();
    if email.contains('@') {
        Some(email)
    } else {
        None
    }
}

fn extract_email_from_soap(body: &str) -> Option<String> {
    for (open, close) in [
        ("<EMailAddress>", "</EMailAddress>"),
        ("<a:EMailAddress>", "</a:EMailAddress>"),
        ("<Mailbox>", "</Mailbox>"),
    ] {
        if let Some(start) = body.find(open).map(|i| i + open.len()) {
            if let Some(end) = body.find(open).and_then(|i| {
                let start = i + open.len();
                body[start..].find(close).map(|j| start + j)
            })
            let email = body[start..end].trim().to_string();
            if email.contains('@') {
                return Some(email);
            }
        }
    }
    None
}

pub fn handle_autodiscover_json(
    host: &str,
    protocol: Option<&str>,
    _email: Option<&str>,
) -> AdResponse {
    let ews_url = format!("https://{}/EWS/Exchange.asmx", host);
    let as_url = format!("https://{}/Microsoft-Server-ActiveSync", host);
    let v1_url = format!("https://{}/autodiscover/autodiscover.xml", host);

    let body = match protocol.unwrap_or("Exchange").to_ascii_lowercase().as_str() {
        "activesync" => format!(
            r#"{{"Protocol":"ActiveSync","Url":"{as_url}","ActiveSyncUrl":"{as_url}","MobileSyncUrl":"{as_url}"}}"#,
            as_url = as_url
        ),
        "ews" => format!(
            r#"{{"Protocol":"Ews","Url":"{ews_url}","EwsUrl":"{ews_url}","ExternalEwsUrl":"{ews_url}","InternalEwsUrl":"{ews_url}"}}"#,
            ews_url = ews_url
        ),
        "autodiscoverv1" => format!(
            r#"{{"Protocol":"AutodiscoverV1","Url":"{v1_url}"}}"#,
            v1_url = v1_url
        ),
        _ => format!(
            r#"{{"Protocol":"Exchange","Url":"{ews_url}","EwsUrl":"{ews_url}","ExternalEwsUrl":"{ews_url}","InternalEwsUrl":"{ews_url}","ActiveSyncUrl":"{as_url}","MobileSyncUrl":"{as_url}","ExternalEwsVersion":"Exchange2016","EwsSupportedSchemas":"Exchange2007,Exchange2007_SP1,Exchange2010,Exchange2010_SP1,Exchange2010_SP2,Exchange2013,Exchange2013_SP1,Exchange2016"}}"#,
            ews_url = ews_url,
            as_url = as_url
        ),
    };

    (StatusCode::OK, no_cache_headers_json(), body)
}

pub fn handle_autodiscover_xml(host: &str, _body: &str, email: &str) -> AdResponse {
    let email_escaped = xml_escape(email);
    let host_escaped = xml_escape(host);
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
        <LoginName>{email}</LoginName>
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
        <DisplayName>Exchange Gateway</DisplayName>
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
        host = host_escaped,
        email = email_escaped,
    );
    (StatusCode::OK, no_cache_headers_xml(), xml)
}

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
        host = host_escaped,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_email_from_v1_body() {
        let body = r#"<Autodiscover xmlns="..."><Request><EMailAddress>alice@example.com</EMailAddress></Request></Autodiscover>"#;
        assert_eq!(
            extract_email_from_body_xml(body),
            Some("alice@example.com".to_string())
        );
    }

    #[test]
    fn autodiscover_json_default_exchange() {
        let (status, hdrs, body) = handle_autodiscover_json("exchange.example.com", None, None);
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("EwsUrl"));
        assert!(body.contains("ActiveSyncUrl"));
        assert!(hdrs.iter().any(|(k, _)| *k == "Content-Type"));
    }

    #[test]
    fn autodiscover_json_activesync_only() {
        let (_, _, body) =
            handle_autodiscover_json("exchange.example.com", Some("ActiveSync"), None);
        assert!(body.contains("ActiveSync"));
        assert!(!body.contains("EwsUrl"));
    }

    #[test]
    fn autodiscover_json_ews_only() {
        let (_, _, body) = handle_autodiscover_json("exchange.example.com", Some("Ews"), None);
        assert!(body.contains("EwsUrl"));
        assert!(!body.contains("ActiveSyncUrl"));
    }

    #[test]
    fn autodiscover_json_autodiscoverv1() {
        let (_, _, body) =
            handle_autodiscover_json("exchange.example.com", Some("AutodiscoverV1"), None);
        assert!(body.contains("AutodiscoverV1"));
        assert!(body.contains("autodiscover.xml"));
    }

    #[test]
    fn autodiscover_xml_contains_required_fields() {
        let (status, _, body) =
            handle_autodiscover_xml("exchange.example.com", "", "alice@example.com");
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<Type>EXCH</Type>"));
        assert!(body.contains("<Type>EXPR</Type>"));
        assert!(body.contains("<Type>MobileSync</Type>"));
        assert!(body.contains("<LoginName>alice@example.com</LoginName>"));
        assert!(body.contains("EWS/Exchange.asmx"));
        assert!(body.contains("Microsoft-Server-ActiveSync"));
    }

    #[test]
    fn autodiscover_soap_contains_settings() {
        let body_in = r#"<a:EMailAddress>alice@example.com</a:EMailAddress>"#;
        let (status, _, body) = handle_autodiscover_soap("exchange.example.com", body_in);
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("ExternalEwsUrl"));
        assert!(body.contains("MobileSyncServer"));
        assert!(body.contains("exchange.example.com"));
    }

    #[test]
    fn xml_escape_special_chars() {
        assert_eq!(xml_escape("<a>&\""), "&lt;a&gt;&amp;&quot;");
    }
}

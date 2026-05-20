// src/autodiscover.rs
//
// Autodiscover handlers for Exchange Gateway.
//
// Handles three autodiscover protocols:
// 1. Outlook desktop (V1 XML POST) — MS-OXDSCLI outlook/responseschema/2006a
// 2. ActiveSync mobile (V1 XML POST) — MS-ASCMD mobilesync/responseschema/2006
// 3. Autodiscover V2 (JSON GET) — used by AutoDetect cloud service and Outlook mobile
//
// Per MS-ASCMD §2.2.3.1, the client includes an <AcceptableResponseSchema> element
// in the POST body that specifies which response format it expects. The server MUST
// return a response matching the requested schema or the client will treat it as an
// error (MS-ASCMD §4.2.5, error code 601 "provider not found").
use crate::util::{nfc, xml_escape};
use axum::http::StatusCode;
use serde::Deserialize;

/// Namespace for the mobilesync response schema (MS-ASCMD §6.2).
const MOBILESYNC_RESPONSE_NS: &str =
    "http://schemas.microsoft.com/exchange/autodiscover/mobilesync/responseschema/2006";

/// Namespace for the Outlook response schema (MS-OXDSCLI §2.2.4.1).
const OUTLOOK_RESPONSE_NS: &str =
    "http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a";

#[derive(Debug, Deserialize)]
pub struct AutodiscoverJsonParams {
    #[serde(rename = "Protocol")]
    pub protocol: Option<String>,
    #[serde(rename = "Email")]
    pub email: Option<String>,
}

pub type AdResponse = (StatusCode, Vec<(&'static str, &'static str)>, String);

fn content_type_xml() -> Vec<(&'static str, &'static str)> {
    vec![("Content-Type", "application/xml; charset=utf-8")]
}

fn content_type_json() -> Vec<(&'static str, &'static str)> {
    vec![("Content-Type", "application/json; charset=utf-8")]
}

fn content_type_soap() -> Vec<(&'static str, &'static str)> {
    vec![("Content-Type", "application/soap+xml; charset=utf-8")]
}

/// Detect which response schema the client is requesting from the POST body.
///
/// Per MS-ASCMD §2.2.3.1, the `<AcceptableResponseSchema>` element specifies
/// the expected response format. ActiveSync clients (including the AutoDetect
/// cloud service for Outlook mobile) request the mobilesync schema.
/// Outlook desktop clients request the outlook schema.
/// If absent or unrecognised, defaults to Outlook for backward compatibility.
///
/// This parser handles the following variations:
/// - Plain tag: `<AcceptableResponseSchema>...</AcceptableResponseSchema>`
/// - Namespaced tag: `<a:AcceptableResponseSchema>...</a:AcceptableResponseSchema>`
/// - Tag with attributes: `<AcceptableResponseSchema xmlns="...">...</AcceptableResponseSchema>`
/// - Mixed: `<a:AcceptableResponseSchema xmlns:a="...">...</a:AcceptableResponseSchema>`
fn detect_response_schema(body: &str) -> ResponseSchema {
    // Scan for any opening tag whose local name is "AcceptableResponseSchema",
    // regardless of namespace prefix or extra attributes.
    // Strategy: find "AcceptableResponseSchema" in the body, then verify it
    // sits inside an opening tag (preceded by '<' or '<' + prefix + ':')
    // and locate the matching close tag.
    let mut search_from = 0;
    while let Some(pos) = body[search_from..].find("AcceptableResponseSchema") {
        let abs_pos = search_from + pos;
        search_from = abs_pos + 1;

        // Verify this is inside an opening tag: look backward for '<' with
        // only a namespace prefix (word chars + ':') between it and the
        // local name. We must not match a closing tag (which has '</' prefix)
        // or a string value that merely contains the word.
        let tag_name_start = if let Some(lt) = body[..abs_pos].rfind('<') {
            let between = &body[lt + 1..abs_pos];
            // Must be a prefix like "a:" or empty (no prefix before local name)
            if between.is_empty() || between.chars().all(|c| c.is_alphanumeric() || c == ':') {
                // Reject if it's a closing tag: '</' directly before the prefix/name
                if body[..abs_pos].ends_with('/') {
                    continue;
                }
                lt + 1 // start of tag name (after '<')
            } else {
                continue;
            }
        } else {
            continue;
        };

        // Find the end of the opening tag ('>')
        let open_tag_end = if let Some(gt) = body[abs_pos..].find('>') {
            abs_pos + gt
        } else {
            continue;
        };

        // The content starts after the '>' of the opening tag
        let content_start = open_tag_end + 1;

        // Find the matching closing tag. We look for the closing tag that
        // uses the same prefix as the opening tag.
        // Extract the prefix (e.g. "a:" or "" for no prefix).
        let prefix = &body[tag_name_start..abs_pos];
        let close_tag = if prefix.is_empty() {
            "</AcceptableResponseSchema>".to_string()
        } else {
            // Build closing tag with the same prefix: e.g. "</a:AcceptableResponseSchema>"
            // The prefix already includes the colon (e.g. "a:"), so the format
            // below produces "</a:AcceptableResponseSchema>" which is correct.
            format!("</{}AcceptableResponseSchema>", prefix)
        };

        if let Some(close_pos) = body[content_start..].find(close_tag.as_str()) {
            let schema = body[content_start..content_start + close_pos].trim();
            if schema.contains("mobilesync") {
                return ResponseSchema::MobileSync;
            }
            if schema.contains("outlook") {
                return ResponseSchema::Outlook;
            }
        }
    }
    ResponseSchema::Outlook
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseSchema {
    Outlook,
    MobileSync,
}

/// Default culture string per MS-ASCMD §4.2.4 example.
const DEFAULT_CULTURE: &str = "en:us";

/// Parse an Accept-Language header value into a culture string suitable
/// for the `<Culture>` element in the mobilesync response.
///
/// Accept-Language format (RFC 7231 §5.3.5): `lang;q=weight, lang;q=weight, ...`
/// The culture string uses the format `{language}:{country}` where the country
/// is derived from the language tag if not explicitly provided.
///
/// Examples:
/// - `"de-DE"` → `"de:de"`
/// - `"en-US"` → `"en:us"`
/// - `"fr"` → `"fr:fr"`
/// - `"ja"` → `"ja:jp"`
/// - `"zh-CN"` → `"zh:cn"`
/// - `""` / `None` → `"en:us"` (default)
fn parse_culture_from_accept_language(accept_lang: Option<&str>) -> String {
    let raw = match accept_lang {
        Some(h) if !h.is_empty() => h,
        _ => return DEFAULT_CULTURE.to_string(),
    };

    // Take the highest-priority language tag (first one, before any ';')
    let primary = raw.split(',').next().unwrap_or("").trim();
    if primary.is_empty() {
        return DEFAULT_CULTURE.to_string();
    }

    // Strip quality value if present: "de-DE;q=0.9" → "de-DE"
    let tag = primary.split(';').next().unwrap_or("").trim();
    if tag.is_empty() {
        return DEFAULT_CULTURE.to_string();
    }

    // Normalize to lowercase for consistent culture string output
    let tag_lower = tag.to_ascii_lowercase();

    // Split on '-' to separate language and country subtags.
    // RFC 5646 format: language[-script][-region]
    // e.g. "en-US", "de-DE", "zh-Hans-CN"
    let parts: Vec<&str> = tag_lower.split('-').collect();
    let language = parts[0];

    // Find the region subtag (2-letter or 3-letter after optional script).
    // RFC 5646: region subtag is 2 alpha (ISO 3166-1) or 3 digits.
    // For simplicity, we take the last 2-alpha subtag as region.
    let region = parts
        .iter()
        .rev()
        .find(|s| s.len() == 2 && s.chars().all(|c| c.is_ascii_alphabetic()));

    let country = region.unwrap_or(&language);
    format!("{}:{}", language, country)
}

pub fn extract_email_from_body_xml(body: &str) -> Option<String> {
    extract_email_from_v1_xml(body)
}

fn extract_email_from_v1_xml(body: &str) -> Option<String> {
    let start = body
        .find("<EMailAddress>")
        .map(|i| i + "<EMailAddress>".len())?;
    let end = body[start..].find("</EMailAddress>").map(|i| start + i)?;
    let email = nfc(body[start..end].trim());
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
        if let Some(end) = body.find(open).and_then(|i| {
            let start = i + open.len();
            body[start..].find(close).map(|j| start + j)
        }) {
            let start = body.find(open).map(|i| i + open.len()).unwrap_or(0);
            let email = nfc(body[start..end].trim());
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

    // Per MS-OXDSCLI, Autodiscover V2 returns a minimal JSON document with
    // just the Protocol and Url for the service sought. Each protocol gets
    // its own response — the fields are NOT interchangeable. Extra fields
    // that belong to a different protocol (e.g. ActiveSyncUrl in an Ews
    // response) must NOT be included; they violate the V2 design intent and
    // can confuse strict clients like AutoDetect.
    //
    // Real Exchange Server V2 responses contain only Protocol + Url:
    //   ?Protocol=ActiveSync     → {"Protocol":"ActiveSync","Url":"<as>"}
    //   ?Protocol=Ews            → {"Protocol":"Ews","Url":"<ews>"}
    //   ?Protocol=AutodiscoverV1 → {"Protocol":"AutodiscoverV1","Url":"<v1>"}
    //
    // "Exchange" is NOT a valid V2 Protocol name — it's a V1 XML concept.
    // When no Protocol is specified, the gateway defaults to ActiveSync,
    // which is what AutoDetect and Outlook mobile need. Real Exchange Server
    // requires Protocol to be specified; the gateway's default is a
    // convenience for browser/diagnostic access.

    let body = match protocol.map(|p| p.to_ascii_lowercase()).as_deref() {
        Some("activesync") => format!(
            r#"{{"Protocol":"ActiveSync","Url":"{as_url}"}}"#,
            as_url = as_url
        ),
        Some("ews") => format!(
            r#"{{"Protocol":"Ews","Url":"{ews_url}"}}"#,
            ews_url = ews_url
        ),
        Some("autodiscoverv1") => format!(
            r#"{{"Protocol":"AutodiscoverV1","Url":"{v1_url}"}}"#,
            v1_url = v1_url
        ),
        Some("rest") => format!(
            r#"{{"Protocol":"Rest","Url":"{ews_url}"}}"#,
            ews_url = ews_url
        ),
        // No Protocol or unrecognized — default to ActiveSync.
        // This is what AutoDetect and Outlook mobile need.
        _ => format!(
            r#"{{"Protocol":"ActiveSync","Url":"{as_url}"}}"#,
            as_url = as_url
        ),
    };

    (StatusCode::OK, content_type_json(), body)
}

/// Handle autodiscover XML POST requests.
///
/// Dispatches to the correct response format based on the
/// `AcceptableResponseSchema` element in the request body.
/// This is critical for Outlook mobile/ActiveSync clients which
/// expect the mobilesync schema, not the Outlook desktop schema.
///
/// `accept_language` is the value of the Accept-Language request header,
/// used to set the `<Culture>` element in the mobilesync response.
/// Pass `None` to use the default ("en:us").
pub fn handle_autodiscover_xml(
    host: &str,
    body: &str,
    email: &str,
    accept_language: Option<&str>,
) -> AdResponse {
    let schema = detect_response_schema(body);
    match schema {
        ResponseSchema::MobileSync => handle_mobilesync_xml(host, email, accept_language),
        ResponseSchema::Outlook => handle_outlook_xml(host, email),
    }
}

/// Generate the mobilesync autodiscover response per MS-ASCMD §4.2.4.
///
/// This format is required by ActiveSync clients including:
/// - The AutoDetect cloud service used by Outlook for iOS/Android
/// - Native Android/iOS Exchange account provisioners
/// - Any client that sends `AcceptableResponseSchema: .../mobilesync/responseschema/2006`
///
/// The response uses the `Action/Settings/Server` structure (not EXCH/EXPR Protocol)
/// and returns the MobileSync (ActiveSync) endpoint URL.
///
/// `accept_language` is parsed per RFC 7231 §5.3.5 to set the `<Culture>` element.
/// Pass `None` to use the default ("en:us").
fn handle_mobilesync_xml(host: &str, email: &str, accept_language: Option<&str>) -> AdResponse {
    let email_escaped = xml_escape(email);
    let host_escaped = xml_escape(host);
    let as_url = format!("https://{}/Microsoft-Server-ActiveSync", host_escaped);
    let culture = parse_culture_from_accept_language(accept_language);

    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">
<Response xmlns="{MOBILESYNC_RESPONSE_NS}">
<Culture>{culture}</Culture>
<User>
<DisplayName>Stalwart Mail</DisplayName>
<EMailAddress>{email}</EMailAddress>
</User>
<Action>
<Settings>
<Server>
<Type>MobileSync</Type>
<Url>{as_url}</Url>
<Name>{as_url}</Name>
</Server>
</Settings>
</Action>
</Response>
</Autodiscover>"#,
        MOBILESYNC_RESPONSE_NS = MOBILESYNC_RESPONSE_NS,
        culture = culture,
        email = email_escaped,
        as_url = as_url,
    );
    (StatusCode::OK, content_type_xml(), xml)
}

/// Generate the Outlook desktop autodiscover response per MS-OXDSCLI §2.2.4.
///
/// This format is used by Outlook for Windows/Mac and includes EXCH/EXPR
/// Protocol elements with EWS and ActiveSync URLs.
///
/// Note: `<ServerExclusiveConnect>` is set to "on" for EXPR so that
/// Outlook clients prioritise this configuration per MS-OXDSCLI §3.1.5.4.
fn handle_outlook_xml(host: &str, email: &str) -> AdResponse {
    let email_escaped = xml_escape(email);
    let host_escaped = xml_escape(host);
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">
  <Response xmlns="{OUTLOOK_RESPONSE_NS}">
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
        <ServerExclusiveConnect>on</ServerExclusiveConnect>
        <TTL>1</TTL>
        <ASUrl>https://{host}/Microsoft-Server-ActiveSync</ASUrl>
        <EwsUrl>https://{host}/EWS/Exchange.asmx</EwsUrl>
        <EmwsUrl>https://{host}/EWS/Exchange.asmx</EmwsUrl>
        <EcpUrl>https://{host}/EWS/Exchange.asmx</EcpUrl>
        <OOFUrl>https://{host}/EWS/Exchange.asmx</OOFUrl>
        <EwsPartnerUrl>https://{host}/EWS/Exchange.asmx</EwsPartnerUrl>
      </Protocol>
    </Account>
  </Response>
</Autodiscover>"#,
        OUTLOOK_RESPONSE_NS = OUTLOOK_RESPONSE_NS,
        host = host_escaped,
        email = email_escaped,
    );
    (StatusCode::OK, content_type_xml(), xml)
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
    (StatusCode::OK, content_type_soap(), xml)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_response_schema_outlook() {
        let body = r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006">
<Request>
<EMailAddress>user@example.com</EMailAddress>
<AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</AcceptableResponseSchema>
</Request>
</Autodiscover>"#;
        assert_eq!(detect_response_schema(body), ResponseSchema::Outlook);
    }

    #[test]
    fn test_detect_response_schema_mobilesync() {
        let body = r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/mobilesync/requestschema/2006">
<Request>
<EMailAddress>user@example.com</EMailAddress>
<AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/mobilesync/responseschema/2006</AcceptableResponseSchema>
</Request>
</Autodiscover>"#;
        assert_eq!(detect_response_schema(body), ResponseSchema::MobileSync);
    }

    #[test]
    fn test_detect_response_schema_default() {
        let body = "<Autodiscover><Request><EMailAddress>user@example.com</EMailAddress></Request></Autodiscover>";
        assert_eq!(detect_response_schema(body), ResponseSchema::Outlook);
    }

    #[test]
    fn test_detect_response_schema_empty() {
        assert_eq!(detect_response_schema(""), ResponseSchema::Outlook);
    }

    #[test]
    fn test_mobilesync_response_format() {
        let (status, _hdrs, body) =
            handle_mobilesync_xml("mail.example.com", "user@example.com", None);
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("mobilesync/responseschema/2006"));
        assert!(body.contains("https://mail.example.com/Microsoft-Server-ActiveSync"));
        assert!(body.contains("<Type>MobileSync</Type>"));
        assert!(!body.contains("<Type>EXCH</Type>"));
        assert!(!body.contains("<Type>EXPR</Type>"));
        assert!(body.contains("<Culture>en:us</Culture>"));
        assert!(body.contains("<Action>"));
        assert!(body.contains("<Settings>"));
    }

    #[test]
    fn test_outlook_response_format() {
        let (status, _hdrs, body) = handle_outlook_xml("mail.example.com", "user@example.com");
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("outlook/responseschema/2006a"));
        assert!(body.contains("https://mail.example.com/EWS/Exchange.asmx"));
        assert!(body.contains("https://mail.example.com/Microsoft-Server-ActiveSync"));
        assert!(body.contains("<Type>EXCH</Type>"));
        assert!(body.contains("<Type>EXPR</Type>"));
        assert!(body.contains("<ServerExclusiveConnect>on</ServerExclusiveConnect>"));
    }

    #[test]
    fn test_autodiscover_xml_dispatches_mobilesync() {
        let body = r#"<Autodiscover><Request>
<EMailAddress>user@example.com</EMailAddress>
<AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/mobilesync/responseschema/2006</AcceptableResponseSchema>
</Request></Autodiscover>"#;
        let (status, _, body_out) =
            handle_autodiscover_xml("mail.example.com", body, "user@example.com", None);
        assert_eq!(status, StatusCode::OK);
        assert!(body_out.contains("mobilesync/responseschema/2006"));
        assert!(!body_out.contains("outlook/responseschema/2006a"));
    }

    #[test]
    fn test_autodiscover_xml_dispatches_outlook() {
        let body = r#"<Autodiscover><Request>
<EMailAddress>user@example.com</EMailAddress>
<AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</AcceptableResponseSchema>
</Request></Autodiscover>"#;
        let (status, _, body_out) =
            handle_autodiscover_xml("mail.example.com", body, "user@example.com", None);
        assert_eq!(status, StatusCode::OK);
        assert!(body_out.contains("outlook/responseschema/2006a"));
        assert!(!body_out.contains("mobilesync/responseschema/2006"));
    }

    #[test]
    fn test_autodiscover_xml_default_is_outlook() {
        let body = "<Autodiscover><Request><EMailAddress>user@example.com</EMailAddress></Request></Autodiscover>";
        let (status, _, body_out) =
            handle_autodiscover_xml("mail.example.com", body, "user@example.com", None);
        assert_eq!(status, StatusCode::OK);
        assert!(body_out.contains("outlook/responseschema/2006a"));
    }

    #[test]
    fn test_detect_response_schema_namespaced_tag() {
        let body = r#"<Autodiscover xmlns:a="http://schemas.microsoft.com/exchange/autodiscover/mobilesync/requestschema/2006">
<a:Request>
<a:EMailAddress>user@example.com</a:EMailAddress>
<a:AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/mobilesync/responseschema/2006</a:AcceptableResponseSchema>
</a:Request>
</Autodiscover>"#;
        assert_eq!(detect_response_schema(body), ResponseSchema::MobileSync);
    }

    #[test]
    fn test_detect_response_schema_tag_with_attributes() {
        let body = r#"<Autodiscover>
<Request>
<EMailAddress>user@example.com</EMailAddress>
<AcceptableResponseSchema xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a">http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</AcceptableResponseSchema>
</Request>
</Autodiscover>"#;
        assert_eq!(detect_response_schema(body), ResponseSchema::Outlook);
    }

    #[test]
    fn test_detect_response_schema_namespaced_outlook() {
        let body = r#"<a:Autodiscover xmlns:a="http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006">
<a:Request>
<a:EMailAddress>user@example.com</a:EMailAddress>
<a:AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</a:AcceptableResponseSchema>
</a:Request>
</a:Autodiscover>"#;
        assert_eq!(detect_response_schema(body), ResponseSchema::Outlook);
    }

    #[test]
    fn test_parse_culture_default() {
        assert_eq!(parse_culture_from_accept_language(None), "en:us");
        assert_eq!(parse_culture_from_accept_language(Some("")), "en:us");
    }

    #[test]
    fn test_parse_culture_from_header() {
        assert_eq!(
            parse_culture_from_accept_language(Some("de-DE, en-US;q=0.5")),
            "de:de"
        );
        assert_eq!(parse_culture_from_accept_language(Some("en-US")), "en:us");
        assert_eq!(parse_culture_from_accept_language(Some("fr")), "fr:fr");
        assert_eq!(parse_culture_from_accept_language(Some("ja")), "ja:ja");
        assert_eq!(parse_culture_from_accept_language(Some("zh-CN")), "zh:cn");
        assert_eq!(
            parse_culture_from_accept_language(Some("zh-Hans-CN")),
            "zh:cn"
        );
        assert_eq!(
            parse_culture_from_accept_language(Some("en-US;q=0.9, de-DE;q=0.8")),
            "en:us"
        );
    }

    #[test]
    fn test_mobilesync_response_culture_from_header() {
        let (status, _hdrs, body) = handle_mobilesync_xml(
            "mail.example.com",
            "user@example.com",
            Some("de-DE, en-US;q=0.5"),
        );
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<Culture>de:de</Culture>"));
    }

    // --- V2 JSON tests ---

    #[test]
    fn test_json_activesync_protocol() {
        let (status, _hdrs, body) =
            handle_autodiscover_json("mail.example.com", Some("ActiveSync"), None);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            r#"{"Protocol":"ActiveSync","Url":"https://mail.example.com/Microsoft-Server-ActiveSync"}"#
        );
    }

    #[test]
    fn test_json_ews_protocol() {
        let (status, _hdrs, body) =
            handle_autodiscover_json("mail.example.com", Some("Ews"), None);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            r#"{"Protocol":"Ews","Url":"https://mail.example.com/EWS/Exchange.asmx"}"#
        );
    }

    #[test]
    fn test_json_autodiscoverv1_protocol() {
        let (status, _hdrs, body) =
            handle_autodiscover_json("mail.example.com", Some("AutodiscoverV1"), None);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            r#"{"Protocol":"AutodiscoverV1","Url":"https://mail.example.com/autodiscover/autodiscover.xml"}"#
        );
    }

    #[test]
    fn test_json_rest_protocol() {
        let (status, _hdrs, body) =
            handle_autodiscover_json("mail.example.com", Some("Rest"), None);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            r#"{"Protocol":"Rest","Url":"https://mail.example.com/EWS/Exchange.asmx"}"#
        );
    }

    #[test]
    fn test_json_default_is_activesync() {
        let (status, _hdrs, body) =
            handle_autodiscover_json("mail.example.com", None, None);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            r#"{"Protocol":"ActiveSync","Url":"https://mail.example.com/Microsoft-Server-ActiveSync"}"#
        );
    }

    #[test]
    fn test_json_unrecognized_protocol_defaults_activesync() {
        let (status, _hdrs, body) =
            handle_autodiscover_json("mail.example.com", Some("Substrate"), None);
        assert_eq!(status, StatusCode::OK);
        // Unrecognized protocols default to ActiveSync
        assert!(body.contains(r#""Protocol":"ActiveSync""#));
    }

    #[test]
    fn test_json_protocol_case_insensitive() {
        let (status, _hdrs, body) =
            handle_autodiscover_json("mail.example.com", Some("activesync"), None);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            r#"{"Protocol":"ActiveSync","Url":"https://mail.example.com/Microsoft-Server-ActiveSync"}"#
        );
    }

    #[test]
    fn test_json_no_extra_fields_in_activesync() {
        let (status, _hdrs, body) =
            handle_autodiscover_json("mail.example.com", Some("ActiveSync"), None);
        assert_eq!(status, StatusCode::OK);
        // V2 JSON must NOT contain V1-XML-era fields like ActiveSyncUrl,
        // MobileSyncUrl, EwsUrl, etc. — only Protocol + Url.
        assert!(!body.contains("ActiveSyncUrl"));
        assert!(!body.contains("MobileSyncUrl"));
        assert!(!body.contains("EwsUrl"));
        assert!(!body.contains("ExternalEwsUrl"));
    }

    #[test]
    fn test_json_no_exchange_protocol_name() {
        let (status, _hdrs, body) =
            handle_autodiscover_json("mail.example.com", None, None);
        assert_eq!(status, StatusCode::OK);
        // "Exchange" is not a valid V2 Protocol name
        assert!(!body.contains(r#""Protocol":"Exchange""#));
    }
}

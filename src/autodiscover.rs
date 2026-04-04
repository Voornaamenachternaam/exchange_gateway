use axum::{
    body::Bytes,
    extract::{Query, State},
    http::HeaderMap,
    response::IntoResponse,
};
use std::collections::HashMap;
use std::sync::Arc;

use crate::handlers::{make_json_response, make_soap_response, make_xml_response};
use crate::models::AppState;

pub async fn autodiscover_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let accept = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let path = headers
        .get("x-original-url")
        .or_else(|| headers.get("x-forwarded-uri"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if path.contains(".json") || accept.contains("json") || params.contains_key("Protocol") {
        return handle_json_autodiscover(&state).await;
    }

    if content_type.contains("soap") || path.contains(".svc") {
        return handle_soap_autodiscover(&state, body).await;
    }

    handle_xml_autodiscover(&state, body).await
}

async fn handle_xml_autodiscover(state: &AppState, body: Bytes) -> impl IntoResponse {
    let email = String::from_utf8_lossy(&body)
        .lines()
        .find(|l| l.contains("EMailAddress") || l.contains("EmailAddress"))
        .and_then(|line| {
            let start = line.find('>')? + 1;
            let end = line.rfind('<')?;
            Some(line[start..end].to_string())
        })
        .unwrap_or_default();

    let domain = extract_domain(&email);
    let gateway_host = extract_gateway_host(&state.cfg.worker_url);

    let xml = format!(r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">
  <Response xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a">
    <User>
      <DisplayName>Stalwart Mail</DisplayName>
      <EMailAddress>{}</EMailAddress>
      <DeploymentId>00000000-0000-0000-0000-000000000000</DeploymentId>
    </User>
    <Account>
      <AccountType>email</AccountType>
      <Action>settings</Action>
      <Protocol>
        <Type>EXCH</Type>
        <Server>{}</Server>
        <ServerDN>/o=Exchange/ou=Exchange Administrative Group/cn=Recipients/cn=user</ServerDN>
        <ServerVersion>15.20.0.0</ServerVersion>
        <MdbDN />
        <ASUrl>https://{}/Microsoft-Server-ActiveSync</ASUrl>
        <EwsUrl>https://{}/EWS/Exchange.asmx</EwsUrl>
        <EmwsUrl>https://{}/EWS/Exchange.asmx</EmwsUrl>
        <EcpUrl>https://{}/EWS/Exchange.asmx</EcpUrl>
        <OABUrl>https://{}/EWS/Exchange.asmx</OABUrl>
        <OOFUrl>https://{}/EWS/Exchange.asmx</OOFUrl>
        <UMUrl>https://{}/EWS/Exchange.asmx</UMUrl>
        <EwsPartnerUrl>https://{}/EWS/Exchange.asmx</EwsPartnerUrl>
        <LoginName>{}</LoginName>
        <DomainRequired>off</DomainRequired>
        <SPA>off</SPA>
        <AuthPackage>Basic</AuthPackage>
        <CertPrincipalName>None</CertPrincipalName>
        <SSL>on</SSL>
        <AuthRequired>on</AuthRequired>
      </Protocol>
      <Protocol>
        <Type>EXPR</Type>
        <Server>{}</Server>
        <SSL>on</SSL>
        <SPA>off</SPA>
        <CertPrincipalName>None</CertPrincipalName>
        <AuthPackage>Basic</AuthPackage>
        <LoginName>{}</LoginName>
        <ServerExclusiveConnect>off</ServerExclusiveConnect>
        <TTL>1</TTL>
        <ASUrl>https://{}/Microsoft-Server-ActiveSync</ASUrl>
        <EwsUrl>https://{}/EWS/Exchange.asmx</EwsUrl>
        <EmwsUrl>https://{}/EWS/Exchange.asmx</EmwsUrl>
        <EcpUrl>https://{}/EWS/Exchange.asmx</EcpUrl>
        <OABUrl>https://{}/EWS/Exchange.asmx</OABUrl>
        <OOFUrl>https://{}/EWS/Exchange.asmx</OOFUrl>
        <EwsPartnerUrl>https://{}/EWS/Exchange.asmx</EwsPartnerUrl>
      </Protocol>
      <Protocol>
        <Type>MobileSync</Type>
        <Server>{}</Server>
        <Name>Exchange Gateway</Name>
        <Url>https://{}/Microsoft-Server-ActiveSync</Url>
        <LoginName>{}</LoginName>
        <DomainRequired>off</DomainRequired>
        <SSL>on</SSL>
        <AuthPackage>Basic</AuthPackage>
        <ASUrl>https://{}/Microsoft-Server-ActiveSync</ASUrl>
      </Protocol>
    </Account>
  </Response>
</Autodiscover>"#,
        xml_escape(&email),
        gateway_host, gateway_host, gateway_host, gateway_host, gateway_host,
        gateway_host, gateway_host, gateway_host, gateway_host, xml_escape(&email),
        gateway_host, xml_escape(&email), gateway_host, gateway_host, gateway_host,
        gateway_host, gateway_host, gateway_host,
        gateway_host, gateway_host, xml_escape(&email), gateway_host
    );

    make_xml_response(xml)
}

async fn handle_soap_autodiscover(state: &AppState, body: Bytes) -> impl IntoResponse {
    let email = String::from_utf8_lossy(&body)
        .lines()
        .find(|l| l.contains("EMailAddress"))
        .and_then(|line| {
            let start = line.find('>')? + 1;
            let end = line.rfind('<')?;
            Some(line[start..end].to_string())
        })
        .unwrap_or_default();

    let gateway_host = extract_gateway_host(&state.cfg.worker_url);

    let xml = format!(r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:a="http://schemas.microsoft.com/exchange/2010/Autodiscover">
  <s:Header>
    <a:ServerVersionInfo MajorVersion="15" MinorVersion="20" MajorBuildNumber="0" MinorBuildNumber="0" Version="Exchange2016" />
  </s:Header>
  <s:Body>
    <a:GetUserSettingsResponseMessage>
      <a:Response>
        <a:ErrorCode>NoError</a:ErrorCode>
        <a:ErrorMessage />
        <a:UserResponses>
          <a:UserResponse>
            <a:ErrorCode>NoError</a:ErrorCode>
            <a:ErrorMessage />
            <a:RedirectTarget />
            <a:UserSettingErrors />
            <a:UserSettings>
              <a:UserSetting><a:Name>UserDisplayName</a:Name><a:Value>Stalwart Mail</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>UserDN</a:Name><a:Value>{}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>AutoDiscoverSMTPAddress</a:Name><a:Value>{}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalRpcClientServer</a:Name><a:Value>{}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalEwsUrl</a:Name><a:Value>https://{}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalEwsUrl</a:Name><a:Value>https://{}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalEmwsUrl</a:Name><a:Value>https://{}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalEmwsUrl</a:Name><a:Value>https://{}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalEcpUrl</a:Name><a:Value>https://{}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalEcpUrl</a:Name><a:Value>https://{}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalOABUrl</a:Name><a:Value>https://{}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalOABUrl</a:Name><a:Value>https://{}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>MobileSyncServer</a:Name><a:Value>{}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalMobileSyncUrl</a:Name><a:Value>https://{}/Microsoft-Server-ActiveSync</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalMobileSyncUrl</a:Name><a:Value>https://{}/Microsoft-Server-ActiveSync</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalEwsVersion</a:Name><a:Value>Exchange2016</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalEwsVersion</a:Name><a:Value>Exchange2016</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>EwsSupportedSchemas</a:Name><a:Value>Exchange2007,Exchange2007_SP1,Exchange2010,Exchange2010_SP1,Exchange2010_SP2,Exchange2013,Exchange2013_SP1,Exchange2016</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>PublicFolderServer</a:Name><a:Value>{}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ActiveDirectoryServer</a:Name><a:Value>{}</a:Value></a:UserSetting>
            </a:UserSettings>
          </a:UserResponse>
        </a:UserResponses>
      </a:Response>
    </a:GetUserSettingsResponseMessage>
  </s:Body>
</s:Envelope>"#,
        xml_escape(&email), xml_escape(&email), gateway_host, gateway_host, gateway_host,
        gateway_host, gateway_host, gateway_host, gateway_host, gateway_host, gateway_host,
        gateway_host, gateway_host, gateway_host, gateway_host
    );

    make_soap_response(xml)
}

async fn handle_json_autodiscover(state: &AppState) -> impl IntoResponse {
    let gateway_host = extract_gateway_host(&state.cfg.worker_url);

    let json = serde_json::json!({
        "Protocol": "Exchange",
        "Url": format!("https://{}/EWS/Exchange.asmx", gateway_host),
        "EwsUrl": format!("https://{}/EWS/Exchange.asmx", gateway_host),
        "ExternalEwsUrl": format!("https://{}/EWS/Exchange.asmx", gateway_host),
        "InternalEwsUrl": format!("https://{}/EWS/Exchange.asmx", gateway_host),
        "ActiveSyncUrl": format!("https://{}/Microsoft-Server-ActiveSync", gateway_host),
        "MobileSyncUrl": format!("https://{}/Microsoft-Server-ActiveSync", gateway_host),
        "ExternalEwsVersion": "Exchange2016",
        "EwsSupportedSchemas": "Exchange2007,Exchange2007_SP1,Exchange2010,Exchange2010_SP1,Exchange2010_SP2,Exchange2013,Exchange2013_SP1,Exchange2016"
    });

    make_json_response(json.to_string())
}

fn extract_domain(email: &str) -> String {
    email.split('@').nth(1).unwrap_or("example.com").to_string()
}

fn extract_gateway_host(worker_url: &str) -> String {
    worker_url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("exchange.example.com")
        .to_string()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

// src/eas.rs
use crate::models::AppState;
use crate::sync;
use crate::wbxml::Wbxml;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::{
    body::Bytes,
    http::{Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use quick_xml::Reader;
use quick_xml::events::Event;
use std::collections::HashMap;
use std::sync::Arc;

fn parse_basic_auth(headers: &HeaderMap) -> Option<(String, String)> {
    if let Some(v) = headers.get(header::AUTHORIZATION)
        && let Ok(s) = v.to_str()
    {
        let s = s.trim();
        if s.to_ascii_lowercase().starts_with("basic ") {
            let b64 = &s[6..].trim();
            let mut out = Vec::new();
            if BASE64.decode_vec(b64.as_bytes(), &mut out).is_ok()
                && let Ok(creds) = String::from_utf8(out)
                && let Some(idx) = creds.find(':')
            {
                return Some((creds[..idx].to_string(), creds[idx + 1..].to_string()));
            }
        }
    }
    None
}

fn extract_root_command(xml: &str) -> Option<String> {
    if xml.trim().is_empty() {
        return None;
    }
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().local_name().as_ref()).to_string();
                return Some(name);
            }
            Ok(Event::Eof) => return None,
            Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

fn extract_first_tag_text(xml: &str, tag: &[u8]) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut inside = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().local_name().as_ref() == tag => {
                inside = true;
            }
            Ok(Event::Text(t)) if inside => {
                return t.decode().ok().map(|v| v.into_owned());
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref() == tag => {
                inside = false;
            }
            Ok(Event::Eof) => return None,
            Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

fn extract_sync_key(xml: &str) -> Option<String> {
    extract_first_tag_text(xml, b"SyncKey")
}

fn command_from_query(query: &HashMap<String, String>) -> Option<String> {
    query
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("Cmd"))
        .map(|(_, v)| v.clone())
}

fn standard_headers(content_type: &'static str) -> [(&'static str, &'static str); 5] {
    [
        (header::CONTENT_TYPE.as_str(), content_type),
        ("MS-Server-ActiveSync", "16.1"),
        ("Cache-Control", "private, no-store"),
        ("Pragma", "no-cache"),
        ("X-MS-ProtocolVersion", "16.1"),
    ]
}

fn unauth_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [
            (
                header::WWW_AUTHENTICATE.as_str(),
                "Basic realm=\"Microsoft-Server-ActiveSync\"",
            ),
            ("MS-Server-ActiveSync", "16.1"),
        ],
        "Unauthorized",
    )
        .into_response()
}

fn options_response() -> Response {
    (
        StatusCode::OK,
        [
            ("Allow", "OPTIONS,POST"),
            ("MS-Server-ActiveSync", "16.1"),
            ("MS-ASProtocolVersions", "12.0,12.1,14.0,14.1,16.0,16.1"),
            (
                "MS-ASProtocolCommands",
                "Sync,SendMail,SmartForward,SmartReply,GetAttachment,FolderSync,FolderCreate,FolderDelete,FolderUpdate,MoveItems,GetItemEstimate,MeetingResponse,Search,Settings,Ping,ItemOperations,Provision,ResolveRecipients,ValidateCert",
            ),
            ("Cache-Control", "private, no-store"),
        ],
        "",
    )
        .into_response()
}

fn xml_or_wbxml_response(wbxml: &Wbxml, as_wbxml: bool, xml: &str) -> Response {
    if as_wbxml {
        match wbxml.encode(xml) {
            Ok(b) => (
                StatusCode::OK,
                standard_headers("application/vnd.ms-sync.wbxml"),
                b,
            )
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                standard_headers("text/plain; charset=utf-8"),
                format!("WBXML Encode Err: {}", e),
            )
                .into_response(),
        }
    } else {
        (
            StatusCode::OK,
            standard_headers("application/xml; charset=utf-8"),
            xml.to_string(),
        )
            .into_response()
    }
}

fn unsupported_command_response(cmd: &str, wbxml: &Wbxml, as_wbxml: bool) -> Response {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><Status xmlns=\"AirSync:\">5</Status><!-- Unsupported command: {} -->",
        cmd
    );
    xml_or_wbxml_response(wbxml, as_wbxml, &body)
}

fn handle_provision(xml: &str, wbxml: &Wbxml, as_wbxml: bool) -> Response {
    let policy_key = extract_first_tag_text(xml, b"PolicyKey").unwrap_or_else(|| "0".to_string());

    // Two-phase provisioning handshake per ActiveSync behavior:
    // - First request (PolicyKey=0) => issue server policy key.
    // - Second request (client echoes policy key) => acknowledge success.
    let server_policy_key = "12345";
    let response = if policy_key == "0" {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<Provision xmlns="Provision:"><Status>1</Status><Policies><Policy><PolicyType>MS-EAS-Provisioning-WBXML</PolicyType><Status>1</Status><PolicyKey>{}</PolicyKey></Policy></Policies></Provision>"#,
            server_policy_key
        )
    } else {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<Provision xmlns="Provision:"><Status>1</Status><Policies><Policy><PolicyType>MS-EAS-Provisioning-WBXML</PolicyType><Status>1</Status><PolicyKey>{}</PolicyKey></Policy></Policies></Provision>"#,
            policy_key
        )
    };

    xml_or_wbxml_response(wbxml, as_wbxml, &response)
}

fn handle_send_mail(wbxml: &Wbxml, as_wbxml: bool) -> Response {
    // MS-ASEMAIL / ComposeMail command path acknowledgement.
    let resp_xml = r#"<?xml version="1.0" encoding="utf-8"?><SendMail xmlns="ComposeMail:"><Status>1</Status></SendMail>"#;
    xml_or_wbxml_response(wbxml, as_wbxml, resp_xml)
}

pub async fn handle(
    State(state): State<Arc<AppState>>,
    method: Method,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if method == Method::OPTIONS {
        return options_response();
    }

    let Some((username, password)) = parse_basic_auth(&headers) else {
        return unauth_response();
    };

    let wbxml = Wbxml::new();
    let payload = body.to_vec();
    let wants_wbxml = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.to_ascii_lowercase().contains("wbxml"))
        .unwrap_or(payload.first().is_some_and(|b| *b != b'<'));

    let xml = if payload.is_empty() {
        String::new()
    } else {
        match wbxml.decode(&payload) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("WBXML Decode Error: {}", e);
                return (
                    StatusCode::BAD_REQUEST,
                    standard_headers("text/plain; charset=utf-8"),
                    format!("Invalid body: {}", e),
                )
                    .into_response();
            }
        }
    };

    let command = extract_root_command(&xml)
        .or_else(|| command_from_query(&query))
        .unwrap_or_default();

    match command.as_str() {
        "FolderSync" => {
            let resp_xml = r#"<?xml version="1.0" encoding="utf-8"?>
<FolderSync xmlns="FolderHierarchy:"><Status>1</Status><SyncKey>1</SyncKey><Changes><Count>1</Count><Add><ServerId>1</ServerId><ParentId>0</ParentId><DisplayName>Calendar</DisplayName><Type>8</Type></Add></Changes></FolderSync>"#;
            xml_or_wbxml_response(&wbxml, wants_wbxml, resp_xml)
        }
        "Provision" => handle_provision(&xml, &wbxml, wants_wbxml),
        "Sync" => {
            let owner = username.as_str();
            let collection_id = "1";
            let incoming_key = extract_sync_key(&xml).unwrap_or_else(|| "0".to_string());

            match sync::perform_sync(
                state,
                owner,
                collection_id,
                &incoming_key,
                100,
                &username,
                &password,
            )
            .await
            {
                Ok(resp_xml) => xml_or_wbxml_response(&wbxml, wants_wbxml, &resp_xml),
                Err(e) => {
                    tracing::error!("Sync Error: {}", e);
                    let err_xml = r#"<?xml version="1.0" encoding="utf-8"?><Sync xmlns="AirSync:"><Status>6</Status></Sync>"#;
                    xml_or_wbxml_response(&wbxml, wants_wbxml, err_xml)
                }
            }
        }
        "Ping" => {
            let resp_xml = r#"<?xml version="1.0" encoding="utf-8"?><Ping xmlns="Ping:"><Status>1</Status></Ping>"#;
            xml_or_wbxml_response(&wbxml, wants_wbxml, resp_xml)
        }
        "Settings" => {
            let resp_xml = r#"<?xml version="1.0" encoding="utf-8"?><Settings xmlns="Settings:"><Status>1</Status></Settings>"#;
            xml_or_wbxml_response(&wbxml, wants_wbxml, resp_xml)
        }
        "SendMail" => handle_send_mail(&wbxml, wants_wbxml),
        "SmartReply" | "SmartForward" => {
            let resp_xml =
                r#"<?xml version="1.0" encoding="utf-8"?><Status xmlns="ComposeMail:">1</Status>"#;
            xml_or_wbxml_response(&wbxml, wants_wbxml, resp_xml)
        }
        _ => unsupported_command_response(&command, &wbxml, wants_wbxml),
    }
}

#[cfg(test)]
mod tests {
    use super::{command_from_query, extract_root_command, extract_sync_key};
    use std::collections::HashMap;

    #[test]
    fn parses_root_command() {
        let xml = r#"<?xml version=\"1.0\"?><Sync xmlns=\"AirSync:\"></Sync>"#;
        assert_eq!(extract_root_command(xml).as_deref(), Some("Sync"));
    }

    #[test]
    fn parses_sync_key() {
        let xml = r#"<Sync xmlns=\"AirSync:\"><Collections><Collection><SyncKey>123</SyncKey></Collection></Collections></Sync>"#;
        assert_eq!(extract_sync_key(xml).as_deref(), Some("123"));
    }

    #[test]
    fn command_from_query_case_insensitive() {
        let mut q = HashMap::new();
        q.insert("cmd".to_string(), "Ping".to_string());
        assert_eq!(command_from_query(&q).as_deref(), Some("Ping"));
    }
}

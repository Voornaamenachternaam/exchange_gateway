// src/ews.rs
use crate::models::AppState;
use axum::{
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use quick_xml::Reader;
use quick_xml::events::Event;
use std::sync::Arc;

pub async fn handle(
    State(_state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if let Some(auth) = headers.get("authorization") {
        if let Ok(auth_str) = auth.to_str() {
            if let Some(b64) = auth_str.trim().strip_prefix("Basic ") {
                let mut decoded = Vec::new();
                if STANDARD.decode_vec(b64.as_bytes(), &mut decoded).is_ok() {
                    if String::from_utf8(decoded).is_ok() {
                        // Authentication passed
                    } else {
                        return unauthorized();
                    }
                } else {
                    return unauthorized();
                }
            } else {
                return unauthorized();
            }
        } else {
            return unauthorized();
        }
    } else {
        return unauthorized();
    }

    let mut reader = Reader::from_str(&body);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut action = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name().local_name();
                if name.as_ref() == b"GetFolder" {
                    action = "GetFolder".to_string();
                } else if name.as_ref() == b"FindItem" {
                    action = "FindItem".to_string();
                } else if name.as_ref() == b"SyncFolderItems" {
                    action = "SyncFolderItems".to_string();
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }

    match action.as_str() {
        "GetFolder" => get_folder_response(),
        _ => soap_response_empty(),
    }
}

fn unauthorized() -> Response {
    (
        axum::http::StatusCode::UNAUTHORIZED,
        [("WWW-Authenticate", "Basic realm=\"EWS\"")],
        "Unauthorized",
    )
        .into_response()
}

fn get_folder_response() -> Response {
    let resp = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
<s:Body>
<m:GetFolderResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <m:ResponseMessages>
    <m:GetFolderResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:Folders>
        <t:CalendarFolder>
          <t:FolderId Id="AQAhAG..." ChangeKey="AQAAAB..." />
          <t:DisplayName>Calendar</t:DisplayName>
          <t:TotalCount>0</t:TotalCount>
        </t:CalendarFolder>
      </m:Folders>
    </m:GetFolderResponseMessage>
  </m:ResponseMessages>
</s:Body>
</s:Envelope>"#;
    (
        axum::http::StatusCode::OK,
        [("Content-Type", "text/xml; charset=utf-8")],
        resp.to_string(),
    )
        .into_response()
}

fn soap_response_empty() -> Response {
    (
        axum::http::StatusCode::OK,
        [("Content-Type", "text/xml; charset=utf-8")],
        r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><ResponseMessage ResponseClass="Success"><ResponseCode>NoError</ResponseCode></ResponseMessage></s:Body></s:Envelope>"#
    ).into_response()
}

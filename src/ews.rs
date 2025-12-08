use bytes::Bytes;
use warp::reply::Response;
use warp::http::StatusCode;
use crate::caldav;
use crate::caldav::AppState;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as base64_engine;
use quick_xml::Reader;
use quick_xml::events::Event;
use std::convert::Infallible;

pub async fn handle_ews(state: std::sync::Arc<AppState>, auth: Option<String>, body: Bytes) -> Result<impl warp::Reply, Infallible> {
    // Basic auth parsing
    let (user, pass) = match parse_basic(auth) {
        Ok(v) => v,
        Err(_) => {
            let res = warp::reply::with_status("Unauthorized", StatusCode::UNAUTHORIZED);
            return Ok(res);
        }
    };

    // Instantiate CalDAV client for this user
    let caldav_client = match caldav::make_caldav_client(&state.cfg, &user, &pass).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("CalDAV client error: {:?}", e);
            return Ok(warp::reply::with_status("Bad Gateway", StatusCode::BAD_GATEWAY));
        }
    };

    // Parse SOAP body to determine operation
    let mut reader = Reader::from_reader(body.reader());
    reader.trim_text(true);
    let mut buf = Vec::new();
    let mut op: Option<String> = None;
    loop {
        match reader.read_event(&mut buf) {
            Ok(Event::Start(e)) => {
                match e.name() {
                    b"FindItem" => { op = Some("FindItem".to_string()); break; }
                    b"GetItem" => { op = Some("GetItem".to_string()); break; }
                    b"CreateItem" => { op = Some("CreateItem".to_string()); break; }
                    b"UpdateItem" => { op = Some("UpdateItem".to_string()); break; }
                    b"DeleteItem" => { op = Some("DeleteItem".to_string()); break; }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => { tracing::error!("XML parse error: {:?}", e); break; }
            _ => {}
        }
        buf.clear();
    }

    match op.as_deref() {
        Some("FindItem") => {
            // map to CalDAV query (implementation placeholder)
            let resp = build_ews_finditem_response().await;
            Ok(warp::reply::with_status(resp, StatusCode::OK))
        }
        Some("GetItem") => {
            let resp = build_ews_getitem_response().await;
            Ok(warp::reply::with_status(resp, StatusCode::OK))
        }
        Some("CreateItem") => {
            let resp = build_ews_createitem_response().await;
            Ok(warp::reply::with_status(resp, StatusCode::OK))
        }
        Some("UpdateItem") => {
            let resp = build_ews_updateitem_response().await;
            Ok(warp::reply::with_status(resp, StatusCode::OK))
        }
        Some("DeleteItem") => {
            let resp = build_ews_deleteitem_response().await;
            Ok(warp::reply::with_status(resp, StatusCode::OK))
        }
        _ => {
            Ok(warp::reply::with_status("Unsupported EWS operation", StatusCode::BAD_REQUEST))
        }
    }
}

async fn build_ews_finditem_response() -> String {
    // Placeholder: real implementation must query CalDAV and render SOAP XML with items.
    r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <m:FindItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
      <m:ResponseMessages>
        <m:FindItemResponseMessage ResponseClass="Success">
          <m:RootFolder TotalItemsInView="0" IncludesLastItemInRange="false"/>
        </m:FindItemResponseMessage>
      </m:ResponseMessages>
    </m:FindItemResponse>
  </s:Body>
</s:Envelope>"#.to_string()
}

async fn build_ews_getitem_response() -> String {
    r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <m:GetItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"/>
  </s:Body>
</s:Envelope>"#.to_string()
}

async fn build_ews_createitem_response() -> String {
    r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <m:CreateItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"/>
  </s:Body>
</s:Envelope>"#.to_string()
}

async fn build_ews_updateitem_response() -> String {
    r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <m:UpdateItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"/>
  </s:Body>
</s:Envelope>"#.to_string()
}

async fn build_ews_deleteitem_response() -> String {
    r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <m:DeleteItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"/>
  </s:Body>
</s:Envelope>"#.to_string()
}

fn parse_basic(header: Option<String>) -> Result<(String, String), ()> {
    if let Some(h) = header {
        let h = h.trim();
        if h.to_lowercase().starts_with("basic ") {
            let b64 = &h[6..];
            if let Ok(decoded) = base64_engine.decode(b64) {
                if let Ok(s) = String::from_utf8(decoded) {
                    let mut parts = s.splitn(2, ':');
                    if let (Some(u), Some(p)) = (parts.next(), parts.next()) {
                        return Ok((u.to_string(), p.to_string()));
                    }
                }
            }
        }
    }
    Err(())
}

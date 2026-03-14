// src/main.rs
mod active_sync;
mod config;
mod db;
mod ews;
mod jmap_client;
mod utils;
mod wbxml;

use std::sync::Arc;

use axum::{
    Router,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::config::AppConfig;

#[tokio::main]
async fn main() {
    let env_filter = match tracing_subscriber::EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(_) => "info,exchange_gateway=debug"
            .parse()
            .expect("valid default filter"),
    };

    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    let config = Arc::new(match AppConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Configuration Error: {}", e);
            std::process::exit(1);
        }
    });
    let app = Router::new()
        .route(
            "/Microsoft-Server-ActiveSync",
            post(handle_active_sync).options(handle_activesync_options),
        )
        .route("/EWS/Exchange.asmx", post(handle_ews))
        .route("/health", get(|| async { "OK" }))
        .layer(TraceLayer::new_for_http())
        .with_state(config);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], 8134));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to bind to address {}: {}", addr, e);
            std::process::exit(1);
        }
    };
    info!(
        "Exchange Gateway v{} listening on {}",
        env!("CARGO_PKG_VERSION"),
        addr
    );
    axum::serve(listener, app).await.unwrap();
}

async fn handle_active_sync(
    State(config): State<Arc<AppConfig>>,
    Query(query): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");

    if utils::decode_basic_auth(auth_header).is_none() {
        return (
            StatusCode::UNAUTHORIZED,
            [(
                header::WWW_AUTHENTICATE,
                r#"Basic realm="exchange_gateway""#,
            )],
            "Unauthorized".to_string(),
        )
            .into_response();
    }

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok());

    let is_explicit_wbxml = content_type
        .map(|ct| ct.to_ascii_lowercase().contains("wbxml"))
        .unwrap_or(false);

    let is_explicit_xml = content_type
        .map(|ct| {
            let lower = ct.to_ascii_lowercase();
            lower.contains("xml") && !lower.contains("wbxml")
        })
        .unwrap_or(false);

    let (xml_body, is_wbxml) = if is_explicit_xml {
        // Explicitly marked as XML — parse as UTF-8 text
        match std::str::from_utf8(&body) {
            Ok(s) => (s.to_string(), false),
            Err(_) => {
                return (StatusCode::BAD_REQUEST, "Invalid UTF-8".to_string()).into_response();
            }
        }
    } else if is_explicit_wbxml {
        // Explicit WBXML content-type — decode must succeed or return 400
        if body.is_empty() {
            (String::new(), true)
        } else {
            match wbxml::decode(&body) {
                Ok(xml) => (xml, true),
                Err(e) => {
                    tracing::error!("WBXML decode error: {:?}", e);
                    return (
                        StatusCode::BAD_REQUEST,
                        "Unable to decode request body".to_string(),
                    )
                        .into_response();
                }
            }
        }
    } else if body.is_empty() {
        // Allow empty bodies through — some ActiveSync commands legitimately send no body.
        (String::new(), true)
    } else {
        // No Content-Type: sniff by first meaningful byte — WBXML never starts with '<'
        let trimmed = body.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&body);
        let first_meaningful = trimmed.iter().find(|b| !b.is_ascii_whitespace());
        if first_meaningful == Some(&b'<') {
            match std::str::from_utf8(trimmed) {
                Ok(s) => (s.to_string(), false),
                Err(_) => {
                    return (StatusCode::BAD_REQUEST, "Invalid UTF-8".to_string()).into_response();
                }
            }
        } else {
            match wbxml::decode(trimmed) {
                Ok(xml) => (xml, true),
                Err(e) => {
                    tracing::error!("WBXML decode error: {:?}", e);
                    return (
                        StatusCode::BAD_REQUEST,
                        "Unable to decode request body".to_string(),
                    )
                        .into_response();
                }
            }
        }
    };

    let query_cmd = query.get("Cmd").cloned().unwrap_or_default();
    let response_xml = active_sync::process_request(&config, &xml_body, &headers, &query_cmd).await;

    if is_wbxml {
        match wbxml::encode(&response_xml) {
            Ok(wbxml_data) => (
                StatusCode::OK,
                [
                    ("content-type", "application/vnd.ms-sync.wbxml"),
                    ("MS-Server-ActiveSync", "16.1"),
                    ("access-control-allow-origin", "*"),
                ],
                wbxml_data,
            )
                .into_response(),
            Err(e) => {
                tracing::error!("WBXML Encode Error: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [
                        ("content-type", "text/plain; charset=utf-8"),
                        ("MS-Server-ActiveSync", "16.1"),
                        ("access-control-allow-origin", "*"),
                    ],
                    "Internal Server Error",
                )
                    .into_response()
            }
        }
    } else {
        (
            StatusCode::OK,
            [
                ("content-type", "application/xml; charset=utf-8"),
                ("MS-Server-ActiveSync", "16.1"),
                ("access-control-allow-origin", "*"),
            ],
            response_xml,
        )
        .into_response()
    }
}

async fn handle_activesync_options() -> impl IntoResponse {
    (
        StatusCode::OK,
        [
            ("allow", "POST, OPTIONS"),
            ("MS-Server-ActiveSync", "16.1"),
            ("access-control-allow-origin", "*"),
            ("access-control-allow-methods", "POST, OPTIONS"),
            ("access-control-allow-headers", "Authorization, Content-Type, X-MS-DeviceId"),
            ("access-control-max-age", "86400")
        ],
        "",
    )
}

async fn handle_ews(
    State(config): State<Arc<AppConfig>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");

    if !auth_header.to_ascii_lowercase().starts_with("basic ") {
        return (
            StatusCode::UNAUTHORIZED,
            [(
                header::WWW_AUTHENTICATE,
                r#"Basic realm="exchange_gateway""#,
            )],
            "Unauthorized".to_string(),
        )
            .into_response();
    }

    let xml_body = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "text/plain")],
                "Invalid UTF-8".to_string(),
            )
                .into_response();
        }
    };

[{
  "file": "src/ews.rs",
  "start_line": 17,
  "end_line": 17,
  "suggestion": "#[derive(Debug, Error)]\npub enum EwsError {\n    #[error(\"authentication failed: {0}\")]\n    Auth(String),\n    #[error(\"unsupported operation: {0}\")]\n    UnsupportedOperation(String),\n    #[error(\"XML parse error: {0}\")]\n    XmlParse(#[from] quick_xml::DeError),\n    #[error(\"JMAP client error: {0}\")]\n    Jmap(#[from] jmap_client::JmapError),\n    #[error(\"DB error: {0}\")]\n    Db(#[from] db::DbError),\n    #[error(\"internal server error: {0}\")]\n    Internal(String),\n}"
},{
  "file": "src/ews.rs",
  "start_line": 127,
  "end_line": 127,
  "suggestion": ""
},{
  "file": "src/ews.rs",
  "start_line": 65,
  "end_line": 107,
  "suggestion": "pub async fn process_request(\n    config: &AppConfig,\n    xml: &str,\n    headers: &HeaderMap,\n) -> Result<String, EwsError> {\n    let auth_header = match headers.get(\"Authorization\").and_then(|v| v.to_str().ok()) {\n        Some(a) => a,\n        None => return Err(EwsError::Auth(\"Missing Authorization header\".into())),\n    };\n\n    let (user, pass) = match utils::decode_basic_auth(auth_header) {\n        Some((u, p)) => (u, p),\n        None => return Err(EwsError::Auth(\"Invalid Authorization header format\".into())),\n    };\n    let session = match jmap_client::get_session(&config.jmap_url, &user, &pass).await {\n        Ok(s) => s,\n        Err(jmap_client::JmapError::Auth(_)) => {\n            return Err(EwsError::Auth(\"Auth Failed\".into()));\n        }\n        Err(e) => {\n            tracing::error!(\"JMAP Auth failed: {}\", e);\n            return Err(EwsError::Internal(\"Auth Failed\".into()));\n        }\n    };\n    let action = extract_action_name(xml);\n    tracing::info!(\"EWS Request: {}\", action);\n\n    match action.as_str() {\n        \"GetFolder\" => handle_get_folder(&session, xml).await,\n        \"FindFolder\" => handle_find_folder(&session).await,\n        \"SyncFolderHierarchy\" => handle_sync_folder_hierarchy(&session, xml).await,\n        \"SyncFolderItems\" => handle_sync_folder_items(&session, config, &user, xml).await,\n        \"CreateItem\" => handle_create_item(&session, config, xml).await,\n        \"UpdateItem\" => handle_update_item(&session, config, xml).await,\n        \"DeleteItem\" => handle_delete_item(&session, config, xml).await,\n        \"GetItem\" => handle_get_item(&session, config, xml).await,\n        \"FindItem\" => handle_find_item().await,\n        \"ResolveNames\" => handle_resolve_names(&session, xml).await,\n        \"GetAttachment\" => handle_get_attachment(&session, xml).await,\n        \"GetRoomLists\" => handle_get_room_lists().await,\n        \"GetRooms\" => handle_get_rooms().await,\n        _ => Err(EwsError::UnsupportedOperation(format!(\"Unsupported: {}\", action))),\n    }\n}"
},{
  "file": "src/ews.rs",
  "start_line": 18,
  "end_line": 63,
  "suggestion": "async fn handle_sync_folder_hierarchy(session: &jmap_client::JmapSession, xml: &str) -> Result<String, EwsError> {\n    let req: SyncFolderHierarchyRequest = parse_body_content(xml).map_err(EwsError::XmlParse)?;\n    let cal_id = jmap_client::get_default_calendar_id(session).await?;\n    // Generate a stable sync state from the calendar ID so clients can\n    // distinguish initial from subsequent syncs.\n    let sync_state = {\n        let mut h = Sha256::new();\n        h.update(b\"folder-hierarchy:\");\n        h.update(cal_id.as_bytes());\n        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, h.finalize())\n    };\n\n    let changes = match req.sync_state.as_deref() {\n        Some(state) if state == sync_state.as_str() => {\n            // Subsequent sync: hierarchy hasn't changed (single calendar folder),\n            // return empty changes.\n            String::new()\n        }\n        Some(_) => {\n            // Stale or unrecognised sync state (e.g. server restart, calendar\n            // ID change).  Fall back to an initial sync so the client can\n            // recover, consistent with handle_sync_folder_items.\n            tracing::warn!(\n                \"SyncFolderHierarchy: client SyncState does not match; falling back to initial sync\"\n            );\n            format!(\n                r#\"<t:Create><t:CalendarFolder><t:FolderId Id=\"{}\" ChangeKey=\"AQAAABYAAA=\" /><t:DisplayName>Calendar</t:DisplayName></t:CalendarFolder></t:Create>\"#,\n                utils::escape_xml(&cal_id)\n            )\n        }\n        None => {\n            // Initial sync: report the calendar folder as created.\n            format!(\n                r#\"<t:Create><t:CalendarFolder><t:FolderId Id=\"{}\" ChangeKey=\"AQAAABYAAA=\" /><t:DisplayName>Calendar</t:DisplayName></t:CalendarFolder></t:Create>\"#,\n                utils::escape_xml(&cal_id)\n            )\n        }\n    };\n\n    Ok(soap_response(&format!(\n        r#\"<m:SyncFolderHierarchyResponse xmlns:m=\"{}\" xmlns:t=\"{}\"><m:ResponseMessages><m:SyncFolderHierarchyResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode><m:SyncState>{}</m:SyncState><m:IncludesLastItemInRange>true</m:IncludesLastItemInRange><m:Changes>{}</m:Changes></m:SyncFolderHierarchyResponseMessage></m:ResponseMessages></m:SyncFolderHierarchyResponse>\"#,\n        NS_M,\n        NS_T,\n        utils::escape_xml(&sync_state),\n        changes\n    )))\n}"
},{
  "file": "src/ews.rs",
  "start_line": 281,
  "end_line": 295,
  "suggestion": "async fn handle_find_folder(session: &jmap_client::JmapSession) -> Result<String, EwsError> {\n    let cal_id = jmap_client::get_default_calendar_id(session).await?;\n    Ok(soap_response(&format!(\n        r#\"<m:FindFolderResponse xmlns:m=\"{}\" xmlns:t=\"{}\"><m:ResponseMessages><m:FindFolderResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode><m:RootFolder TotalItemsInView=\"1\" IncludesLastItemInRange=\"true\"><t:Folders><t:CalendarFolder><t:FolderId Id=\"{}\" ChangeKey=\"AQAAABYAAA=\" /><t:DisplayName>Calendar</m:DisplayName></t:CalendarFolder></t:Folders></m:RootFolder></m:FindFolderResponseMessage></m:ResponseMessages></m:FindFolderResponse>\"#,\n        NS_M,\n        NS_T,\n        utils::escape_xml(&cal_id)\n    )))\n}"
},{
  "file": "src/ews.rs",
  "start_line": 297,
  "end_line": 346,
  "suggestion": "async fn handle_resolve_names(session: &jmap_client::JmapSession, xml: &str) -> Result<String, EwsError> {\n    let req: ResolveNamesRequest = parse_body_content(xml).map_err(EwsError::XmlParse)?;\n    const MAX_RESOLVE_NAMES_RESULTS: usize = 10;\n\n    let results = jmap_client::search_principals(session, &req.unresolved_entry).await?;\n    if results.is_empty() {\n        return Ok(soap_response(&format!(\n            r#\"<m:ResolveNamesResponse xmlns:m=\"{}\" xmlns:t=\"{}\"><m:ResponseMessages><m:ResolveNamesResponseMessage ResponseClass=\"Error\"><m:ResponseCode>ErrorNameResolutionNoResults</m:ResponseCode><m:MessageText>No results were found.</m:MessageText></m:ResolveNamesResponseMessage></m:ResponseMessages></m:ResolveNamesResponse>\"#,\n            NS_M, NS_T,\n        )));\n    }\n    let mut resolutions = String::new();\n    for p in &results {\n        resolutions.push_str(&format!(r#\"<t:Resolution><t:Mailbox><t:Name>{}</t:Name><t:EmailAddress>{}</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType></t:Mailbox></t:Resolution>\"#,\n            utils::escape_xml(&p.name), utils::escape_xml(&p.email)));\n    }\n    Ok(soap_response(&format!(\n        r#\"<m:ResolveNamesResponse xmlns:m=\"{}\" xmlns:t=\"{}\"><m:ResponseMessages><m:ResolveNamesResponseMessage ResponseClass=\"{}\"><m:ResponseCode>{}</m:ResponseCode>{}<m:ResolutionSet TotalItemsInView=\"{}\">{}</m:ResolutionSet></m:ResolveNamesResponseMessage></m:ResponseMessages></m:ResolveNamesResponse>\"#,\n        NS_M,\n        NS_T,\n        if results.len() > 1 {\n            \"Warning\"\n        } else {\n            \"Success\"\n        },\n        if results.len() > 1 {\n            \"ErrorNameResolutionMultipleResults\"\n        } else {\n            \"NoError\"\n        },\n        if results.len() > 1 {\n            \"<m:MessageText>Multiple results were found.</m:MessageText>\"\n        } else {\n            \"\"\n        },\n        results.len(),\n        resolutions\n    )))\n}"
},{
  "file": "src/ews.rs",
  "start_line": 348,
  "end_line": 384,
  "suggestion": "async fn handle_get_attachment(session: &jmap_client::JmapSession, xml: &str) -> Result<String, EwsError> {\n    let req: GetAttachmentRequest = parse_body_content(xml).map_err(EwsError::XmlParse)?;\n    let mut response_messages = String::new();\n    for attachment_id in req.attachment_ids.items {\n        let id_str = &attachment_id.id;\n        match jmap_client::get_blob(session, id_str).await {\n            Ok(data) => {\n                let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);\n                response_messages.push_str(&format!(\n                    r#\"<m:GetAttachmentResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode><m:Attachments><t:FileAttachment><t:AttachmentId Id=\"{}\"/><t:Content>{}</t:Content></t:FileAttachment></m:Attachments></m:GetAttachmentResponseMessage>\"#,\n                    utils::escape_xml(id_str),\n                    b64\n                ));\n            }\n            Err(jmap_client::JmapError::NotFound(_)) => {\n                tracing::warn!(\"get_blob not found for attachment {}\", id_str);\n                response_messages.push_str(&format!(\n                    r#\"<m:GetAttachmentResponseMessage ResponseClass=\"Error\"><m:ResponseCode>ErrorItemNotFound</m:ResponseCode><m:MessageText>Attachment not found</m:MessageText><m:Attachments><t:FileAttachment><t:AttachmentId Id=\"{}\"/></t:FileAttachment></m:Attachments></m:GetAttachmentResponseMessage>\"#,\n                    utils::escape_xml(id_str)\n                ));\n            }\n            Err(e) => {\n                tracing::error!(\"get_blob failed for attachment {}: {}\", id_str, e);\n                response_messages.push_str(&format!(\n                    r#\"<m:GetAttachmentResponseMessage ResponseClass=\"Error\"><m:ResponseCode>ErrorInternalServerError</m:ResponseCode><m:MessageText>Failed to get attachment</m:MessageText><m:Attachments><t:FileAttachment><t:AttachmentId Id=\"{}\"/></t:FileAttachment></m:Attachments></m:GetAttachmentResponseMessage>\"#,\n                    utils::escape_xml(id_str)\n                ));\n            }\n        }\n    }\n    Ok(soap_response(&format!(\n        r#\"<m:GetAttachmentResponse xmlns:m=\"{}\" xmlns:t=\"{}\"><m:ResponseMessages>{}</m:ResponseMessages></m:GetAttachmentResponse>\"#,\n        NS_M, NS_T, response_messages\n    )))\n}"
},{
  "file": "src/ews.rs",
  "start_line": 387,
  "end_line": 420,
  "suggestion": "async fn handle_get_item(\n    session: &jmap_client::JmapSession,\n    config: &AppConfig,\n    xml: &str,\n) -> Result<String, EwsError> {\n    let req: GetItemRequest = parse_body_content(xml).map_err(EwsError::XmlParse)?;\n    let ids: Vec<String> = req.item_ids.items.iter().map(|i| i.id.clone()).collect();\n    let events = jmap_client::get_events_by_ids(session, &ids).await?;\n    let mut response_messages = String::new();\n    for item_id in &req.item_ids.items {\n        if let Some(event) = events.iter().find(|e| e.id.as_deref() == Some(&item_id.id)) {\n            let rendered = render_ews_calendar_item(event, &config.timezone);\n            response_messages.push_str(&format!(\n                r#\"<m:GetItemResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode><m:Items>{}</m:Items></m:GetItemResponseMessage>\"#,\n                rendered\n            ));\n        } else {\n            response_messages.push_str(r#\"<m:GetItemResponseMessage ResponseClass=\"Error\"><m:ResponseCode>ErrorItemNotFound</m:ResponseCode><m:Items/></m:GetItemResponseMessage>\"#);\n        }\n    }\n    Ok(soap_response(&format!(\n        r#\"<m:GetItemResponse xmlns:m=\"{}\" xmlns:t=\"{}\"><m:ResponseMessages>{}</m:ResponseMessages></m:GetItemResponse>\"#,\n        NS_M, NS_T, response_messages\n    )))\n}"
},{
  "file": "src/ews.rs",
  "start_line": 502,
  "end_line": 682,
  "suggestion": "async fn handle_update_item(\n    session: &jmap_client::JmapSession,\n    config: &AppConfig,\n    xml: &str,\n) -> Result<String, EwsError> {\n    let req: UpdateItemRequest = parse_body_content(xml).map_err(EwsError::XmlParse)?;\n    let tz: Tz = config.timezone.parse().unwrap_or(chrono_tz::UTC);\n\n    // Enum to track the outcome for each ItemChange in request order.\n    enum ResponsePlaceholder {\n        ImmediateSuccess,\n        ImmediateError { id: String, code: &'static str, message: &'static str },\n        Pending { id: String },\n    }\n\n    let mut placeholders = Vec::with_capacity(req.item_changes.items.len());\n    let mut merged: std::collections::HashMap<String, serde_json::Map<String, serde_json::Value>> =\n        std::collections::HashMap::new();\n\n    for change in req.item_changes.items {\n        let id = change.item_id.id;\n\n        // Check for unsupported operations (DeleteItemField, AppendToItemField)\n        if !change.updates.delete_fields.is_empty() || !change.updates.append_fields.is_empty() {\n            tracing::warn!(\n                \"Unsupported update operation for item {}: delete or append fields present\",
}

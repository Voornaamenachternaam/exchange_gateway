use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use std::sync::Arc;

use crate::handlers::{
    extract_basic_auth, extract_device_info, make_wbxml_response, unauthorized_response,
};
use crate::models::AppState;
use crate::sync::{
    apply_client_sync_mutations, filter_type_to_start, perform_sync, render_client_mutation_responses,
    SyncOptions,
};

pub async fn sync_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let is_wbxml = content_type.contains("wbxml") || content_type.contains("vnd.ms-sync");

    let xml = if is_wbxml && !body.is_empty() {
        match state.wbxml.decode(&body) {
            Ok(decoded) => decoded,
            Err(e) => {
                tracing::warn!("WBXML decode error: {}", e);
                return error_response("2");
            }
        }
    } else {
        String::from_utf8_lossy(&body).to_string()
    };

    let (username, password) = match extract_basic_auth(&headers) {
        Some(creds) => creds,
        None => return unauthorized_response(),
    };

    let (device_id, device_type) = extract_device_info(&headers);
    let owner = username.clone();

    let cmd = extract_cmd(&headers).unwrap_or_default();

    let response_xml = match cmd.as_str() {
        "FolderSync" => handle_folder_sync(&state, &xml, &owner).await,
        "FolderCreate" => handle_folder_create(&state, &xml, &owner).await,
        "FolderDelete" => handle_folder_delete(&state, &xml, &owner).await,
        "FolderUpdate" => handle_folder_update(&state, &xml, &owner).await,
        "Sync" => handle_sync(&state, &xml, &owner, &username, &password).await,
        "GetItemEstimate" => handle_get_item_estimate(&state, &xml, &owner).await,
        "MeetingResponse" => handle_meeting_response(&state, &xml, &owner, &username, &password).await,
        "Ping" => handle_ping(&state, &xml, &owner).await,
        "Provision" => handle_provision(&state, &xml, &owner, &device_id).await,
        "Settings" => handle_settings(&state, &xml, &owner, &device_id, &device_type, &headers).await,
        "ResolveRecipients" => handle_resolve_recipients(&state, &xml, &owner).await,
        "ValidateCert" => handle_validate_cert(&state, &xml, &owner).await,
        "Search" => handle_search(&state, &xml, &owner).await,
        "ItemOperations" => handle_item_operations(&state, &xml, &owner).await,
        "MoveItems" => handle_move_items(&state, &xml, &owner).await,
        "" => {
            if xml.contains("FolderSync") {
                handle_folder_sync(&state, &xml, &owner).await
            } else if xml.contains("Sync") {
                handle_sync(&state, &xml, &owner, &username, &password).await
            } else if xml.contains("Provision") {
                handle_provision(&state, &xml, &owner, &device_id).await
            } else if xml.contains("Settings") {
                handle_settings(&state, &xml, &owner, &device_id, &device_type, &headers).await
            } else {
                error_response("2")
            }
        }
        _ => error_response("2"),
    };

    if is_wbxml {
        match state.wbxml.encode(&response_xml) {
            Ok(encoded) => make_wbxml_response(encoded).into_response(),
            Err(e) => {
                tracing::warn!("WBXML encode error: {}", e);
                error_response("2").into_response()
            }
        }
    } else {
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/xml; charset=utf-8")
            .body(axum::body::Body::from(response_xml))
            .unwrap()
    }
}

fn extract_cmd(headers: &HeaderMap) -> Option<String> {
    headers
        .get("ms-asprotocolversion")
        .or_else(|| headers.get("X-MS-ASProtocolVersion"));

    headers
        .get("x-ms-ascmd")
        .or_else(|| headers.get("X-MS-ASCmd"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

async fn handle_folder_sync(state: &Arc<AppState>, xml: &str, owner: &str) -> String {
    let sync_key = extract_tag(xml, "SyncKey").unwrap_or_else(|| "0".to_string());

    let new_sync_key = uuid::Uuid::new_v4().to_string();

    let _ = state
        .storage
        .set_sync_key(owner, "folders", &new_sync_key, None)
        .await;

    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<FolderSync xmlns="FolderHierarchy:">
  <Status>1</Status>
  <SyncKey>{}</SyncKey>
  <Changes>
    <Count>1</Count>
    <Add>
      <ServerId>1</ServerId>
      <ParentId>0</ParentId>
      <DisplayName>Calendar</DisplayName>
      <Type>8</Type>
    </Add>
  </Changes>
</FolderSync>"#,
        new_sync_key
    )
}

async fn handle_folder_create(state: &Arc<AppState>, xml: &str, owner: &str) -> String {
    let display_name = extract_tag(xml, "DisplayName").unwrap_or_default();
    let parent_id = extract_tag(xml, "ParentId").unwrap_or_else(|| "0".to_string());
    let folder_type = extract_tag(xml, "Type").unwrap_or_else(|| "1".to_string());

    let server_id = format!("{}", uuid::Uuid::new_v4());

    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<FolderCreate xmlns="FolderHierarchy:">
  <Status>1</Status>
  <SyncKey>{}</SyncKey>
  <ServerId>{}</ServerId>
</FolderCreate>"#,
        uuid::Uuid::new_v4(),
        server_id
    )
}

async fn handle_folder_delete(state: &Arc<AppState>, xml: &str, owner: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<FolderDelete xmlns="FolderHierarchy:">
  <Status>1</Status>
  <SyncKey>{}</SyncKey>
</FolderDelete>"#,
        uuid::Uuid::new_v4()
    )
}

async fn handle_folder_update(state: &Arc<AppState>, xml: &str, owner: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<FolderUpdate xmlns="FolderHierarchy:">
  <Status>1</Status>
  <SyncKey>{}</SyncKey>
</FolderUpdate>"#,
        uuid::Uuid::new_v4()
    )
}

async fn handle_sync(
    state: &Arc<AppState>,
    xml: &str,
    owner: &str,
    username: &str,
    password: &str,
) -> String {
    let collection_id = extract_tag(xml, "CollectionId").unwrap_or_else(|| "1".to_string());
    let sync_key = extract_tag(xml, "SyncKey").unwrap_or_else(|| "0".to_string());
    let class = extract_tag(xml, "Class").unwrap_or_else(|| "Calendar".to_string());

    let window_size = extract_tag(xml, "WindowSize")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(100);

    let filter_type = extract_tag(xml, "FilterType")
        .and_then(|s| s.parse::<u8>().ok())
        .unwrap_or(4);

    let get_changes = !xml.contains("<GetChanges>0</GetChanges>");

    let client_mutation_results = match apply_client_sync_mutations(
        state.clone(),
        owner,
        &collection_id,
        username,
        password,
        xml,
    )
    .await
    {
        Ok(results) => results,
        Err(e) => {
            tracing::warn!("Client mutation error: {}", e);
            Vec::new()
        }
    };

    let client_responses = render_client_mutation_responses(&client_mutation_results);

    let opts = SyncOptions {
        window_size,
        get_changes,
        filter_start: filter_type_to_start(filter_type),
    };

    match perform_sync(
        state.clone(),
        owner,
        &collection_id,
        &collection_id,
        &sync_key,
        &class,
        opts,
        username,
        password,
        &client_responses,
    )
    .await
    {
        Ok(response) => response,
        Err(e) => {
            tracing::error!("Sync error: {}", e);
            error_response("3")
        }
    }
}

async fn handle_get_item_estimate(state: &Arc<AppState>, xml: &str, owner: &str) -> String {
    let collection_id = extract_tag(xml, "CollectionId").unwrap_or_else(|| "1".to_string());

    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<GetItemEstimate xmlns="GetItemEstimate:">
  <Response>
    <Status>1</Status>
    <Collection>
      <CollectionId>{}</CollectionId>
      <Estimate>0</Estimate>
    </Collection>
  </Response>
</GetItemEstimate>"#,
        collection_id
    )
}

async fn handle_meeting_response(
    state: &Arc<AppState>,
    xml: &str,
    owner: &str,
    username: &str,
    password: &str,
) -> String {
    let request_id = extract_tag(xml, "RequestId").unwrap_or_default();
    let user_response = extract_tag(xml, "UserResponse")
        .and_then(|s| s.parse::<u8>().ok())
        .unwrap_or(0);

    if let Err(e) = crate::sync::apply_meeting_response(
        state.clone(),
        owner,
        username,
        password,
        &request_id,
        user_response,
    )
    .await
    {
        tracing::warn!("Meeting response error: {}", e);
    }

    let status = match user_response {
        1 => "1",
        2 => "1",
        3 => "1",
        _ => "2",
    };

    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<MeetingResponse xmlns="MeetingResponse:">
  <Result>
    <RequestId>{}</RequestId>
    <Status>{}</Status>
  </Result>
</MeetingResponse>"#,
        request_id, status
    )
}

async fn handle_ping(state: &Arc<AppState>, xml: &str, owner: &str) -> String {
    let heartbeat = extract_tag(xml, "HeartbeatInterval")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(480);

    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<Ping xmlns="Ping:">
  <Status>1</Status>
  <HeartbeatInterval>{}</HeartbeatInterval>
  <Folders>
    <Folder>
      <Id>1</Id>
      <Class>Calendar</Class>
    </Folder>
  </Folders>
</Ping>"#,
        heartbeat
    )
}

async fn handle_provision(state: &Arc<AppState>, xml: &str, owner: &str, device_id: &str) -> String {
    let policy_key = uuid::Uuid::new_v4().to_string();

    let _ = state
        .storage
        .set_provision_policy(owner, device_id, &policy_key, "1")
        .await;

    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<Provision xmlns="Provision:">
  <Status>1</Status>
  <Policies>
    <Policy>
      <PolicyType>MS-EAS-Provisioning-WBXML</PolicyType>
      <PolicyKey>{}</PolicyKey>
      <Status>1</Status>
    </Policy>
  </Policies>
</Provision>"#,
        policy_key
    )
}

async fn handle_settings(
    state: &Arc<AppState>,
    xml: &str,
    owner: &str,
    device_id: &str,
    device_type: &str,
    headers: &HeaderMap,
) -> String {
    let friendly_name = extract_tag(xml, "FriendlyName").unwrap_or_default();
    let model = extract_tag(xml, "Model").unwrap_or_default();
    let os = extract_tag(xml, "OS").unwrap_or_default();
    let phone_number = extract_tag(xml, "PhoneNumber").unwrap_or_default();
    let imei = extract_tag(xml, "IMEI").unwrap_or_default();
    let user_agent = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let _ = state
        .storage
        .upsert_device_info(
            owner,
            device_id,
            &friendly_name,
            &model,
            &os,
            &phone_number,
            &imei,
            &user_agent,
        )
        .await;

    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<Settings xmlns="Settings:">
  <Status>1</Status>
  <DeviceInformation>
    <Status>1</Status>
  </DeviceInformation>
  <UserInformation>
    <Status>1</Status>
    <Get>
      <EmailAddresses>
        <SMTPAddress>{}</SMTPAddress>
        <PrimarySmtpAddress>{}</PrimarySmtpAddress>
      </EmailAddresses>
    </Get>
  </UserInformation>
</Settings>"#,
        owner, owner
    )
}

async fn handle_resolve_recipients(state: &Arc<AppState>, xml: &str, owner: &str) -> String {
    let to = extract_tag(xml, "To").unwrap_or_default();

    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<ResolveRecipients xmlns="ResolveRecipients:">
  <Status>1</Status>
  <Response>
    <To>{}</To>
    <Status>1</Status>
    <RecipientCount>1</RecipientCount>
    <Recipient>
      <DisplayName>{}</DisplayName>
      <EmailAddress>{}</EmailAddress>
      <Type>1</Type>
    </Recipient>
  </Response>
</ResolveRecipients>"#,
        to, to, to
    )
}

async fn handle_validate_cert(state: &Arc<AppState>, xml: &str, owner: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<ValidateCert xmlns="ValidateCert:">
  <Status>1</Status>
  <CertificateStatus>1</CertificateStatus>
</ValidateCert>"#)
}

async fn handle_search(state: &Arc<AppState>, xml: &str, owner: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<Search xmlns="Search:">
  <Status>1</Status>
  <Response>
    <Store>
      <Status>1</Status>
      <Total>0</Total>
    </Store>
  </Response>
</Search>"#)
}

async fn handle_item_operations(state: &Arc<AppState>, xml: &str, owner: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<ItemOperations xmlns="ItemOperations:">
  <Status>1</Status>
  <Response>
    <Fetch>
      <Status>1</Status>
    </Fetch>
  </Response>
</ItemOperations>"#)
}

async fn handle_move_items(state: &Arc<AppState>, xml: &str, owner: &str) -> String {
    let src_msg_id = extract_tag(xml, "SrcMsgId").unwrap_or_default();

    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<MoveItems xmlns="Move:">
  <Response>
    <SrcMsgId>{}</SrcMsgId>
    <Status>1</Status>
    <DstMsgId>{}</DstMsgId>
  </Response>
</MoveItems>"#,
        src_msg_id,
        uuid::Uuid::new_v4()
    )
}

fn extract_tag(xml: &str, tag: &str) -> Option<String> {
    let open_tag = format!("<{}>", tag);
    let close_tag = format!("</{}>", tag);

    let start = xml.find(&open_tag)? + open_tag.len();
    let end = xml.find(&close_tag)?;

    Some(xml[start..end].to_string())
}

fn error_response(status: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<Sync xmlns="AirSync:">
  <Status>{}</Status>
</Sync>"#,
        status
    )
}

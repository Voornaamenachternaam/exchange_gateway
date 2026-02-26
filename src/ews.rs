// src/ews.rs
use crate::{config::AppConfig, db, jmap_client, utils};
use axum::http::HeaderMap;
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use quick_xml::Reader;
use quick_xml::escape;
use quick_xml::events::Event;

pub async fn process_request(config: &AppConfig, xml: &str, headers: &HeaderMap) -> String {
    let auth = headers.get("Authorization").unwrap().to_str().unwrap();
    let (user, pass) = utils::decode_basic_auth(auth);

    let session = match jmap_client::get_session(&config.jmap_url, &user, &pass).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("JMAP Auth failed: {}", e);
            return soap_fault("ErrorInternalServerError", "Auth Failed");
        }
    };

    let mut buf = Vec::new();
    let mut action = String::new();
    let mut reader = Reader::from_str(xml);

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let local = e.local_name();
                let name = std::str::from_utf8(local.as_ref()).unwrap_or("");
                if name != "Envelope" && name != "Header" && name != "Body" {
                    action = name.to_string();
                    break;
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
    }

    tracing::info!("EWS Action: {}", action);

    match action.as_str() {
        "GetFolder" => handle_get_folder(&session).await,
        "SyncFolderItems" => handle_sync_folder_items(&session, config, &user, xml).await,
        "CreateItem" => handle_create_item(&session, config, xml).await,
        "FindItem" => handle_find_item(&session).await,
        "ResolveNames" => handle_resolve_names(&session).await,
        _ => soap_fault("ErrorInvalidRequest", "Unsupported EWS Action"),
    }
}

async fn handle_get_folder(session: &jmap_client::JmapSession) -> String {
    let cal_id = match jmap_client::get_default_calendar_id(
        &session.api_url,
        &session.access_token,
        &session.account_id,
    )
    .await
    {
        Ok(id) => id,
        Err(_) => "calendar-default".to_string(),
    };

    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <m:GetFolderResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" 
                         xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
      <m:ResponseMessages>
        <m:GetFolderResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
          <m:Folders>
            <t:CalendarFolder>
              <t:FolderId Id="{}" ChangeKey="AQAAABYAAA=" />
              <t:DisplayName>Calendar</t:DisplayName>
            </t:CalendarFolder>
          </m:Folders>
        </m:GetFolderResponseMessage>
      </m:ResponseMessages>
    </m:GetFolderResponse>
  </s:Body>
</s:Envelope>"#,
        cal_id
    )
}

async fn handle_sync_folder_items(
    session: &jmap_client::JmapSession,
    config: &AppConfig,
    user: &str,
    xml: &str,
) -> String {
    let mut sync_state_in = String::new();
    let mut buf = Vec::new();
    let mut reader = Reader::from_str(xml);

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                if std::str::from_utf8(e.local_name().as_ref()).unwrap_or("") == "SyncState"
                    && let Ok(Event::Text(t)) = reader.read_event_into(&mut buf)
                {
                    // Fix: Use std::str::from_utf8(&t)
                    let text_str = std::str::from_utf8(&t).unwrap_or("");
                    sync_state_in = escape::unescape(text_str).unwrap_or_default().into_owned();
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
    }

    let current_jmap_state = match jmap_client::get_calendar_state(
        &session.api_url,
        &session.access_token,
        &session.account_id,
    )
    .await
    {
        Ok(s) => s,
        Err(_) => return soap_fault("ErrorInternalServerError", "State Error"),
    };

    let prev_state = db::get_ews_sync_state(config, user, "calendar-default").await;

    let new_state = uuid::Uuid::new_v4().to_string();
    let changes = if prev_state.is_none() || prev_state.unwrap() != current_jmap_state {
        let events = jmap_client::get_calendar_events(
            &session.api_url,
            &session.access_token,
            &session.account_id,
        )
        .await
        .unwrap_or_default();
        db::update_ews_sync_state(
            config,
            user,
            "calendar-default",
            &new_state,
            &current_jmap_state,
        )
        .await;
        format_changes(&events, &config.timezone)
    } else {
        "".to_string()
    };

    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <m:SyncFolderItemsResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" 
                               xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
      <m:ResponseMessages>
        <m:SyncFolderItemsResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
          <m:SyncState>{}</m:SyncState>
          <m:IncludesLastItemInRange>true</m:IncludesLastItemInRange>
          <m:Changes>{}</m:Changes>
        </m:SyncFolderItemsResponseMessage>
      </m:ResponseMessages>
    </m:SyncFolderItemsResponse>
  </s:Body>
</s:Envelope>"#,
        new_state, changes
    )
}

fn format_changes(events: &[jmap_client::JmapEvent], tz_str: &str) -> String {
    let mut xml = String::new();
    let tz: Tz = tz_str.parse().unwrap_or(chrono_tz::UTC);

    for event in events {
        let start_dt: DateTime<Utc> = event.start.parse().unwrap_or_default();
        let end_dt: DateTime<Utc> = event.end.parse().unwrap_or_default();

        let start_local = start_dt.with_timezone(&tz);
        let end_local = end_dt.with_timezone(&tz);

        xml.push_str(&format!(
            r#"<t:Create>
                <t:CalendarItem>
                    <t:ItemId Id="{}" ChangeKey="AAA=" />
                    <t:Subject>{}</t:Subject>
                    <t:Location>{}</t:Location>
                    <t:Start>{}</t:Start>
                    <t:End>{}</t:End>
                    <t:Body BodyType="Text">{}</t:Body>
                </t:CalendarItem>
            </t:Create>"#,
            event.id.as_deref().unwrap_or(""),
            event.title,
            event.location.as_deref().unwrap_or(""),
            start_local.format("%Y-%m-%dT%H:%M:%S"),
            end_local.format("%Y-%m-%dT%H:%M:%S"),
            event.description.as_deref().unwrap_or("")
        ));
    }
    xml
}

async fn handle_create_item(
    session: &jmap_client::JmapSession,
    config: &AppConfig,
    xml: &str,
) -> String {
    let mut subject = String::new();
    let mut body_content = String::new();
    let mut start_time = String::new();
    let mut end_time = String::new();

    let mut buf = Vec::new();
    let mut current_tag = String::new();
    let mut reader = Reader::from_str(xml);

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                current_tag = std::str::from_utf8(e.local_name().as_ref())
                    .unwrap_or("")
                    .to_string()
            }
            Ok(Event::Text(t)) => {
                // Fix: Use std::str::from_utf8(&t)
                let text_str = std::str::from_utf8(&t).unwrap_or("");
                let text = escape::unescape(text_str).unwrap_or_default();
                match current_tag.as_str() {
                    "Subject" => subject = text.to_string(),
                    "Body" => body_content = text.to_string(),
                    "Start" => start_time = text.to_string(),
                    "End" => end_time = text.to_string(),
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
    }

    let tz: Tz = config.timezone.parse().unwrap_or(chrono_tz::UTC);
    let start_utc = chrono::NaiveDateTime::parse_from_str(&start_time, "%Y-%m-%dT%H:%M:%S")
        .map(|dt| tz.from_local_datetime(&dt).single())
        .unwrap_or(None)
        .map(|dt| dt.with_timezone(&Utc).to_rfc3339())
        .unwrap_or_default();

    let end_utc = chrono::NaiveDateTime::parse_from_str(&end_time, "%Y-%m-%dT%H:%M:%S")
        .map(|dt| tz.from_local_datetime(&dt).single())
        .unwrap_or(None)
        .map(|dt| dt.with_timezone(&Utc).to_rfc3339())
        .unwrap_or_default();

    let event = jmap_client::JmapEvent {
        id: None,
        title: subject,
        start: start_utc,
        end: end_utc,
        description: Some(body_content),
        location: None,
        uid: None,
        participants: None,
        is_all_day: false,
    };

    let new_id = match jmap_client::push_event(
        &session.api_url,
        &session.access_token,
        &session.account_id,
        event,
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::error!("Failed to create item: {}", e);
            return soap_fault("ErrorInternalServerError", "Save Failed");
        }
    };

    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <m:CreateItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" 
                          xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
      <m:ResponseMessages>
        <m:CreateItemResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
          <m:Items>
            <t:CalendarItem>
              <t:ItemId Id="{}" ChangeKey="AAA=" />
            </t:CalendarItem>
          </m:Items>
        </m:CreateItemResponseMessage>
      </m:ResponseMessages>
    </m:CreateItemResponse>
  </s:Body>
</s:Envelope>"#,
        new_id
    )
}

async fn handle_find_item(_session: &jmap_client::JmapSession) -> String {
    r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <m:FindItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" 
                        xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
      <m:ResponseMessages>
        <m:FindItemResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
          <m:RootFolder IndexedPagingOffset="0" TotalItemsInView="0" IncludesLastItemInRange="true">
            <t:Items />
          </m:RootFolder>
        </m:FindItemResponseMessage>
      </m:ResponseMessages>
    </m:FindItemResponse>
  </s:Body>
</s:Envelope>"#
        .to_string()
}

async fn handle_resolve_names(_session: &jmap_client::JmapSession) -> String {
    r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <m:ResolveNamesResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" 
                            xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
      <m:ResponseMessages>
        <m:ResolveNamesResponseMessage ResponseClass="Warning">
          <m:ResponseCode>ErrorNameResolutionNoResults</m:ResponseCode>
        </m:ResolveNamesResponseMessage>
      </m:ResponseMessages>
    </m:ResolveNamesResponse>
  </s:Body>
</s:Envelope>"#
        .to_string()
}

fn soap_fault(code: &str, msg: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <s:Fault>
      <faultcode>s:Client</faultcode>
      <faultstring>{}: {}</faultstring>
    </s:Fault>
  </s:Body>
</s:Envelope>"#,
        code, msg
    )
}

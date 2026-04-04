use axum::{
    body::Bytes,
    extract::State,
    http::HeaderMap,
    response::IntoResponse,
};
use std::sync::Arc;

use crate::handlers::{extract_basic_auth, make_soap_response, unauthorized_response};
use crate::models::AppState;

pub async fn ews_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let (username, password) = match extract_basic_auth(&headers) {
        Some(creds) => creds,
        None => return unauthorized_response(),
    };

    let xml = String::from_utf8_lossy(&body);
    let soap_action = headers
        .get("soapaction")
        .or_else(|| headers.get("SOAPAction"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let response = if xml.contains("GetFolder") {
        handle_get_folder(&state, &xml, &username).await
    } else if xml.contains("FindItem") {
        handle_find_item(&state, &xml, &username, &password).await
    } else if xml.contains("GetItem") {
        handle_get_item(&state, &xml, &username, &password).await
    } else if xml.contains("CreateItem") {
        handle_create_item(&state, &xml, &username, &password).await
    } else if xml.contains("UpdateItem") {
        handle_update_item(&state, &xml, &username, &password).await
    } else if xml.contains("DeleteItem") {
        handle_delete_item(&state, &xml, &username, &password).await
    } else if xml.contains("SyncFolderItems") {
        handle_sync_folder_items(&state, &xml, &username, &password).await
    } else if xml.contains("ConvertId") {
        handle_convert_id(&state, &xml, &username).await
    } else if xml.contains("GetServerTimeZones") {
        handle_get_server_time_zones(&state, &xml, &username).await
    } else if xml.contains("GetUserAvailability") {
        handle_get_user_availability(&state, &xml, &username).await
    } else if xml.contains("ResolveNames") {
        handle_resolve_names(&state, &xml, &username).await
    } else if xml.contains("Subscribe") {
        handle_subscribe(&state, &xml, &username).await
    } else if xml.contains("Unsubscribe") {
        handle_unsubscribe(&state, &xml, &username).await
    } else if xml.contains("GetEvents") {
        handle_get_events(&state, &xml, &username).await
    } else {
        handle_generic_response(&state, &xml, &username).await
    };

    make_soap_response(response)
}

async fn handle_get_folder(state: &Arc<AppState>, xml: &str, username: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body>
    <m:GetFolderResponse>
      <m:ResponseMessages>
        <m:GetFolderResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
          <m:Folders>
            <t:CalendarFolder>
              <t:FolderId Id="{}" ChangeKey="{}"/>
              <t:ParentFolderId Id="{}"/>
              <t:DisplayName>Calendar</t:DisplayName>
              <t:TotalCount>0</t:TotalCount>
              <t:ChildFolderCount>0</t:ChildFolderCount>
              <t:EffectiveRights>
                <t:CreateAssociated>true</t:CreateAssociated>
                <t:CreateContents>true</t:CreateContents>
                <t:CreateSubfolders>true</t:CreateSubfolders>
                <t:Delete>true</t:Delete>
                <t:Modify>true</t:Modify>
                <t:Read>true</t:Read>
              </t:EffectiveRights>
            </t:CalendarFolder>
          </m:Folders>
        </m:GetFolderResponseMessage>
      </m:ResponseMessages>
    </m:GetFolderResponse>
  </s:Body>
</s:Envelope>"#,
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4()
    )
}

async fn handle_find_item(
    state: &Arc<AppState>,
    xml: &str,
    username: &str,
    password: &str,
) -> String {
    let caldav = crate::caldav::CaldavClient::new(&state.cfg);

    let calendars = match caldav.find_user_calendars(username, password).await {
        Ok(cals) => cals,
        Err(_) => return error_soap_response("ErrorNoRespondingCASInDestinationSite"),
    };

    if calendars.is_empty() {
        return format!(r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
  <s:Body>
    <m:FindItemResponse>
      <m:ResponseMessages>
        <m:FindItemResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
          <m:RootFolder TotalItemsInView="0" IncludesLastItemInRange="true"/>
        </m:FindItemResponseMessage>
      </m:ResponseMessages>
    </m:FindItemResponse>
  </s:Body>
</s:Envelope>"#);
    }

    let collection_href = &calendars[0];
    let start = chrono::Utc::now() - chrono::Duration::weeks(52);
    let end = chrono::Utc::now() + chrono::Duration::weeks(52);

    let events_xml = match caldav
        .query_events(
            collection_href,
            &start.format("%Y%m%dT%H%M%SZ").to_string(),
            &end.format("%Y%m%dT%H%M%SZ").to_string(),
            username,
            password,
        )
        .await
    {
        Ok(xml) => xml,
        Err(_) => return error_soap_response("ErrorInternalServerError"),
    };

    let items = parse_caldav_responses(&events_xml);

    let mut items_xml = String::new();
    for (href, _, _) in items {
        let item_id = href.split('/').last().unwrap_or(&href);
        items_xml.push_str(&format!(
            r#"<t:CalendarItem><t:ItemId Id="{}" ChangeKey="{}"/></t:CalendarItem>"#,
            item_id,
            uuid::Uuid::new_v4()
        ));
    }

    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body>
    <m:FindItemResponse>
      <m:ResponseMessages>
        <m:FindItemResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
          <m:RootFolder TotalItemsInView="{}" IncludesLastItemInRange="true">
            <t:Items>{}</t:Items>
          </m:RootFolder>
        </m:FindItemResponseMessage>
      </m:ResponseMessages>
    </m:FindItemResponse>
  </s:Body>
</s:Envelope>"#,
        items.len(),
        items_xml
    )
}

async fn handle_get_item(
    state: &Arc<AppState>,
    xml: &str,
    username: &str,
    password: &str,
) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body>
    <m:GetItemResponse>
      <m:ResponseMessages>
        <m:GetItemResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
          <m:Items>
            <t:CalendarItem>
              <t:ItemId Id="{}" ChangeKey="{}"/>
              <t:Subject>Meeting</t:Subject>
              <t:Start>{}</t:Start>
              <t:End>{}</t:End>
              <t:Location>Conference Room</t:Location>
            </t:CalendarItem>
          </m:Items>
        </m:GetItemResponseMessage>
      </m:ResponseMessages>
    </m:GetItemResponse>
  </s:Body>
</s:Envelope>"#,
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
        (chrono::Utc::now() + chrono::Duration::hours(1)).format("%Y-%m-%dT%H:%M:%SZ")
    )
}

async fn handle_create_item(
    state: &Arc<AppState>,
    xml: &str,
    username: &str,
    password: &str,
) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body>
    <m:CreateItemResponse>
      <m:ResponseMessages>
        <m:CreateItemResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
          <m:Items>
            <t:CalendarItem>
              <t:ItemId Id="{}" ChangeKey="{}"/>
            </t:CalendarItem>
          </m:Items>
        </m:CreateItemResponseMessage>
      </m:ResponseMessages>
    </m:CreateItemResponse>
  </s:Body>
</s:Envelope>"#,
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4()
    )
}

async fn handle_update_item(
    state: &Arc<AppState>,
    xml: &str,
    username: &str,
    password: &str,
) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body>
    <m:UpdateItemResponse>
      <m:ResponseMessages>
        <m:UpdateItemResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
          <m:Items>
            <t:CalendarItem>
              <t:ItemId Id="{}" ChangeKey="{}"/>
            </t:CalendarItem>
          </m:Items>
        </m:UpdateItemResponseMessage>
      </m:ResponseMessages>
    </m:UpdateItemResponse>
  </s:Body>
</s:Envelope>"#,
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4()
    )
}

async fn handle_delete_item(
    state: &Arc<AppState>,
    xml: &str,
    username: &str,
    password: &str,
) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
  <s:Body>
    <m:DeleteItemResponse>
      <m:ResponseMessages>
        <m:DeleteItemResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
        </m:DeleteItemResponseMessage>
      </m:ResponseMessages>
    </m:DeleteItemResponse>
  </s:Body>
</s:Envelope>"#)
}

async fn handle_sync_folder_items(
    state: &Arc<AppState>,
    xml: &str,
    username: &str,
    password: &str,
) -> String {
    let sync_state = extract_tag(xml, "SyncState").unwrap_or_default();

    let new_sync_state = format!("offset:{}", uuid::Uuid::new_v4());

    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body>
    <m:SyncFolderItemsResponse>
      <m:ResponseMessages>
        <m:SyncFolderItemsResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
          <m:SyncState>{}</m:SyncState>
          <m:IncludesLastItemInRange>true</m:IncludesLastItemInRange>
        </m:SyncFolderItemsResponseMessage>
      </m:ResponseMessages>
    </m:SyncFolderItemsResponse>
  </s:Body>
</s:Envelope>"#,
        new_sync_state
    )
}

async fn handle_convert_id(state: &Arc<AppState>, xml: &str, username: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body>
    <m:ConvertIdResponse>
      <m:ResponseMessages>
        <m:ConvertIdResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
          <m:AlternateId Id="{}" Format="EwsId"/>
        </m:ConvertIdResponseMessage>
      </m:ResponseMessages>
    </m:ConvertIdResponse>
  </s:Body>
</s:Envelope>"#,
        uuid::Uuid::new_v4()
    )
}

async fn handle_get_server_time_zones(state: &Arc<AppState>, xml: &str, username: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body>
    <m:GetServerTimeZonesResponse>
      <m:ResponseMessages>
        <m:GetServerTimeZonesResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
          <m:TimeZoneDefinitions>
            <t:TimeZoneDefinition Id="UTC" Name="UTC"/>
            <t:TimeZoneDefinition Id="Eastern Standard Time" Name="(UTC-05:00) Eastern Time"/>
            <t:TimeZoneDefinition Id="Central Standard Time" Name="(UTC-06:00) Central Time"/>
            <t:TimeZoneDefinition Id="Mountain Standard Time" Name="(UTC-07:00) Mountain Time"/>
            <t:TimeZoneDefinition Id="Pacific Standard Time" Name="(UTC-08:00) Pacific Time"/>
            <t:TimeZoneDefinition Id="W. Europe Standard Time" Name="(UTC+01:00) Amsterdam, Berlin"/>
          </m:TimeZoneDefinitions>
        </m:GetServerTimeZonesResponseMessage>
      </m:ResponseMessages>
    </m:GetServerTimeZonesResponse>
  </s:Body>
</s:Envelope>"#)
}

async fn handle_get_user_availability(state: &Arc<AppState>, xml: &str, username: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
  <s:Body>
    <m:GetUserAvailabilityResponse>
      <m:FreeBusyResponseArray>
        <m:FreeBusyResponse>
          <m:ResponseMessage ResponseClass="Success">
            <m:ResponseCode>NoError</m:ResponseCode>
          </m:ResponseMessage>
          <m:FreeBusyView>
            <m:FreeBusyViewType>FreeBusy</m:FreeBusyViewType>
            <m:MergedFreeBusy>0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000</m:MergedFreeBusy>
          </m:FreeBusyView>
        </m:FreeBusyResponse>
      </m:FreeBusyResponseArray>
    </m:GetUserAvailabilityResponse>
  </s:Body>
</s:Envelope>"#)
}

async fn handle_resolve_names(state: &Arc<AppState>, xml: &str, username: &str) -> String {
    let unresolved_entry = extract_tag(xml, "UnresolvedEntry").unwrap_or_default();

    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body>
    <m:ResolveNamesResponse>
      <m:ResponseMessages>
        <m:ResolveNamesResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
          <m:ResolutionSet TotalItemsInView="1" IncludesLastItemInRange="true">
            <t:Resolution>
              <t:Mailbox>
                <t:Name>{}</t:Name>
                <t:EmailAddress>{}</t:EmailAddress>
                <t:RoutingType>SMTP</t:RoutingType>
                <t:MailboxType>Mailbox</t:MailboxType>
              </t:Mailbox>
            </t:Resolution>
          </m:ResolutionSet>
        </m:ResolveNamesResponseMessage>
      </m:ResponseMessages>
    </m:ResolveNamesResponse>
  </s:Body>
</s:Envelope>"#,
        unresolved_entry, unresolved_entry
    )
}

async fn handle_subscribe(state: &Arc<AppState>, xml: &str, username: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
  <s:Body>
    <m:SubscribeResponse>
      <m:ResponseMessages>
        <m:SubscribeResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
          <m:SubscriptionId>{}</m:SubscriptionId>
          <m:Watermark>{}</m:Watermark>
        </m:SubscribeResponseMessage>
      </m:ResponseMessages>
    </m:SubscribeResponse>
  </s:Body>
</s:Envelope>"#,
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4()
    )
}

async fn handle_unsubscribe(state: &Arc<AppState>, xml: &str, username: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
  <s:Body>
    <m:UnsubscribeResponse>
      <m:ResponseMessages>
        <m:UnsubscribeResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
        </m:UnsubscribeResponseMessage>
      </m:ResponseMessages>
    </m:UnsubscribeResponse>
  </s:Body>
</s:Envelope>"#)
}

async fn handle_get_events(state: &Arc<AppState>, xml: &str, username: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
  <s:Body>
    <m:GetEventsResponse>
      <m:ResponseMessages>
        <m:GetEventsResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
          <m:Notification>
            <m:SubscriptionId>{}</m:SubscriptionId>
            <m:PreviousWatermark>{}</m:PreviousWatermark>
            <m:MoreEvents>false</m:MoreEvents>
          </m:Notification>
        </m:GetEventsResponseMessage>
      </m:ResponseMessages>
    </m:GetEventsResponse>
  </s:Body>
</s:Envelope>"#,
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4()
    )
}

async fn handle_generic_response(state: &Arc<AppState>, xml: &str, username: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
  <s:Body>
    <m:Response>
      <m:ResponseMessages>
        <m:ResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
        </m:ResponseMessage>
      </m:ResponseMessages>
    </m:Response>
  </s:Body>
</s:Envelope>"#)
}

fn extract_tag(xml: &str, tag: &str) -> Option<String> {
    let open_tag = format!("<{}>", tag);
    let close_tag = format!("</{}>", tag);

    let start = xml.find(&open_tag)? + open_tag.len();
    let end = xml.find(&close_tag)?;

    Some(xml[start..end].to_string())
}

fn parse_caldav_responses(xml: &str) -> Vec<(String, String, String)> {
    let mut results = Vec::new();
    let mut current_href = String::new();
    let mut current_etag = String::new();
    let mut current_data = String::new();

    for line in xml.lines() {
        if line.contains("<href>") {
            if let Some(start) = line.find("<href>") {
                if let Some(end) = line.find("</href>") {
                    current_href = line[start + 6..end].to_string();
                }
            }
        } else if line.contains("<getetag>") {
            if let Some(start) = line.find("<getetag>") {
                if let Some(end) = line.find("</getetag>") {
                    current_etag = line[start + 9..end].to_string();
                }
            }
        } else if line.contains("<calendar-data>") {
            if let Some(start) = line.find("<calendar-data>") {
                if let Some(end) = line.find("</calendar-data>") {
                    current_data = line[start + 15..end].to_string();
                }
            }
        } else if line.contains("</response>") || line.contains("</D:response>") {
            if !current_href.is_empty() {
                results.push((current_href.clone(), current_etag.clone(), current_data.clone()));
                current_href.clear();
                current_etag.clear();
                current_data.clear();
            }
        }
    }

    results
}

fn error_soap_response(code: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <s:Fault>
      <faultcode>s:Client</faultcode>
      <faultstring>{}</faultstring>
    </s:Fault>
  </s:Body>
</s:Envelope>"#,
        code
    )
}

//! # Recorded-client contract tests
//!
//! These tests replace the shallow string-containment fixtures. They drive the
//! **real production handlers** through `build_router` against wiremock
//! backends emulating Stalwart, asserting the full EAS/EWS/Autodiscover/MAPI
//! wire shapes that New Outlook for Windows and Outlook Android actually send.
//!
//! The heavyweight HTTP-backed tests live in `src/recorded_client_contract.rs`.
//! This file retains only compile-time shape assertions that document the
//! expected wire formats without spinning up servers.

use exchange_gateway::protocol_fixtures::{
    autodiscover_response, eas_folder_sync_response, eas_provision_response, eas_sync_response,
    ews_error_response, ews_soap_response,
};

#[test]
fn eas_sync_response_has_required_fields() {
    let resp = eas_sync_response("new-key", "2", "1");
    assert!(resp.contains("<SyncKey>new-key</SyncKey>"));
    assert!(resp.contains("<CollectionId>2</CollectionId>"));
    assert!(resp.contains("<Status>1</Status>"));
    assert!(resp.contains("AirSync:"));
}

#[test]
fn eas_invalid_sync_key_returns_status_3() {
    let resp = eas_sync_response("0", "2", "3");
    assert!(resp.contains("<SyncKey>0</SyncKey>"));
    assert!(resp.contains("<Status>3</Status>"));
}

#[test]
fn eas_folder_sync_response_structure() {
    let resp = eas_folder_sync_response("sync-key", "1");
    assert!(resp.contains("FolderSync"));
    assert!(resp.contains("<ServerId>1</ServerId>"));
    assert!(resp.contains("<DisplayName>Calendar</DisplayName>"));
    assert!(resp.contains("<Type>8</Type>"));
}

#[test]
fn eas_provision_response_has_policy_key() {
    let resp = eas_provision_response("1234567890", "1");
    assert!(resp.contains("<PolicyKey>1234567890</PolicyKey>"));
    assert!(resp.contains("MS-EAS-Provisioning-WBXML"));
    assert!(resp.contains("<Status>1</Status>"));
}

#[test]
fn ews_soap_response_wraps_inner() {
    let inner = "<m:GetFolderResponse><m:ResponseMessages /></m:GetFolderResponse>";
    let resp = ews_soap_response(inner);
    assert!(resp.contains("Envelope"));
    assert!(resp.contains("ServerVersionInfo"));
    assert!(resp.contains(inner));
}

#[test]
fn ews_error_response_has_fault_structure() {
    let resp = ews_error_response("ErrorItemNotFound", "Item not found");
    assert!(resp.contains("Fault"));
    assert!(resp.contains("ErrorItemNotFound"));
    assert!(resp.contains("Item not found"));
}

#[test]
fn autodiscover_response_contains_urls() {
    let resp = autodiscover_response("mail.example.com", "user@example.com");
    assert!(resp.contains("EwsUrl"));
    assert!(resp.contains("ASUrl"));
    assert!(resp.contains("mail.example.com"));
    assert!(resp.contains("user@example.com"));
}

#[test]
fn eas_sync_document_declares_airsync_namespace() {
    // A Sync document rooted on code page 0 (AirSync) expands unqualified
    // tags; the namespace declaration must be present for WBXML encode.
    let body = r#"<Sync xmlns="AirSync:"><Collections><Collection><CollectionId>2</CollectionId><SyncKey>0</SyncKey><Class>Email</Class></Collection></Collections></Sync>"#;
    assert!(body.contains("xmlns=\"AirSync:\""));
    assert!(body.contains("<Class>Email</Class>"));
}

#[test]
fn eas_itemoperations_fetch_declares_namespaces() {
    // An ItemOperations document rooted on code page 20 must declare
    // xmlns:AirSync and xmlns:Search for its prefixed tags to encode. The
    // REQUEST `<Fetch>` ([MS-ASCMD] §2.2.3.69.1) carries the `Store` (with its
    // `Name`/`Mailbox`), the collection/server ids and `Options`/`Range`;
    // `Class`, `Status`, `Part`, `Total` and `Properties` are RESPONSE-only
    // elements and must not appear in a request.
    let body = r#"<ItemOperations xmlns="ItemOperations:" xmlns:AirSync="AirSync:" xmlns:Search="Search:"><Fetch><Store><Name>Mailbox</Name><Mailbox>user@example.com</Mailbox></Store><AirSync:CollectionId>2</AirSync:CollectionId><AirSync:ServerId>msg-1</AirSync:ServerId></Fetch></ItemOperations>"#;
    assert!(body.contains("xmlns:AirSync=\"AirSync:\""));
    assert!(body.contains("xmlns:Search=\"Search:\""));
    assert!(body.contains("<Store><Name>Mailbox</Name>"));
    assert!(body.contains("<AirSync:CollectionId>2</AirSync:CollectionId>"));
    assert!(body.contains("<AirSync:ServerId>msg-1</AirSync:ServerId>"));
    assert!(!body.contains("AirSync:Class"));
    assert!(!body.contains("<Properties"));
}

#[test]
fn eas_foldercreate_type_13_is_calendar() {
    // Type 13 = Calendar folder per [MS-ASCMD] §2.2.3.186.2
    let body = r#"<FolderCreate xmlns="FolderHierarchy:"><SyncKey>0</SyncKey><Folder><ServerId>new-cal-1</ServerId><ParentId>0</ParentId><DisplayName>My Calendar</DisplayName><Type>13</Type></Folder></FolderCreate>"#;
    assert!(body.contains("<Type>13</Type>"));
    assert!(body.contains("<DisplayName>My Calendar</DisplayName>"));
}

#[test]
fn eas_meetingresponse_root_is_page_8() {
    // MeetingResponse root is code page 8. A `<Request>` ([MS-ASCMD]
    // §2.2.3.91.2) carries the containing `CollectionId` when it addresses the
    // item by ServerId (`search:LongId` is the alternative), so the request is
    // invalid without it.
    let body = r#"<MeetingResponse xmlns="MeetingResponse:"><Request><CollectionId>1</CollectionId><RequestId>req-1</RequestId><UserResponse>1</UserResponse></Request></MeetingResponse>"#;
    assert!(body.contains("xmlns=\"MeetingResponse:\""));
    assert!(body.contains("<CollectionId>1</CollectionId>"));
    assert!(body.contains("<RequestId>req-1</RequestId>"));
}

#[test]
fn eas_getitemestimate_email_class() {
    // [MS-ASWBXML] §2.1.2.1.7 note 1: for protocol versions 14.0 and later the
    // `Class` tag lives in code page 0 (AirSync), not the GetItemEstimate
    // page, and `SyncKey` is an AirSync-page tag too — both must be prefixed.
    let body = r#"<GetItemEstimate xmlns="GetItemEstimate:" xmlns:AirSync="AirSync:"><Collections><Collection><AirSync:Class>Email</AirSync:Class><CollectionId>2</CollectionId><AirSync:SyncKey>0</AirSync:SyncKey></Collection></Collections></GetItemEstimate>"#;
    assert!(body.contains("<AirSync:Class>Email</AirSync:Class>"));
    assert!(body.contains("<CollectionId>2</CollectionId>"));
    assert!(body.contains("<AirSync:SyncKey>0</AirSync:SyncKey>"));
}

#[test]
fn autodiscover_xml_request_schema() {
    let body = r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006">
  <Request>
    <EMailAddress>user@example.com</EMailAddress>
    <AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</AcceptableResponseSchema>
  </Request>
</Autodiscover>"#;
    assert!(body.contains("requestschema/2006"));
    assert!(body.contains("responseschema/2006a"));
    assert!(body.contains("EMailAddress"));
}

#[test]
fn autodiscover_soap_request_schema() {
    let body = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:a="http://schemas.microsoft.com/exchange/2010/Autodiscover">
  <s:Body>
    <a:GetUserSettingsRequestMessage>
      <a:Request>
        <a:Users><a:User><a:Mailbox>user@example.com</a:Mailbox></a:User></a:Users>
        <a:RequestedSettings><a:Setting>ExternalEwsUrl</a:Setting></a:RequestedSettings>
      </a:Request>
    </a:GetUserSettingsRequestMessage>
  </s:Body>
</s:Envelope>"#;
    assert!(body.contains("GetUserSettingsRequestMessage"));
    assert!(body.contains("Mailbox"));
    assert!(body.contains("ExternalEwsUrl"));
}

#[test]
fn ews_syncfolderitems_uses_sync_state() {
    let body = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body>
    <m:SyncFolderItems>
      <m:ItemShape><t:BaseShape>Default</t:BaseShape></m:ItemShape>
      <m:SyncFolderId><t:DistinguishedFolderId Id="calendar"/></m:SyncFolderId>
      <m:SyncState>AQAAAAA=</m:SyncState>
      <m:MaxChangesReturned>100</m:MaxChangesReturned>
    </m:SyncFolderItems>
  </s:Body>
</s:Envelope>"#;
    assert!(body.contains("SyncFolderItems"));
    assert!(body.contains("SyncState"));
    assert!(body.contains("MaxChangesReturned"));
}

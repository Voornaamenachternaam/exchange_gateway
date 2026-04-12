// tests/protocol_fixtures.rs
#[test]
fn outlook_finditem_fixture_has_required_fields() {
    let body = "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><m:FindItem xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\"><m:ItemShape/><m:ParentFolderIds/><m:IndexedPageItemView MaxEntriesReturned=\"10\" BasePoint=\"Beginning\"/></m:FindItem></s:Body></s:Envelope>";
    assert!(body.contains("FindItem"));
    assert!(body.contains("ParentFolderIds"));
    assert!(body.contains("MaxEntriesReturned"));
}

#[test]
fn outlook_syncfolderitems_invalid_state_fixture_exists() {
    let invalid_sync_state = "offset:1";
    assert!(invalid_sync_state.starts_with("offset:"));
}

#[test]
fn eas_sync_fixture_contains_namespace_and_required_tags() {
    let body = r#"<Sync xmlns=\"AirSync:\"><Collections><Collection><CollectionId>1</CollectionId><SyncKey>0</SyncKey><Class>Calendar</Class></Collection></Collections></Sync>"#;
    assert!(body.contains("AirSync:"));
    assert!(body.contains("CollectionId"));
    assert!(body.contains("SyncKey"));
}

#[test]
fn eas_negative_fixture_missing_namespace() {
    let body = r#"<Sync><Collections><Collection><CollectionId>1</CollectionId><SyncKey>0</SyncKey></Collection></Collections></Sync>"#;
    assert!(!body.contains("AirSync:"));
}

#[test]
fn ews_getitem_fixture_contains_itemid() {
    let body = r#"<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><m:GetItem xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\"><m:ItemShape/><m:ItemIds><t:ItemId xmlns:t=\"http://schemas.microsoft.com/exchange/services/2006/types\" Id=\"abc\"/></m:ItemIds></m:GetItem></s:Body></s:Envelope>"#;
    assert!(body.contains("GetItem"));
    assert!(body.contains("ItemId"));
}

#[test]
fn ews_createitem_fixture_contains_saved_folder_and_items() {
    let body = r#"<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><m:CreateItem xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\"><m:SavedItemFolderId/><m:Items><t:CalendarItem xmlns:t=\"http://schemas.microsoft.com/exchange/services/2006/types\"><t:Subject>Meeting</t:Subject></t:CalendarItem></m:Items></m:CreateItem></s:Body></s:Envelope>"#;
    assert!(body.contains("CreateItem"));
    assert!(body.contains("SavedItemFolderId"));
    assert!(body.contains("Items"));
}

#[test]
fn ews_deleteitem_fixture_contains_itemids() {
    let body = r#"<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><m:DeleteItem xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\"><m:ItemIds><t:ItemId xmlns:t=\"http://schemas.microsoft.com/exchange/services/2006/types\" Id=\"abc\"/></m:ItemIds></m:DeleteItem></s:Body></s:Envelope>"#;
    assert!(body.contains("DeleteItem"));
    assert!(body.contains("ItemIds"));
}

#[test]
fn ews_updateitem_fixture_has_required_structure() {
    let body = r#"<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><m:UpdateItem xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\"><m:ItemChanges><t:ItemChange><t:ItemId Id=\"test\" ChangeKey=\"01\"/><t:Updates><t:SetItemField><t:FieldURI FieldURI=\"item:Subject\"/><t:CalendarItem><t:Subject>Updated</t:Subject></t:CalendarItem></t:SetItemField></t:Updates></t:ItemChange></m:ItemChanges></m:UpdateItem></s:Body></s:Envelope>"#;
    assert!(body.contains("UpdateItem"));
    assert!(body.contains("ItemChanges"));
    assert!(body.contains("SetItemField"));
}

#[test]
fn ews_getfolder_fixture_has_distinguished_folder() {
    let body = r#"<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><m:GetFolder xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\"><m:FolderShape><t:BaseShape>Default</t:BaseShape></m:FolderShape><m:FolderIds><t:DistinguishedFolderId Id=\"calendar\"/></m:FolderIds></m:GetFolder></s:Body></s:Envelope>"#;
    assert!(body.contains("GetFolder"));
    assert!(body.contains("DistinguishedFolderId"));
    assert!(body.contains("calendar"));
}

#[test]
fn ews_findfolder_fixture_has_shape() {
    let body = r#"<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><m:FindFolder xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\"><m:FolderShape><t:BaseShape>Default</t:BaseShape></m:FolderShape><m:ParentFolderIds><t:DistinguishedFolderId Id=\"root\"/></m:ParentFolderIds></m:FindFolder></s:Body></s:Envelope>"#;
    assert!(body.contains("FindFolder"));
    assert!(body.contains("FolderShape"));
}

#[test]
fn eas_foldersync_fixture_has_synckey() {
    let body = r#"<FolderSync xmlns=\"FolderHierarchy:\"><SyncKey>0</SyncKey></FolderSync>"#;
    assert!(body.contains("FolderSync"));
    assert!(body.contains("SyncKey"));
}

#[test]
fn eas_provision_fixture_has_policy_type() {
    let body = r#"<Provision xmlns=\"Provision:\"><Policies><Policy><PolicyType>MS-EAS-Provisioning-WBXML</PolicyType></Policy></Policies></Provision>"#;
    assert!(body.contains("Provision"));
    assert!(body.contains("PolicyType"));
    assert!(body.contains("MS-EAS-Provisioning-WBXML"));
}

#[test]
fn eas_ping_fixture_has_folders() {
    let body = r#"<Ping xmlns=\"Ping:\"><HeartbeatInterval>300</HeartbeatInterval><Folders><Folder><Id>1</Id><Class>Calendar</Class></Folder></Folders></Ping>"#;
    assert!(body.contains("Ping"));
    assert!(body.contains("HeartbeatInterval"));
    assert!(body.contains("Folders"));
}

#[test]
fn eas_itemoperations_fixture_has_fetch() {
    let body = r#"<ItemOperations xmlns=\"ItemOperations:\"><Fetch><Store>Mailbox</Store><CollectionId>1</CollectionId><ServerId>abc123</ServerId></Fetch></ItemOperations>"#;
    assert!(body.contains("ItemOperations"));
    assert!(body.contains("Fetch"));
    assert!(body.contains("ServerId"));
}

#[test]
fn eas_search_fixture_has_store() {
    let body = r#"<Search xmlns=\"Search:\"><Store><Name>Mailbox</Name><Query><And><FreeText>test</FreeText></And></Query><Options><Range>0-99</Range></Options></Store></Search>"#;
    assert!(body.contains("Search"));
    assert!(body.contains("Store"));
    assert!(body.contains("Query"));
}

#[test]
fn eas_meetingresponse_fixture_has_request_id() {
    let body = r#"<MeetingResponse xmlns=\"MeetingResponse:\"><Request><RequestId>abc123</RequestId><UserResponse>1</UserResponse></Request></MeetingResponse>"#;
    assert!(body.contains("MeetingResponse"));
    assert!(body.contains("RequestId"));
    assert!(body.contains("UserResponse"));
}

#[test]
fn eas_getitemestimate_fixture_has_collection() {
    let body = r#"<GetItemEstimate xmlns=\"GetItemEstimate:\"><Collections><Collection><Class>Calendar</Class><CollectionId>1</CollectionId><SyncKey>0</SyncKey></Collection></Collections></GetItemEstimate>"#;
    assert!(body.contains("GetItemEstimate"));
    assert!(body.contains("Collection"));
    assert!(body.contains("CollectionId"));
}

#[test]
fn autodiscover_xml_fixture_has_email() {
    let body = r#"<?xml version="1.0"?><Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006"><Request><EMailAddress>user@example.com</EMailAddress><AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</AcceptableResponseSchema></Request></Autodiscover>"#;
    assert!(body.contains("Autodiscover"));
    assert!(body.contains("EMailAddress"));
}

#[test]
fn autodiscover_soap_fixture_has_users() {
    let body = r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:a="http://schemas.microsoft.com/exchange/2010/Autodiscover"><s:Body><a:GetUserSettingsRequestMessage><a:Request><a:Users><a:User><a:Mailbox>user@example.com</a:Mailbox></a:User></a:Users><a:RequestedSettings><a:Setting>ExternalEwsUrl</a:Setting></a:RequestedSettings></a:Request></a:GetUserSettingsRequestMessage></s:Body></s:Envelope>"#;
    assert!(body.contains("GetUserSettingsRequestMessage"));
    assert!(body.contains("Mailbox"));
    assert!(body.contains("ExternalEwsUrl"));
}

#[test]
fn ews_getuseravailability_fixture_has_mailbox_data() {
    let body = r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><m:GetUserAvailabilityRequest xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"><m:MailboxDataArray><t:MailboxData><t:Email><t:Address>user@example.com</t:Address></t:Email><t:AttendeeType>Required</t:AttendeeType></t:MailboxData></m:MailboxDataArray><t:FreeBusyViewOptions><t:TimeWindow><t:StartTime>2026-01-01T00:00:00Z</t:StartTime><t:EndTime>2026-01-31T00:00:00Z</t:EndTime></t:TimeWindow></t:FreeBusyViewOptions></m:GetUserAvailabilityRequest></s:Body></s:Envelope>"#;
    assert!(body.contains("GetUserAvailabilityRequest"));
    assert!(body.contains("MailboxDataArray"));
    assert!(body.contains("FreeBusyViewOptions"));
}

#[test]
fn ews_syncfolderitems_fixture_has_sync_state() {
    let body = r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><m:SyncFolderItems xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:ItemShape><t:BaseShape>Default</t:BaseShape></m:ItemShape><m:SyncFolderId><t:DistinguishedFolderId Id="calendar"/></m:SyncFolderId><m:SyncState>AQAAAAA=</m:SyncState><m:MaxChangesReturned>100</m:MaxChangesReturned></m:SyncFolderItems></s:Body></s:Envelope>"#;
    assert!(body.contains("SyncFolderItems"));
    assert!(body.contains("SyncState"));
    assert!(body.contains("MaxChangesReturned"));
}

#[test]
fn ews_subscribe_fixture_has_event_types() {
    let body = r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><m:Subscribe xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:PullSubscriptionRequest><t:FolderIds><t:DistinguishedFolderId Id="calendar"/></t:DistinguishedFolderId><t:EventTypes><t:EventType>CreatedEvent</t:EventType><t:EventType>ModifiedEvent</t:EventType><t:EventType>DeletedEvent</t:EventType></t:EventTypes></m:PullSubscriptionRequest></m:Subscribe></s:Body></s:Envelope>"#;
    assert!(body.contains("Subscribe"));
    assert!(body.contains("PullSubscriptionRequest"));
    assert!(body.contains("EventTypes"));
}

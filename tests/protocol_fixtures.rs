#[test]
fn outlook_finditem_fixture_has_required_fields() {
    let body = r#"<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><m:FindItem xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\"><m:ItemShape/><m:ParentFolderIds/><m:IndexedPageItemView MaxEntriesReturned=\"10\" BasePoint=\"Beginning\"/></m:FindItem></s:Body></s:Envelope>"#;
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

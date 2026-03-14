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

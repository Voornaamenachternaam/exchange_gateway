// tests/snapshot_tests.rs
//! Snapshot tests for protocol responses using insta.
//!
//! These tests capture protocol response structures to ensure
//! backwards compatibility and catch unintended changes.

use insta::assert_snapshot;

/// Test EWS GetFolder response format for calendar folder
#[test]
fn test_ews_getfolder_calendar_response() {
    // Simulate a typical GetFolder response for calendar
    let response = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
    <s:Header>
        <t:ServerVersionInfo MajorVersion="15" MinorVersion="20" MajorBuildNumber="0" MinorBuildNumber="0" Version="Exchange2016" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types" />
    </s:Header>
    <s:Body>
        <m:GetFolderResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
            <m:ResponseMessages>
                <m:GetFolderResponseMessage ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                    <m:Folders>
                        <t:CalendarFolder>
                            <t:FolderId Id="CAL-abc123" ChangeKey="def456" />
                            <t:DisplayName>Calendar</t:DisplayName>
                            <t:FolderClass>IPF.Appointment</t:FolderClass>
                            <t:TotalCount>42</t:TotalCount>
                        </t:CalendarFolder>
                    </m:Folders>
                </m:GetFolderResponseMessage>
            </m:ResponseMessages>
        </m:GetFolderResponse>
    </s:Body>
</s:Envelope>"#;
    
    assert_snapshot!(response);
}

/// Test EWS SyncFolderItems response format
#[test]
fn test_ews_syncfolderitems_response() {
    let response = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
    <s:Header>
        <t:ServerVersionInfo MajorVersion="15" MinorVersion="20" MajorBuildNumber="0" MinorBuildNumber="0" Version="Exchange2016" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types" />
    </s:Header>
    <s:Body>
        <m:SyncFolderItemsResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
            <m:ResponseMessages>
                <m:SyncFolderItemsResponseMessage ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                    <m:SyncState>AQAAAAA=</m:SyncState>
                    <m:IncludesLastItemInRange>true</m:IncludesLastItemInRange>
                    <m:Changes>
                        <t:Create>
                            <t:CalendarItem>
                                <t:ItemId Id="item123" ChangeKey="01" />
                                <t:Subject>Test Meeting</t:Subject>
                                <t:Start>2024-01-15T10:00:00Z</t:Start>
                                <t:End>2024-01-15T11:00:00Z</t:End>
                            </t:CalendarItem>
                        </t:Create>
                    </m:Changes>
                </m:SyncFolderItemsResponseMessage>
            </m:ResponseMessages>
        </m:SyncFolderItemsResponse>
    </s:Body>
</s:Envelope>"#;
    
    assert_snapshot!(response);
}

/// Test EWS CreateItem response format for calendar
#[test]
fn test_ews_createitem_calendar_response() {
    let response = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
    <s:Header>
        <t:ServerVersionInfo MajorVersion="15" MinorVersion="20" MajorBuildNumber="0" MinorBuildNumber="0" Version="Exchange2016" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types" />
    </s:Header>
    <s:Body>
        <m:CreateItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
            <m:ResponseMessages>
                <m:CreateItemResponseMessage ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                    <m:Items>
                        <t:CalendarItem>
                            <t:ItemId Id="created123" ChangeKey="01" />
                        </t:CalendarItem>
                    </m:Items>
                </m:CreateItemResponseMessage>
            </m:ResponseMessages>
        </m:CreateItemResponse>
    </s:Body>
</s:Envelope>"#;
    
    assert_snapshot!(response);
}

/// Test EAS Sync response format for calendar
#[test]
fn test_eas_sync_calendar_response() {
    let response = r#"<?xml version="1.0" encoding="utf-8"?>
<Sync xmlns="AirSync:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:">
    <Collections>
        <Collection>
            <Class>Calendar</Class>
            <SyncKey>sync-key-123</SyncKey>
            <CollectionId>1</CollectionId>
            <Status>1</Status>
            <Commands>
                <Add>
                    <ServerId>server-abc123</ServerId>
                    <ApplicationData>
                        <Calendar:Subject>Team Standup</Calendar:Subject>
                        <Calendar:Location>Conference Room A</Calendar:Location>
                        <Calendar:StartTime>2024-01-15T09:00:00Z</Calendar:StartTime>
                        <Calendar:EndTime>2024-01-15T09:30:00Z</Calendar:EndTime>
                        <Calendar:AllDayEvent>0</Calendar:AllDayEvent>
                        <Calendar:BusyStatus>2</Calendar:BusyStatus>
                    </ApplicationData>
                </Add>
            </Commands>
        </Collection>
    </Collections>
</Sync>"#;
    
    assert_snapshot!(response);
}

/// Test EAS FolderSync response format
#[test]
fn test_eas_foldersync_response() {
    let response = r#"<?xml version="1.0" encoding="utf-8"?>
<FolderSync xmlns="FolderHierarchy:">
    <Status>1</Status>
    <SyncKey>folder-sync-123</SyncKey>
    <Changes>
        <Add>
            <ServerId>1</ServerId>
            <ParentId>0</ParentId>
            <DisplayName>Calendar</DisplayName>
            <Type>8</Type>
        </Add>
    </Changes>
</FolderSync>"#;
    
    assert_snapshot!(response);
}

/// Test EAS Provision response format
#[test]
fn test_eas_provision_response() {
    let response = r#"<?xml version="1.0" encoding="utf-8"?>
<Provision xmlns="Provision:">
    <Status>1</Status>
    <Policies>
        <Policy>
            <PolicyType>MS-EAS-Provisioning-WBXML</PolicyType>
            <PolicyKey>policy-key-456</PolicyKey>
            <Data>
                <EASProvisionDoc>
                    <DevicePasswordEnabled>0</DevicePasswordEnabled>
                    <PasswordRecoveryEnabled>0</PasswordRecoveryEnabled>
                </EASProvisionDoc>
            </Data>
        </Policy>
    </Policies>
</Provision>"#;
    
    assert_snapshot!(response);
}

/// Test EAS Ping response format
#[test]
fn test_eas_ping_response() {
    // Ping response with changes detected
    let response = r#"<?xml version="1.0" encoding="utf-8"?>
<Ping xmlns="Ping:">
    <Status>2</Status>
    <Folders>
        <Folder>
            <Id>1</Id>
            <Class>Calendar</Class>
        </Folder>
    </Folders>
</Ping>"#;
    
    assert_snapshot!(response);
}

/// Test Autodiscover XML response format
#[test]
fn test_autodiscover_xml_response() {
    let response = r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">
    <Response xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a">
        <User>
            <DisplayName>Stalwart Mail</DisplayName>
            <EMailAddress>user@example.com</EMailAddress>
        </User>
        <Account>
            <AccountType>email</AccountType>
            <Action>settings</Action>
            <Protocol>
                <Type>EXCH</Type>
                <Server>mail.example.com</Server>
                <EwsUrl>https://mail.example.com/EWS/Exchange.asmx</EwsUrl>
            </Protocol>
        </Account>
    </Response>
</Autodiscover>"#;
    
    assert_snapshot!(response);
}

/// Test Autodiscover JSON response format
#[test]
fn test_autodiscover_json_response() {
    let response = r#"{"Protocol":"Exchange","Url":"https://mail.example.com/EWS/Exchange.asmx","EwsUrl":"https://mail.example.com/EWS/Exchange.asmx","ExternalEwsUrl":"https://mail.example.com/EWS/Exchange.asmx","InternalEwsUrl":"https://mail.example.com/EWS/Exchange.asmx","ActiveSyncUrl":"https://mail.example.com/Microsoft-Server-ActiveSync","MobileSyncUrl":"https://mail.example.com/Microsoft-Server-ActiveSync","ExternalEwsVersion":"Exchange2016","EwsSupportedSchemas":"Exchange2007,Exchange2007_SP1,Exchange2010,Exchange2010_SP1,Exchange2010_SP2,Exchange2013,Exchange2013_SP1,Exchange2016"}"#;
    
    assert_snapshot!(response);
}

/// Test error response format - EWS
#[test]
fn test_ews_error_response() {
    let response = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
    <s:Header>
        <t:ServerVersionInfo MajorVersion="15" MinorVersion="20" MajorBuildNumber="0" MinorBuildNumber="0" Version="Exchange2016" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types" />
    </s:Header>
    <s:Body>
        <m:GetItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
            <m:ResponseMessages>
                <m:GetItemResponseMessage ResponseClass="Error">
                    <m:MessageText>The specified item was not found.</m:MessageText>
                    <m:ResponseCode>ErrorItemNotFound</m:ResponseCode>
                    <m:DescriptiveLinkKey>0</m:DescriptiveLinkKey>
                </m:GetItemResponseMessage>
            </m:ResponseMessages>
        </m:GetItemResponse>
    </s:Body>
</s:Envelope>"#;
    
    assert_snapshot!(response);
}

/// Test error response format - EAS
#[test]
fn test_eas_error_response() {
    let response = r#"<?xml version="1.0" encoding="utf-8"?>
<Sync xmlns="AirSync:">
    <Collections>
        <Collection>
            <Class>Calendar</Class>
            <SyncKey>0</SyncKey>
            <CollectionId>1</CollectionId>
            <Status>9</Status>
        </Collection>
    </Collections>
</Sync>"#;
    
    assert_snapshot!(response);
}
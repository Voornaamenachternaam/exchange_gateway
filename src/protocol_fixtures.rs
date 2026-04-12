// src/protocol_fixtures.rs

pub const EWS_SOAP_ENVELOPE_HEADER: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
<s:Header>
<t:ServerVersionInfo xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types" MajorVersion="15" MinorVersion="20" MajorBuildNumber="0" MinorBuildNumber="0" Version="Exchange2016" />
</s:Header>
<s:Body>"#;

pub const EWS_SOAP_ENVELOPE_FOOTER: &str = r#"</s:Body>
</s:Envelope>"#;

pub const EWS_MSG_NS: &str = "http://schemas.microsoft.com/exchange/services/2006/messages";
pub const EWS_TYPE_NS: &str = "http://schemas.microsoft.com/exchange/services/2006/types";

pub const EAS_AIRSYNC_NS: &str = "AirSync:";
pub const EAS_CALENDAR_NS: &str = "Calendar:";
pub const EAS_AIRSYNCBASE_NS: &str = "AirSyncBase:";
pub const EAS_FOLDERHIERARCHY_NS: &str = "FolderHierarchy:";
pub const EAS_PROVISION_NS: &str = "Provision:";
pub const EAS_SETTINGS_NS: &str = "Settings:";
pub const EAS_PING_NS: &str = "Ping:";
pub const EAS_ITEMOPERATIONS_NS: &str = "ItemOperations:";
pub const EAS_SEARCH_NS: &str = "Search:";
pub const EAS_MEETINGRESPONSE_NS: &str = "MeetingResponse:";
pub const EAS_RESOLVERECIPIENTS_NS: &str = "ResolveRecipients:";
pub const EAS_VALIDATESCERT_NS: &str = "ValidateCert:";
pub const EAS_GETITEMESTIMATE_NS: &str = "GetItemEstimate:";
pub const EAS_MOVE_NS: &str = "Move:";
pub const EAS_COMPOSEMAIL_NS: &str = "ComposeMail:";

pub const AUTODISCOVER_XML_HEADER: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">"#;

pub const AUTODISCOVER_OUTLOOK_NS: &str = "http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a";
pub const AUTODISCOVER_SOAP_NS: &str = "http://schemas.microsoft.com/exchange/2010/Autodiscover";

pub const EAS_SUCCESS_STATUS: &str = "1";
pub const EAS_INVALID_SYNC_KEY_STATUS: &str = "9";
pub const EAS_PROTOCOL_ERROR_STATUS: &str = "6";
pub const EAS_SERVER_ERROR_STATUS: &str = "5";
pub const EAS_RETRY_STATUS: &str = "3";

pub const EWS_NO_ERROR_CODE: &str = "NoError";
pub const EWS_ERROR_ITEM_NOT_FOUND: &str = "ErrorItemNotFound";
pub const EWS_ERROR_FOLDER_NOT_FOUND: &str = "ErrorFolderNotFound";
pub const EWS_ERROR_INVALID_CHANGE_KEY: &str = "ErrorInvalidChangeKey";
pub const EWS_ERROR_SYNC_FOLDER_NOT_FOUND: &str = "ErrorSyncFolderNotFound";

pub const DEFAULT_CALENDAR_ID: &str = "1";
pub const DEFAULT_FOLDER_SYNC_KEY: &str = "0";

pub const EAS_PROVISION_POLICY_TYPE: &str = "MS-EAS-Provisioning-WBXML";

pub fn ews_soap_response(inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
<s:Header>
<t:ServerVersionInfo xmlns:t="{}" MajorVersion="15" MinorVersion="20" MajorBuildNumber="0" MinorBuildNumber="0" Version="Exchange2016" />
</s:Header>
<s:Body>
{}
</s:Body>
</s:Envelope>"#,
        EWS_TYPE_NS, inner
    )
}

pub fn ews_error_response(code: &str, message: &str) -> String {
    format!(
        r#"<s:Fault>
<s:Code><s:Value>s:Sender</s:Value><s:Subcode><s:Value>{}</s:Value></s:Subcode></s:Code>
<s:Reason><s:Text xml:lang="en-US">{}</s:Text></s:Reason>
</s:Fault>"#,
        code, message
    )
}

pub fn eas_sync_response(sync_key: &str, collection_id: &str, status: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Sync xmlns="AirSync:">
<Collections>
<Collection>
<Class>Calendar</Class>
<SyncKey>{}</SyncKey>
<CollectionId>{}</CollectionId>
<Status>{}</Status>
</Collection>
</Collections>
</Sync>"#,
        sync_key, collection_id, status
    )
}

pub fn eas_folder_sync_response(sync_key: &str, status: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<FolderSync xmlns="FolderHierarchy:">
<Status>{}</Status>
<SyncKey>{}</SyncKey>
<Changes>
<Add>
<ServerId>1</ServerId>
<ParentId>0</ParentId>
<DisplayName>Calendar</DisplayName>
<Type>8</Type>
</Add>
</Changes>
</FolderSync>"#,
        status, sync_key
    )
}

pub fn eas_provision_response(policy_key: &str, status: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Provision xmlns="Provision:">
<Status>{}</Status>
<Policies>
<Policy>
<PolicyType>MS-EAS-Provisioning-WBXML</PolicyType>
<Status>1</Status>
<PolicyKey>{}</PolicyKey>
<Data>
<Security>
<ApprovedApplicationList />
<Password>
<PasswordRecoveryEnabled>1</PasswordRecoveryEnabled>
</Password>
</Security>
</Data>
</Policy>
</Policies>
</Provision>"#,
        status, policy_key
    )
}

pub fn autodiscover_response(host: &str, email: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">
<Response xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a">
<User>
<DisplayName>Stalwart Mail</DisplayName>
<EMailAddress>{}</EMailAddress>
<DeploymentId>00000000-0000-0000-0000-000000000000</DeploymentId>
</User>
<Account>
<AccountType>email</AccountType>
<Action>settings</Action>
<Protocol>
<Type>EXCH</Type>
<Server>{}</Server>
<ServerDN>/o=Exchange/ou=Exchange Administrative Group/cn=Recipients/cn={}</ServerDN>
<ServerVersion>15.20.0.0</ServerVersion>
<ASUrl>https://{}/Microsoft-Server-ActiveSync</ASUrl>
<EwsUrl>https://{}/EWS/Exchange.asmx</EwsUrl>
<AuthPackage>Basic</AuthPackage>
<SSL>on</SSL>
<AuthRequired>on</AuthRequired>
</Protocol>
<Protocol>
<Type>EXPR</Type>
<Server>{}</Server>
<SSL>on</SSL>
<AuthPackage>Basic</AuthPackage>
<ASUrl>https://{}/Microsoft-Server-ActiveSync</ASUrl>
<EwsUrl>https://{}/EWS/Exchange.asmx</EwsUrl>
</Protocol>
<Protocol>
<Type>MobileSync</Type>
<Server>{}</Server>
<Url>https://{}/Microsoft-Server-ActiveSync</Url>
</Protocol>
</Account>
</Response>
</Autodiscover>"#,
        email,
        host,
        email,
        host,
        host,
        host,
        host,
        host,
        host,
        host
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ews_soap_response_wraps_inner() {
        let inner = "<m:GetFolderResponse><m:ResponseMessages /></m:GetFolderResponse>";
        let resp = ews_soap_response(inner);
        assert!(resp.contains("Envelope"));
        assert!(resp.contains("ServerVersionInfo"));
        assert!(resp.contains(inner));
    }

    #[test]
    fn test_eas_sync_response_contains_key() {
        let resp = eas_sync_response("new-key", "1", "1");
        assert!(resp.contains("<SyncKey>new-key</SyncKey>"));
        assert!(resp.contains("<Status>1</Status>"));
    }

    #[test]
    fn test_eas_folder_sync_response_structure() {
        let resp = eas_folder_sync_response("sync-key", "1");
        assert!(resp.contains("FolderSync"));
        assert!(resp.contains("<ServerId>1</ServerId>"));
        assert!(resp.contains("<DisplayName>Calendar</DisplayName>"));
    }

    #[test]
    fn test_eas_provision_response_has_policy_key() {
        let resp = eas_provision_response("1234567890", "1");
        assert!(resp.contains("<PolicyKey>1234567890</PolicyKey>"));
        assert!(resp.contains("MS-EAS-Provisioning-WBXML"));
    }

    #[test]
    fn test_autodiscover_response_contains_urls() {
        let resp = autodiscover_response("mail.example.com", "user@example.com");
        assert!(resp.contains("EwsUrl"));
        assert!(resp.contains("ASUrl"));
        assert!(resp.contains("mail.example.com"));
        assert!(resp.contains("user@example.com"));
    }
}

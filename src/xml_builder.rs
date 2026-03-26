// src/xml_builder.rs
// XML Builder for EAS responses
//
// Features:
// - Type-safe XML building
// - Namespace support
// - Automatic escaping
// - Response building helpers
//
// March 2026 - Production-ready, security-hardened

use axum::{body::Body, http::StatusCode, response::Response};

/// EAS namespace constants
pub const NS_AIR_SYNC: &str = "AirSync";
pub const NS_AIR_SYNC_BASE: &str = "AirSyncBase";
pub const NS_CALENDAR: &str = "Calendar";
pub const NS_CONTACTS: &str = "Contacts";
pub const NS_EMAIL: &str = "Email";
pub const NS_FOLDER_HIERARCHY: &str = "FolderHierarchy";
pub const NS_GET_ITEM_ESTIMATE: &str = "GetItemEstimate";
pub const NS_ITEM_OPERATIONS: &str = "ItemOperations";
pub const NS_MEETING_RESPONSE: &str = "MeetingResponse";
pub const NS_MOVE: &str = "Move";
pub const NS_PING: &str = "Ping";
pub const NS_PROVISION: &str = "Provision";
pub const NS_RESOLVE_RECIPIENTS: &str = "ResolveRecipients";
pub const NS_SEARCH: &str = "Search";
pub const NS_SETTINGS: &str = "Settings";
pub const NS_TASKS: &str = "Tasks";
pub const NS_VALIDATE_CERT: &str = "ValidateCert";
pub const NS_COMPOSE_MAIL: &str = "ComposeMail";

/// XML Builder for constructing EAS responses
pub struct EasXmlBuilder {
    xml: String,
    element_stack: Vec<String>,
    namespaces: std::collections::HashMap<String, String>,
}

impl EasXmlBuilder {
    /// Create a new XML builder
    pub fn new() -> Self {
        let mut builder = Self {
            xml: String::new(),
            element_stack: Vec::new(),
            namespaces: std::collections::HashMap::new(),
        };

        // Add XML declaration
        builder
            .xml
            .push_str(r#"<?xml version="1.0" encoding="utf-8"?>"#);

        builder
    }

    /// Register a namespace
    pub fn register_namespace(&mut self, prefix: &str, uri: &str) {
        self.namespaces.insert(prefix.to_string(), uri.to_string());
    }

    /// Start a new element
    pub fn start_element(&mut self, name: &str, namespace: &str) -> &mut Self {
        let full_name = if namespace.is_empty() {
            name.to_string()
        } else {
            format!("{}:{}", namespace, name)
        };

        // Close previous element if needed
        if !self.xml.is_empty() && !self.xml.ends_with('>') {
            self.xml.push('>');
        }

        self.xml.push_str(&format!("<{}>", full_name));
        self.element_stack.push(full_name);
        self
    }

    /// Start a new element with attributes
    pub fn start_element_with_attrs(
        &mut self,
        name: &str,
        namespace: &str,
        attrs: &[(&str, &str)],
    ) -> &mut Self {
        let full_name = if namespace.is_empty() {
            name.to_string()
        } else {
            format!("{}:{}", namespace, name)
        };

        if !self.xml.is_empty() && !self.xml.ends_with('>') {
            self.xml.push('>');
        }

        self.xml.push('<');
        self.xml.push_str(&full_name);

        for (key, value) in attrs {
            self.xml
                .push_str(&format!(" {}=\"{}\"", key, xml_escape(value)));
        }

        self.xml.push('>');
        self.element_stack.push(full_name);
        self
    }

    /// Add a simple element with text content
    pub fn add_element(&mut self, name: &str, content: &str) -> &mut Self {
        self.add_element_ns(name, "", content)
    }

    /// Add a simple element with namespace and text content
    pub fn add_element_ns(&mut self, name: &str, namespace: &str, content: &str) -> &mut Self {
        let full_name = if namespace.is_empty() {
            name.to_string()
        } else {
            format!("{}:{}", namespace, name)
        };

        if !self.xml.is_empty() && !self.xml.ends_with('>') {
            self.xml.push('>');
        }

        self.xml.push_str(&format!(
            "<{}>{}</{}>",
            full_name,
            xml_escape(content),
            full_name
        ));

        self
    }

    /// Add a self-closing element
    pub fn add_empty_element(&mut self, name: &str, namespace: &str) -> &mut Self {
        let full_name = if namespace.is_empty() {
            name.to_string()
        } else {
            format!("{}:{}", namespace, name)
        };

        if !self.xml.is_empty() && !self.xml.ends_with('>') {
            self.xml.push('>');
        }

        self.xml.push_str(&format!("<{} />", full_name));
        self
    }

    /// Add raw XML content (use with caution - no escaping)
    pub fn add_raw(&mut self, content: &str) -> &mut Self {
        if !self.xml.is_empty() && !self.xml.ends_with('>') {
            self.xml.push('>');
        }

        self.xml.push_str(content);
        self
    }

    /// Add CDATA section
    pub fn add_cdata(&mut self, content: &str) -> &mut Self {
        if !self.xml.is_empty() && !self.xml.ends_with('>') {
            self.xml.push('>');
        }

        self.xml.push_str("<![CDATA[");
        self.xml.push_str(content);
        self.xml.push_str("]]>");
        self
    }

    /// End the current element
    pub fn end_element(&mut self, name: &str) -> &mut Self {
        if let Some(expected) = self.element_stack.pop() {
            // Check if we're closing the right element
            if !expected.ends_with(&format!(":{}", name)) && expected != name {
                // Mismatched tags - try to recover
                self.xml.push_str(&format!("</{}>", expected));
            } else {
                self.xml.push_str(&format!("</{}>", expected));
            }
        }
        self
    }

    /// End all remaining elements
    pub fn end_all(&mut self) -> &mut Self {
        while let Some(name) = self.element_stack.pop() {
            self.xml.push_str(&format!("</{}>", name));
        }
        self
    }

    /// Get the built XML string
    pub fn to_xml(&mut self) -> String {
        self.end_all();
        self.xml.clone()
    }

    /// Build an HTTP response from the XML
    pub fn build_response(&mut self) -> Response {
        let xml = self.to_xml();

        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/vnd.ms-sync.wbxml")
            .body(Body::from(xml))
            .unwrap()
    }

    /// Build an HTTP response with custom status
    pub fn build_response_with_status(&mut self, status: StatusCode) -> Response {
        let xml = self.to_xml();

        Response::builder()
            .status(status)
            .header("Content-Type", "application/vnd.ms-sync.wbxml")
            .body(Body::from(xml))
            .unwrap()
    }
}

impl Default for EasXmlBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// XML escape helper
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Build EAS error response
pub fn build_eas_error(status: u16, message: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Error xmlns="AirSync:">
    <Status>{}</Status>
    <Message>{}</Message>
</Error>"#,
        status,
        xml_escape(message)
    )
}

/// Build EAS success response
pub fn build_eas_success() -> String {
    r#"<?xml version="1.0" encoding="utf-8"?>
<Success xmlns="AirSync:">
    <Status>1</Status>
</Success>"#
        .to_string()
}

/// Build EAS provision response
pub fn build_eas_provision_response(policy_key: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Provision xmlns="Provision:">
    <Status>1</Status>
    <Policies>
        <Policy>
            <PolicyType>MS-EAS-Provisioning-WBXML</PolicyType>
            <Status>1</Status>
            <PolicyKey>{}</PolicyKey>
            <Data>
                <DevicePasswordEnabled>0</DevicePasswordEnabled>
                <AlphanumericDevicePasswordRequired>0</AlphanumericDevicePasswordRequired>
                <RequireStorageCardEncryption>0</RequireStorageCardEncryption>
                <PasswordRecoveryEnabled>0</PasswordRecoveryEnabled>
            </Data>
        </Policy>
    </Policies>
</Provision>"#,
        xml_escape(policy_key)
    )
}

/// Build EAS folder sync response
pub fn build_eas_folder_sync_response(
    sync_key: &str,
    folders: &[(String, String, String)],
) -> String {
    let mut builder = EasXmlBuilder::new();
    builder.start_element("FolderSync", NS_FOLDER_HIERARCHY);
    builder.add_element("Status", "1");
    builder.add_element("SyncKey", sync_key);
    builder.start_element("Changes", NS_FOLDER_HIERARCHY);
    builder.add_element("Count", &folders.len().to_string());

    for (server_id, parent_id, display_name) in folders {
        builder.start_element("Add", NS_FOLDER_HIERARCHY);
        builder.add_element("ServerId", server_id);
        builder.add_element("ParentId", parent_id);
        builder.add_element("DisplayName", display_name);
        builder.add_element("Type", "8"); // Calendar folder
        builder.end_element("Add");
    }

    builder.end_element("Changes");
    builder.end_element("FolderSync");
    builder.to_xml()
}

/// Build EAS sync response
pub fn build_eas_sync_response(
    sync_key: &str,
    collection_id: &str,
    changes: &[(String, String, Option<String>)], // (server_id, change_type, content)
) -> String {
    let mut builder = EasXmlBuilder::new();
    builder.start_element("Sync", NS_AIR_SYNC);
    builder.add_element("Status", "1");
    builder.start_element("Collections", NS_AIR_SYNC);
    builder.start_element("Collection", NS_AIR_SYNC);
    builder.add_element("SyncKey", sync_key);
    builder.add_element("CollectionId", collection_id);
    builder.add_element("Status", "1");

    if !changes.is_empty() {
        builder.start_element("Commands", NS_AIR_SYNC);
        for (server_id, change_type, content) in changes {
            builder.start_element(change_type, NS_AIR_SYNC);
            builder.add_element("ServerId", server_id);
            if let Some(ref data) = content {
                builder.start_element("ApplicationData", NS_AIR_SYNC);
                builder.add_raw(data);
                builder.end_element("ApplicationData");
            }
            builder.end_element(change_type);
        }
        builder.end_element("Commands");
    }

    builder.end_element("Collection");
    builder.end_element("Collections");
    builder.end_element("Sync");
    builder.to_xml()
}

/// Build EAS ping response
pub fn build_eas_ping_response(status: &str, folders: Option<&[String]>) -> String {
    let mut builder = EasXmlBuilder::new();
    builder.start_element("Ping", NS_PING);
    builder.add_element("Status", status);

    if let Some(folder_list) = folders {
        builder.start_element("Folders", NS_PING);
        for folder in folder_list {
            builder.add_element("Folder", folder);
        }
        builder.end_element("Folders");
    }

    builder.end_element("Ping");
    builder.to_xml()
}

/// Build EAS GetItemEstimate response
pub fn build_eas_get_item_estimate_response(collection_id: &str, estimate: usize) -> String {
    let mut builder = EasXmlBuilder::new();
    builder.start_element("GetItemEstimate", NS_GET_ITEM_ESTIMATE);
    builder.add_element("Status", "1");
    builder.start_element("Response", NS_GET_ITEM_ESTIMATE);
    builder.add_element("CollectionId", collection_id);
    builder.add_element("Estimate", &estimate.to_string());
    builder.end_element("Response");
    builder.end_element("GetItemEstimate");
    builder.to_xml()
}

/// Build EAS Search response
pub fn build_eas_search_response(
    results: &[(String, String)],
    total: usize,
    range: (usize, usize),
) -> String {
    let mut builder = EasXmlBuilder::new();
    builder.start_element("Search", NS_SEARCH);
    builder.add_element("Status", "1");
    builder.start_element("Response", NS_SEARCH);
    builder.add_element("Store", "Mailbox");
    builder.add_element("Status", "1");
    builder.add_element("Range", &format!("{}-{}", range.0, range.1));
    builder.add_element("Total", &total.to_string());

    for (id, content) in results {
        builder.start_element("Result", NS_SEARCH);
        builder.add_element("Class", "Calendar");
        builder.start_element("Properties", NS_SEARCH);
        builder.add_raw(content);
        builder.end_element("Properties");
        builder.end_element("Result");
    }

    builder.end_element("Response");
    builder.end_element("Search");
    builder.to_xml()
}

/// Build EAS ItemOperations response
pub fn build_eas_item_operations_response(
    status: &str,
    results: &[(String, Option<String>)],
) -> String {
    let mut builder = EasXmlBuilder::new();
    builder.start_element("ItemOperations", NS_ITEM_OPERATIONS);
    builder.add_element("Status", status);

    for (item_id, content) in results {
        builder.start_element("Response", NS_ITEM_OPERATIONS);
        builder.add_element("Status", "1");

        if let Some(ref data) = content {
            builder.start_element("Properties", NS_ITEM_OPERATIONS);
            builder.add_raw(data);
            builder.end_element("Properties");
        }

        builder.end_element("Response");
    }

    builder.end_element("ItemOperations");
    builder.to_xml()
}

/// Build EAS MeetingResponse response
pub fn build_eas_meeting_response_response(results: &[(String, &str, &str)]) -> String {
    let mut builder = EasXmlBuilder::new();
    builder.start_element("MeetingResponse", NS_MEETING_RESPONSE);
    builder.add_element("Status", "1");

    for (request_id, status, calendar_id) in results {
        builder.start_element("Result", NS_MEETING_RESPONSE);
        builder.add_element("RequestId", request_id);
        builder.add_element("Status", status);
        builder.add_element("CalendarId", calendar_id);
        builder.end_element("Result");
    }

    builder.end_element("MeetingResponse");
    builder.to_xml()
}

/// Build EAS ResolveRecipients response
pub fn build_eas_resolve_recipients_response(resolved: &[(String, String, String)]) -> String {
    let mut builder = EasXmlBuilder::new();
    builder.start_element("ResolveRecipients", NS_RESOLVE_RECIPIENTS);
    builder.add_element("Status", "1");

    for (to, display_name, email) in resolved {
        builder.start_element("Response", NS_RESOLVE_RECIPIENTS);
        builder.add_element("To", to);
        builder.add_element("Status", "1");
        builder.start_element("RecipientCount", NS_RESOLVE_RECIPIENTS);
        builder.add_element("Count", "1");
        builder.start_element("Recipient", NS_RESOLVE_RECIPIENTS);
        builder.add_element("Type", "1");
        builder.add_element("DisplayName", display_name);
        builder.add_element("EmailAddress", email);
        builder.end_element("Recipient");
        builder.end_element("RecipientCount");
        builder.end_element("Response");
    }

    builder.end_element("ResolveRecipients");
    builder.to_xml()
}

/// Build EAS ValidateCert response
pub fn build_eas_validate_cert_response(results: &[(String, u8)]) -> String {
    let mut builder = EasXmlBuilder::new();
    builder.start_element("ValidateCert", NS_VALIDATE_CERT);
    builder.add_element("Status", "1");
    builder.start_element("Certificate", NS_VALIDATE_CERT);

    for (_cert_id, status) in results {
        builder.start_element("Validation", NS_VALIDATE_CERT);
        builder.add_element("Status", &status.to_string());
        builder.end_element("Validation");
    }

    builder.end_element("Certificate");
    builder.end_element("ValidateCert");
    builder.to_xml()
}

/// Build EAS Settings response
pub fn build_eas_settings_response() -> String {
    let mut builder = EasXmlBuilder::new();
    builder.start_element("Settings", NS_SETTINGS);
    builder.add_element("Status", "1");

    builder.start_element("DeviceInformation", NS_SETTINGS);
    builder.add_element("Status", "1");
    builder.end_element("DeviceInformation");

    builder.start_element("UserInformation", NS_SETTINGS);
    builder.add_element("Status", "1");
    builder.end_element("UserInformation");

    builder.end_element("Settings");
    builder.to_xml()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_xml_builder_basic() {
        let mut builder = EasXmlBuilder::new();
        builder.start_element("Test", "AirSync");
        builder.add_element("Status", "1");
        builder.end_element("Test");

        let xml = builder.to_xml();
        assert!(xml.contains("<?xml version=\"1.0\" encoding=\"utf-8\"?>"));
        assert!(xml.contains("<AirSync:Test>"));
        assert!(xml.contains("<AirSync:Status>1</AirSync:Status>"));
        assert!(xml.contains("</AirSync:Test>"));
    }

    #[test]
    fn test_xml_escaping() {
        let mut builder = EasXmlBuilder::new();
        builder.start_element("Test", "");
        builder.add_element("Content", "<script>alert('test')</script>");
        builder.end_element("Test");

        let xml = builder.to_xml();
        assert!(xml.contains("&lt;script&gt;"));
        assert!(!xml.contains("<script>"));
    }

    #[test]
    fn test_build_eas_error() {
        let error = build_eas_error(103, "Protocol error");
        assert!(error.contains("<Status>103</Status>"));
        assert!(error.contains("Protocol error"));
    }

    #[test]
    fn test_build_eas_folder_sync() {
        let folders = vec![("1".to_string(), "0".to_string(), "Calendar".to_string())];
        let xml = build_eas_folder_sync_response("12345", &folders);
        assert!(xml.contains("<SyncKey>12345</SyncKey>"));
        assert!(xml.contains("<DisplayName>Calendar</DisplayName>"));
    }
}

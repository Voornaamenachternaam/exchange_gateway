// src/delegate_ews.rs
use crate::permission::delegate::DelegateManager;
use crate::permission::types::{DelegateInfo, PermissionLevel};
use crate::protocol_fixtures::{EWS_MSG_NS, EWS_TYPE_NS};
use crate::storage::Storage;
use crate::util::xml_escape;
use quick_xml::Reader;
use quick_xml::events::Event;
use std::sync::Arc;

pub struct DelegateEwsHandler {
    storage: Arc<Storage>,
}

impl DelegateEwsHandler {
    pub fn new(storage: Arc<Storage>) -> Self {
        Self { storage }
    }

    pub async fn handle_add_delegate(
        &self,
        auth_email: &str,
        body: &str,
    ) -> String {
        let parsed = parse_add_delegate_request(body);
        let delegate_email = match parsed.delegate_email {
            Some(e) => e,
            None => {
                return render_delegate_error(
                    "ErrorInvalidDelegate",
                    "Delegate email address is required",
                );
            }
        };

        let manager = DelegateManager::new(&self.storage);
        match manager
            .add_delegate(
                auth_email,
                &delegate_email,
                parsed.delegate_name.as_deref(),
                parsed.calendar_permission.unwrap_or(PermissionLevel::Reviewer),
                auth_email,
            )
            .await
        {
            Ok(delegate) => render_add_delegate_response(&delegate, &manager),
            Err(e) => render_delegate_error("ErrorDelegateAlreadyExists", &e.to_string()),
        }
    }

    pub async fn handle_remove_delegate(
        &self,
        auth_email: &str,
        body: &str,
    ) -> String {
        let delegate_email = match parse_remove_delegate_request(body) {
            Some(e) => e,
            None => {
                return render_delegate_error(
                    "ErrorInvalidDelegate",
                    "Delegate email address is required",
                );
            }
        };

        let manager = DelegateManager::new(&self.storage);
        match manager
            .remove_delegate(auth_email, &delegate_email, auth_email)
            .await
        {
            Ok(()) => render_remove_delegate_response(),
            Err(e) => render_delegate_error("ErrorDelegateNotFound", &e.to_string()),
        }
    }

    pub async fn handle_update_delegate(
        &self,
        auth_email: &str,
        body: &str,
    ) -> String {
        let parsed = parse_update_delegate_request(body);
        let delegate_email = match parsed.delegate_email {
            Some(e) => e,
            None => {
                return render_delegate_error(
                    "ErrorInvalidDelegate",
                    "Delegate email address is required",
                );
            }
        };

        let manager = DelegateManager::new(&self.storage);
        match manager
            .update_delegate(
                auth_email,
                &delegate_email,
                parsed.calendar_permission,
                parsed.receive_copies,
                parsed.receive_infos,
                parsed.view_private,
                auth_email,
            )
            .await
        {
            Ok(delegate) => render_update_delegate_response(&delegate, &manager),
            Err(e) => render_delegate_error("ErrorDelegateNotFound", &e.to_string()),
        }
    }

    pub async fn handle_get_delegate(
        &self,
        auth_email: &str,
    ) -> String {
        let manager = DelegateManager::new(&self.storage);
        match manager.get_delegates(auth_email).await {
            Ok(delegates) => render_get_delegate_response(&delegates, &manager),
            Err(e) => render_delegate_error("ErrorDelegateNotFound", &e.to_string()),
        }
    }
}

#[derive(Default)]
struct ParsedDelegateRequest {
    delegate_email: Option<String>,
    delegate_name: Option<String>,
    calendar_permission: Option<PermissionLevel>,
    receive_copies: Option<bool>,
    receive_infos: Option<bool>,
    view_private: Option<bool>,
}


fn parse_add_delegate_request(xml: &str) -> ParsedDelegateRequest {
    let mut result = ParsedDelegateRequest::default();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut in_email = false;
    let mut in_display_name = false;
    let mut in_calendar_perm = false;
    let mut _in_inbox_perm = false;
    let mut in_receive_copies = false;
    let mut in_view_private = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let local = e.name().local_name();
                match local.as_ref() {
                    b"EmailAddress" => {
                        in_email = true;
                    }
                    b"DisplayName" => {
                        in_display_name = true;
                    }
                    b"CalendarFolderPermissionLevel" => {
                        in_calendar_perm = true;
                    }
                    b"InboxFolderPermissionLevel" => {
                        _in_inbox_perm = true;
                    }
                    b"ReceiveCopiesOfMeetingMessages" => {
                        in_receive_copies = true;
                    }
                    b"ViewPrivateItems" => {
                        in_view_private = true;
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(e)) => {
                if let Ok(text) = e.decode() {
                    let text = text.into_owned();
                    if in_email {
                        result.delegate_email = Some(text);
                    } else if in_display_name {
                        result.delegate_name = Some(text);
                    } else if in_calendar_perm {
                        result.calendar_permission = parse_delegate_permission_level(&text);
                    } else if in_receive_copies {
                        result.receive_copies = Some(text.eq_ignore_ascii_case("true"));
                    } else if in_view_private {
                        result.view_private = Some(text.eq_ignore_ascii_case("true"));
                    }
                }
            }
            Ok(Event::End(e)) => {
                let local = e.name().local_name();
                match local.as_ref() {
                    b"EmailAddress" => in_email = false,
                    b"DisplayName" => in_display_name = false,
                    b"CalendarFolderPermissionLevel" => in_calendar_perm = false,
                    b"InboxFolderPermissionLevel" => _in_inbox_perm = false,
                    b"ReceiveCopiesOfMeetingMessages" => in_receive_copies = false,
                    b"ViewPrivateItems" => in_view_private = false,
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    result
}

fn parse_remove_delegate_request(xml: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut in_email = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e))
                if e.name().local_name().as_ref() == b"EmailAddress" => {
                    in_email = true;
                }
            Ok(Event::Text(e)) => {
                if in_email
                    && let Ok(text) = e.decode() {
                        return Some(text.into_owned());
                    }
            }
            Ok(Event::End(_)) => {
                in_email = false;
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    None
}

fn parse_update_delegate_request(xml: &str) -> ParsedDelegateRequest {
    parse_add_delegate_request(xml)
}

fn parse_delegate_permission_level(s: &str) -> Option<PermissionLevel> {
    match s.to_ascii_lowercase().as_str() {
        "none" => Some(PermissionLevel::None),
        "reviewer" => Some(PermissionLevel::Reviewer),
        "author" => Some(PermissionLevel::Author),
        "editor" => Some(PermissionLevel::Editor),
        "custom" => Some(PermissionLevel::Reviewer),
        _ => None,
    }
}

fn render_add_delegate_response(delegate: &DelegateInfo, manager: &DelegateManager) -> String {
    let delegate_xml = manager.render_delegate_xml(delegate);
    format!(
        r#"<m:AddDelegateResponse xmlns:m="{}" xmlns:t="{}">
            <m:ResponseMessages>
                <m:DelegateUserResponseMessageType ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                    {}
                </m:DelegateUserResponseMessageType>
            </m:ResponseMessages>
            <m:DeliverMeetingRequests>DelegatesAndMe</m:DeliverMeetingRequests>
        </m:AddDelegateResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        delegate_xml,
    )
}

fn render_remove_delegate_response() -> String {
    format!(
        r#"<m:RemoveDelegateResponse xmlns:m="{}" xmlns:t="{}">
            <m:ResponseMessages>
                <m:DelegateUserResponseMessageType ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                </m:DelegateUserResponseMessageType>
            </m:ResponseMessages>
        </m:RemoveDelegateResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
    )
}

fn render_update_delegate_response(delegate: &DelegateInfo, manager: &DelegateManager) -> String {
    let delegate_xml = manager.render_delegate_xml(delegate);
    format!(
        r#"<m:UpdateDelegateResponse xmlns:m="{}" xmlns:t="{}">
            <m:ResponseMessages>
                <m:DelegateUserResponseMessageType ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                    {}
                </m:DelegateUserResponseMessageType>
            </m:ResponseMessages>
        </m:UpdateDelegateResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        delegate_xml,
    )
}

fn render_get_delegate_response(delegates: &[DelegateInfo], manager: &DelegateManager) -> String {
    let delegates_xml = delegates
        .iter()
        .map(|d| {
            format!(
                r#"<m:DelegateUserResponseMessageType ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                    {}
                </m:DelegateUserResponseMessageType>"#,
                manager.render_delegate_xml(d),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let deliver_meeting_requests = "DelegatesAndMe";

    format!(
        r#"<m:GetDelegateResponse xmlns:m="{}" xmlns:t="{}">
            <m:ResponseMessages>
                {}
            </m:ResponseMessages>
            <m:DeliverMeetingRequests>{}</m:DeliverMeetingRequests>
        </m:GetDelegateResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        delegates_xml,
        deliver_meeting_requests,
    )
}

fn render_delegate_error(code: &str, message: &str) -> String {
    format!(
        r#"<m:GetDelegateResponse xmlns:m="{}" xmlns:t="{}">
            <m:ResponseMessages>
                <m:DelegateUserResponseMessageType ResponseClass="Error">
                    <m:MessageText>{}</m:MessageText>
                    <m:ResponseCode>{}</m:ResponseCode>
                    <m:DescriptiveLinkKey>0</m:DescriptiveLinkKey>
                </m:DelegateUserResponseMessageType>
            </m:ResponseMessages>
        </m:GetDelegateResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        xml_escape(message),
        xml_escape(code),
    )
}
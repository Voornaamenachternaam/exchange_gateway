// src/meeting/scheduling.rs
use crate::calendar::CalendarItem;
use crate::meeting::attendee::AttendeeStatus;
use crate::meeting::message::{MeetingMessage, MeetingMessageGenerator};
use crate::util::xml_escape;
use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};

pub const CALDAV_SCHEDULING_NAMESPACE: &str = "urn:ietf:params:xml:ns:caldav";
pub const CALDAV_CALENDAR_ACCESS_NAMESPACE: &str = "urn:ietf:params:xml:ns:caldav";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SchedulingResult {
    pub success: bool,
    pub message: String,
    pub scheduling_href: Option<String>,
    pub delivery_status: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SchedulingError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for SchedulingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SchedulingError({}): {}", self.code, self.message)
    }
}

impl std::error::Error for SchedulingError {}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FreeBusyEntry {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub busy_type: String,
}

#[derive(Clone, Debug)]
pub struct ScheduleOutboxEntry {
    pub href: String,
    pub display_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ScheduleInboxEntry {
    pub href: String,
    pub display_name: Option<String>,
}

pub struct CaldavScheduling {
    caldav_base: String,
    http_client: reqwest::Client,
    message_generator: MeetingMessageGenerator,
}

impl CaldavScheduling {
    pub fn new(caldav_base: &str) -> Self {
        Self {
            caldav_base: caldav_base.to_string(),
            http_client: reqwest::Client::new(),
            message_generator: MeetingMessageGenerator::new(),
        }
    }

    pub async fn discover_scheduling_collections(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(Option<String>, Option<String>)> {
        let home_url = format!(
            "{}/cal/{}/",
            self.caldav_base.trim_end_matches('/'),
            username
        );

        let propfind_body = format!(r#"<?xml version="1.0" encoding="utf-8"?>
<propfind xmlns="DAV:" xmlns:C="{}">
    <prop>
        <C:schedule-outbox-URL/>
        <C:schedule-inbox-URL/>
        <C:calendar-user-address-set/>
    </prop>
</propfind>"#, CALDAV_SCHEDULING_NAMESPACE);

        let resp = self.http_client
            .request(reqwest::Method::from_bytes(b"PROPFIND")?, &home_url)
            .basic_auth(username, Some(password))
            .header("Depth", "0")
            .header(CONTENT_TYPE, "application/xml; charset=utf-8")
            .body(propfind_body)
            .send()
            .await?;

        if !resp.status().is_success() && resp.status().as_u16() != 207 {
            return Err(anyhow!("Failed to discover scheduling collections: {}", resp.status()));
        }

        let body = resp.text().await?;
        let outbox = extract_href(&body, "schedule-outbox-URL");
        let inbox = extract_href(&body, "schedule-inbox-URL");

        Ok((outbox, inbox))
    }

    pub async fn send_meeting_request(
        &self,
        item: &CalendarItem,
        username: &str,
        password: &str,
    ) -> Result<SchedulingResult> {
        let (outbox_url, _) = self.discover_scheduling_collections(username, password).await?;
        let outbox = outbox_url.unwrap_or_else(|| {
            format!(
                "{}/cal/{}/outbox/",
                self.caldav_base.trim_end_matches('/'),
                username
            )
        });

        let msg = MeetingMessage::new_request(item);
        let ics = self.message_generator.generate_ical(&msg);

        let schedule_body = format!(r#"<?xml version="1.0" encoding="utf-8"?>
<C:schedule xmlns:C="{}">
    <C:calendar-data content-type="text/calendar" charset="utf-8">{}</C:calendar-data>
</C:schedule>"#, CALDAV_SCHEDULING_NAMESPACE, xml_escape(&ics));

        let resp = self.http_client
            .post(&outbox)
            .basic_auth(username, Some(password))
            .header(CONTENT_TYPE, "application/xml; charset=utf-8")
            .body(schedule_body)
            .send()
            .await?;

        if resp.status().is_success() || resp.status().as_u16() == 207 {
            Ok(SchedulingResult {
                success: true,
                message: "Meeting request sent successfully".to_string(),
                scheduling_href: Some(outbox),
                delivery_status: Some("delivered".to_string()),
            })
        } else {
            Ok(SchedulingResult {
                success: false,
                message: format!("Failed to send meeting request: {}", resp.status()),
                scheduling_href: None,
                delivery_status: Some("failed".to_string()),
            })
        }
    }

    pub async fn send_meeting_update(
        &self,
        item: &CalendarItem,
        sequence: u32,
        username: &str,
        password: &str,
    ) -> Result<SchedulingResult> {
        let (outbox_url, _) = self.discover_scheduling_collections(username, password).await?;
        let outbox = outbox_url.unwrap_or_else(|| {
            format!(
                "{}/cal/{}/outbox/",
                self.caldav_base.trim_end_matches('/'),
                username
            )
        });

        let msg = MeetingMessage::new_update(item, sequence);
        let ics = self.message_generator.generate_ical(&msg);

        let schedule_body = format!(r#"<?xml version="1.0" encoding="utf-8"?>
<C:schedule xmlns:C="{}">
    <C:calendar-data content-type="text/calendar" charset="utf-8">{}</C:calendar-data>
</C:schedule>"#, CALDAV_SCHEDULING_NAMESPACE, xml_escape(&ics));

        let resp = self.http_client
            .post(&outbox)
            .basic_auth(username, Some(password))
            .header(CONTENT_TYPE, "application/xml; charset=utf-8")
            .body(schedule_body)
            .send()
            .await?;

        if resp.status().is_success() || resp.status().as_u16() == 207 {
            Ok(SchedulingResult {
                success: true,
                message: "Meeting update sent successfully".to_string(),
                scheduling_href: Some(outbox),
                delivery_status: Some("delivered".to_string()),
            })
        } else {
            Ok(SchedulingResult {
                success: false,
                message: format!("Failed to send meeting update: {}", resp.status()),
                scheduling_href: None,
                delivery_status: Some("failed".to_string()),
            })
        }
    }

    pub async fn send_cancellation(
        &self,
        item: &CalendarItem,
        sequence: u32,
        username: &str,
        password: &str,
    ) -> Result<SchedulingResult> {
        let (outbox_url, _) = self.discover_scheduling_collections(username, password).await?;
        let outbox = outbox_url.unwrap_or_else(|| {
            format!(
                "{}/cal/{}/outbox/",
                self.caldav_base.trim_end_matches('/'),
                username
            )
        });

        let msg = MeetingMessage::new_cancellation(item, sequence);
        let ics = self.message_generator.generate_ical(&msg);

        let schedule_body = format!(r#"<?xml version="1.0" encoding="utf-8"?>
<C:schedule xmlns:C="{}">
    <C:calendar-data content-type="text/calendar" charset="utf-8">{}</C:calendar-data>
</C:schedule>"#, CALDAV_SCHEDULING_NAMESPACE, xml_escape(&ics));

        let resp = self.http_client
            .post(&outbox)
            .basic_auth(username, Some(password))
            .header(CONTENT_TYPE, "application/xml; charset=utf-8")
            .body(schedule_body)
            .send()
            .await?;

        if resp.status().is_success() || resp.status().as_u16() == 207 {
            Ok(SchedulingResult {
                success: true,
                message: "Meeting cancellation sent successfully".to_string(),
                scheduling_href: Some(outbox),
                delivery_status: Some("delivered".to_string()),
            })
        } else {
            Ok(SchedulingResult {
                success: false,
                message: format!("Failed to send cancellation: {}", resp.status()),
                scheduling_href: None,
                delivery_status: Some("failed".to_string()),
            })
        }
    }

    pub async fn send_response(
        &self,
        uid: &str,
        organizer_email: &str,
        subject: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        status: AttendeeStatus,
        sequence: u32,
        username: &str,
        password: &str,
    ) -> Result<SchedulingResult> {
        let (outbox_url, _) = self.discover_scheduling_collections(username, password).await?;
        let outbox = outbox_url.unwrap_or_else(|| {
            format!(
                "{}/cal/{}/outbox/",
                self.caldav_base.trim_end_matches('/'),
                username
            )
        });

        let msg = MeetingMessage::new_response(uid, organizer_email, subject, start, end, status, sequence);
        let ics = self.message_generator.generate_ical(&msg);

        let _recipient = format!("mailto:{}", organizer_email);
        
        let schedule_body = format!(r#"<?xml version="1.0" encoding="utf-8"?>
<C:schedule xmlns:C="{}">
    <C:calendar-data content-type="text/calendar" charset="utf-8">{}</C:calendar-data>
</C:schedule>"#, CALDAV_SCHEDULING_NAMESPACE, xml_escape(&ics));

        let resp = self.http_client
            .post(&outbox)
            .basic_auth(username, Some(password))
            .header(CONTENT_TYPE, "application/xml; charset=utf-8")
            .body(schedule_body)
            .send()
            .await?;

        if resp.status().is_success() || resp.status().as_u16() == 207 {
            Ok(SchedulingResult {
                success: true,
                message: "Response sent successfully".to_string(),
                scheduling_href: Some(outbox),
                delivery_status: Some("delivered".to_string()),
            })
        } else {
            Ok(SchedulingResult {
                success: false,
                message: format!("Failed to send response: {}", resp.status()),
                scheduling_href: None,
                delivery_status: Some("failed".to_string()),
            })
        }
    }

    pub async fn query_freebusy(
        &self,
        calendar_href: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        username: &str,
        password: &str,
    ) -> Result<Vec<FreeBusyEntry>> {
        let start_str = start.format("%Y%m%dT%H%M%SZ").to_string();
        let end_str = end.format("%Y%m%dT%H%M%SZ").to_string();

        let report_body = format!(r#"<?xml version="1.0" encoding="utf-8"?>
<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
    <D:prop>
        <D:getetag/>
        <C:calendar-data content-type="text/calendar">
            <C:comp name="VCALENDAR">
                <C:comp name="VEVENT">
                    <C:prop name="DTSTART"/>
                    <C:prop name="DTEND"/>
                    <C:prop name="SUMMARY"/>
                    <C:prop name="X-MICROSOFT-CDO-BUSYSTATUS"/>
                </C:prop>
            </C:comp>
        </C:calendar-data>
    </D:prop>
    <C:filter>
        <C:comp-filter name="VCALENDAR">
            <C:comp-filter name="VEVENT">
                <C:time-range start="{start}" end="{end}"/>
            </C:comp-filter>
        </C:comp-filter>
    </C:filter>
</C:calendar-query>"#, start = start_str, end = end_str);

        let resp = self.http_client
            .request(reqwest::Method::from_bytes(b"REPORT")?, calendar_href)
            .basic_auth(username, Some(password))
            .header(CONTENT_TYPE, "application/xml; charset=utf-8")
            .header("Depth", "1")
            .body(report_body)
            .send()
            .await?;

        if !resp.status().is_success() && resp.status().as_u16() != 207 {
            return Err(anyhow!("Freebusy query failed: {}", resp.status()));
        }

        let body = resp.text().await?;
        parse_freebusy_response(&body)
    }

    pub async fn get_scheduling_inbox_messages(
        &self,
        inbox_href: &str,
        username: &str,
        password: &str,
    ) -> Result<Vec<SchedulingMessage>> {
        let report_body = r#"<?xml version="1.0" encoding="utf-8"?>
<propfind xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
    <prop>
        <D:getetag/>
        <C:schedule-tag/>
        <C:calendar-data/>
    </prop>
</propfind>"#;

        let resp = self.http_client
            .request(reqwest::Method::from_bytes(b"PROPFIND")?, inbox_href)
            .basic_auth(username, Some(password))
            .header("Depth", "1")
            .header(CONTENT_TYPE, "application/xml; charset=utf-8")
            .body(report_body)
            .send()
            .await?;

        if !resp.status().is_success() && resp.status().as_u16() != 207 {
            return Err(anyhow!("Failed to get inbox messages: {}", resp.status()));
        }

        let body = resp.text().await?;
        parse_scheduling_messages(&body)
    }

    pub fn message_generator(&self) -> &MeetingMessageGenerator {
        &self.message_generator
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SchedulingMessage {
    pub href: String,
    pub etag: Option<String>,
    pub schedule_tag: Option<String>,
    pub ical_data: Option<String>,
    pub method: Option<String>,
    pub uid: Option<String>,
    pub sender: Option<String>,
    pub recipient: Option<String>,
}

fn extract_href(xml: &str, tag: &str) -> Option<String> {
    let search_open = format!("<{}>", tag);
    let _search_close = format!("</{}>", tag);
    
    if let Some(start) = xml.find(&search_open) {
        let rest = &xml[start + search_open.len()..];
        if let Some(href_start) = rest.find("<DAV:href>") {
            let href_rest = &rest[href_start + 10..];
            if let Some(href_end) = href_rest.find("</DAV:href>") {
                return Some(href_rest[..href_end].to_string());
            }
        }
    }
    None
}

fn parse_freebusy_response(xml: &str) -> Result<Vec<FreeBusyEntry>> {
    let mut entries = Vec::new();
    
    let vevents: Vec<&str> = xml.match_indices("BEGIN:VEVENT")
        .filter_map(|(i, _)| {
            let rest = &xml[i..];
            rest.find("END:VEVENT").map(|j| &rest[..j + 11])
        })
        .collect();

    for vevent in vevents {
        if let (Some(start), Some(end)) = (extract_property(vevent, "DTSTART"), extract_property(vevent, "DTEND")) {
            let busy_type = extract_property(vevent, "X-MICROSOFT-CDO-BUSYSTATUS")
                .unwrap_or_else(|| "BUSY".to_string());
            
            if let (Ok(start_dt), Ok(end_dt)) = (
                parse_ical_datetime(&start),
                parse_ical_datetime(&end)
            ) {
                entries.push(FreeBusyEntry {
                    start: start_dt,
                    end: end_dt,
                    busy_type,
                });
            }
        }
    }

    Ok(entries)
}

fn parse_scheduling_messages(xml: &str) -> Result<Vec<SchedulingMessage>> {
    let mut messages = Vec::new();
    
    let responses: Vec<&str> = xml.match_indices("<response>")
        .filter_map(|(i, _)| {
            let rest = &xml[i..];
            rest.find("</response>").map(|j| &rest[..j + 11])
        })
        .collect();

    for resp in responses {
        let href = extract_tag_content(resp, "href").unwrap_or_default();
        let etag = extract_tag_content(resp, "getetag");
        let schedule_tag = extract_tag_content(resp, "schedule-tag");
        let ical_data = extract_tag_content(resp, "calendar-data");
        
        let (method, uid, sender, recipient) = if let Some(ref ical) = ical_data {
            (
                extract_property(ical, "METHOD"),
                extract_property(ical, "UID"),
                extract_attendee_or_organizer(ical, "ORGANIZER"),
                extract_attendee_or_organizer(ical, "ATTENDEE"),
            )
        } else {
            (None, None, None, None)
        };

        messages.push(SchedulingMessage {
            href,
            etag,
            schedule_tag,
            ical_data,
            method,
            uid,
            sender,
            recipient,
        });
    }

    Ok(messages)
}

fn extract_tag_content(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    
    if let Some(start) = xml.find(&open) {
        let rest = &xml[start + open.len()..];
        if let Some(end) = rest.find(&close) {
            return Some(rest[..end].to_string());
        }
    }
    None
}

fn extract_property(ical: &str, name: &str) -> Option<String> {
    for line in ical.lines() {
        if line.starts_with(name) {
            if let Some(pos) = line.find(':') {
                return Some(line[pos + 1..].to_string());
            }
        }
    }
    None
}

fn extract_attendee_or_organizer(ical: &str, name: &str) -> Option<String> {
    for line in ical.lines() {
        if line.starts_with(name) {
            if let Some(pos) = line.find("mailto:") {
                let rest = &line[pos + 7..];
                let end = rest.find(|c: char| c == '\r' || c == '\n').unwrap_or(rest.len());
                return Some(rest[..end].to_string());
            }
        }
    }
    None
}

fn parse_ical_datetime(s: &str) -> Result<DateTime<Utc>> {
    let s = s.trim_end_matches('Z');
    let ndt = chrono::NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%S")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%S%.f"))?;
    Ok(ndt.and_utc())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_property() {
        let ical = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nUID:test-uid\r\nEND:VCALENDAR";
        assert_eq!(extract_property(ical, "METHOD"), Some("REQUEST".to_string()));
        assert_eq!(extract_property(ical, "UID"), Some("test-uid".to_string()));
    }

    #[test]
    fn test_parse_freebusy_response() {
        let xml = r#"<response><calendar-data>BEGIN:VCALENDAR
BEGIN:VEVENT
DTSTART:20240115T100000Z
DTEND:20240115T110000Z
X-MICROSOFT-CDO-BUSYSTATUS:BUSY
END:VEVENT
END:VCALENDAR</calendar-data></response>"#;

        let entries = parse_freebusy_response(xml).unwrap();
        assert_eq!(entries.len(), 1);
    }
}
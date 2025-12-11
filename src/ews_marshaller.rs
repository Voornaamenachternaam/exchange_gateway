use anyhow::{Result, anyhow};
use quick_xml::Reader;
use quick_xml::events::Event as QEvent;
use chrono::{Utc, DateTime};
use uuid::Uuid;

/// Convert EWS CalendarItem XML -> ICS string.
/// This implementation parses a minimal set of EWS CalendarItem fields:
/// Subject, Location, Body, Start, End (if present) and then builds an ICS string.
/// It uses quick-xml stable decode helper to get text content.
pub fn ews_calendaritem_to_ics(xml: &str) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    // NOTE: some quick-xml versions expose trim_text; if your installed quick-xml version
    // does not have it, this line may be removed. It is optional behavior.
    // We avoid depending on it for correctness.
    // reader.trim_text(true);

    let mut buf = Vec::new();

    let mut cur_elem: Option<String> = None;
    let mut subject: Option<String> = None;
    let mut location: Option<String> = None;
    let mut description: Option<String> = None;
    let mut dtstart: Option<String> = None;
    let mut dtend: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(QEvent::Start(e)) => {
                if let Ok(name) = std::str::from_utf8(e.local_name().as_ref()) {
                    cur_elem = Some(name.to_lowercase());
                }
            }
            Ok(QEvent::Text(t)) => {
                // Use stable helper to decode/unescape text into String
                match t.unescape_and_decode(&reader) {
                    Ok(txt) => {
                        if let Some(ref el) = cur_elem {
                            match el.as_str() {
                                "t:subject" | "subject" => subject = Some(txt),
                                "t:location" | "location" => location = Some(txt),
                                "t:body" | "body" => description = Some(txt),
                                "t:start" | "start" => dtstart = Some(txt),
                                "t:end" | "end" => dtend = Some(txt),
                                _ => {}
                            }
                        }
                    }
                    Err(_) => {
                        // ignore text parsing errors for best-effort
                    }
                }
            }
            Ok(QEvent::End(_)) => {
                cur_elem = None;
            }
            Ok(QEvent::Eof) => break,
            Err(e) => return Err(anyhow!("XML parse error: {}", e)),
            _ => {}
        }
        buf.clear();
    }

    // Build ICS manually (RFC 5545 minimal)
    let start_dt: DateTime<Utc> = if let Some(s) = dtstart {
        match DateTime::parse_from_rfc3339(&s) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(_) => Utc::now(),
        }
    } else {
        Utc::now()
    };

    let end_dt: DateTime<Utc> = if let Some(s) = dtend {
        match DateTime::parse_from_rfc3339(&s) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(_) => start_dt + chrono::Duration::hours(1),
        }
    } else {
        start_dt + chrono::Duration::hours(1)
    };

    let uid = Uuid::new_v4().to_string();
    let summary = subject.as_deref().unwrap_or("Event");
    let descr = description.as_deref().unwrap_or("");
    let loc = location.as_deref().unwrap_or("");

    let ics = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//ExchangeGateway//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nSUMMARY:{summary}\r\nDESCRIPTION:{descr}\r\nLOCATION:{loc}\r\nDTSTAMP:{dtstamp}\r\nDTSTART:{dtstart}\r\nDTEND:{dtend}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        uid = uid,
        summary = escape_ics(summary),
        descr = escape_ics(descr),
        loc = escape_ics(loc),
        dtstamp = Utc::now().format("%Y%m%dT%H%M%SZ"),
        dtstart = start_dt.format("%Y%m%dT%H%M%SZ"),
        dtend = end_dt.format("%Y%m%dT%H%M%SZ"),
    );

    Ok(ics)
}

fn escape_ics(s: &str) -> String {
    s.replace("\\", "\\\\")
     .replace("\n", "\\n")
     .replace(",", "\\,")
     .replace(";", "\\;")
}

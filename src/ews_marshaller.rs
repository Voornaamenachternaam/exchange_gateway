use anyhow::{Result, anyhow};
use quick_xml::Reader;
use quick_xml::events::Event as QEvent;
use chrono::{Utc, DateTime};
use uuid::Uuid;

/// Convert EWS CalendarItem XML -> ICS string.
/// This implementation parses a minimal set of EWS CalendarItem fields:
/// Subject, Location, Body, Start, End (if present) and then builds an ICS string.
/// It uses reader.read_text(...) to obtain decoded element text in a robust way.
pub fn ews_calendaritem_to_ics(xml: &str) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    // Do not call reader.trim_text(true) here to avoid API variance across quick-xml versions.
    let mut buf = Vec::new();

    let mut subject: Option<String> = None;
    let mut location: Option<String> = None;
    let mut description: Option<String> = None;
    let mut dtstart: Option<String> = None;
    let mut dtend: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(QEvent::Start(e)) => {
                // Use read_text to consume the element's text (decodes/unescapes) in one call.
                let name = std::str::from_utf8(e.local_name().as_ref()).unwrap_or("").to_lowercase();
                match reader.read_text(e.local_name(), &mut Vec::new()) {
                    Ok(txt) => {
                        match name.as_str() {
                            "t:subject" | "subject" => subject = Some(txt),
                            "t:location" | "location" => location = Some(txt),
                            "t:body" | "body" => description = Some(txt),
                            "t:start" | "start" => dtstart = Some(txt),
                            "t:end" | "end" => dtend = Some(txt),
                            _ => {}
                        }
                    }
                    Err(_) => {
                        // If we cannot read the inner text, continue gracefully.
                    }
                }
            }
            Ok(QEvent::End(_)) => {}
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

use anyhow::{Result, anyhow};
use quick_xml::Reader;
use quick_xml::events::Event as QEvent;
use chrono::{Utc, DateTime};
use uuid::Uuid;

/// Convert EWS CalendarItem XML -> ICS string.
/// This implementation parses a minimal set of EWS CalendarItem fields:
/// Subject, Location, Body, Start, End (if present) and then builds an ICS string.
///
/// Implementation notes:
/// - Uses `reader.read_text(e.name())` to obtain decoded element text (returns Cow<'_, str>).
/// - Converts the Cow to an owned String with `into_owned()` before assigning to Option<String>.
pub fn ews_calendaritem_to_ics(xml: &str) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();

    let mut subject: Option<String> = None;
    let mut location: Option<String> = None;
    let mut description: Option<String> = None;
    let mut dtstart: Option<String> = None;
    let mut dtend: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(QEvent::Start(e)) => {
                // e.name() returns the QName needed by read_text in quick-xml 0.38.x
                match reader.read_text(e.name()) {
                    Ok(txt_cow) => {
                        let txt: String = txt_cow.into_owned();
                        if let Ok(name_str) = std::str::from_utf8(e.local_name().as_ref()) {
                            match name_str.to_lowercase().as_str() {
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
                        // non-fatal: skip element text if it cannot be read/decoded
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

use anyhow::Result;
use icalendar::{Calendar, Event};
use icalendar::Component;
use icalendar::EventLike;
use quick_xml::Reader;
use quick_xml::events::Event as QEvent;
use std::io::Cursor;
use chrono::Utc;
use chrono::{DateTime, FixedOffset};
use uuid::Uuid;

/// Convert ICS -> minimal EWS CalendarItem XML snippet (string).
/// We produce a minimal CalendarItem XML that includes ItemId if provided.
pub fn ics_to_ews_calendaritem(ics: &str, item_id: &str, change_key: &str) -> Result<String> {
    // For simplicity produce a minimal CalendarItem XML using string formatting.
    // This function is a helper for wrapping ICS content into an EWS response shape.
    let subject = "Calendar event";
    let body = ics;
    let xml = format!(
        r#"<t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <t:ItemId Id="{id}" ChangeKey="{ck}"/>
  <t:Subject>{sub}</t:Subject>
  <t:Body>{body}</t:Body>
</t:CalendarItem>"#,
        id = item_id, ck = change_key, sub = xml_escape(subject), body = xml_escape(body)
    );
    Ok(xml)
}

/// Convert EWS CalendarItem XML -> ICS string.
/// The parser extracts Subject, Location, Body, Start, End (if present) and builds an ICS.
pub fn ews_calendaritem_to_ics(xml: &str) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    reader.trim_text(true);
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
                if let Ok(txt) = t.unescape() {
                    if let Some(ref el) = cur_elem {
                        match el.as_str() {
                            "t:subject" | "subject" => subject = Some(txt.to_string()),
                            "t:location" | "location" => location = Some(txt.to_string()),
                            "t:body" | "body" => description = Some(txt.to_string()),
                            "t:start" | "start" => dtstart = Some(txt.to_string()),
                            "t:end" | "end" => dtend = Some(txt.to_string()),
                            _ => {}
                        }
                    }
                }
            }
            Ok(QEvent::End(_)) => {
                cur_elem = None;
            }
            Ok(QEvent::Eof) => break,
            Err(e) => return Err(anyhow::anyhow!("XML parse error: {}", e)),
            _ => {}
        }
        buf.clear();
    }

    // Build ical event
    let mut cal = Calendar::new();
    let mut ev = Event::new();

    if let Some(s) = subject { ev.summary(&s); }
    if let Some(d) = description { ev.description(&d); }
    if let Some(l) = location { ev.location(&l); }

    // Parse datetimes if present (try rfc3339)
    let start_dt: DateTime<Utc> = if let Some(s) = dtstart {
        match DateTime::parse_from_rfc3339(&s) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(_) => {
                // try parse as naive local RFC format fallback (best-effort)
                Utc::now()
            }
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

    ev.starts(start_dt);
    ev.ends(end_dt);
    ev.uid(&Uuid::new_v4().to_string());
    cal.add_event(ev);

    Ok(cal.to_string())
}

fn xml_escape(s: &str) -> String {
    s.replace("&", "&amp;").replace("<","&lt;").replace(">","&gt;")
}

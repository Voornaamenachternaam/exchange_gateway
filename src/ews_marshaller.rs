// src/ews_marshaller.rs

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use quick_xml::Reader;
use quick_xml::events::Event as QEvent;
use uuid::Uuid;

/// Convert a minimal subset of EWS CalendarItem XML into an ICS string.
/// Supports Subject, Location, Body, Start, End. (No attendees/recurrence in this stub.)
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
                if let Ok(txt_cow) = reader.read_text(e.name()) {
                    let txt: String = txt_cow.into_owned();
                    if let Ok(name_str) = std::str::from_utf8(e.local_name().as_ref()) {
                        match name_str {
                            "t:Subject" | "Subject" => subject = Some(txt),
                            "t:Location" | "Location" => location = Some(txt),
                            "t:Body" | "Body" => description = Some(txt),
                            "t:Start" | "Start" => dtstart = Some(txt),
                            "t:End" | "End" => dtend = Some(txt),
                            _ => {}
                        }
                    }
                }
            }
            Ok(QEvent::Eof) => break,
            Err(e) => return Err(anyhow!("XML parse error: {}", e)),
            _ => {}
        }
        buf.clear();
    }

    // Build ICS (RFC5545) with minimal fields.
    let start_dt: DateTime<Utc> = if let Some(s) = dtstart {
        match DateTime::parse_from_rfc3339(&s) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(_) => Utc::now(),
        }
    } else { Utc::now() };

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

    // Note: no timezone handling, output Zulu times.
    let ics = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//ExchangeGateway//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:{stamp}\r\nDTSTART:{dtstart}\r\nDTEND:{dtend}\r\nSUMMARY:{summary}\r\nDESCRIPTION:{descr}\r\nLOCATION:{loc}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        uid = uid,
        stamp = Utc::now().format("%Y%m%dT%H%M%SZ"),
        dtstart = start_dt.format("%Y%m%dT%H%M%SZ"),
        dtend = end_dt.format("%Y%m%dT%H%M%SZ"),
        summary = summary,
        descr = descr,
        loc = loc,
    );
    Ok(ics)
}

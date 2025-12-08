use anyhow::Result;
use icalendar::{Calendar, Event};
use quick_xml::Writer;
use quick_xml::events::{Event as QEvent, BytesStart, BytesText, BytesEnd};
use std::io::Cursor;
use chrono::Utc;

/// Convert ICS -> EWS CalendarItem XML snippet
pub fn ics_to_ews_calendaritem(ics: &str, item_id: &str, change_key: &str) -> Result<String> {
    let cal = icalendar::parse_calendar(ics)?;
    let comp = cal.components.get(0).ok_or_else(|| anyhow::anyhow!("no VEVENT"))?;
    let mut writer = Writer::new(Cursor::new(Vec::new()));

    writer.write_event(QEvent::Start(BytesStart::borrowed_name(b"t:CalendarItem")))?;
    let mut itemid = BytesStart::borrowed_name(b"t:ItemId");
    itemid.push_attribute(("Id", item_id));
    itemid.push_attribute(("ChangeKey", change_key));
    writer.write_event(QEvent::Empty(itemid.to_borrowed()))?;

    if let Some(p) = comp.get_property("SUMMARY") {
        writer.write_event(QEvent::Start(BytesStart::borrowed_name(b"t:Subject")))?;
        writer.write_event(QEvent::Text(BytesText::from_plain_str(&p.value)))?;
        writer.write_event(QEvent::End(BytesEnd::borrowed(b"t:Subject")))?;
    }
    if let Some(p) = comp.get_property("LOCATION") {
        writer.write_event(QEvent::Start(BytesStart::borrowed_name(b"t:Location")))?;
        writer.write_event(QEvent::Text(BytesText::from_plain_str(&p.value)))?;
        writer.write_event(QEvent::End(BytesEnd::borrowed(b"t:Location")))?;
    }
    if let Some(p) = comp.get_property("DESCRIPTION") {
        writer.write_event(QEvent::Start(BytesStart::borrowed_name(b"t:Body")))?;
        writer.write_event(QEvent::Text(BytesText::from_plain_str(&p.value)))?;
        writer.write_event(QEvent::End(BytesEnd::borrowed(b"t:Body")))?;
    }
    if let Some(p) = comp.get_property("DTSTART") {
        writer.write_event(QEvent::Start(BytesStart::borrowed_name(b"t:Start")))?;
        writer.write_event(QEvent::Text(BytesText::from_plain_str(&p.value)))?;
        writer.write_event(QEvent::End(BytesEnd::borrowed(b"t:Start")))?;
    }
    if let Some(p) = comp.get_property("DTEND") {
        writer.write_event(QEvent::Start(BytesStart::borrowed_name(b"t:End")))?;
        writer.write_event(QEvent::Text(BytesText::from_plain_str(&p.value)))?;
        writer.write_event(QEvent::End(BytesEnd::borrowed(b"t:End")))?;
    }

    writer.write_event(QEvent::End(BytesEnd::borrowed(b"t:CalendarItem")))?;
    let vec = writer.into_inner().into_inner();
    Ok(String::from_utf8(vec)?)
}

/// Convert EWS CalendarItem XML -> ICS
pub fn ews_calendaritem_to_ics(xml: &str) -> Result<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.trim_text(true);
    let mut buf = Vec::new();
    let mut subject = None;
    let mut location = None;
    let mut description = None;
    let mut dtstart = None;
    let mut dtend = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) => {
                let name = std::str::from_utf8(e.local_name().as_ref()).unwrap_or("").to_lowercase();
                if name.ends_with("subject") { if let Ok(q) = reader.read_text(e.local_name().as_ref(), &mut Vec::new()) { subject = Some(q); } }
                if name.ends_with("location") { if let Ok(q) = reader.read_text(e.local_name().as_ref(), &mut Vec::new()) { location = Some(q); } }
                if name.ends_with("body") { if let Ok(q) = reader.read_text(e.local_name().as_ref(), &mut Vec::new()) { description = Some(q); } }
                if name.ends_with("start") { if let Ok(q) = reader.read_text(e.local_name().as_ref(), &mut Vec::new()) { dtstart = Some(q); } }
                if name.ends_with("end") { if let Ok(q) = reader.read_text(e.local_name().as_ref(), &mut Vec::new()) { dtend = Some(q); } }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }

    let mut cal = Calendar::new();
    let mut ev = Event::new();

    if let Some(s) = subject { ev.summary(&s); }
    if let Some(d) = description { ev.description(&d); }
    if let Some(l) = location { ev.location(&l); }

    let start_dt = if let Some(s) = dtstart {
        chrono::DateTime::parse_from_rfc3339(&s).map(|d| d.with_timezone(&chrono::Utc)).unwrap_or(chrono::Utc::now())
    } else { chrono::Utc::now() };
    let end_dt = if let Some(s) = dtend {
        chrono::DateTime::parse_from_rfc3339(&s).map(|d| d.with_timezone(&chrono::Utc)).unwrap_or(start_dt + chrono::Duration::hours(1))
    } else { start_dt + chrono::Duration::hours(1) };

    ev.starts(start_dt);
    ev.ends(end_dt);
    ev.uid(&uuid::Uuid::new_v4().to_string());
    cal.add_event(ev);
    Ok(cal.to_string())
}

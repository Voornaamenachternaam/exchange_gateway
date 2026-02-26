// src/wbxml.rs
use std::collections::HashMap;
use std::sync::LazyLock;
use quick_xml::{events::Event, Reader, Writer};
use quick_xml::events::{BytesEnd, BytesStart, BytesText};
use base64::{Engine as _, engine::general_purpose};

const SWITCH_PAGE: u8 = 0x00;
const END: u8 = 0x01;
const STR_I: u8 = 0x03;
const STR_T: u8 = 0x83;
const OPAQUE: u8 = 0xC3;

const PAGE_AIRSYNC: u8 = 0x00;
const PAGE_CALENDAR: u8 = 0x04;
const PAGE_BASE: u8 = 0x06;
const PAGE_SETTINGS: u8 = 0x07;
const PAGE_PING: u8 = 0x09;
const PAGE_PROVISION: u8 = 0x0E;

static TAG_MAP: LazyLock<HashMap<(u8, u8), (&'static str, &'static str)>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    m.insert((0, 0x05), ("AirSync", "Sync"));
    m.insert((0, 0x07), ("AirSync", "Collections"));
    m.insert((0, 0x08), ("AirSync", "Collection"));
    m.insert((0, 0x09), ("AirSync", "ApplicationData"));
    m.insert((0, 0x14), ("AirSync", "Commands"));
    m.insert((0, 0x17), ("AirSync", "Add"));
    m.insert((0, 0x18), ("AirSync", "Change"));
    m.insert((0, 0x19), ("AirSync", "Delete"));
    m.insert((0, 0x1C), ("AirSync", "SyncKey"));
    m.insert((0, 0x1D), ("AirSync", "CollectionId"));
    m.insert((0, 0x22), ("AirSync", "ServerId"));
    m.insert((0, 0x24), ("AirSync", "Status"));
    
    m.insert((4, 0x01), ("Calendar", "AllDayEvent"));
    m.insert((4, 0x06), ("Calendar", "Location"));
    m.insert((4, 0x0A), ("Calendar", "Subject"));
    m.insert((4, 0x0C), ("Calendar", "Start"));
    m.insert((4, 0x0D), ("Calendar", "End"));
    m.insert((4, 0x14), ("Calendar", "UID"));
    
    m.insert((6, 0x00), ("AirSyncBase", "Body"));
    m.insert((6, 0x01), ("AirSyncBase", "Data"));
    
    m.insert((7, 0x00), ("Settings", "Settings"));
    
    m.insert((9, 0x00), ("Ping", "Ping"));
    m.insert((9, 0x01), ("Ping", "Status"));
    
    m.insert((14, 0x00), ("Provision", "Provision"));
    m.insert((14, 0x04), ("Provision", "PolicyKey"));
    m.insert((14, 0x06), ("Provision", "Status"));
    m
});

static REV_TAG_MAP: LazyLock<HashMap<(&'static str, &'static str), (u8, u8)>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    for ((p, t), v) in TAG_MAP.iter() {
        m.insert(*v, (*p, *t));
    }
    m
});

pub fn decode(input: &[u8]) -> Result<String, anyhow::Error> {
    if input.len() < 6 { return Err(anyhow::anyhow!("Input too short")); }
    
    let mut output = String::new();
    let mut pos = 6;
    let mut current_page = 0u8;
    let mut tag_stack: Vec<String> = Vec::new();
    
    output.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>");

    while pos < input.len() {
        let token = input[pos];
        pos += 1;

        if token == SWITCH_PAGE {
            if pos >= input.len() { break; }
            current_page = input[pos];
            pos += 1;
            continue;
        }
        
        if token == END {
            if let Some(tag) = tag_stack.pop() {
                output.push_str(&format!("</{}>", tag));
            }
            continue;
        }
        
        if token == STR_I {
            let start = pos;
            while pos < input.len() && input[pos] != 0 { pos += 1; }
            let content = String::from_utf8_lossy(&input[start..pos]).into_owned();
            output.push_str(&quick_xml::escape::escape(&content));
            pos += 1;
            continue;
        }
        
        if token == OPAQUE {
            let (len, bytes_read) = read_multibyte_int(&input[pos..])?;
            pos += bytes_read;
            let data = &input[pos..pos+len];
            
            if let Ok(s) = std::str::from_utf8(data) {
                output.push_str(&quick_xml::escape::escape(s));
            } else {
                output.push_str(&general_purpose::STANDARD.encode(data));
            }
            pos += len;
            continue;
        }

        let has_content = (token & 0x40) != 0;
        let tag_id = token & 0x3F;

        if let Some((ns, name)) = TAG_MAP.get(&(current_page, tag_id)) {
            let full_name = if ns.is_empty() { name.to_string() } else { format!("{}:{}", ns, name) };
            if has_content {
                output.push_str(&format!("<{}>", full_name));
                tag_stack.push(full_name);
            } else {
                output.push_str(&format!("<{}/>", full_name));
            }
        } else {
            let full_name = format!("Unknown_{}:{}", current_page, tag_id);
            if has_content {
                output.push_str(&format!("<{}>", full_name));
                tag_stack.push(full_name);
            }
        }
    }
    Ok(output)
}

pub fn encode(xml: &str) -> Result<Vec<u8>, anyhow::Error> {
    let mut output = vec![0x03, 0x01, 0x6A, 0x00, 0x00, 0x00]; 
    let mut reader = Reader::from_str(xml);
    let mut current_page = 0xff;
    let mut buf = Vec::new();
    
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let local = e.local_name();
                let local_str = std::str::from_utf8(local.as_ref()).unwrap_or("");
                let parts: Vec<&str> = local_str.split(':').collect();
                let (ns, tag_name) = if parts.len() == 2 { (parts[0], parts[1]) } else { ("", parts[0]) };

                let page_id = match ns {
                    "AirSync" => PAGE_AIRSYNC,
                    "Calendar" => PAGE_CALENDAR,
                    "AirSyncBase" => PAGE_BASE,
                    "Settings" => PAGE_SETTINGS,
                    "Ping" => PAGE_PING,
                    "Provision" => PAGE_PROVISION,
                    _ => PAGE_AIRSYNC,
                };

                if page_id != current_page {
                    output.push(SWITCH_PAGE);
                    output.push(page_id);
                    current_page = page_id;
                }

                let tag_id = if let Some((_, id)) = REV_TAG_MAP.get(&(ns, tag_name)) { *id } else { 0xFF };
                let is_empty = matches!(reader.read_event_into(&mut buf), Ok(Event::Empty(_)));
                let byte = if is_empty { tag_id } else { tag_id | 0x40 };
                output.push(byte);
            }
            Ok(Event::End(_)) => {
                output.push(END);
            }
            Ok(Event::Text(t)) => {
                output.push(STR_I);
                output.extend_from_slice(t.as_ref());
                output.push(0x00);
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
    }
    Ok(output)
}

fn read_multibyte_int(buf: &[u8]) -> Result<(usize, usize), anyhow::Error> {
    let mut result = 0;
    let mut count = 0;
    loop {
        if count >= buf.len() { return Err(anyhow::anyhow!("Unexpected end of input")); }
        let byte = buf[count];
        count += 1;
        result = (result << 7) | ((byte & 0x7F) as usize);
        if byte & 0x80 == 0 { break; }
    }
    Ok((result, count))
}

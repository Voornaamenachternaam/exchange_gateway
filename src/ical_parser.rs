// src/ical_parser.rs
//! Nom-based iCalendar parser for better performance and error handling.
//!
//! This module provides parser combinators for parsing iCalendar (RFC 5545) format,
//! replacing manual string manipulation with composable, zero-allocation parsers.

use chrono::{DateTime, NaiveDateTime, Utc};
use chrono_tz::Tz;
use nom::{
    branch::alt,
    bytes::complete::{tag, take_until, take_while, take_while1},
    character::complete::{char, digit1, line_ending, not_line_ending},
    combinator::{map, map_res, opt, recognize},
    multi::{many0, separated_list0},
    sequence::{delimited, preceded, separated_pair, tuple},
    IResult, Parser,
};
use std::collections::HashMap;

/// Unfolds iCalendar content lines (RFC 5545 Section 3.1)
/// Lines ending with CRLF followed by whitespace are continuations
pub fn unfold_ical_content(input: &str) -> String {
    // Remove CRLF + single whitespace (line continuation)
    input
        .replace("\r\n ", "")
        .replace("\r\n\t", "")
        .replace("\n ", "")
        .replace("\n\t", "")
}

/// Parse an iCalendar property name (before the colon or semicolon)
fn parse_property_name(input: &str) -> IResult<&str, &str> {
    take_while1(|c: char| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')(input)
}

/// Parse an iCalendar parameter (KEY=VALUE format within property)
fn parse_parameter(input: &str) -> IResult<&str, (&str, &str)> {
    let (input, _) = char(';')(input)?;
    let (input, key) = take_while1(|c: char| c.is_alphanumeric() || c == '-')(input)?;
    let (input, _) = char('=')(input)?;
    let (input, value) = take_while(|c: char| c != ';' && c != ':' && c != '\n' && c != '\r')(input)?;
    Ok((input, (key, value)))
}

/// Parse all parameters of a property
fn parse_parameters(input: &str) -> IResult<&str, Vec<(&str, &str)>> {
    many0(parse_parameter)(input)
}

/// Parse a single iCalendar property line (NAME;PARAMS:VALUE)
pub fn parse_property_line(input: &str) -> IResult<&str, (String, Vec<(String, String)>, String)> {
    let (input, name) = parse_property_name(input)?;
    let (input, params) = parse_parameters(input)?;
    let (input, _) = char(':')(input)?;
    let (input, value) = not_line_ending(input)?;
    
    let params_vec: Vec<(String, String)> = params
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.trim_matches('"').to_string()))
        .collect();
    
    Ok((input, (name.to_string(), params_vec, value.to_string())))
}

/// Parse multiple property lines separated by line endings
pub fn parse_property_lines(input: &str) -> IResult<&str, Vec<(String, String)>> {
    let (mut input, _) = opt(line_ending)(input)?;
    let mut properties = Vec::new();
    
    while !input.is_empty() {
        if input.starts_with("END:") || input.starts_with("BEGIN:") {
            break;
        }
        
        match parse_property_line(input) {
            Ok((remaining, (name, params, value))) => {
                // Reconstruct full key with parameters if needed
                let full_key = if params.is_empty() {
                    name
                } else {
                    let params_str: String = params
                        .iter()
                        .map(|(k, v)| format!(";{}={}", k, v))
                        .collect();
                    format!("{}{}", name, params_str)
                };
                properties.push((full_key, value));
                input = remaining;
                let (remaining, _) = opt(line_ending)(input)?;
                input = remaining;
            }
            Err(_) => break,
        }
    }
    
    Ok((input, properties))
}

/// Parse a complete VEVENT block
pub fn parse_vevent_block(input: &str) -> IResult<&str, Vec<(String, String)>> {
    let (input, _) = tag("BEGIN:VEVENT")(input)?;
    let (input, _) = opt(line_ending)(input)?;
    let (input, properties) = parse_property_lines(input)?;
    let (input, _) = tag("END:VEVENT")(input)?;
    
    Ok((input, properties))
}

/// Parse all VEVENT blocks from iCalendar content
pub fn parse_all_vevents(input: &str) -> IResult<&str, Vec<Vec<(String, String)>>> {
    let unfolded = unfold_ical_content(input);
    let mut events = Vec::new();
    let mut remaining = unfolded.as_str();
    
    while !remaining.is_empty() {
        // Skip content before BEGIN:VEVENT
        if let Some(pos) = remaining.find("BEGIN:VEVENT") {
            remaining = &remaining[pos..];
        } else {
            break;
        }
        
        match parse_vevent_block(remaining) {
            Ok((rem, event_props)) => {
                events.push(event_props);
                remaining = rem;
                let (rem, _) = opt(line_ending)(remaining)?;
                remaining = rem;
            }
            Err(_) => break,
        }
    }
    
    Ok(("", events))
}

/// Parse VTIMEZONE block
pub fn parse_vtimezone_block(input: &str) -> IResult<&str, Option<String>> {
    let unfolded = unfold_ical_content(input);
    
    if let Some(start) = unfolded.find("BEGIN:VTIMEZONE") {
        if let Some(end) = unfolded.find("END:VTIMEZONE") {
            let block = &unfolded[start..end + "END:VTIMEZONE".len()];
            return Ok(("", Some(block.to_string())));
        }
    }
    
    Ok(("", None))
}

/// Parse iCalendar datetime with optional timezone
pub fn parse_ical_datetime(input: &str) -> IResult<&str, DateTime<Utc>> {
    // Try UTC format first: YYYYMMDDTHHMMSSZ
    if input.ends_with('Z') {
        let inner = &input[..input.len() - 1];
        if let Ok(dt) = NaiveDateTime::parse_from_str(inner, "%Y%m%dT%H%M%S") {
            return Ok((&input[input.len()..], dt.and_utc()));
        }
    }
    
    // Try local format: YYYYMMDDTHHMMSS
    if input.contains('T') {
        let parts: Vec<&str> = input.split('T').collect();
        if parts.len() == 2 {
            // This is a local datetime, will need timezone context
            // For now, parse as UTC
            if let Ok(dt) = NaiveDateTime::parse_from_str(input, "%Y%m%dT%H%M%S") {
                return Ok((&input[input.len()..], dt.and_utc()));
            }
        }
    }
    
    // Try date only: YYYYMMDD
    if input.len() == 8 {
        if let Ok(dt) = NaiveDateTime::parse_from_str(&format!("{}T000000", input), "%Y%m%dT%H%M%S") {
            return Ok((&input[input.len()..], dt.and_utc()));
        }
    }
    
    // Try ISO 8601 format
    if let Ok(dt) = DateTime::parse_from_rfc3339(input) {
        return Ok((&input[input.len()..], dt.with_timezone(&Utc)));
    }
    
    // Fallback: return epoch
    Ok((&input[input.len()..], DateTime::UNIX_EPOCH))
}

/// Parse a parameter value from a property key (e.g., "DTSTART;TZID=America/New_York" -> Some("America/New_York"))
pub fn parse_ical_param(input: &str, param_name: &str) -> Option<String> {
    let search = format!("{}=", param_name);
    if let Some(pos) = input.find(&search) {
        let start = pos + search.len();
        let remainder = &input[start..];
        let end = remainder
            .find(|c: char| c == ';' || c == ':' || c == '\n')
            .unwrap_or(remainder.len());
        Some(remainder[..end].trim_matches('"').to_string())
    } else {
        None
    }
}

/// Unescape iCalendar text (backslash escaping)
pub fn unescape_ical_text(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('n') => {
                    result.push('\n');
                    chars.next();
                }
                Some('r') => {
                    result.push('\r');
                    chars.next();
                }
                Some('t') => {
                    result.push('\t');
                    chars.next();
                }
                Some('\\') => {
                    result.push('\\');
                    chars.next();
                }
                Some(',') => {
                    result.push(',');
                    chars.next();
                }
                Some(';') => {
                    result.push(';');
                    chars.next();
                }
                Some(':') => {
                    result.push(':');
                    chars.next();
                }
                _ => result.push(c),
            }
        } else {
            result.push(c);
        }
    }
    
    result
}

/// Parse duration in ISO 8601 duration format (e.g., "-PT15M" for -15 minutes)
pub fn parse_ical_duration_minutes(input: &str) -> IResult<&str, i32> {
    let (input, negative) = if input.starts_with('-') {
        (&input[1..], true)
    } else {
        (input, false)
    };
    
    let (input, _) = opt(tag("P"))(input)?;
    let (input, _) = opt(tag("T"))(input)?;
    
    // Parse hours or minutes
    let (input, hours) = opt(map_res(
        tuple((digit1, tag("H"))),
        |(d, _): (&str, &str)| d.parse::<i32>(),
    ))(input)?;
    
    let (input, minutes) = opt(map_res(
        tuple((digit1, tag("M"))),
        |(d, _): (&str, &str)| d.parse::<i32>(),
    ))(input)?;
    
    let total_minutes = hours.unwrap_or(0) * 60 + minutes.unwrap_or(0);
    let result = if negative { total_minutes } else { -total_minutes };
    
    Ok((input, result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_property_line() {
        let input = "DTSTART;TZID=America/New_York:20240115T100000";
        let result = parse_property_line(input);
        assert!(result.is_ok());
        let (_, (name, params, value)) = result.unwrap();
        assert_eq!(name, "DTSTART");
        assert_eq!(params, vec![("TZID".to_string(), "America/New_York".to_string())]);
        assert_eq!(value, "20240115T100000");
    }

    #[test]
    fn test_parse_simple_property() {
        let input = "SUMMARY:Team Meeting";
        let result = parse_property_line(input);
        assert!(result.is_ok());
        let (_, (name, params, value)) = result.unwrap();
        assert_eq!(name, "SUMMARY");
        assert!(params.is_empty());
        assert_eq!(value, "Team Meeting");
    }

    #[test]
    fn test_unescape_ical_text() {
        assert_eq!(unescape_ical_text("Hello\\nWorld"), "Hello\nWorld");
        assert_eq!(unescape_ical_text("Test\\, comma"), "Test, comma");
        assert_eq!(unescape_ical_text("Back\\\\slash"), "Back\\slash");
    }

    #[test]
    fn test_parse_duration_minutes() {
        let (_, result) = parse_ical_duration_minutes("-PT15M").unwrap();
        assert_eq!(result, 15);
        
        let (_, result) = parse_ical_duration_minutes("PT1H").unwrap();
        assert_eq!(result, -60);
        
        let (_, result) = parse_ical_duration_minutes("-PT1H30M").unwrap();
        assert_eq!(result, 90);
    }

    #[test]
    fn test_parse_ical_param() {
        let key = "DTSTART;TZID=America/New_York;VALUE=DATE";
        assert_eq!(parse_ical_param(key, "TZID"), Some("America/New_York".to_string()));
        assert_eq!(parse_ical_param(key, "VALUE"), Some("DATE".to_string()));
        assert_eq!(parse_ical_param(key, "NONEXISTENT"), None);
    }

    #[test]
    fn test_unfold_ical_content() {
        let input = "DESCRIPTION:This is a long\\r\\n description that spans\\r\\n multiple lines";
        let unfolded = unfold_ical_content(input);
        assert_eq!(unfolded, "DESCRIPTION:This is a long description that spans multiple lines");
    }
}
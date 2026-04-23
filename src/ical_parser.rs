// src/ical_parser.rs
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};

pub fn unfold_ical_content(input: &str) -> String {
    icalendar::parser::unfold(input)
}

pub type PropertyLine = (String, Vec<(String, String)>, String);

pub fn parse_property_line(input: &str) -> Result<PropertyLine, nom::Err<nom::error::Error<&str>>> {
    let colon_pos = find_value_colon(input).ok_or_else(|| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Tag))
    })?;
    let before_colon = &input[..colon_pos];
    let value = &input[colon_pos + 1..];

    let semicolon_pos = before_colon.find(';');
    let (name, params_str) = match semicolon_pos {
        Some(pos) => (&before_colon[..pos], &before_colon[pos + 1..]),
        None => (before_colon, ""),
    };

    let params: Vec<(String, String)> = if params_str.is_empty() {
        Vec::new()
    } else {
        parse_parameters(params_str)
    };

    Ok((name.to_string(), params, value.to_string()))
}

fn find_value_colon(input: &str) -> Option<usize> {
    let mut in_quotes = false;
    let mut escape_next = false;

    for (i, c) in input.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        match c {
            '\\' => escape_next = true,
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => return Some(i),
            _ => {}
        }
    }
    None
}

fn parse_parameters(params_str: &str) -> Vec<(String, String)> {
    let mut params = Vec::new();
    let mut current_param = String::new();
    let mut current_value = String::new();
    let mut in_quotes = false;
    let mut escape_next = false;
    let mut parsing_value = false;
    let chars = params_str.chars();

    for c in chars {
        if escape_next {
            if parsing_value {
                current_value.push(c);
            } else {
                current_param.push(c);
            }
            escape_next = false;
            continue;
        }

        match c {
            '\\' => {
                if parsing_value {
                    current_value.push(c);
                } else {
                    current_param.push(c);
                }
                escape_next = true;
            }
            '"' => {
                in_quotes = !in_quotes;
                if parsing_value {
                    current_value.push(c);
                }
            }
            '=' if !in_quotes && !parsing_value => {
                parsing_value = true;
            }
            ';' if !in_quotes => {
                if !current_param.is_empty() {
                    params.push((
                        current_param.clone(),
                        current_value.trim_matches('"').to_string(),
                    ));
                }
                current_param.clear();
                current_value.clear();
                parsing_value = false;
            }
            _ => {
                if parsing_value {
                    current_value.push(c);
                } else {
                    current_param.push(c);
                }
            }
        }
    }

    if !current_param.is_empty() {
        params.push((current_param, current_value.trim_matches('"').to_string()));
    }

    params
}

pub fn parse_property_lines(
    input: &str,
) -> Result<Vec<(String, String)>, nom::Err<nom::error::Error<&str>>> {
    let mut properties = Vec::new();
    let mut remaining = input;

    while !remaining.is_empty() {
        let line_end = remaining
            .find("\r\n")
            .or_else(|| remaining.find('\n'))
            .unwrap_or(remaining.len());
        let line = remaining[..line_end].trim();

        if line.is_empty() {
            remaining = if line_end < remaining.len() {
                &remaining[line_end + 1..]
            } else {
                ""
            };
            continue;
        }

        if line.starts_with("END:") || line.starts_with("BEGIN:") {
            break;
        }

        if line.contains(':') {
            match parse_property_line(line) {
                Ok((name, params, value)) => {
                    let full_key = if params.is_empty() {
                        name
                    } else {
                        format!(
                            "{};{}",
                            name,
                            params
                                .iter()
                                .map(|(k, v)| if v.is_empty() {
                                    k.clone()
                                } else {
                                    format!("{}={}", k, v)
                                })
                                .collect::<Vec<_>>()
                                .join(";")
                        )
                    };
                    properties.push((full_key, value));
                }
                Err(_) => break,
            }
        }

        remaining = if line_end < remaining.len() {
            &remaining[line_end + 1..]
        } else {
            ""
        };
    }

    Ok(properties)
}

pub fn parse_vevent_block(
    input: &str,
) -> Result<Vec<(String, String)>, nom::Err<nom::error::Error<&str>>> {
    let start = input.find("BEGIN:VEVENT").ok_or_else(|| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Tag))
    })?;
    let rest = &input[start + 12..];
    let end = rest.find("END:VEVENT").ok_or_else(|| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Tag))
    })?;
    let content = &rest[..end];

    let content = content.trim_start_matches('\r').trim_start_matches('\n');
    parse_property_lines(content)
}

pub type VeventProps = Vec<(String, String)>;

pub type NomError<'i> = nom::Err<nom::error::Error<&'i str>>;

pub fn parse_all_vevents(input: &str) -> Result<Vec<VeventProps>, NomError<'_>> {
    let unfolded = unfold_ical_content(input);
    let mut events = Vec::new();
    let mut remaining = unfolded.as_str();

    while let Some(start) = remaining.find("BEGIN:VEVENT") {
        remaining = &remaining[start..];

        match parse_vevent_block(remaining) {
            Ok(props) => {
                events.push(props);
                if let Some(end) = remaining.find("END:VEVENT") {
                    remaining = &remaining[end + 11..];
                } else {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    Ok(events)
}

pub fn parse_vtimezone_block(
    input: &str,
) -> Result<Option<String>, nom::Err<nom::error::Error<&str>>> {
    let unfolded = unfold_ical_content(input);

    if let (Some(start), Some(end)) = (
        unfolded.find("BEGIN:VTIMEZONE"),
        unfolded.find("END:VTIMEZONE"),
    ) {
        let block = &unfolded[start..end + "END:VTIMEZONE".len()];
        return Ok(Some(block.to_string()));
    }

    Ok(None)
}

pub fn parse_ical_datetime(
    input: &str,
) -> Result<DateTime<Utc>, nom::Err<nom::error::Error<&str>>> {
    let input = input.trim();

    if let Some(inner) = input.strip_suffix('Z') {
        if let Ok(dt) = NaiveDateTime::parse_from_str(inner, "%Y%m%dT%H%M%S") {
            return Ok(dt.and_utc());
        }
        if let Ok(dt) = NaiveDateTime::parse_from_str(inner, "%Y-%m-%dT%H:%M:%S") {
            return Ok(dt.and_utc());
        }
    }

    if input.contains('T') {
        if let Ok(dt) = NaiveDateTime::parse_from_str(input, "%Y%m%dT%H%M%S") {
            return Ok(dt.and_utc());
        }
        if let Ok(dt) = DateTime::parse_from_rfc3339(input) {
            return Ok(dt.with_timezone(&Utc));
        }
    }

    if input.len() == 8
        && input.chars().all(|c| c.is_ascii_digit())
        && let Ok(date) = NaiveDate::parse_from_str(input, "%Y%m%d")
        && let Some(dt) = date.and_hms_opt(0, 0, 0)
    {
        return Ok(dt.and_utc());
    }

    Err(nom::Err::Error(nom::error::Error::new(
        input,
        nom::error::ErrorKind::Verify,
    )))
}

pub fn parse_ical_param(input: &str, param_name: &str) -> Option<String> {
    let search = format!("{}=", param_name);
    if let Some(pos) = input.find(&search) {
        let start = pos + search.len();
        let remainder = &input[start..];
        let mut in_quotes = false;
        let end = remainder
            .char_indices()
            .find(|&(_, c)| {
                if c == '"' {
                    in_quotes = !in_quotes;
                    false
                } else {
                    !in_quotes && (c == ';' || c == ':' || c == '\n')
                }
            })
            .map(|(idx, _)| idx)
            .unwrap_or(remainder.len());
        Some(remainder[..end].trim_matches('"').to_string())
    } else {
        None
    }
}
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

pub fn parse_ical_duration_minutes(input: &str) -> Result<i32, nom::Err<nom::error::Error<&str>>> {
    let (negative, input) = if let Some(stripped) = input.strip_prefix('-') {
        (true, stripped)
    } else {
        (false, input)
    };

    let input = input.strip_prefix('P').unwrap_or(input);
    let input = input.strip_prefix('T').unwrap_or(input);

    let mut total_minutes: i32 = 0;
    let mut remaining = input;

    while !remaining.is_empty() {
        let digit_end = remaining
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(remaining.len());

        if digit_end == 0 {
            break;
        }

        let value: i32 = remaining[..digit_end].parse().unwrap_or(0);
        let unit = remaining.chars().nth(digit_end).unwrap_or('M');

        match unit {
            'H' => total_minutes += value * 60,
            'M' => total_minutes += value,
            'S' => {}
            _ => break,
        }

        remaining = &remaining[digit_end + 1..];
    }

    Ok(if negative {
        -total_minutes
    } else {
        total_minutes
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unescape_ical_text() {
        assert_eq!(unescape_ical_text("Hello\\nWorld"), "Hello\nWorld");
        assert_eq!(unescape_ical_text("Test\\, comma"), "Test, comma");
        assert_eq!(unescape_ical_text("Back\\\\slash"), "Back\\slash");
    }

    #[test]
    fn test_parse_ical_param() {
        let key = "DTSTART;TZID=America/New_York;VALUE=DATE";
        assert_eq!(
            parse_ical_param(key, "TZID"),
            Some("America/New_York".to_string())
        );
        assert_eq!(parse_ical_param(key, "VALUE"), Some("DATE".to_string()));
        assert_eq!(parse_ical_param(key, "NONEXISTENT"), None);
    }

    #[test]
    fn test_parse_ical_duration_minutes() {
        assert_eq!(parse_ical_duration_minutes("-PT15M").unwrap(), -15);
        assert_eq!(parse_ical_duration_minutes("PT1H").unwrap(), 60);
        assert_eq!(parse_ical_duration_minutes("-PT1H30M").unwrap(), -90);
    }

    #[test]
    fn test_unfold_ical_content() {
        let input = "DESCRIPTION:This is a long\r\n description that spans\r\n multiple lines";
        let unfolded = unfold_ical_content(input);
        assert_eq!(
            unfolded,
            "DESCRIPTION:This is a longdescription that spansmultiple lines"
        );
    }
}

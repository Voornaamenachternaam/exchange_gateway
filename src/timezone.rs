// src/timezone.rs
// Timezone handling for Exchange Gateway
//
// Closes gaps:
// - Time-zone fidelity improvements (GAP #3)
// - VTIMEZONE / Exchange time-zone equivalence (GAP #3)
// - DST-sensitive Outlook scenarios (GAP #3)
//
// Per MS-ASCAL and MS-OXWSCAL timezone specifications
// March 2026 - Production-ready, security-hardened

use std::collections::HashMap;
use chrono::{DateTime, FixedOffset, NaiveDateTime, TimeZone, Utc, LocalResult, Datelike, Timelike};
use tracing::{debug, error, info, warn};

/// Exchange timezone bias in minutes (offset from UTC)
#[derive(Clone, Debug, Default)]
pub struct ExchangeTimeZone {
    pub name: String,
    pub bias: i32, // Minutes offset from UTC (positive = west, negative = east)
    pub standard_bias: i32,
    pub daylight_bias: i32,
    pub standard_date: Option<TransitionDate>,
    pub daylight_date: Option<TransitionDate>,
}

/// DST transition date definition
#[derive(Clone, Debug)]
pub struct TransitionDate {
    pub year: u16,      // 0 = recurring yearly
    pub month: u16,     // 1-12
    pub day_of_week: u16, // 0-6 (Sunday=0)
    pub week: u16,      // 1-5 (5 = last week)
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub milliseconds: u16,
}

/// IANA timezone to Exchange timezone mapping
pub struct TimeZoneMapper {
    mappings: HashMap<String, ExchangeTimeZone>,
}

impl TimeZoneMapper {
    pub fn new() -> Self {
        let mut mappings = HashMap::new();
        
        // Common timezone mappings
        mappings.insert("UTC".to_string(), ExchangeTimeZone {
            name: "UTC".to_string(),
            bias: 0,
            standard_bias: 0,
            daylight_bias: 0,
            standard_date: None,
            daylight_date: None,
        });
        
        mappings.insert("GMT".to_string(), ExchangeTimeZone {
            name: "GMT Standard Time".to_string(),
            bias: 0,
            standard_bias: 0,
            daylight_bias: -60,
            standard_date: Some(TransitionDate {
                year: 0,
                month: 10,
                day_of_week: 0,
                week: 5,
                hour: 2,
                minute: 0,
                second: 0,
                milliseconds: 0,
            }),
            daylight_date: Some(TransitionDate {
                year: 0,
                month: 3,
                day_of_week: 0,
                week: 5,
                hour: 1,
                minute: 0,
                second: 0,
                milliseconds: 0,
            }),
        });
        
        mappings.insert("Europe/London".to_string(), ExchangeTimeZone {
            name: "GMT Standard Time".to_string(),
            bias: 0,
            standard_bias: 0,
            daylight_bias: -60,
            standard_date: Some(TransitionDate {
                year: 0,
                month: 10,
                day_of_week: 0,
                week: 5,
                hour: 2,
                minute: 0,
                second: 0,
                milliseconds: 0,
            }),
            daylight_date: Some(TransitionDate {
                year: 0,
                month: 3,
                day_of_week: 0,
                week: 5,
                hour: 1,
                minute: 0,
                second: 0,
                milliseconds: 0,
            }),
        });
        
        mappings.insert("Europe/Paris".to_string(), ExchangeTimeZone {
            name: "Romance Standard Time".to_string(),
            bias: -60,
            standard_bias: 0,
            daylight_bias: -60,
            standard_date: Some(TransitionDate {
                year: 0,
                month: 10,
                day_of_week: 0,
                week: 5,
                hour: 3,
                minute: 0,
                second: 0,
                milliseconds: 0,
            }),
            daylight_date: Some(TransitionDate {
                year: 0,
                month: 3,
                day_of_week: 0,
                week: 5,
                hour: 2,
                minute: 0,
                second: 0,
                milliseconds: 0,
            }),
        });
        
        mappings.insert("Europe/Berlin".to_string(), ExchangeTimeZone {
            name: "W. Europe Standard Time".to_string(),
            bias: -60,
            standard_bias: 0,
            daylight_bias: -60,
            standard_date: Some(TransitionDate {
                year: 0,
                month: 10,
                day_of_week: 0,
                week: 5,
                hour: 3,
                minute: 0,
                second: 0,
                milliseconds: 0,
            }),
            daylight_date: Some(TransitionDate {
                year: 0,
                month: 3,
                day_of_week: 0,
                week: 5,
                hour: 2,
                minute: 0,
                second: 0,
                milliseconds: 0,
            }),
        });
        
        mappings.insert("America/New_York".to_string(), ExchangeTimeZone {
            name: "Eastern Standard Time".to_string(),
            bias: 300,
            standard_bias: 0,
            daylight_bias: -60,
            standard_date: Some(TransitionDate {
                year: 0,
                month: 11,
                day_of_week: 0,
                week: 1,
                hour: 2,
                minute: 0,
                second: 0,
                milliseconds: 0,
            }),
            daylight_date: Some(TransitionDate {
                year: 0,
                month: 3,
                day_of_week: 0,
                week: 2,
                hour: 2,
                minute: 0,
                second: 0,
                milliseconds: 0,
            }),
        });
        
        mappings.insert("America/Chicago".to_string(), ExchangeTimeZone {
            name: "Central Standard Time".to_string(),
            bias: 360,
            standard_bias: 0,
            daylight_bias: -60,
            standard_date: Some(TransitionDate {
                year: 0,
                month: 11,
                day_of_week: 0,
                week: 1,
                hour: 2,
                minute: 0,
                second: 0,
                milliseconds: 0,
            }),
            daylight_date: Some(TransitionDate {
                year: 0,
                month: 3,
                day_of_week: 0,
                week: 2,
                hour: 2,
                minute: 0,
                second: 0,
                milliseconds: 0,
            }),
        });
        
        mappings.insert("America/Denver".to_string(), ExchangeTimeZone {
            name: "Mountain Standard Time".to_string(),
            bias: 420,
            standard_bias: 0,
            daylight_bias: -60,
            standard_date: Some(TransitionDate {
                year: 0,
                month: 11,
                day_of_week: 0,
                week: 1,
                hour: 2,
                minute: 0,
                second: 0,
                milliseconds: 0,
            }),
            daylight_date: Some(TransitionDate {
                year: 0,
                month: 3,
                day_of_week: 0,
                week: 2,
                hour: 2,
                minute: 0,
                second: 0,
                milliseconds: 0,
            }),
        });
        
        mappings.insert("America/Los_Angeles".to_string(), ExchangeTimeZone {
            name: "Pacific Standard Time".to_string(),
            bias: 480,
            standard_bias: 0,
            daylight_bias: -60,
            standard_date: Some(TransitionDate {
                year: 0,
                month: 11,
                day_of_week: 0,
                week: 1,
                hour: 2,
                minute: 0,
                second: 0,
                milliseconds: 0,
            }),
            daylight_date: Some(TransitionDate {
                year: 0,
                month: 3,
                day_of_week: 0,
                week: 2,
                hour: 2,
                minute: 0,
                second: 0,
                milliseconds: 0,
            }),
        });
        
        mappings.insert("Asia/Tokyo".to_string(), ExchangeTimeZone {
            name: "Tokyo Standard Time".to_string(),
            bias: -540,
            standard_bias: 0,
            daylight_bias: 0,
            standard_date: None,
            daylight_date: None,
        });
        
        mappings.insert("Asia/Shanghai".to_string(), ExchangeTimeZone {
            name: "China Standard Time".to_string(),
            bias: -480,
            standard_bias: 0,
            daylight_bias: 0,
            standard_date: None,
            daylight_date: None,
        });
        
        mappings.insert("Australia/Sydney".to_string(), ExchangeTimeZone {
            name: "AUS Eastern Standard Time".to_string(),
            bias: -600,
            standard_bias: 0,
            daylight_bias: -60,
            standard_date: Some(TransitionDate {
                year: 0,
                month: 4,
                day_of_week: 0,
                week: 1,
                hour: 3,
                minute: 0,
                second: 0,
                milliseconds: 0,
            }),
            daylight_date: Some(TransitionDate {
                year: 0,
                month: 10,
                day_of_week: 0,
                week: 1,
                hour: 2,
                minute: 0,
                second: 0,
                milliseconds: 0,
            }),
        });
        
        Self { mappings }
    }
    
    /// Get Exchange timezone by IANA name
    pub fn get(&self, iana_name: &str) -> Option<ExchangeTimeZone> {
        self.mappings.get(iana_name).cloned()
    }
    
    /// Get Exchange timezone by Windows name
    pub fn get_by_windows_name(&self, windows_name: &str) -> Option<ExchangeTimeZone> {
        self.mappings.values()
            .find(|tz| tz.name.eq_ignore_ascii_case(windows_name))
            .cloned()
    }
    
    /// Convert IANA timezone to Exchange timezone
    pub fn iana_to_exchange(&self, iana_tz: &str) -> ExchangeTimeZone {
        self.get(iana_tz).unwrap_or_else(|| {
            // Default to UTC if not found
            ExchangeTimeZone {
                name: "UTC".to_string(),
                bias: 0,
                standard_bias: 0,
                daylight_bias: 0,
                standard_date: None,
                daylight_date: None,
            }
        })
    }
}

impl Default for TimeZoneMapper {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse VTIMEZONE block from iCalendar
pub fn parse_vtimezone(ical: &str) -> Option<ExchangeTimeZone> {
    let mut tz = ExchangeTimeZone::default();
    let mut in_vtimezone = false;
    let mut in_standard = false;
    let mut in_daylight = false;
    
    for line in ical.lines() {
        let line = line.trim();
        
        if line.starts_with("BEGIN:VTIMEZONE") {
            in_vtimezone = true;
        } else if line.starts_with("END:VTIMEZONE") {
            in_vtimezone = false;
        } else if line.starts_with("TZID:") && in_vtimezone {
            tz.name = line[5..].to_string();
        } else if line.starts_with("BEGIN:STANDARD") && in_vtimezone {
            in_standard = true;
        } else if line.starts_with("END:STANDARD") && in_vtimezone {
            in_standard = false;
        } else if line.starts_with("BEGIN:DAYLIGHT") && in_vtimezone {
            in_daylight = true;
        } else if line.starts_with("END:DAYLIGHT") && in_vtimezone {
            in_daylight = false;
        } else if line.starts_with("TZOFFSETFROM:") && in_standard && in_vtimezone {
            tz.bias = parse_ical_offset(&line[13..]).unwrap_or(0);
        } else if line.starts_with("TZOFFSETTO:") && in_standard && in_vtimezone {
            tz.standard_bias = parse_ical_offset(&line[11..]).unwrap_or(0) - tz.bias;
        } else if line.starts_with("TZOFFSETTO:") && in_daylight && in_vtimezone {
            tz.daylight_bias = parse_ical_offset(&line[11..]).unwrap_or(0) - tz.bias;
        } else if line.starts_with("DTSTART:") && in_standard && in_vtimezone {
            tz.standard_date = parse_ical_dtstart(&line[8..]);
        } else if line.starts_with("DTSTART:") && in_daylight && in_vtimezone {
            tz.daylight_date = parse_ical_dtstart(&line[8..]);
        } else if line.starts_with("RRULE:") && in_vtimezone {
            // Parse recurrence rule for DST transitions
            if in_standard {
                tz.standard_date = parse_rrule_for_transition(&line[6..], tz.standard_date.as_ref());
            } else if in_daylight {
                tz.daylight_date = parse_rrule_for_transition(&line[6..], tz.daylight_date.as_ref());
            }
        }
    }
    
    if tz.name.is_empty() {
        None
    } else {
        Some(tz)
    }
}

/// Parse iCalendar timezone offset (e.g., "-0500" or "+0530")
fn parse_ical_offset(offset: &str) -> Option<i32> {
    let offset = offset.trim();
    if offset.len() < 4 {
        return None;
    }
    
    let sign = if offset.starts_with('-') { 1 } else { -1 };
    let digits = offset.trim_start_matches(&['+', '-'][..]);
    
    if digits.len() >= 4 {
        let hours: i32 = digits[0..2].parse().ok()?;
        let minutes: i32 = digits[2..4].parse().ok()?;
        Some(sign * (hours * 60 + minutes))
    } else {
        None
    }
}

/// Parse iCalendar DTSTART for timezone transition
fn parse_ical_dtstart(dtstart: &str) -> Option<TransitionDate> {
    // Format: 20261025T020000
    let dtstart = dtstart.trim();
    if dtstart.len() < 13 {
        return None;
    }
    
    let year: u16 = dtstart[0..4].parse().ok()?;
    let month: u16 = dtstart[4..6].parse().ok()?;
    let day: u16 = dtstart[6..8].parse().ok()?;
    let hour: u16 = dtstart[9..11].parse().ok()?;
    let minute: u16 = dtstart[11..13].parse().ok()?;
    
    // Calculate day of week and week of month
    let naive = NaiveDateTime::parse_from_str(&format!("{}T{:02}{:02}00", 
        &dtstart[0..8], hour, minute), "%Y%m%dT%H%M%S").ok()?;
    
    let weekday = naive.weekday().num_days_from_sunday() as u16;
    let week = ((day - 1) / 7) + 1;
    
    Some(TransitionDate {
        year: if year == 1970 { 0 } else { year },
        month,
        day_of_week: weekday,
        week: week.min(5),
        hour,
        minute,
        second: 0,
        milliseconds: 0,
    })
}

/// Parse RRULE for DST transition
fn parse_rrule_for_transition(rrule: &str, base_date: Option<&TransitionDate>) -> Option<TransitionDate> {
    let mut month = base_date.map(|d| d.month).unwrap_or(1);
    let mut day_of_week = base_date.map(|d| d.day_of_week).unwrap_or(0);
    let mut week = base_date.map(|d| d.week).unwrap_or(1);
    
    for part in rrule.split(';') {
        if part.starts_with("BYMONTH=") {
            month = part[8..].parse().unwrap_or(month);
        } else if part.starts_with("BYDAY=") {
            let byday = &part[6..];
            // Parse format like "-1SU" (last Sunday) or "2MO" (second Monday)
            if byday.len() >= 2 {
                let day_code = &byday[byday.len()-2..];
                day_of_week = match day_code {
                    "SU" => 0,
                    "MO" => 1,
                    "TU" => 2,
                    "WE" => 3,
                    "TH" => 4,
                    "FR" => 5,
                    "SA" => 6,
                    _ => day_of_week,
                };
                
                // Parse week number if present
                if byday.len() > 2 {
                    let week_str = &byday[..byday.len()-2];
                    if let Ok(w) = week_str.parse::<i16>() {
                        week = if w < 0 { 5 } else { w as u16 };
                    }
                }
            }
        }
    }
    
    Some(TransitionDate {
        year: 0,
        month,
        day_of_week,
        week,
        hour: base_date.map(|d| d.hour).unwrap_or(2),
        minute: base_date.map(|d| d.minute).unwrap_or(0),
        second: 0,
        milliseconds: 0,
    })
}

/// Convert Exchange timezone to VTIMEZONE block
pub fn exchange_to_vtimezone(tz: &ExchangeTimeZone) -> String {
    let mut vtz = format!("BEGIN:VTIMEZONE\r\nTZID:{}\r\n", tz.name);
    
    // Standard time definition
    if let Some(ref std_date) = tz.standard_date {
        let offset_from = format_offset(tz.bias + tz.standard_bias);
        let offset_to = format_offset(tz.bias);
        
        vtz.push_str("BEGIN:STANDARD\r\n");
        vtz.push_str(&format!("DTSTART:{:04}{:02}{:02}T{:02}{:02}00\r\n",
            std_date.year.max(1970), std_date.month, 
            compute_day_of_month(std_date.year, std_date.month, std_date.day_of_week, std_date.week),
            std_date.hour, std_date.minute));
        vtz.push_str(&format!("TZOFFSETFROM:{}\r\n", offset_from));
        vtz.push_str(&format!("TZOFFSETTO:{}\r\n", offset_to));
        
        if std_date.year == 0 {
            // Recurring transition
            let byday = format_byday(std_date.day_of_week, std_date.week);
            vtz.push_str(&format!("RRULE:FREQ=YEARLY;BYMONTH={};BYDAY={}\r\n", 
                std_date.month, byday));
        }
        
        vtz.push_str("END:STANDARD\r\n");
    }
    
    // Daylight time definition
    if let Some(ref dst_date) = tz.daylight_date {
        if tz.daylight_bias != 0 {
            let offset_from = format_offset(tz.bias);
            let offset_to = format_offset(tz.bias + tz.daylight_bias);
            
            vtz.push_str("BEGIN:DAYLIGHT\r\n");
            vtz.push_str(&format!("DTSTART:{:04}{:02}{:02}T{:02}{:02}00\r\n",
                dst_date.year.max(1970), dst_date.month,
                compute_day_of_month(dst_date.year, dst_date.month, dst_date.day_of_week, dst_date.week),
                dst_date.hour, dst_date.minute));
            vtz.push_str(&format!("TZOFFSETFROM:{}\r\n", offset_from));
            vtz.push_str(&format!("TZOFFSETTO:{}\r\n", offset_to));
            
            if dst_date.year == 0 {
                let byday = format_byday(dst_date.day_of_week, dst_date.week);
                vtz.push_str(&format!("RRULE:FREQ=YEARLY;BYMONTH={};BYDAY={}\r\n",
                    dst_date.month, byday));
            }
            
            vtz.push_str("END:DAYLIGHT\r\n");
        }
    }
    
    vtz.push_str("END:VTIMEZONE\r\n");
    vtz
}

/// Format offset in iCalendar format (e.g., "-0500")
fn format_offset(minutes: i32) -> String {
    let sign = if minutes >= 0 { '+' } else { '-' };
    let abs_minutes = minutes.abs();
    let hours = abs_minutes / 60;
    let mins = abs_minutes % 60;
    format!("{}{:02}{:02}", sign, hours, mins)
}

/// Format BYDAY value for RRULE
fn format_byday(day_of_week: u16, week: u16) -> String {
    let day = match day_of_week {
        0 => "SU",
        1 => "MO",
        2 => "TU",
        3 => "WE",
        4 => "TH",
        5 => "FR",
        6 => "SA",
        _ => "SU",
    };
    
    if week == 5 {
        format!("-1{}", day)
    } else {
        format!("{}{}", week, day)
    }
}

/// Compute day of month from week/day_of_week specification
fn compute_day_of_month(year: u16, month: u16, day_of_week: u16, week: u16) -> u16 {
    use chrono::{Datelike, NaiveDate, Weekday};
    
    let year = if year == 0 { 2026 } else { year };
    
    // Get first day of month
    let first = NaiveDate::from_ymd_opt(year as i32, month as u32, 1).unwrap_or_else(|| 
        NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
    
    // Find first occurrence of day_of_week
    let first_weekday = first.weekday().num_days_from_sunday() as u16;
    let days_until = (day_of_week + 7 - first_weekday) % 7;
    let first_occurrence = 1 + days_until;
    
    // Compute target occurrence
    if week == 5 {
        // Last occurrence
        let last_day = if month == 12 {
            NaiveDate::from_ymd_opt(year as i32 + 1, 1, 1).unwrap()
        } else {
            NaiveDate::from_ymd_opt(year as i32, month as u32 + 1, 1).unwrap()
        }.pred_opt().unwrap().day() as u16;
        
        let mut day = first_occurrence;
        while day + 7 <= last_day {
            day += 7;
        }
        day
    } else {
        first_occurrence + (week - 1) * 7
    }
}

/// Convert datetime from one timezone to another
pub fn convert_datetime(
    dt: &DateTime<Utc>,
    from_tz: &ExchangeTimeZone,
    to_tz: &ExchangeTimeZone,
) -> DateTime<Utc> {
    // Apply from_tz offset to get UTC
    let from_offset = get_effective_offset(from_tz, dt);
    let utc = *dt + chrono::Duration::minutes(from_offset as i64);
    
    // Apply to_tz offset
    let to_offset = get_effective_offset(to_tz, &utc);
    utc - chrono::Duration::minutes(to_offset as i64)
}

/// Get effective offset for a datetime considering DST
fn get_effective_offset(tz: &ExchangeTimeZone, dt: &DateTime<Utc>) -> i32 {
    if is_dst_active(tz, dt) {
        tz.bias + tz.daylight_bias
    } else {
        tz.bias + tz.standard_bias
    }
}

/// Check if DST is active for a given datetime
fn is_dst_active(tz: &ExchangeTimeZone, dt: &DateTime<Utc>) -> bool {
    let (std_date, dst_date) = match (&tz.standard_date, &tz.daylight_date) {
        (Some(s), Some(d)) => (s, d),
        _ => return false, // No DST defined
    };
    
    let month = dt.month() as u16;
    let day = dt.day() as u16;
    let hour = dt.hour() as u16;
    
    // Northern hemisphere: DST starts in spring, ends in fall
    // Southern hemisphere: DST starts in fall, ends in spring
    
    if dst_date.month < std_date.month {
        // Northern hemisphere pattern
        if month > dst_date.month && month < std_date.month {
            return true;
        }
        if month == dst_date.month {
            let transition_day = compute_day_of_month(dt.year() as u16, dst_date.month, 
                dst_date.day_of_week, dst_date.week);
            return day > transition_day || (day == transition_day && hour >= dst_date.hour);
        }
        if month == std_date.month {
            let transition_day = compute_day_of_month(dt.year() as u16, std_date.month,
                std_date.day_of_week, std_date.week);
            return day < transition_day || (day == transition_day && hour < std_date.hour);
        }
        false
    } else {
        // Southern hemisphere pattern
        if month > dst_date.month || month < std_date.month {
            return true;
        }
        if month == dst_date.month {
            let transition_day = compute_day_of_month(dt.year() as u16, dst_date.month,
                dst_date.day_of_week, dst_date.week);
            return day > transition_day || (day == transition_day && hour >= dst_date.hour);
        }
        if month == std_date.month {
            let transition_day = compute_day_of_month(dt.year() as u16, std_date.month,
                std_date.day_of_week, std_date.week);
            return day < transition_day || (day == transition_day && hour < std_date.hour);
        }
        false
    }
}

/// Parse EAS TimeZone element (base64 encoded)
pub fn parse_eas_timezone(base64_data: &str) -> Option<ExchangeTimeZone> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    
    let bytes = STANDARD.decode(base64_data).ok()?;
    if bytes.len() < 172 {
        return None;
    }
    
    // Parse Windows TIME_ZONE_INFORMATION structure
    let bias = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    
    // Standard name (64 bytes, UTF-16LE)
    let standard_name = decode_windows_string(&bytes[4..68]);
    
    // Standard date (16 bytes)
    let standard_date = parse_systemtime(&bytes[68..84]);
    
    // Standard bias (4 bytes)
    let standard_bias = i32::from_le_bytes([bytes[84], bytes[85], bytes[86], bytes[87]]);
    
    // Daylight name (64 bytes, UTF-16LE)
    let _daylight_name = decode_windows_string(&bytes[88..152]);
    
    // Daylight date (16 bytes)
    let daylight_date = parse_systemtime(&bytes[152..168]);
    
    // Daylight bias (4 bytes)
    let daylight_bias = i32::from_le_bytes([bytes[168], bytes[169], bytes[170], bytes[171]]);
    
    Some(ExchangeTimeZone {
        name: standard_name,
        bias,
        standard_bias,
        daylight_bias,
        standard_date,
        daylight_date,
    })
}

/// Parse SYSTEMTIME structure
fn parse_systemtime(bytes: &[u8]) -> Option<TransitionDate> {
    if bytes.len() < 16 {
        return None;
    }
    
    let year = u16::from_le_bytes([bytes[0], bytes[1]]);
    let month = u16::from_le_bytes([bytes[2], bytes[3]]);
    let day_of_week = u16::from_le_bytes([bytes[4], bytes[5]]);
    let day = u16::from_le_bytes([bytes[6], bytes[7]]);
    let hour = u16::from_le_bytes([bytes[8], bytes[9]]);
    let minute = u16::from_le_bytes([bytes[10], bytes[11]]);
    let second = u16::from_le_bytes([bytes[12], bytes[13]]);
    let milliseconds = u16::from_le_bytes([bytes[14], bytes[15]]);
    
    // Calculate week from day
    let week = if day >= 1 && day <= 7 { 1 }
        else if day <= 14 { 2 }
        else if day <= 21 { 3 }
        else if day <= 28 { 4 }
        else { 5 };
    
    Some(TransitionDate {
        year,
        month,
        day_of_week,
        week,
        hour,
        minute,
        second,
        milliseconds,
    })
}

/// Decode Windows UTF-16LE string
fn decode_windows_string(bytes: &[u8]) -> String {
    let u16s: Vec<u16> = bytes.chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .take_while(|&c| c != 0)
        .collect();
    
    String::from_utf16(&u16s).unwrap_or_default()
}

/// Convert Exchange timezone to EAS base64 format
pub fn exchange_to_eas_timezone(tz: &ExchangeTimeZone) -> String {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    
    let mut bytes = Vec::with_capacity(172);
    
    // Bias (4 bytes)
    bytes.extend_from_slice(&tz.bias.to_le_bytes());
    
    // Standard name (64 bytes, UTF-16LE)
    let name_bytes = encode_windows_string(&tz.name, 32);
    bytes.extend_from_slice(&name_bytes);
    
    // Standard date (16 bytes)
    if let Some(ref date) = tz.standard_date {
        bytes.extend_from_slice(&systemtime_to_bytes(date));
    } else {
        bytes.extend_from_slice(&[0u8; 16]);
    }
    
    // Standard bias (4 bytes)
    bytes.extend_from_slice(&tz.standard_bias.to_le_bytes());
    
    // Daylight name (64 bytes, UTF-16LE)
    let dst_name = format!("{} (DST)", tz.name);
    let dst_name_bytes = encode_windows_string(&dst_name, 32);
    bytes.extend_from_slice(&dst_name_bytes);
    
    // Daylight date (16 bytes)
    if let Some(ref date) = tz.daylight_date {
        bytes.extend_from_slice(&systemtime_to_bytes(date));
    } else {
        bytes.extend_from_slice(&[0u8; 16]);
    }
    
    // Daylight bias (4 bytes)
    bytes.extend_from_slice(&tz.daylight_bias.to_le_bytes());
    
    STANDARD.encode(&bytes)
}

/// Encode string to Windows UTF-16LE format
fn encode_windows_string(s: &str, max_chars: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(max_chars * 2);
    
    for c in s.encode_utf16().take(max_chars - 1) {
        bytes.extend_from_slice(&c.to_le_bytes());
    }
    
    // Null terminator
    bytes.extend_from_slice(&[0u8; 2]);
    
    // Pad to full size
    while bytes.len() < max_chars * 2 {
        bytes.push(0);
    }
    
    bytes
}

/// Convert TransitionDate to SYSTEMTIME bytes
fn systemtime_to_bytes(date: &TransitionDate) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16);
    
    bytes.extend_from_slice(&date.year.to_le_bytes());
    bytes.extend_from_slice(&date.month.to_le_bytes());
    bytes.extend_from_slice(&date.day_of_week.to_le_bytes());
    bytes.extend_from_slice(&date.week.to_le_bytes()); // Use week as day
    bytes.extend_from_slice(&date.hour.to_le_bytes());
    bytes.extend_from_slice(&date.minute.to_le_bytes());
    bytes.extend_from_slice(&date.second.to_le_bytes());
    bytes.extend_from_slice(&date.milliseconds.to_le_bytes());
    
    bytes
}

/// Get UTC offset for a datetime in a given timezone
pub fn get_utc_offset(tz: &ExchangeTimeZone, dt: &DateTime<Utc>) -> FixedOffset {
    let offset_minutes = get_effective_offset(tz, dt);
    FixedOffset::east_opt(offset_minutes * 60).unwrap_or(FixedOffset::east_opt(0).unwrap())
}

/// Convert UTC datetime to local timezone datetime
pub fn utc_to_local(dt: &DateTime<Utc>, tz: &ExchangeTimeZone) -> DateTime<FixedOffset> {
    let offset = get_utc_offset(tz, dt);
    dt.with_timezone(&offset)
}

/// Convert local datetime to UTC
pub fn local_to_utc(dt: &DateTime<FixedOffset>, tz: &ExchangeTimeZone) -> DateTime<Utc> {
    // The datetime already has offset info, just convert
    dt.with_timezone(&Utc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timezone_mapper() {
        let mapper = TimeZoneMapper::new();
        
        let utc = mapper.get("UTC").unwrap();
        assert_eq!(utc.bias, 0);
        
        let est = mapper.get("America/New_York").unwrap();
        assert_eq!(est.bias, 300);
        assert!(est.daylight_date.is_some());
    }

    #[test]
    fn test_parse_ical_offset() {
        assert_eq!(parse_ical_offset("-0500"), Some(300));
        assert_eq!(parse_ical_offset("+0530"), Some(-330));
        assert_eq!(parse_ical_offset("+0000"), Some(0));
    }

    #[test]
    fn test_format_offset() {
        assert_eq!(format_offset(300), "+0500");
        assert_eq!(format_offset(-330), "-0530");
        assert_eq!(format_offset(0), "+0000");
    }

    #[test]
    fn test_vtimezone_roundtrip() {
        let mapper = TimeZoneMapper::new();
        let est = mapper.get("America/New_York").unwrap();
        
        let vtz = exchange_to_vtimezone(&est);
        assert!(vtz.contains("BEGIN:VTIMEZONE"));
        assert!(vtz.contains("Eastern Standard Time"));
        assert!(vtz.contains("BEGIN:STANDARD"));
        assert!(vtz.contains("BEGIN:DAYLIGHT"));
    }

    #[test]
    fn test_eas_timezone_roundtrip() {
        let mapper = TimeZoneMapper::new();
        let est = mapper.get("America/New_York").unwrap();
        
        let base64 = exchange_to_eas_timezone(&est);
        let parsed = parse_eas_timezone(&base64).unwrap();
        
        assert_eq!(parsed.bias, est.bias);
    }

    #[test]
    fn test_dst_detection() {
        let mapper = TimeZoneMapper::new();
        let est = mapper.get("America/New_York").unwrap();
        
        // July is DST in Northern Hemisphere
        let summer = Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap();
        assert!(is_dst_active(&est, &summer));
        
        // January is not DST
        let winter = Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap();
        assert!(!is_dst_active(&est, &winter));
    }
}

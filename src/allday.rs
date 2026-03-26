pub mod allday;
// All-Day Event Handling for Exchange Gateway
//
// Closes gaps:
// - All-day / recurrence / timezone edge-case behavior (GAP #3)
// - All-day event semantics per MS-ASCAL
//
// Per MS-ASCAL all-day event specifications
// March 2026 - Production-ready, security-hardened

use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use tracing::{debug, error, warn};

/// All-day event handling
#[derive(Clone, Debug)]
pub struct AllDayEvent {
    /// The date of the all-day event (no time component)
    pub date: NaiveDate,
    /// Duration in days
    pub duration_days: i64,
    /// Original timezone
    pub timezone: Option<String>,
}

impl AllDayEvent {
    /// Create new all-day event
    pub fn new(date: NaiveDate) -> Self {
        Self {
            date,
            duration_days: 1,
            timezone: None,
        }
    }
    
    /// Create multi-day all-day event
    pub fn new_multi_day(start: NaiveDate, end: NaiveDate) -> Self {
        let duration = (end - start).num_days() + 1;
        Self {
            date: start,
            duration_days: duration,
            timezone: None,
        }
    }
    
    /// Get start time in UTC (midnight at start of day)
    pub fn start_utc(&self) -> DateTime<Utc> {
        Utc.from_utc_datetime(&self.date.and_hms_opt(0, 0, 0).unwrap())
    }
    
    /// Get end time in UTC (midnight at end of last day)
    pub fn end_utc(&self) -> DateTime<Utc> {
        let end_date = self.date + Duration::days(self.duration_days);
        Utc.from_utc_datetime(&end_date.and_hms_opt(0, 0, 0).unwrap())
    }
    
    /// Convert to iCalendar format
    pub fn to_ical(&self, uid: &str, summary: &str) -> String {
        let dtstart = self.date.format("%Y%m%d").to_string();
        let end_date = self.date + Duration::days(self.duration_days);
        let dtend = end_date.format("%Y%m%d").to_string();
        
        let sanitize = |s: &str| s.replace("\r", "").replace("\n", "");
        
        format!(
            "BEGIN:VEVENT\r\n\
             UID:{}\r\n\
             SUMMARY:{}\r\n\
             DTSTART;VALUE=DATE:{}\r\n\
             DTEND;VALUE=DATE:{}\r\n\
             TRANSP:TRANSPARENT\r\n\
             END:VEVENT\r\n",
            sanitize(uid), sanitize(summary), dtstart, dtend
        )
    }
    
    /// Check if a datetime falls within this all-day event
    pub fn contains(&self, dt: &DateTime<Utc>) -> bool {
        let dt_date = dt.date_naive();
        dt_date >= self.date && dt_date < self.date + Duration::days(self.duration_days)
    }
    
    /// Get the day of week (0 = Sunday, 6 = Saturday)
    pub fn day_of_week(&self) -> u32 {
        self.date.weekday().num_days_from_sunday()
    }
}

/// All-day event parser for iCalendar
pub struct AllDayParser;

impl AllDayParser {
    /// Parse all-day event from iCalendar DTSTART/DTEND
    pub fn parse_from_ical(dtstart: &str, dtend: Option<&str>) -> Option<AllDayEvent> {
        // Check if it's a DATE value (all-day)
        if dtstart.contains("VALUE=DATE") || dtstart.len() == 8 {
            // Extract date
            let date_str = if let Some(pos) = dtstart.find(':') {
                &dtstart[pos + 1..]
            } else {
                dtstart
            };
            
            let date = Self::parse_date(date_str)?;
            
            // Get duration
            let duration = if let Some(end) = dtend {
                let end_str = if let Some(pos) = end.find(':') {
                    &end[pos + 1..]
                } else {
                    end
                };
                let end_date = Self::parse_date(end_str)?;
                (end_date - date).num_days()
            } else {
                1
            };
            
            return Some(AllDayEvent {
                date,
                duration_days: duration,
                timezone: None,
            });
        }
        
        None
    }
    
    /// Parse date string (YYYYMMDD)
    fn parse_date(s: &str) -> Option<NaiveDate> {
        let s = s.trim();
        if s.len() < 8 {
            return None;
        }
        
        let year: i32 = s[0..4].parse().ok()?;
        let month: u32 = s[4..6].parse().ok()?;
        let day: u32 = s[6..8].parse().ok()?;
        
        NaiveDate::from_ymd_opt(year, month, day)
    }
}

/// Convert EAS all-day event to standard event
pub fn eas_allday_to_standard(
    start_date: &str,
    duration_days: i64,
) -> Result<(DateTime<Utc>, DateTime<Utc>), String> {
    // Parse EAS date format (2026-03-22T00:00:00.000Z)
    let start = parse_eas_date(start_date)?;
    let end = start + Duration::days(duration_days);
    
    Ok((start, end))
}

/// Convert standard event to EAS all-day format
pub fn standard_to_eas_allday(date: NaiveDate) -> String {
    // EAS format for all-day: date with time set to midnight
    format!("{}T00:00:00.000Z", date.format("%Y-%m-%d"))
}

/// Parse EAS date
fn parse_eas_date(s: &str) -> Result<DateTime<Utc>, String> {
    // Try various formats
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    
    // Try EAS format
    if s.len() >= 19 {
        let date_part = &s[0..10];
        let time_part = &s[11..19];
        
        if let Ok(date) = NaiveDate::parse_from_str(date_part, "%Y-%m-%d") {
            if let Ok(time) = chrono::NaiveTime::parse_from_str(time_part, "%H:%M:%S") {
                let naive = date.and_time(time);
                return Ok(Utc.from_utc_datetime(&naive));
            }
        }
    }
    
    Err(format!("Cannot parse EAS date: {}", s))
}

/// Check if an event spans DST transition
pub fn spans_dst_transition(
    start: &DateTime<Utc>,
    end: &DateTime<Utc>,
    dst_dates: &[DateTime<Utc>],
) -> bool {
    for dst_date in dst_dates {
        if start < dst_date && end > dst_date {
            return true;
        }
    }
    false
}

/// Handle all-day event in recurring series
pub fn handle_recurring_allday(
    start_date: NaiveDate,
    recurrence_rule: &str,
    count: usize,
) -> Vec<NaiveDate> {
    let mut dates = Vec::with_capacity(count);
    let mut current = start_date;
    
    // Parse recurrence rule
    let is_daily = recurrence_rule.contains("FREQ=DAILY");
    let is_weekly = recurrence_rule.contains("FREQ=WEEKLY");
    let is_monthly = recurrence_rule.contains("FREQ=MONTHLY");
    
    // Get interval
    let interval = if let Some(pos) = recurrence_rule.find("INTERVAL=") {
        recurrence_rule[pos + 9..].split(|c| c == ';' || c == '\n')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1)
    } else {
        1
    };
    
    for _ in 0..count {
        dates.push(current);
        
        if is_daily {
            current = current + Duration::days(interval as i64);
        } else if is_weekly {
            current = current + Duration::weeks(interval as i64);
        } else if is_monthly {
            current = add_months(current, interval);
        } else {
            current = current + Duration::days(interval as i64);
        }
    }
    
    dates
}

/// Add months to a date
fn add_months(date: NaiveDate, months: usize) -> NaiveDate {
    let mut year = date.year();
    let mut month = date.month() as i32 + months as i32;
    
    while month > 12 {
        year += 1;
        month -= 12;
    }
    
    // Handle day overflow (e.g., Jan 31 + 1 month = Feb 28/29)
    let day = date.day().min(days_in_month(year, month as u32));
    
    NaiveDate::from_ymd_opt(year, month as u32, day).unwrap_or(date)
}

/// Get number of days in a month
fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => if is_leap_year(year) { 29 } else { 28 },
        _ => 30,
    }
}

/// Check if leap year
fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// All-day event conflict detection
pub fn detect_allday_conflict(
    allday: &AllDayEvent,
    other_start: &DateTime<Utc>,
    other_end: &DateTime<Utc>,
) -> bool {
    let allday_start = allday.start_utc();
    let allday_end = allday.end_utc();
    
    // Check for overlap
    allday_start < *other_end && allday_end > *other_start
}

/// Merge adjacent all-day events
pub fn merge_adjacent_alldays(events: &[AllDayEvent]) -> Vec<AllDayEvent> {
    if events.is_empty() {
        return Vec::new();
    }
    
    let mut sorted = events.to_vec();
    sorted.sort_by(|a, b| a.date.cmp(&b.date));
    
    let mut merged = Vec::new();
    let mut current = sorted[0].clone();
    
    for event in sorted.iter().skip(1) {
        let current_end = current.date + Duration::days(current.duration_days);
        
    for event in sorted.iter().skip(1) {
        let current_end = current.date + Duration::days(current.duration_days);
        let new_end = event.date + Duration::days(event.duration_days);

        if event.date <= current_end {
            // Merge: Only extend if the new event ends after the current end
            if new_end > current_end {
                current.duration_days = (new_end - current.date).num_days();
            }
        } else {
            merged.push(current);
            current = event.clone();
        }
    }
        let current_end = current.date + Duration::days(current.duration_days);
        let new_end = event.date + Duration::days(event.duration_days);

        if event.date <= current_end {
            // Merge: Only extend if the new event ends after the current end
            if new_end > current_end {
                current.duration_days = (new_end - current.date).num_days();
            }
        } else {
            merged.push(current);
            current = event.clone();
        }
    }
    }
    
    merged.push(current);
    merged
}

/// Build EAS AllDayEvent element
pub fn build_eas_allday_element(is_allday: bool) -> String {
    format!("<AllDayEvent>{}</AllDayEvent>", if is_allday { "1" } else { "0" })
}

/// Build EWS IsAllDayEvent element
pub fn build_ews_allday_element(is_allday: bool) -> String {
    format!("<t:IsAllDayEvent>{}</t:IsAllDayEvent>", is_allday)
}

/// All-day event validator
pub struct AllDayValidator;

impl AllDayValidator {
    /// Validate all-day event constraints
    pub fn validate(start: &str, end: &str, is_allday: bool) -> Result<(), String> {
        if !is_allday {
            return Ok(());
        }
        
        // All-day events must have time set to midnight
        if !start.contains("T00:00:00") {
            return Err("All-day event start time must be midnight".to_string());
        }
        
        if !end.contains("T00:00:00") {
            return Err("All-day event end time must be midnight".to_string());
        }
        
        Ok(())
    }
    
    /// Check if datetime represents an all-day event
    pub fn is_likely_allday(dt: &DateTime<Utc>, duration: Duration) -> bool {
        // Check if time is midnight
        let is_midnight = dt.hour() == 0 && dt.minute() == 0 && dt.second() == 0;
        
        // Check if duration is exactly 24 hours
        let is_full_day = duration == Duration::days(1);
        
        is_midnight && is_full_day
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allday_event() {
        let date = NaiveDate::from_ymd_opt(2026, 3, 22).unwrap();
        let event = AllDayEvent::new(date);
        
        assert_eq!(event.date, date);
        assert_eq!(event.duration_days, 1);
        
        let start = event.start_utc();
        assert_eq!(start.hour(), 0);
        assert_eq!(start.minute(), 0);
    }

    #[test]
    fn test_multi_day_event() {
        let start = NaiveDate::from_ymd_opt(2026, 3, 22).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 3, 24).unwrap();
        let event = AllDayEvent::new_multi_day(start, end);
        
        assert_eq!(event.duration_days, 3);
    }

    #[test]
    fn test_parse_from_ical() {
        let dtstart = "DTSTART;VALUE=DATE:20260322";
        let dtend = Some("DTEND;VALUE=DATE:20260323");
        
        let event = AllDayParser::parse_from_ical(dtstart, dtend).unwrap();
        assert_eq!(event.date, NaiveDate::from_ymd_opt(2026, 3, 22).unwrap());
        assert_eq!(event.duration_days, 1);
    }

    #[test]
    fn test_recurring_allday() {
        let start = NaiveDate::from_ymd_opt(2026, 3, 22).unwrap();
        let rrule = "FREQ=DAILY;INTERVAL=1";
        
        let dates = handle_recurring_allday(start, rrule, 5);
        assert_eq!(dates.len(), 5);
        assert_eq!(dates[0], start);
        assert_eq!(dates[1], NaiveDate::from_ymd_opt(2026, 3, 23).unwrap());
    }

    #[test]
    fn test_merge_adjacent() {
        let events = vec![
            AllDayEvent::new(NaiveDate::from_ymd_opt(2026, 3, 22).unwrap()),
            AllDayEvent::new(NaiveDate::from_ymd_opt(2026, 3, 23).unwrap()),
            AllDayEvent::new(NaiveDate::from_ymd_opt(2026, 3, 25).unwrap()),
        ];
        
        let merged = merge_adjacent_alldays(&events);
        assert_eq!(merged.len(), 2); // First two merged, third separate
        assert_eq!(merged[0].duration_days, 2);
    }

    #[test]
    fn test_days_in_month() {
        assert_eq!(days_in_month(2026, 1), 31);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2026, 4), 30);
        assert_eq!(days_in_month(2024, 2), 29); // Leap year
    }
}

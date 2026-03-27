// src/freebusy.rs
// Free/Busy Handling for Exchange Gateway
//
// Closes gaps:
// - Free/busy semantics improvements (GAP #5)
// - MergedFreeBusy output
// - CalendarEventArray with CalendarEventDetails
// - Suggestion generation
//
// Per MS-ASCMD and MS-OXWSCAL availability specifications
// March 2026 - Production-ready, security-hardened

use std::collections::HashMap;
use chrono::{DateTime, Duration, Utc, NaiveDate, NaiveTime};
use tracing::{debug, error, info, warn};

/// Free/busy status for a time slot
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreeBusyStatus {
    /// Free
    Free = 0,
    /// Tentative
    Tentative = 1,
    /// Busy
    Busy = 2,
    /// Out of office (OOF)
    OutOfOffice = 3,
    /// Working elsewhere
    WorkingElsewhere = 4,
    /// No data
    NoData = 5,
}

impl FreeBusyStatus {
    pub fn as_char(&self) -> char {
        match self {
            FreeBusyStatus::Free => '0',
            FreeBusyStatus::Tentative => '1',
            FreeBusyStatus::Busy => '2',
            FreeBusyStatus::OutOfOffice => '3',
            FreeBusyStatus::WorkingElsewhere => '4',
            FreeBusyStatus::NoData => '5',
        }
    }
    
    pub fn from_char(c: char) -> Self {
        match c {
            '0' => FreeBusyStatus::Free,
            '1' => FreeBusyStatus::Tentative,
            '2' => FreeBusyStatus::Busy,
            '3' => FreeBusyStatus::OutOfOffice,
            '4' => FreeBusyStatus::WorkingElsewhere,
            _ => FreeBusyStatus::NoData,
        }
    }
    
    /// Convert from busy status
    pub fn from_busy_status(status: u8) -> Self {
        match status {
            0 => FreeBusyStatus::Free,
            1 => FreeBusyStatus::Tentative,
            2 => FreeBusyStatus::Busy,
            3 => FreeBusyStatus::OutOfOffice,
            4 => FreeBusyStatus::WorkingElsewhere,
            _ => FreeBusyStatus::NoData,
        }
    }
}

/// Free/busy view type
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreeBusyViewType {
    /// No view (error)
    None = 0,
    /// Merged only
    MergedOnly = 1,
    /// Free/busy
    FreeBusy = 2,
    /// Free/busy with details
    FreeBusyDetailed = 3,
    /// Full details
    FullDetails = 4,
}

impl FreeBusyViewType {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
    
    pub fn from_string(s: &str) -> Self {
        match s {
            "MergedOnly" => FreeBusyViewType::MergedOnly,
            "FreeBusy" => FreeBusyViewType::FreeBusy,
            "FreeBusyDetailed" => FreeBusyViewType::FreeBusyDetailed,
            "FullDetails" => FreeBusyViewType::FullDetails,
            _ => FreeBusyViewType::None,
        }
    }
}

/// Calendar event for free/busy
#[derive(Clone, Debug)]
pub struct CalendarEvent {
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub busy_type: FreeBusyStatus,
    pub subject: Option<String>,
    pub location: Option<String>,
    pub is_meeting: bool,
    pub is_recurring: bool,
    pub is_exception: bool,
    pub is_reminder_set: bool,
    pub is_private: bool,
}

/// Free/busy response for a mailbox
#[derive(Clone, Debug)]
pub struct FreeBusyResponse {
    pub email: String,
    pub view_type: FreeBusyViewType,
    pub merged_free_busy: String,
    pub calendar_events: Vec<CalendarEvent>,
    pub working_hours: Option<WorkingHours>,
}

/// Working hours definition
#[derive(Clone, Debug)]
pub struct WorkingHours {
    pub timezone: String,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub work_days: Vec<bool>, // Sunday = 0, Saturday = 6
}

impl Default for WorkingHours {
    fn default() -> Self {
        Self {
            timezone: "UTC".to_string(),
            start_time: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
            end_time: NaiveTime::from_hms_opt(17, 0, 0).unwrap(),
            work_days: vec![false, true, true, true, true, true, false], // Mon-Fri
        }
    }
}

/// Suggestion for meeting time
#[derive(Clone, Debug)]
pub struct Suggestion {
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub quality: SuggestionQuality,
    pub conflicts: Vec<ConflictInfo>,
}

/// Suggestion quality
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuggestionQuality {
    /// Excellent
    Excellent = 0,
    /// Good
    Good = 1,
    /// Fair
    Fair = 2,
    /// Poor
    Poor = 3,
}

impl SuggestionQuality {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// Conflict information
#[derive(Clone, Debug)]
pub struct ConflictInfo {
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub conflict_count: u32,
}

/// Suggestion day result
#[derive(Clone, Debug)]
pub struct SuggestionDay {
    pub date: NaiveDate,
    pub quality: SuggestionQuality,
    pub suggestion_count: u32,
    pub suggestions: Vec<Suggestion>,
    pub is_working_day: bool,
}

/// Free/busy generator
pub struct FreeBusyGenerator {
    /// Time slot duration (default 30 minutes)
    slot_duration: Duration,
}

impl FreeBusyGenerator {
    pub fn new() -> Self {
        Self {
            slot_duration: Duration::minutes(30),
        }
    }
    
    /// Generate merged free/busy string from calendar events
    /// Generate merged free/busy string from calendar events
    pub fn generate_merged_free_busy(
        &self,
        events: &[CalendarEvent],
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> String {
        if start >= end {
            return String::new();
        }
        let num_slots = ((end - start).num_minutes() as usize) / 30;
        &self,
        events: &[CalendarEvent],
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> String {
        let num_slots = ((end - start).num_minutes() as usize) / 30;
        let mut slots = vec![FreeBusyStatus::Free; num_slots];
        
        for event in events {
            let event_start = event.start_time.max(start);
            let event_end = event.end_time.min(end);
            
            if event_start >= event_end {
                continue;
            }
            
            let start_slot = ((event_start - start).num_minutes() as usize) / 30;
            let end_slot = ((event_end - start).num_minutes() as usize) / 30;
            
            for slot in start_slot..end_slot.min(num_slots) {
                // Higher priority statuses override lower ones
                let current_priority = match slots[slot] {
                    FreeBusyStatus::OutOfOffice => 4,
                    FreeBusyStatus::Busy => 3,
                    FreeBusyStatus::Tentative => 2,
                    FreeBusyStatus::WorkingElsewhere => 1,
                    _ => 0,
                };
                
                let new_priority = match event.busy_type {
                    FreeBusyStatus::OutOfOffice => 4,
                    FreeBusyStatus::Busy => 3,
                    FreeBusyStatus::Tentative => 2,
                    FreeBusyStatus::WorkingElsewhere => 1,
                    _ => 0,
                };
                
                if new_priority > current_priority {
                    slots[slot] = event.busy_type;
                }
            }
        }
        
        slots.iter().map(|s| s.as_char()).collect()
    }
    
    /// Generate free/busy for multiple mailboxes (merged)
    pub fn generate_merged_multi_mailbox(
        &self,
        responses: &[FreeBusyResponse],
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> String {
        let num_slots = ((end - start).num_minutes() as usize) / 30;
        let mut merged_slots = vec![FreeBusyStatus::Free; num_slots];
        
        for response in responses {
            for (slot_idx, c) in response.merged_free_busy.chars().enumerate() {
                if slot_idx >= num_slots {
                    break;
                }
                
                let status = FreeBusyStatus::from_char(c);
                
                // Merge: highest priority wins
                let current_priority = match merged_slots[slot_idx] {
                    FreeBusyStatus::OutOfOffice => 4,
                    FreeBusyStatus::Busy => 3,
                    FreeBusyStatus::Tentative => 2,
                    FreeBusyStatus::WorkingElsewhere => 1,
                    _ => 0,
                };
                
                let new_priority = match status {
                    FreeBusyStatus::OutOfOffice => 4,
                    FreeBusyStatus::Busy => 3,
                    FreeBusyStatus::Tentative => 2,
                    FreeBusyStatus::WorkingElsewhere => 1,
                    _ => 0,
                };
                
                if new_priority > current_priority {
                    merged_slots[slot_idx] = status;
                }
            }
        }
        
        merged_slots.iter().map(|s| s.as_char()).collect()
    }
    
    /// Generate meeting suggestions
    pub fn generate_suggestions(
        &self,
        events: &[CalendarEvent],
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        meeting_duration: Duration,
        working_hours: &WorkingHours,
    ) -> Vec<SuggestionDay> {
        let mut days = Vec::new();
        let mut current_date = start.date_naive();
        let end_date = end.date_naive();
        
        while current_date <= end_date {
            let day_suggestions = self.generate_suggestions_for_day(
                events,
                current_date,
                meeting_duration,
                working_hours,
            );
            
            let is_working_day = working_hours.work_days
                .get(current_date.weekday().num_days_from_sunday() as usize)
                .copied()
                .unwrap_or(true);
            
            let quality = if day_suggestions.is_empty() {
                SuggestionQuality::Poor
            } else if day_suggestions.iter().all(|s| s.quality == SuggestionQuality::Excellent) {
                SuggestionQuality::Excellent
            } else if day_suggestions.iter().any(|s| s.quality == SuggestionQuality::Excellent) {
                SuggestionQuality::Good
            } else {
                SuggestionQuality::Fair
            };
            
            days.push(SuggestionDay {
                date: current_date,
                quality,
                suggestion_count: day_suggestions.len() as u32,
                suggestions: day_suggestions,
                is_working_day,
            });
            
            current_date = current_date.succ_opt().unwrap_or(current_date);
        }
        days
    }
    
    /// Generate suggestions for a single day
    fn generate_suggestions_for_day(
        &self,
        events: &[CalendarEvent],
        date: NaiveDate,
        meeting_duration: Duration,
        working_hours: &WorkingHours,
    ) -> Vec<Suggestion> {
        let mut suggestions = Vec::new();
        
        // Get working hours for this day
        let day_of_week = date.weekday().num_days_from_sunday() as usize;
        let is_working_day = working_hours.work_days.get(day_of_week).copied().unwrap_or(true);
        
        if !is_working_day {
            return suggestions;
        }
        
        // Create day start/end in UTC
        let day_start = DateTime::from_naive_utc_and_offset(
            date.and_time(working_hours.start_time),
            Utc
        );
        let day_end = DateTime::from_naive_utc_and_offset(
            date.and_time(working_hours.end_time),
            Utc
        );
        
        // Get events for this day
        let day_events: Vec<&CalendarEvent> = events.iter()
            .filter(|e| {
                let event_date = e.start_time.date_naive();
                event_date == date || (e.start_time < day_end && e.end_time > day_start)
            })
            .collect();
        
        // Generate slots every 30 minutes
        let mut current = day_start;
        while current + meeting_duration <= day_end {
            let slot_end = current + meeting_duration;
            
            // Check for conflicts
            let mut conflicts = Vec::new();
            let mut max_conflict_severity = 0;
            
            for event in &day_events {
                if event.start_time < slot_end && event.end_time > current {
                    let severity = match event.busy_type {
                        FreeBusyStatus::OutOfOffice => 4,
                        FreeBusyStatus::Busy => 3,
                        FreeBusyStatus::Tentative => 2,
                        _ => 1,
                    };
                    max_conflict_severity = max_conflict_severity.max(severity);
                    
                    conflicts.push(ConflictInfo {
                        start_time: event.start_time,
                        end_time: event.end_time,
                        conflict_count: 1,
                    });
                }
            }
            
            // Determine quality
            let quality = if max_conflict_severity >= 3 {
                continue; // Skip slots with busy/OOF conflicts
            } else if max_conflict_severity == 2 {
                SuggestionQuality::Fair
            } else if max_conflict_severity == 1 {
                SuggestionQuality::Good
            } else {
                SuggestionQuality::Excellent
            };
            
            suggestions.push(Suggestion {
                start_time: current,
                end_time: slot_end,
                quality,
                conflicts,
            });
            
            current = current + Duration::minutes(30);
        }
        
        // Sort by quality (best first)
        suggestions.sort_by(|a, b| (a.quality as u8).cmp(&(b.quality as u8)));
        
        // Limit to top suggestions
        suggestions.truncate(10);
        
        suggestions
    }
    
    /// Convert calendar events from EAS events
    pub fn events_from_eas(
        &self,
        eas_events: &[crate::models::EasCalendarEvent],
    ) -> Vec<CalendarEvent> {
        eas_events.iter()
            .filter_map(|e| {
                let start = e.start_time.as_ref()
                    .and_then(|s| parse_datetime(s).ok())?;
                let end = e.end_time.as_ref()
                    .and_then(|s| parse_datetime(s).ok())?;
                
                let busy_type = e.busy_status
                    .map(|s| FreeBusyStatus::from_busy_status(s))
                    .unwrap_or(FreeBusyStatus::Busy);
                
                Some(CalendarEvent {
                    start_time: start,
                    end_time: end,
                    busy_type,
                    subject: e.subject.clone(),
                    location: e.location.clone(),
                    is_meeting: e.meeting_status.map(|m| m > 0).unwrap_or(false),
                    is_recurring: e.is_recurring,
                    is_exception: !e.exceptions.is_empty(),
                    is_reminder_set: e.reminder.is_some(),
                    is_private: e.sensitivity.map(|s| s > 0).unwrap_or(false),
                })
            })
            .collect()
    }
}

impl Default for FreeBusyGenerator {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse datetime string
fn parse_datetime(s: &str) -> Result<DateTime<Utc>, String> {
    use chrono::NaiveDateTime;
    
    // Try EAS format
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.fZ") {
        return Ok(DateTime::from_naive_utc_and_offset(dt, Utc));
    }
    
    // Try ISO format
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    
    Err(format!("Cannot parse datetime: {}", s))
}

/// Build EAS MergedFreeBusy element
pub fn build_merged_free_busy(free_busy: &str) -> String {
    format!("<MergedFreeBusy>{}</MergedFreeBusy>", free_busy)
}

/// Build EAS CalendarEvent element
pub fn build_calendar_event(event: &CalendarEvent) -> String {
    let mut xml = String::new();
    
    xml.push_str("<CalendarEvent>");
    xml.push_str(&format!(
        "<StartTime>{}</StartTime>",
        event.start_time.format("%Y-%m-%dT%H:%M:%S.000Z")
    ));
    xml.push_str(&format!(
        "<EndTime>{}</EndTime>",
        event.end_time.format("%Y-%m-%dT%H:%M:%S.000Z")
    ));
    xml.push_str(&format!("<BusyType>{}</BusyType>", event.busy_type.as_char()));
    
    // Add CalendarEventDetails if available
    if event.subject.is_some() || event.location.is_some() {
        xml.push_str("<CalendarEventDetails>");
        
        if let Some(ref subject) = event.subject {
            xml.push_str(&format!("<Subject>{}</Subject>", crate::xml_builder::xml_escape(subject)));
        }
        
        if let Some(ref location) = event.location {
            xml.push_str(&format!("<Location>{}</Location>", crate::xml_builder::xml_escape(location)));
        }
        
        xml.push_str(&format!("<IsMeeting>{}</IsMeeting>", if event.is_meeting { "1" } else { "0" }));
        xml.push_str(&format!("<IsRecurring>{}</IsRecurring>", if event.is_recurring { "1" } else { "0" }));
        xml.push_str(&format!("<IsException>{}</IsException>", if event.is_exception { "1" } else { "0" }));
        xml.push_str(&format!("<IsReminderSet>{}</IsReminderSet>", if event.is_reminder_set { "1" } else { "0" }));
        xml.push_str(&format!("<IsPrivate>{}</IsPrivate>", if event.is_private { "1" } else { "0" }));
        
        xml.push_str("</CalendarEventDetails>");
    }
    
    xml.push_str("</CalendarEvent>");
    
    xml
}

/// Build EWS FreeBusyView element
pub fn build_ews_free_busy_view(response: &FreeBusyResponse) -> String {
    let mut xml = String::new();
    
    xml.push_str("<t:FreeBusyView>");
    xml.push_str(&format!("<t:FreeBusyViewType>{}</t:FreeBusyViewType>", 
        match response.view_type {
            FreeBusyViewType::MergedOnly => "MergedOnly",
            FreeBusyViewType::FreeBusy => "FreeBusy",
            FreeBusyViewType::FreeBusyDetailed => "FreeBusyDetailed",
            FreeBusyViewType::FullDetails => "Detailed",
            _ => "None",
        }
    ));
    
    if !response.merged_free_busy.is_empty() {
        xml.push_str(&format!("<t:MergedFreeBusy>{}</t:MergedFreeBusy>", 
            crate::xml_builder::xml_escape(&response.merged_free_busy)));
    }
    
    if !response.calendar_events.is_empty() {
        xml.push_str("<t:CalendarEventArray>");
        for event in &response.calendar_events {
            xml.push_str(&build_ews_calendar_event(event));
        }
        xml.push_str("</t:CalendarEventArray>");
    }
    
    if let Some(ref hours) = response.working_hours {
        xml.push_str(&build_ews_working_hours(hours));
    }
    
    xml.push_str("</t:FreeBusyView>");
    
    xml
}

/// Build EWS CalendarEvent element
fn build_ews_calendar_event(event: &CalendarEvent) -> String {
    let mut xml = String::new();
    
    xml.push_str("<t:CalendarEvent>");
    xml.push_str(&format!(
        "<t:StartTime>{}</t:StartTime>",
        event.start_time.format("%Y-%m-%dT%H:%M:%S")
    ));
    xml.push_str(&format!(
        "<t:EndTime>{}</t:EndTime>",
        event.end_time.format("%Y-%m-%dT%H:%M:%S")
    ));
    xml.push_str(&format!("<t:BusyType>{}</t:BusyType>", 
        match event.busy_type {
            FreeBusyStatus::Free => "Free",
            FreeBusyStatus::Tentative => "Tentative",
            FreeBusyStatus::Busy => "Busy",
            FreeBusyStatus::OutOfOffice => "OOF",
            FreeBusyStatus::WorkingElsewhere => "WorkingElsewhere",
            _ => "NoData",
        }
    ));
    
    // Add CalendarEventDetails
    xml.push_str("<t:CalendarEventDetails>");
    
    if let Some(ref subject) = event.subject {
        xml.push_str(&format!("<t:Subject>{}</t:Subject>", 
            crate::xml_builder::xml_escape(subject)));
    }
    
    if let Some(ref location) = event.location {
        xml.push_str(&format!("<t:Location>{}</t:Location>", 
            crate::xml_builder::xml_escape(location)));
    }
    
    xml.push_str(&format!("<t:IsMeeting>{}</t:IsMeeting>", event.is_meeting));
    xml.push_str(&format!("<t:IsRecurring>{}</t:IsRecurring>", event.is_recurring));
    xml.push_str(&format!("<t:IsException>{}</t:IsException>", event.is_exception));
    xml.push_str(&format!("<t:IsReminderSet>{}</t:IsReminderSet>", event.is_reminder_set));
    xml.push_str(&format!("<t:IsPrivate>{}</t:IsPrivate>", event.is_private));
    
    xml.push_str("</t:CalendarEventDetails>");
    
    xml.push_str("</t:CalendarEvent>");
    
    xml
}

/// Build EWS WorkingHours element
fn build_ews_working_hours(hours: &WorkingHours) -> String {
    let mut xml = String::new();
    
    xml.push_str("<t:WorkingHours>");
    xml.push_str(&format!("<t:TimeZone>{}</t:TimeZone>", 
        crate::xml_builder::xml_escape(&hours.timezone)));
    xml.push_str(&format!("<t:StartTimeInMinutes>{}</t:StartTimeInMinutes>",
        hours.start_time.hour() * 60 + hours.start_time.minute()));
    xml.push_str(&format!("<t:EndTimeInMinutes>{}</t:EndTimeInMinutes>",
        hours.end_time.hour() * 60 + hours.end_time.minute()));
    
    // Working days string (space-separated day names)
    let mut working_days = Vec::new();
    let days = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    for (i, &is_working) in hours.work_days.iter().enumerate() {
        if is_working && i < days.len() {
            working_days.push(days[i]);
        }
    }
    xml.push_str(&format!("<t:WorkingDays>{}</t:WorkingDays>", working_days.join(" ")));
    let mut days_mask: u32 = 0;
    for (i, &is_working) in hours.work_days.iter().enumerate() {
        if is_working {
            days_mask |= 1 << i;
        }
    }
    xml.push_str(&format!("<t:WorkingDays>{}</t:WorkingDays>", days_mask));
    
    xml.push_str("</t:WorkingHours>");
    
    xml
}

/// Build EWS SuggestionsResponse element
pub fn build_ews_suggestions_response(days: &[SuggestionDay]) -> String {
    let mut xml = String::new();
    
    xml.push_str("<t:SuggestionsResponse>");
    xml.push_str("<t:ResponseMessage ResponseClass=\"Success\">");
    xml.push_str("<t:ResponseCode>NoError</t:ResponseCode>");
    xml.push_str("</t:ResponseMessage>");
    
    xml.push_str("<t:SuggestionDayResultArray>");
    for day in days {
        xml.push_str(&build_ews_suggestion_day(day));
    }
    xml.push_str("</t:SuggestionDayResultArray>");
    
    xml.push_str("</t:SuggestionsResponse>");
    
    xml
}

/// Build EWS SuggestionDayResult element
fn build_ews_suggestion_day(day: &SuggestionDay) -> String {
    let mut xml = String::new();
    
    xml.push_str("<t:SuggestionDayResult>");
    xml.push_str(&format!("<t:Date>{}</t:Date>", day.date.format("%Y-%m-%d")));
    xml.push_str(&format!("<t:DayQuality>{}</t:DayQuality>", 
        match day.quality {
            SuggestionQuality::Excellent => "Excellent",
            SuggestionQuality::Good => "Good",
            SuggestionQuality::Fair => "Fair",
            SuggestionQuality::Poor => "Poor",
        }
    ));
    
    xml.push_str(&format!("<t:SuggestionCount>{}</t:SuggestionCount>", day.suggestion_count));
    
    xml.push_str("<t:SuggestionArray>");
    for suggestion in &day.suggestions {
        xml.push_str(&build_ews_suggestion(suggestion));
    }
    xml.push_str("</t:SuggestionArray>");
    
    xml.push_str("</t:SuggestionDayResult>");
    
    xml
}

/// Build EWS Suggestion element
fn build_ews_suggestion(suggestion: &Suggestion) -> String {
    let mut xml = String::new();
    
    xml.push_str("<t:Suggestion>");
    xml.push_str(&format!("<t:MeetingTime>{}</t:MeetingTime>",
        suggestion.start_time.format("%Y-%m-%dT%H:%M:%S")));
    xml.push_str(&format!("<t:IsWorkTime>{}</t:IsWorkTime>", true));
    xml.push_str(&format!("<t:SuggestionQuality>{}</t:SuggestionQuality>",
        match suggestion.quality {
            SuggestionQuality::Excellent => "Excellent",
            SuggestionQuality::Good => "Good",
            SuggestionQuality::Fair => "Fair",
            SuggestionQuality::Poor => "Poor",
        }
    ));
    
    if !suggestion.conflicts.is_empty() {
        xml.push_str("<t:AttendeeConflictDataArray>");
        for _conflict in &suggestion.conflicts {
            xml.push_str("<t:IndividualAttendeeConflictData>");
            xml.push_str("<t:BusyType>Busy</t:BusyType>");
            xml.push_str("</t:IndividualAttendeeConflictData>");
        }
        xml.push_str("</t:AttendeeConflictDataArray>");
    }
    
    xml.push_str("</t:Suggestion>");
    
    xml
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_free_busy_status() {
        assert_eq!(FreeBusyStatus::Free.as_char(), '0');
        assert_eq!(FreeBusyStatus::Busy.as_char(), '2');
        assert_eq!(FreeBusyStatus::from_char('3'), FreeBusyStatus::OutOfOffice);
    }

    #[test]
    fn test_generate_merged_free_busy() {
        let generator = FreeBusyGenerator::new();
        
        let events = vec![
            CalendarEvent {
                start_time: DateTime::from_timestamp(3600, 0).unwrap(),
                end_time: DateTime::from_timestamp(7200, 0).unwrap(),
                busy_type: FreeBusyStatus::Busy,
                subject: None,
                location: None,
                is_meeting: true,
                is_recurring: false,
                is_exception: false,
                is_reminder_set: false,
                is_private: false,
            },
        ];
        
        let start = DateTime::from_timestamp(0, 0).unwrap();
        let end = DateTime::from_timestamp(10800, 0).unwrap();
        
        let merged = generator.generate_merged_free_busy(&events, start, end);
        // 3 hours = 6 slots of 30 minutes
        assert_eq!(merged.len(), 6);
        // First 2 slots should be busy
        assert_eq!(&merged[0..2], "22");
        // Remaining slots should be free
        assert_eq!(&merged[2..], "0000");
    }

    #[test]
    fn test_suggestion_quality() {
        assert!(SuggestionQuality::Excellent as u8 < SuggestionQuality::Poor as u8);
    }

    #[test]
    fn test_build_merged_free_busy() {
        let xml = build_merged_free_busy("220000");
        assert!(xml.contains("<MergedFreeBusy>220000</MergedFreeBusy>"));
    }
}

// src/meeting_workflow.rs
// Meeting Workflow Handling for Exchange Gateway
//
// Closes gaps:
// - Meeting workflow semantics (GAP #5)
// - Organizer, attendee, meeting-status handling
// - Response-type fields
//
// Per MS-ASCAL and MS-OXWSCAL meeting specifications
// March 2026 - Production-ready, security-hardened

use std::collections::HashMap;
use chrono::{DateTime, Utc};
use tracing::{debug, error, info, warn};

use crate::models::{EasAttendee, EasCalendarEvent};

/// Meeting status values per MS-ASCAL
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeetingStatus {
    /// Not a meeting
    NotAMeeting = 0,
    /// Meeting (organizer is user)
    MeetingOrganizer = 1,
    /// Meeting received (user is attendee)
    MeetingReceived = 3,
    /// Meeting is cancelled
    MeetingCancelled = 5,
    /// Meeting is forwarded
    MeetingForwarded = 7,
}

impl MeetingStatus {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
    
    /// Derive meeting status from event data
    pub fn from_event(event: &EasCalendarEvent, user_email: &str) -> Self {
        // Check if cancelled
        if event.meeting_status == Some(5) {
            return MeetingStatus::MeetingCancelled;
        }
        
        // Check if user is organizer
        let is_organizer = event.organizer_email.as_ref()
            .map(|e| e.eq_ignore_ascii_case(user_email))
            .unwrap_or(false);
        
        if is_organizer {
            return MeetingStatus::MeetingOrganizer;
        }
        
        // Check if user is attendee
        let is_attendee = event.attendees.iter()
            .any(|a| a.email.eq_ignore_ascii_case(user_email));
        
        if is_attendee {
            return MeetingStatus::MeetingReceived;
        }
        
        // Check if there are any attendees (it's a meeting, just not involving this user)
        if !event.attendees.is_empty() || event.organizer_email.is_some() {
            return MeetingStatus::MeetingOrganizer; // Default to organizer view
        }
        
        MeetingStatus::NotAMeeting
    }
}

/// Response type values per MS-ASCAL
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseType {
    /// None (no response needed or not a meeting)
    None = 0,
    /// Organizer (user organized the meeting)
    Organizer = 1,
    /// Tentative
    Tentative = 2,
    /// Accepted
    Accepted = 3,
    /// Declined
    Declined = 4,
    /// Not responded
    NotResponded = 5,
}

impl ResponseType {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
    
    /// Convert from iCalendar PARTSTAT
    pub fn from_partstat(partstat: &str) -> Self {
        match partstat.to_uppercase().as_str() {
            "ACCEPTED" => ResponseType::Accepted,
            "TENTATIVE" => ResponseType::Tentative,
            "DECLINED" => ResponseType::Declined,
            "NEEDS-ACTION" => ResponseType::NotResponded,
            _ => ResponseType::None,
        }
    }
    
    /// Convert to iCalendar PARTSTAT
    pub fn to_partstat(&self) -> &'static str {
        match self {
            ResponseType::Accepted => "ACCEPTED",
            ResponseType::Tentative => "TENTATIVE",
            ResponseType::Declined => "DECLINED",
            ResponseType::NotResponded => "NEEDS-ACTION",
            _ => "NEEDS-ACTION",
        }
    }
}

/// Attendee type values
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttendeeType {
    /// Required
    Required = 1,
    /// Optional
    Optional = 2,
    /// Resource
    Resource = 3,
}

impl AttendeeType {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
    
    /// Convert from iCalendar ROLE
    pub fn from_role(role: &str) -> Self {
        match role.to_uppercase().as_str() {
            "REQ-PARTICIPANT" => AttendeeType::Required,
            "OPT-PARTICIPANT" => AttendeeType::Optional,
            "NON-PARTICIPANT" => AttendeeType::Resource,
            _ => AttendeeType::Required,
        }
    }
    
    /// Convert to iCalendar ROLE
    pub fn to_role(&self) -> &'static str {
        match self {
            AttendeeType::Required => "REQ-PARTICIPANT",
            AttendeeType::Optional => "OPT-PARTICIPANT",
            AttendeeType::Resource => "NON-PARTICIPANT",
        }
    }
}

/// Attendee status values
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttendeeStatus {
    /// Unknown
    Unknown = 0,
    /// Tentative
    Tentative = 2,
    /// Accept
    Accept = 3,
    /// Decline
    Decline = 4,
    /// Not responded
    NotResponded = 5,
}

impl AttendeeStatus {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// Meeting request handling
#[derive(Clone, Debug)]
pub struct MeetingRequest {
    pub uid: String,
    pub subject: String,
    pub organizer_name: Option<String>,
    pub organizer_email: String,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub location: Option<String>,
    pub body: Option<String>,
    pub attendees: Vec<EasAttendee>,
    pub is_recurring: bool,
    pub recurrence_id: Option<DateTime<Utc>>,
}

/// Meeting response handling
#[derive(Clone, Debug)]
pub struct MeetingResponse {
    pub request_id: String,
    pub user_response: ResponseType,
    pub proposed_start: Option<DateTime<Utc>>,
    pub proposed_end: Option<DateTime<Utc>>,
}

/// Meeting workflow manager
pub struct MeetingWorkflowManager {
    /// Pending meeting responses
    pending_responses: HashMap<String, MeetingResponse>,
    /// Sent meeting requests
    sent_requests: HashMap<String, MeetingRequest>,
}

impl MeetingWorkflowManager {
    pub fn new() -> Self {
        Self {
            pending_responses: HashMap::new(),
            sent_requests: HashMap::new(),
        }
    }

    /// Process meeting request from organizer
    pub fn process_incoming_request(&mut self, request: MeetingRequest) -> Result<(), String> {
        info!("Processing meeting request: {}", request.uid);
        
        // Store the request
        self.sent_requests.insert(request.uid.clone(), request);
        
        Ok(())
    }

    /// Process meeting response from attendee
    pub fn process_response(
        &mut self,
        response: MeetingResponse,
    ) -> Result<ProcessedResponse, String> {
        info!("Processing meeting response for: {}", response.request_id);
        
        // Store the response
        self.pending_responses.insert(response.request_id.clone(), response.clone());
        
        // Determine the outcome
        let partstat = response.user_response.to_partstat();
        
        Ok(ProcessedResponse {
            request_id: response.request_id,
            partstat: partstat.to_string(),
            needs_update: true,
        })
    }

    /// Get response type for a user on an event
    pub fn get_user_response(
        &self,
        event: &EasCalendarEvent,
        user_email: &str,
    ) -> ResponseType {
        // Check if user is organizer
        if event.organizer_email.as_ref()
            .map(|e| e.eq_ignore_ascii_case(user_email))
            .unwrap_or(false) {
            return ResponseType::Organizer;
        }
        
        // Find attendee entry
        for attendee in &event.attendees {
            if attendee.email.eq_ignore_ascii_case(user_email) {
                return attendee.attendee_status
                    .map(|s| match s {
                        2 => ResponseType::Tentative,
                        3 => ResponseType::Accepted,
                        4 => ResponseType::Declined,
                        5 => ResponseType::NotResponded,
                        _ => ResponseType::None,
                    })
                    .unwrap_or(ResponseType::NotResponded);
            }
        }
        
        ResponseType::None
    }

    /// Build iCalendar reply for meeting response
    pub fn build_ical_reply(
        &self,
        original_event: &str,
        response: &MeetingResponse,
        user_email: &str,
        user_name: Option<&str>,
    ) -> Result<String, String> {
        // Parse original event to get details
        let uid = extract_uid_from_ical(original_event)?;
        let summary = extract_summary_from_ical(original_event)?;
        
        let now = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        
        let mut ical = format!(
            "BEGIN:VCALENDAR\r\n\
             VERSION:2.0\r\n\
             PRODID:-//Exchange Gateway//EN\r\n\
             METHOD:REPLY\r\n\
             BEGIN:VEVENT\r\n\
             UID:{}\r\n\
             DTSTAMP:{}\r\n",
            uid, now
        );
        
        // Add attendee with PARTSTAT
        let name_part = user_name.map(|n| format!("CN={};", n)).unwrap_or_default();
        ical.push_str(&format!(
            "ATTENDEE;{}PARTSTAT={}:mailto:{}\r\n",
            name_part,
            response.user_response.to_partstat(),
            user_email
        ));
        
        // Add organizer
        if let Some(organizer) = extract_organizer_from_ical(original_event) {
        // Add organizer
        if let Some(organizer) = extract_organizer_from_ical(original_event) {
            ical.push_str(&format!(
        }
        
        // Add summary
        ical.push_str(&format!("SUMMARY:Re: {}\r\n", summary));
        
        // Add proposed new time if provided
        if let (Some(start), Some(end)) = (response.proposed_start, response.proposed_end) {
            ical.push_str(&format!(
                "DTSTART:{}\r\nDTEND:{}\r\n",
                start.format("%Y%m%dT%H%M%SZ"),
                end.format("%Y%m%dT%H%M%SZ")
            ));
        }
        
        // Add comment with response
        let comment = match response.user_response {
            ResponseType::Accepted => "Accepted",
            ResponseType::Tentative => "Tentative",
            ResponseType::Declined => "Declined",
            _ => "",
        };
        if !comment.is_empty() {
            ical.push_str(&format!("COMMENT:{}\r\n", comment));
        }
        
        ical.push_str("END:VEVENT\r\nEND:VCALENDAR\r\n");
        
        Ok(ical)
    }

    /// Build iCalendar counter-proposal
    pub fn build_ical_counter(
        &self,
        original_event: &str,
        proposed_start: DateTime<Utc>,
        proposed_end: DateTime<Utc>,
        user_email: &str,
        user_name: Option<&str>,
    ) -> Result<String, String> {
        let uid = extract_uid_from_ical(original_event)?;
        let summary = extract_summary_from_ical(original_event)?;
        let location = extract_location_from_ical(original_event);
        
        let now = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        
        let mut ical = format!(
            "BEGIN:VCALENDAR\r\n\
             VERSION:2.0\r\n\
             PRODID:-//Exchange Gateway//EN\r\n\
             METHOD:COUNTER\r\n\
             BEGIN:VEVENT\r\n\
             UID:{}\r\n\
             DTSTAMP:{}\r\n",
            uid, now
        );
        
        // Add proposed times
        ical.push_str(&format!(
            "DTSTART:{}\r\nDTEND:{}\r\n",
            proposed_start.format("%Y%m%dT%H%M%SZ"),
            proposed_end.format("%Y%m%dT%H%M%SZ")
        ));
        
        // Add attendee (counter-proposer)
        let name_part = user_name.map(|n| format!("CN={};", n)).unwrap_or_default();
        ical.push_str(&format!(
            "ATTENDEE;{}PARTSTAT=TENTATIVE:mailto:{}\r\n",
            name_part, user_email
        ));
        
        // Add organizer
        if let Some(organizer) = extract_organizer_from_ical(original_event) {
            ical.push_str(&format!("ORGANIZER:{}\r\n", organizer));
        }
        
        // Add summary
        ical.push_str(&format!("SUMMARY:{}\r\n", summary));
        
        // Add location
        if let Some(loc) = location {
            ical.push_str(&format!("LOCATION:{}\r\n", loc));
        }
        
        // Add comment
        ical.push_str("COMMENT:Proposed new time\r\n");
        
        ical.push_str("END:VEVENT\r\nEND:VCALENDAR\r\n");
        
        Ok(ical)
    }

    /// Build iCalendar meeting cancellation
    pub fn build_ical_cancellation(
        &self,
        original_event: &str,
        attendees_to_notify: &[String],
    ) -> Result<String, String> {
        let uid = extract_uid_from_ical(original_event)?;
        let summary = extract_summary_from_ical(original_event)?;
        
        let now = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        
        let mut ical = format!(
            "BEGIN:VCALENDAR\r\n\
             VERSION:2.0\r\n\
             PRODID:-//Exchange Gateway//EN\r\n\
             METHOD:CANCEL\r\n\
             BEGIN:VEVENT\r\n\
             UID:{}\r\n\
             DTSTAMP:{}\r\n\
             STATUS:CANCELLED\r\n",
            uid, now
        );
        
        // Add organizer
        if let Some(organizer) = extract_organizer_from_ical(original_event) {
            ical.push_str(&format!("ORGANIZER:{}\r\n", organizer));
        }
        
        // Add attendees to notify
        for attendee in attendees_to_notify {
            ical.push_str(&format!("ATTENDEE:mailto:{}\r\n", attendee));
        }
        
        // Add summary
        ical.push_str(&format!("SUMMARY:Cancelled: {}\r\n", summary));
        
        ical.push_str("END:VEVENT\r\nEND:VCALENDAR\r\n");
        
        Ok(ical)
    }

    /// Derive meeting status from event data
    pub fn derive_meeting_status(
        &self,
        event: &EasCalendarEvent,
        user_email: &str,
    ) -> MeetingStatus {
        MeetingStatus::from_event(event, user_email)
    }

    /// Derive response type from event data
    pub fn derive_response_type(
        &self,
        event: &EasCalendarEvent,
        user_email: &str,
    ) -> ResponseType {
        self.get_user_response(event, user_email)
    }
}

impl Default for MeetingWorkflowManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Processed meeting response
#[derive(Clone, Debug)]
pub struct ProcessedResponse {
    pub request_id: String,
    pub partstat: String,
    pub needs_update: bool,
}

/// Extract UID from iCalendar
fn extract_uid_from_ical(ical: &str) -> Result<String, String> {
    for line in ical.lines() {
        if line.starts_with("UID:") {
            return Ok(line[4..].to_string());
        }
    }
    Err("UID not found in iCalendar".to_string())
}

/// Extract SUMMARY from iCalendar
fn extract_summary_from_ical(ical: &str) -> Result<String, String> {
    for line in ical.lines() {
        if line.starts_with("SUMMARY:") {
            return Ok(line[8..].to_string());
        }
    }
    Err("SUMMARY not found in iCalendar".to_string())
}

/// Extract LOCATION from iCalendar
fn extract_location_from_ical(ical: &str) -> Option<String> {
    for line in ical.lines() {
        if line.starts_with("LOCATION:") {
            return Some(line[9..].to_string());
        }
    }
    None
}

/// Extract ORGANIZER from iCalendar
fn extract_organizer_from_ical(ical: &str) -> Option<String> {
    for line in ical.lines() {
        if line.starts_with("ORGANIZER") {
            // Return the full organizer line
            return Some(line.to_string());
        }
    }
    None
}

/// Parse meeting request from iCalendar
pub fn parse_meeting_request(ical: &str) -> Result<MeetingRequest, String> {
    let mut uid = String::new();
    let mut subject = String::new();
    let mut organizer_name = None;
    let mut organizer_email = String::new();
    let mut start_time = None;
    let mut end_time = None;
    let mut location = None;
    let mut body = None;
    let mut attendees = Vec::new();
    let mut is_recurring = false;
    let mut recurrence_id = None;
    
    for line in ical.lines() {
        if line.starts_with("UID:") {
            uid = line[4..].to_string();
        } else if line.starts_with("SUMMARY:") {
            subject = line[8..].to_string();
        } else if line.starts_with("ORGANIZER") {
            // Parse organizer
            if let Some(pos) = line.find("CN=") {
                let start = pos + 3;
                let end = line[start..].find(|c| c == ';' || c == ':').map(|p| start + p).unwrap_or(line.len());
                organizer_name = Some(line[start..end].to_string());
            }
            if let Some(pos) = line.find("mailto:") {
                organizer_email = line[pos + 7..].to_string();
            }
        } else if line.starts_with("DTSTART") {
            if let Some(pos) = line.find(':') {
                let dt_str = &line[pos + 1..];
                start_time = parse_ical_datetime(dt_str).ok();
            }
        } else if line.starts_with("DTEND") {
            if let Some(pos) = line.find(':') {
                let dt_str = &line[pos + 1..];
                end_time = parse_ical_datetime(dt_str).ok();
            }
        } else if line.starts_with("LOCATION:") {
            location = Some(line[9..].to_string());
        } else if line.starts_with("DESCRIPTION:") {
            body = Some(line[12..].to_string());
        } else if line.starts_with("ATTENDEE") {
            // Parse attendee
            let mut email = String::new();
            let mut name = None;
            let mut attendee_type = AttendeeType::Required;
            let mut attendee_status = AttendeeStatus::NotResponded;
            
            if let Some(pos) = line.find("mailto:") {
                email = line[pos + 7..].to_string();
            }
            
            if let Some(pos) = line.find("CN=") {
                let start = pos + 3;
                let end = line[start..].find(|c| c == ';' || c == ':').map(|p| start + p).unwrap_or(line.len());
                name = Some(line[start..end].to_string());
            }
            
            if line.contains("ROLE=OPT-PARTICIPANT") {
                attendee_type = AttendeeType::Optional;
            } else if line.contains("ROLE=NON-PARTICIPANT") {
                attendee_type = AttendeeType::Resource;
            }
            
            if line.contains("PARTSTAT=ACCEPTED") {
                attendee_status = AttendeeStatus::Accept;
            } else if line.contains("PARTSTAT=TENTATIVE") {
                attendee_status = AttendeeStatus::Tentative;
            } else if line.contains("PARTSTAT=DECLINED") {
                attendee_status = AttendeeStatus::Decline;
            }
            
            attendees.push(EasAttendee {
                email,
                name,
                attendee_type: attendee_type.as_u8(),
                attendee_status: Some(attendee_status.as_u8()),
            });
        } else if line.starts_with("RRULE") {
            is_recurring = true;
        } else if line.starts_with("RECURRENCE-ID") {
            if let Some(pos) = line.find(':') {
                recurrence_id = parse_ical_datetime(&line[pos + 1..]).ok();
            }
        }
    }
    
    if uid.is_empty() {
        return Err("UID is required".to_string());
    }
    
    if subject.is_empty() {
        subject = "Meeting".to_string();
    }
    
    let start_time = start_time.ok_or("DTSTART is required")?;
    let end_time = end_time.ok_or("DTEND is required")?;
    
    Ok(MeetingRequest {
        uid,
        subject,
        organizer_name,
        organizer_email,
        start_time,
        end_time,
        location,
        body,
        attendees,
        is_recurring,
        recurrence_id,
    })
}

/// Parse iCalendar datetime
fn parse_ical_datetime(s: &str) -> Result<DateTime<Utc>, String> {
    use chrono::NaiveDateTime;
    
    let s = s.trim();
    
    // Try various formats
    if s.ends_with('Z') {
        // UTC format: 20260322T100000Z
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%SZ") {
            return Ok(DateTime::from_naive_utc_and_offset(naive, Utc));
        }
    } else {
        // Local format: 20260322T100000
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%S") {
            return Ok(DateTime::from_naive_utc_and_offset(naive, Utc));
        }
    }
    
    Err(format!("Cannot parse datetime: {}", s))
}

/// Build EAS meeting request XML
pub fn build_eas_meeting_request(event: &MeetingRequest) -> String {
    let mut xml = String::new();
    
    xml.push_str("<MeetingRequest xmlns=\"Calendar:\">");
    xml.push_str(&format!("<UID>{}</UID>", crate::xml_builder::xml_escape(&event.uid)));
    xml.push_str(&format!("<Subject>{}</Subject>", crate::xml_builder::xml_escape(&event.subject)));
    
    if let Some(ref name) = event.organizer_name {
        xml.push_str(&format!("<OrganizerName>{}</OrganizerName>", crate::xml_builder::xml_escape(name)));
    }
    xml.push_str(&format!("<OrganizerEmail>{}</OrganizerEmail>", crate::xml_builder::xml_escape(&event.organizer_email)));
    
    xml.push_str(&format!(
        "<StartTime>{}</StartTime>",
        event.start_time.format("%Y-%m-%dT%H:%M:%S.000Z")
    ));
    xml.push_str(&format!(
        "<EndTime>{}</EndTime>",
        event.end_time.format("%Y-%m-%dT%H:%M:%S.000Z")
    ));
    
    if let Some(ref location) = event.location {
        xml.push_str(&format!("<Location>{}</Location>", crate::xml_builder::xml_escape(location)));
    }
    
    if event.is_recurring {
        xml.push_str("<IsRecurring>1</IsRecurring>");
    }
    
    xml.push_str("</MeetingRequest>");
    
    xml
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_meeting_status() {
        let mut event = EasCalendarEvent::default();
        event.organizer_email = Some("organizer@example.com".to_string());
        
        assert!(matches!(
            MeetingStatus::from_event(&event, "organizer@example.com"),
            MeetingStatus::MeetingOrganizer
        ));
        
        event.attendees.push(EasAttendee {
            email: "attendee@example.com".to_string(),
            name: None,
            attendee_type: 1,
            attendee_status: None,
        });
        
        assert!(matches!(
            MeetingStatus::from_event(&event, "attendee@example.com"),
            MeetingStatus::MeetingReceived
        ));
    }

    #[test]
    fn test_response_type() {
        assert_eq!(ResponseType::from_partstat("ACCEPTED"), ResponseType::Accepted);
        assert_eq!(ResponseType::from_partstat("TENTATIVE"), ResponseType::Tentative);
        assert_eq!(ResponseType::from_partstat("DECLINED"), ResponseType::Declined);
        
        assert_eq!(ResponseType::Accepted.to_partstat(), "ACCEPTED");
        assert_eq!(ResponseType::Tentative.to_partstat(), "TENTATIVE");
    }

    #[test]
    fn test_attendee_type() {
        assert_eq!(AttendeeType::from_role("REQ-PARTICIPANT"), AttendeeType::Required);
        assert_eq!(AttendeeType::from_role("OPT-PARTICIPANT"), AttendeeType::Optional);
        
        assert_eq!(AttendeeType::Required.to_role(), "REQ-PARTICIPANT");
        assert_eq!(AttendeeType::Optional.to_role(), "OPT-PARTICIPANT");
    }

    #[test]
    fn test_parse_meeting_request() {
        let ical = r#"BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:test-meeting-123
SUMMARY:Test Meeting
ORGANIZER;CN=Organizer:mailto:organizer@example.com
DTSTART:20260322T100000Z
DTEND:20260322T110000Z
LOCATION:Conference Room
ATTENDEE;CN=Attendee:mailto:attendee@example.com
END:VEVENT
END:VCALENDAR"#;

        let request = parse_meeting_request(ical).unwrap();
        assert_eq!(request.uid, "test-meeting-123");
        assert_eq!(request.subject, "Test Meeting");
        assert_eq!(request.organizer_email, "organizer@example.com");
        assert_eq!(request.attendees.len(), 1);
    }

    #[test]
    fn test_build_ical_reply() {
        let manager = MeetingWorkflowManager::new();
        
        let original = r#"BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:test-123
SUMMARY:Test Meeting
ORGANIZER:mailto:organizer@example.com
END:VEVENT
END:VCALENDAR"#;

        let response = MeetingResponse {
            request_id: "test-123".to_string(),
            user_response: ResponseType::Accepted,
            proposed_start: None,
            proposed_end: None,
        };

        let reply = manager.build_ical_reply(original, &response, "attendee@example.com", Some("Attendee")).unwrap();
        assert!(reply.contains("METHOD:REPLY"));
        assert!(reply.contains("PARTSTAT=ACCEPTED"));
        assert!(reply.contains("mailto:attendee@example.com"));
    }
}

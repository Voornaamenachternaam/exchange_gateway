// src/meeting/attendee.rs
use crate::util::normalize_email;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AttendeeStatus {
    #[default]
    NeedsAction = 0,
    Accepted = 1,
    Declined = 2,
    Tentative = 3,
    NotResponded = 5,
}

impl From<u8> for AttendeeStatus {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::NeedsAction,
            1 => Self::Accepted,
            2 => Self::Declined,
            3 => Self::Tentative,
            5 => Self::NotResponded,
            _ => Self::NeedsAction,
        }
    }
}

impl fmt::Display for AttendeeStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NeedsAction => write!(f, "NEEDS-ACTION"),
            Self::Accepted => write!(f, "ACCEPTED"),
            Self::Declined => write!(f, "DECLINED"),
            Self::Tentative => write!(f, "TENTATIVE"),
            Self::NotResponded => write!(f, "NEEDS-ACTION"),
        }
    }
}

impl AttendeeStatus {
    pub fn from_partstat(partstat: &str) -> Self {
        match partstat.to_uppercase().as_str() {
            "ACCEPTED" => Self::Accepted,
            "DECLINED" => Self::Declined,
            "TENTATIVE" => Self::Tentative,
            "NEEDS-ACTION" | "DELEGATED" | "IN-PROCESS" => Self::NeedsAction,
            _ => Self::NeedsAction,
        }
    }

    pub fn to_partstat(&self) -> &'static str {
        match self {
            Self::Accepted => "ACCEPTED",
            Self::Declined => "DECLINED",
            Self::Tentative => "TENTATIVE",
            Self::NeedsAction | Self::NotResponded => "NEEDS-ACTION",
        }
    }

    pub fn to_eas_status(&self) -> u8 {
        match self {
            Self::NeedsAction => 0,
            Self::Accepted => 3,
            Self::Declined => 4,
            Self::Tentative => 2,
            Self::NotResponded => 5,
        }
    }

    pub fn to_ews_response_type(&self) -> &'static str {
        match self {
            Self::Accepted => "Accept",
            Self::Declined => "Decline",
            Self::Tentative => "Tentative",
            Self::NeedsAction | Self::NotResponded => "NoResponseReceived",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AttendeeRole {
    #[default]
    Required = 1,
    Optional = 2,
    Resource = 3,
}

impl From<u8> for AttendeeRole {
    fn from(value: u8) -> Self {
        match value {
            1 => Self::Required,
            2 => Self::Optional,
            3 => Self::Resource,
            _ => Self::Required,
        }
    }
}

impl fmt::Display for AttendeeRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Required => write!(f, "REQ-PARTICIPANT"),
            Self::Optional => write!(f, "OPT-PARTICIPANT"),
            Self::Resource => write!(f, "NON-PARTICIPANT"),
        }
    }
}

impl AttendeeRole {
    pub fn from_ical_role(role: &str) -> Self {
        match role.to_uppercase().as_str() {
            "REQ-PARTICIPANT" | "CHAIR" => Self::Required,
            "OPT-PARTICIPANT" => Self::Optional,
            "NON-PARTICIPANT" => Self::Resource,
            _ => Self::Required,
        }
    }

    pub fn to_ical_role(&self) -> &'static str {
        match self {
            Self::Required => "REQ-PARTICIPANT",
            Self::Optional => "OPT-PARTICIPANT",
            Self::Resource => "NON-PARTICIPANT",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttendeeResponse {
    pub email: String,
    pub name: Option<String>,
    pub status: AttendeeStatus,
    pub role: AttendeeRole,
    pub response_time: Option<DateTime<Utc>>,
    pub proposed_start: Option<DateTime<Utc>>,
    pub proposed_end: Option<DateTime<Utc>>,
    pub sequence: u32,
}

impl AttendeeResponse {
    pub fn new(email: String, name: Option<String>, role: AttendeeRole) -> Self {
        Self {
            email,
            name,
            status: AttendeeStatus::NeedsAction,
            role,
            response_time: None,
            proposed_start: None,
            proposed_end: None,
            sequence: 0,
        }
    }

    pub fn respond(&mut self, status: AttendeeStatus) {
        self.status = status;
        self.response_time = Some(Utc::now());
    }

    pub fn propose_new_time(&mut self, start: DateTime<Utc>, end: DateTime<Utc>) {
        self.proposed_start = Some(start);
        self.proposed_end = Some(end);
        self.status = AttendeeStatus::Tentative;
        self.response_time = Some(Utc::now());
    }

    pub fn is_counter_proposal(&self) -> bool {
        self.proposed_start.is_some() || self.proposed_end.is_some()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttendeeRecord {
    pub meeting_uid: String,
    pub owner: String,
    pub email: String,
    pub name: Option<String>,
    pub status: u8,
    pub role: u8,
    pub response_time: Option<DateTime<Utc>>,
    pub sequence: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AttendeeRecord {
    pub fn from_response(meeting_uid: String, owner: String, response: &AttendeeResponse) -> Self {
        Self {
            meeting_uid,
            owner,
            email: response.email.clone(),
            name: response.name.clone(),
            status: response.status as u8,
            role: response.role as u8,
            response_time: response.response_time,
            sequence: response.sequence,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    pub fn to_response(&self) -> AttendeeResponse {
        AttendeeResponse {
            email: self.email.clone(),
            name: self.name.clone(),
            status: AttendeeStatus::from(self.status),
            role: AttendeeRole::from(self.role),
            response_time: self.response_time,
            proposed_start: None,
            proposed_end: None,
            sequence: self.sequence,
        }
    }
}

pub struct AttendeeTracker {
    attendees: Vec<AttendeeResponse>,
    meeting_uid: String,
}

impl AttendeeTracker {
    pub fn new(meeting_uid: String) -> Self {
        Self {
            attendees: Vec::new(),
            meeting_uid,
        }
    }

    pub fn meeting_uid(&self) -> &str {
        &self.meeting_uid
    }

    pub fn attendees(&self) -> &[AttendeeResponse] {
        &self.attendees
    }

    pub fn add_attendee(&mut self, email: String, name: Option<String>, role: AttendeeRole) {
        let email = normalize_email(&email);
        if !self.attendees.iter().any(|a| a.email == email) {
            self.attendees
                .push(AttendeeResponse::new(email, name, role));
        }
    }

    pub fn remove_attendee(&mut self, email: &str) -> bool {
        let email = normalize_email(email);
        let len_before = self.attendees.len();
        self.attendees.retain(|a| a.email != email);
        self.attendees.len() != len_before
    }

    pub fn record_response(
        &mut self,
        email: &str,
        status: AttendeeStatus,
    ) -> Result<(), &'static str> {
        let email = normalize_email(email);
        if let Some(attendee) = self.attendees.iter_mut().find(|a| a.email == email) {
            attendee.respond(status);
            Ok(())
        } else {
            Err("Attendee not found")
        }
    }

    pub fn record_counter_proposal(
        &mut self,
        email: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<(), &'static str> {
        let email = normalize_email(email);
        if let Some(attendee) = self.attendees.iter_mut().find(|a| a.email == email) {
            attendee.propose_new_time(start, end);
            Ok(())
        } else {
            Err("Attendee not found")
        }
    }

    pub fn get_attendee(&self, email: &str) -> Option<&AttendeeResponse> {
        let email = normalize_email(email);
        self.attendees.iter().find(|a| a.email == email)
    }

    pub fn get_attendee_mut(&mut self, email: &str) -> Option<&mut AttendeeResponse> {
        let email = normalize_email(email);
        self.attendees.iter_mut().find(|a| a.email == email)
    }

    pub fn required_attendees(&self) -> Vec<&AttendeeResponse> {
        self.attendees
            .iter()
            .filter(|a| a.role == AttendeeRole::Required)
            .collect()
    }

    pub fn optional_attendees(&self) -> Vec<&AttendeeResponse> {
        self.attendees
            .iter()
            .filter(|a| a.role == AttendeeRole::Optional)
            .collect()
    }

    pub fn resource_attendees(&self) -> Vec<&AttendeeResponse> {
        self.attendees
            .iter()
            .filter(|a| a.role == AttendeeRole::Resource)
            .collect()
    }

    pub fn all_required_responded(&self) -> bool {
        self.required_attendees().iter().all(|a| {
            a.status != AttendeeStatus::NeedsAction && a.status != AttendeeStatus::NotResponded
        })
    }

    pub fn all_accepted(&self) -> bool {
        self.attendees
            .iter()
            .all(|a| a.status == AttendeeStatus::Accepted)
    }

    pub fn any_declined(&self) -> bool {
        self.attendees
            .iter()
            .any(|a| a.status == AttendeeStatus::Declined)
    }

    pub fn response_summary(&self) -> AttendeeSummary {
        let mut summary = AttendeeSummary::default();
        for attendee in &self.attendees {
            match attendee.status {
                AttendeeStatus::Accepted => summary.accepted += 1,
                AttendeeStatus::Declined => summary.declined += 1,
                AttendeeStatus::Tentative => summary.tentative += 1,
                AttendeeStatus::NeedsAction | AttendeeStatus::NotResponded => summary.pending += 1,
            }
        }
        summary.total = self.attendees.len();
        summary
    }

    pub fn to_records(&self, owner: &str) -> Vec<AttendeeRecord> {
        self.attendees
            .iter()
            .map(|a| AttendeeRecord::from_response(self.meeting_uid.clone(), owner.to_string(), a))
            .collect()
    }

    pub fn load_from_records(records: Vec<AttendeeRecord>) -> Self {
        let meeting_uid = records
            .first()
            .map(|r| r.meeting_uid.clone())
            .unwrap_or_default();
        let attendees = records.into_iter().map(|r| r.to_response()).collect();
        Self {
            attendees,
            meeting_uid,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct AttendeeSummary {
    pub total: usize,
    pub accepted: usize,
    pub declined: usize,
    pub tentative: usize,
    pub pending: usize,
}

impl AttendeeSummary {
    pub fn is_quorum_reached(&self, quorum_size: usize) -> bool {
        self.accepted >= quorum_size
    }

    pub fn acceptance_rate(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        (self.accepted + self.tentative) as f64 / self.total as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_attendee_status_conversion() {
        assert_eq!(
            AttendeeStatus::from_partstat("ACCEPTED"),
            AttendeeStatus::Accepted
        );
        assert_eq!(
            AttendeeStatus::from_partstat("DECLINED"),
            AttendeeStatus::Declined
        );
        assert_eq!(
            AttendeeStatus::from_partstat("TENTATIVE"),
            AttendeeStatus::Tentative
        );
        assert_eq!(
            AttendeeStatus::from_partstat("NEEDS-ACTION"),
            AttendeeStatus::NeedsAction
        );
    }

    #[test]
    fn test_attendee_role_conversion() {
        assert_eq!(
            AttendeeRole::from_ical_role("REQ-PARTICIPANT"),
            AttendeeRole::Required
        );
        assert_eq!(
            AttendeeRole::from_ical_role("OPT-PARTICIPANT"),
            AttendeeRole::Optional
        );
        assert_eq!(
            AttendeeRole::from_ical_role("NON-PARTICIPANT"),
            AttendeeRole::Resource
        );
    }

    #[test]
    fn test_attendee_tracker() {
        let mut tracker = AttendeeTracker::new("test-uid".to_string());
        tracker.add_attendee(
            "user1@example.com".to_string(),
            Some("User One".to_string()),
            AttendeeRole::Required,
        );
        tracker.add_attendee(
            "user2@example.com".to_string(),
            Some("User Two".to_string()),
            AttendeeRole::Optional,
        );

        assert_eq!(tracker.attendees().len(), 2);
        assert!(tracker.required_attendees().len() == 1);
        assert!(tracker.optional_attendees().len() == 1);

        tracker
            .record_response("user1@example.com", AttendeeStatus::Accepted)
            .unwrap();
        assert_eq!(
            tracker.get_attendee("user1@example.com").unwrap().status,
            AttendeeStatus::Accepted
        );
    }

    #[test]
    fn test_response_summary() {
        let mut tracker = AttendeeTracker::new("test-uid".to_string());
        tracker.add_attendee(
            "user1@example.com".to_string(),
            None,
            AttendeeRole::Required,
        );
        tracker.add_attendee(
            "user2@example.com".to_string(),
            None,
            AttendeeRole::Required,
        );
        tracker.add_attendee(
            "user3@example.com".to_string(),
            None,
            AttendeeRole::Optional,
        );

        tracker
            .record_response("user1@example.com", AttendeeStatus::Accepted)
            .unwrap();
        tracker
            .record_response("user2@example.com", AttendeeStatus::Declined)
            .unwrap();

        let summary = tracker.response_summary();
        assert_eq!(summary.total, 3);
        assert_eq!(summary.accepted, 1);
        assert_eq!(summary.declined, 1);
        assert_eq!(summary.pending, 1);
    }
}

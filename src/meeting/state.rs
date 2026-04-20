// src/meeting/state.rs
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

const ASF_MEETING: u8 = 0x01;
const ASF_RECEIVED: u8 = 0x02;
const ASF_CANCELED: u8 = 0x04;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[derive(Default)]
pub enum MeetingStatus {
    #[default]
    Appointment = 0,
    Organizer = 1,
    Tentative = 2,
    Accepted = 3,
    Rejected = 4,
    OrganizerCanceled = 5,
    ReceivedCanceled = 7,
}


impl From<u8> for MeetingStatus {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Appointment,
            1 => Self::Organizer,
            2 => Self::Tentative,
            3 => Self::Accepted,
            4 => Self::Rejected,
            5 => Self::OrganizerCanceled,
            7 => Self::ReceivedCanceled,
            _ => Self::Appointment,
        }
    }
}

impl fmt::Display for MeetingStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Appointment => write!(f, "Appointment"),
            Self::Organizer => write!(f, "Organizer"),
            Self::Tentative => write!(f, "Tentative"),
            Self::Accepted => write!(f, "Accepted"),
            Self::Rejected => write!(f, "Rejected"),
            Self::OrganizerCanceled => write!(f, "OrganizerCanceled"),
            Self::ReceivedCanceled => write!(f, "ReceivedCanceled"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingStateFlags {
    pub is_meeting: bool,
    pub is_received: bool,
    pub is_canceled: bool,
}

impl MeetingStateFlags {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_byte(value: u8) -> Self {
        Self {
            is_meeting: value & ASF_MEETING != 0,
            is_received: value & ASF_RECEIVED != 0,
            is_canceled: value & ASF_CANCELED != 0,
        }
    }

    pub fn to_byte(&self) -> u8 {
        let mut value = 0u8;
        if self.is_meeting {
            value |= ASF_MEETING;
        }
        if self.is_received {
            value |= ASF_RECEIVED;
        }
        if self.is_canceled {
            value |= ASF_CANCELED;
        }
        value
    }

    pub fn to_meeting_status(&self, is_organizer: bool, response_type: Option<u8>) -> MeetingStatus {
        if self.is_canceled {
            if is_organizer {
                return MeetingStatus::OrganizerCanceled;
            } else {
                return MeetingStatus::ReceivedCanceled;
            }
        }
        
        if !self.is_meeting {
            return MeetingStatus::Appointment;
        }
        
        if is_organizer {
            return MeetingStatus::Organizer;
        }
        
        match response_type {
            Some(2) => MeetingStatus::Tentative,
            Some(3) => MeetingStatus::Accepted,
            Some(4) => MeetingStatus::Rejected,
            _ => MeetingStatus::Tentative,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[derive(Default)]
pub enum MeetingState {
    #[default]
    Draft,
    RequestSent,
    PendingResponses,
    Confirmed,
    Cancelled,
    Completed,
}


impl fmt::Display for MeetingState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Draft => write!(f, "Draft"),
            Self::RequestSent => write!(f, "RequestSent"),
            Self::PendingResponses => write!(f, "PendingResponses"),
            Self::Confirmed => write!(f, "Confirmed"),
            Self::Cancelled => write!(f, "Cancelled"),
            Self::Completed => write!(f, "Completed"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MeetingContext {
    pub uid: String,
    pub sequence: u32,
    pub organizer_email: String,
    pub organizer_name: Option<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub state: MeetingState,
    pub state_flags: MeetingStateFlags,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_sequence_time: Option<DateTime<Utc>>,
}

impl MeetingContext {
    pub fn new(uid: String, organizer_email: String, start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        Self::with_timestamp(uid, organizer_email, start, end, Utc::now())
    }

    /// Create a new MeetingContext with a specific timestamp (for testing)
    pub fn with_timestamp(
        uid: String,
        organizer_email: String,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            uid,
            sequence: 0,
            organizer_email,
            organizer_name: None,
            start,
            end,
            state: MeetingState::Draft,
            state_flags: MeetingStateFlags::default(),
            created_at: now,
            updated_at: now,
            last_sequence_time: None,
        }
    }

    pub fn increment_sequence(&mut self) {
        self.increment_sequence_with_timestamp(Utc::now())
    }

    /// Increment sequence with a specific timestamp (for testing)
    pub fn increment_sequence_with_timestamp(&mut self, now: DateTime<Utc>) {
        self.sequence = self.sequence.saturating_add(1);
        self.last_sequence_time = Some(now);
        self.updated_at = now;
    }

    pub fn is_past_meeting(&self) -> bool {
        self.is_past_meeting_at(Utc::now())
    }

    /// Check if meeting is past at a specific timestamp (for testing)
    pub fn is_past_meeting_at(&self, now: DateTime<Utc>) -> bool {
        self.end < now
    }
}

pub struct MeetingStateMachine {
    context: MeetingContext,
}

impl MeetingStateMachine {
    pub fn new(context: MeetingContext) -> Self {
        Self { context }
    }

    pub fn context(&self) -> &MeetingContext {
        &self.context
    }

    pub fn context_mut(&mut self) -> &mut MeetingContext {
        &mut self.context
    }

    pub fn current_state(&self) -> &MeetingState {
        &self.context.state
    }

    pub fn can_send_request(&self) -> bool {
        matches!(self.context.state, MeetingState::Draft)
    }

    pub fn can_update(&self) -> bool {
        matches!(
            self.context.state,
            MeetingState::RequestSent | MeetingState::PendingResponses | MeetingState::Confirmed
        )
    }

    pub fn can_cancel(&self) -> bool {
        matches!(
            self.context.state,
            MeetingState::RequestSent | MeetingState::PendingResponses | MeetingState::Confirmed
        )
    }

    pub fn can_respond(&self) -> bool {
        matches!(
            self.context.state,
            MeetingState::RequestSent | MeetingState::PendingResponses
        )
    }

    pub fn send_request(&mut self) -> Result<(), &'static str> {
        if !self.can_send_request() {
            return Err("Cannot send request from current state");
        }
        self.context.state_flags.is_meeting = true;
        self.context.state = MeetingState::RequestSent;
        self.context.increment_sequence();
        Ok(())
    }

    pub fn receive_request(&mut self) -> Result<(), &'static str> {
        self.receive_request_with_timestamp(Utc::now())
    }

    /// Receive request with a specific timestamp (for testing)
    pub fn receive_request_with_timestamp(&mut self, now: DateTime<Utc>) -> Result<(), &'static str> {
        if self.context.state != MeetingState::Draft {
            return Err("Cannot receive request in current state");
        }
        self.context.state_flags.is_meeting = true;
        self.context.state_flags.is_received = true;
        self.context.state = MeetingState::PendingResponses;
        self.context.updated_at = now;
        Ok(())
    }

    pub fn update_meeting(&mut self, significant_change: bool) -> Result<(), &'static str> {
        self.update_meeting_with_timestamp(significant_change, Utc::now())
    }

    /// Update meeting with a specific timestamp (for testing)
    pub fn update_meeting_with_timestamp(&mut self, significant_change: bool, now: DateTime<Utc>) -> Result<(), &'static str> {
        if !self.can_update() {
            return Err("Cannot update meeting from current state");
        }
        if significant_change {
            self.context.increment_sequence_with_timestamp(now);
        }
        self.context.updated_at = now;
        Ok(())
    }

    pub fn cancel(&mut self) -> Result<(), &'static str> {
        if !self.can_cancel() {
            return Err("Cannot cancel meeting from current state");
        }
        self.context.state_flags.is_canceled = true;
        self.context.state = MeetingState::Cancelled;
        self.context.increment_sequence();
        Ok(())
    }

    pub fn mark_completed(&mut self) -> Result<(), &'static str> {
        self.mark_completed_with_timestamp(Utc::now())
    }

    /// Mark as completed with a specific timestamp (for testing)
    pub fn mark_completed_with_timestamp(&mut self, now: DateTime<Utc>) -> Result<(), &'static str> {
        if !self.context.is_past_meeting_at(now) {
            return Err("Meeting has not ended yet");
        }
        self.context.state = MeetingState::Completed;
        self.context.updated_at = now;
        Ok(())
    }

    pub fn transition_to_pending(&mut self) {
        self.transition_to_pending_with_timestamp(Utc::now())
    }

    /// Transition to pending with a specific timestamp (for testing)
    pub fn transition_to_pending_with_timestamp(&mut self, now: DateTime<Utc>) {
        if self.context.state == MeetingState::RequestSent {
            self.context.state = MeetingState::PendingResponses;
            self.context.updated_at = now;
        }
    }

    pub fn transition_to_confirmed(&mut self) {
        self.transition_to_confirmed_with_timestamp(Utc::now())
    }

    /// Transition to confirmed with a specific timestamp (for testing)
    pub fn transition_to_confirmed_with_timestamp(&mut self, now: DateTime<Utc>) {
        self.context.state = MeetingState::Confirmed;
        self.context.updated_at = now;
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MeetingStateRecord {
    pub uid: String,
    pub owner: String,
    pub sequence: u32,
    pub state: String,
    pub state_flags: u8,
    pub is_organizer: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl MeetingStateRecord {
    pub fn from_context(owner: String, is_organizer: bool, ctx: &MeetingContext) -> Self {
        Self {
            uid: ctx.uid.clone(),
            owner,
            sequence: ctx.sequence,
            state: ctx.state.to_string(),
            state_flags: ctx.state_flags.to_byte(),
            is_organizer,
            created_at: ctx.created_at,
            updated_at: ctx.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_flags_from_byte() {
        let flags = MeetingStateFlags::from_byte(0x01);
        assert!(flags.is_meeting);
        assert!(!flags.is_received);
        assert!(!flags.is_canceled);

        let flags = MeetingStateFlags::from_byte(0x03);
        assert!(flags.is_meeting);
        assert!(flags.is_received);
        assert!(!flags.is_canceled);

        let flags = MeetingStateFlags::from_byte(0x07);
        assert!(flags.is_meeting);
        assert!(flags.is_received);
        assert!(flags.is_canceled);
    }

    #[test]
    fn test_state_flags_to_byte() {
        let flags = MeetingStateFlags {
            is_meeting: true,
            is_received: false,
            is_canceled: false,
        };
        assert_eq!(flags.to_byte(), 0x01);

        let flags = MeetingStateFlags {
            is_meeting: true,
            is_received: true,
            is_canceled: false,
        };
        assert_eq!(flags.to_byte(), 0x03);

        let flags = MeetingStateFlags {
            is_meeting: true,
            is_received: true,
            is_canceled: true,
        };
        assert_eq!(flags.to_byte(), 0x07);
    }

    #[test]
    fn test_meeting_state_machine_transitions() {
        let ctx = MeetingContext::new(
            "test-uid".to_string(),
            "organizer@example.com".to_string(),
            Utc::now(),
            Utc::now() + chrono::Duration::hours(1),
        );
        let mut machine = MeetingStateMachine::new(ctx);

        assert!(machine.can_send_request());
        assert!(!machine.can_cancel());

        machine.send_request().unwrap();
        assert_eq!(machine.current_state(), &MeetingState::RequestSent);
        assert!(machine.can_update());
        assert!(machine.can_cancel());
    }

    #[test]
    fn test_sequence_increment() {
        let ctx = MeetingContext::new(
            "test-uid".to_string(),
            "organizer@example.com".to_string(),
            Utc::now(),
            Utc::now() + chrono::Duration::hours(1),
        );
        let mut machine = MeetingStateMachine::new(ctx);

        assert_eq!(machine.context().sequence, 0);
        
        machine.send_request().unwrap();
        assert_eq!(machine.context().sequence, 1);
        
        machine.update_meeting(true).unwrap();
        assert_eq!(machine.context().sequence, 2);
    }

    #[test]
    fn test_meeting_status_conversion() {
        let flags = MeetingStateFlags::from_byte(0x01);
        let status = flags.to_meeting_status(true, None);
        assert_eq!(status, MeetingStatus::Organizer);

        let flags = MeetingStateFlags::from_byte(0x03);
        let status = flags.to_meeting_status(false, Some(3));
        assert_eq!(status, MeetingStatus::Accepted);

        let flags = MeetingStateFlags::from_byte(0x05);
        let status = flags.to_meeting_status(true, None);
        assert_eq!(status, MeetingStatus::OrganizerCanceled);
    }
}
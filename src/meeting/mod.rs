// src/meeting/mod.rs
pub mod attendee;
pub mod message;
pub mod response;
pub mod scheduling;
pub mod state;

pub use attendee::{AttendeeResponse, AttendeeRole, AttendeeStatus, AttendeeTracker};
pub use message::{MeetingMessage, MeetingMessageGenerator, MeetingMessageType};
pub use response::{
    MeetingInvitation, ResponseDecision, parse_meeting_request, submit_meeting_response,
};
pub use state::{MeetingState, MeetingStateFlags, MeetingStateMachine, MeetingStatus};

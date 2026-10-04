// src/meeting/mod.rs
pub mod attendee;
pub mod message;
pub mod response;
pub mod rsvp;
pub mod scheduling;
pub mod state;

pub use attendee::{AttendeeResponse, AttendeeRole, AttendeeStatus, AttendeeTracker};
pub use message::{
    MeetingMessage, MeetingMessageGenerator, MeetingMessageType, MeetingResponseResult,
};
pub use response::{
    MeetingInvitation, ResponseDecision, parse_meeting_request, submit_meeting_response,
};
pub use rsvp::{
    ResolveFailure, RsvpOutcome, RsvpRequest, RsvpSource, STATUS_INSTANCE_MALFORMED,
    STATUS_INSTANCE_NOT_RECURRING, STATUS_INVALID_ITEM, STATUS_SERVER_ERROR, STATUS_SUCCESS,
};
pub use state::{MeetingState, MeetingStateFlags, MeetingStateMachine, MeetingStatus};

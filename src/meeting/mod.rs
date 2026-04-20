// src/meeting/mod.rs
pub mod state;
pub mod message;
pub mod attendee;
pub mod scheduling;

pub use state::{MeetingState, MeetingStateFlags, MeetingStateMachine, MeetingStatus};
pub use message::{MeetingMessage, MeetingMessageType, MeetingMessageGenerator};
pub use attendee::{AttendeeTracker, AttendeeResponse, AttendeeRole, AttendeeStatus};
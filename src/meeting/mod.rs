// src/meeting/mod.rs
pub mod attendee;
pub mod message;
pub mod scheduling;
pub mod state;

pub use attendee::{AttendeeResponse, AttendeeRole, AttendeeStatus, AttendeeTracker};
pub use message::{MeetingMessage, MeetingMessageGenerator, MeetingMessageType};
pub use scheduling::{CaldavScheduling, SchedulingError, SchedulingResult};
pub use state::{MeetingState, MeetingStateFlags, MeetingStateMachine, MeetingStatus};

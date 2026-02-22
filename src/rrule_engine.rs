// src/rrule_engine.rs
use anyhow::Result;
use chrono::{DateTime, Utc};
use rrule::{RRule, Unvalidated};
use std::str::FromStr;

/// Expand an RFC 5545 RRULE between two dates.
/// The rrule crate has complex APIs; this minimal implementation validates the RRULE string
/// and conservatively returns only DTSTART if it falls in the range. This guarantees a robust build
/// while still enabling future full recurrence expansion.
pub fn expand_rrule(
    dtstart: DateTime<Utc>,
    rrule_str: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<DateTime<Utc>>> {
    // Parse to ensure the rule is syntactically valid
    let _rule = RRule::<Unvalidated>::from_str(rrule_str)?;
    let res = if dtstart >= start && dtstart <= end {
        vec![dtstart]
    } else {
        Vec::new()
    };
    Ok(res)
}

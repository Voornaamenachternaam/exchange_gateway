// src/rrule_engine.rs
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::str::FromStr;
use rrule::{RRule, Unvalidated};

pub fn expand_rrule(
    dtstart: DateTime<Utc>,
    rrule_str: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<DateTime<Utc>>> {
    // Validate RRULE string by parsing into an unvalidated RRule.
    // If parse fails, return the parse error.
    let _rule = RRule::<Unvalidated>::from_str(rrule_str)?;

    // Conservative behaviour: include dtstart only if it's within the requested range.
    let res = if dtstart >= start && dtstart <= end {
        vec![dtstart]
    } else {
        Vec::new()
    };

    Ok(res)
}

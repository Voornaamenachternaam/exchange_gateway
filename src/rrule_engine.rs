// src/rrule_engine.rs
use anyhow::Result;
use chrono::{DateTime, Utc};
use rrule::{RRuleSet, Tz};

/// Expand an RFC 5545 RRULE between two dates.
pub fn expand_rrule(
    _dtstart: DateTime<Utc>,
    rrule_str: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<DateTime<Utc>>> {
    let rrule_set: RRuleSet = rrule_str.parse()?;
    let tz_start: DateTime<Tz> = start.with_timezone(&Tz::UTC);
    let tz_end: DateTime<Tz> = end.with_timezone(&Tz::UTC);
    let result = rrule_set.after(tz_start).before(tz_end).all(u16::MAX);
    let res: Vec<DateTime<Utc>> = result.dates.into_iter()
        .map(|d| d.with_timezone(&Utc))
        .collect();
    Ok(res)
}

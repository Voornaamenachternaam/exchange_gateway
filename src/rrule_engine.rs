use anyhow::Result;
use chrono::{DateTime, Utc};
use rrule::RRule;

/// Expand RRULE into occurrences between start..end
pub fn expand_rrule(dtstart: DateTime<Utc>, rrule_str: &str, start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Vec<DateTime<Utc>>> {
    let rule = rrule_str.parse::<RRule>()?;
    let all = rule.all(chrono::Utc, Some(dtstart), Some(end))?;
    let res: Vec<DateTime<Utc>> = all.into_iter().filter(|d| *d >= start && *d <= end).collect();
    Ok(res)
}

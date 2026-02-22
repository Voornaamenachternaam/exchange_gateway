// src/utils.rs
use anyhow::Result;
use std::time::{SystemTime, UNIX_EPOCH};

/// Return a simple unix timestamp string used for lightweight ids
pub fn unix_ts() -> Result<String> {
    let now = SystemTime::now();
    let dur = now.duration_since(UNIX_EPOCH)?;
    Ok(format!("{}", dur.as_secs()))
}

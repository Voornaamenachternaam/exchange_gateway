pub fn escape_xml(s: &str) -> String {
    quick_xml::escape::escape(s).into_owned()
}

/// Parse a local datetime string into UTC RFC 3339 format.
/// Handles UTC‑suffixed strings, normal local times, DST gaps, and ambiguities.
pub fn parse_local_to_utc(local_str: &str, tz: chrono_tz::Tz) -> String {
    use chrono::{TimeZone, Utc};

    // Already UTC (trailing Z)
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(local_str, "%Y-%m-%dT%H:%M:%SZ") {
        return Utc.from_utc_datetime(&dt).to_rfc3339();
    }

    // Parse as naive local datetime
    let naive = match chrono::NaiveDateTime::parse_from_str(local_str, "%Y-%m-%dT%H:%M:%S") {
        Ok(dt) => dt,
        Err(_) => return local_str.to_string(), // unparseable → return as‑is
    };

    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => dt.with_timezone(&Utc).to_rfc3339(),
        chrono::LocalResult::Ambiguous(earliest, _) => earliest.with_timezone(&Utc).to_rfc3339(),
        chrono::LocalResult::None => {
            // DST gap: local time does not exist. Advance minute by minute
            // until we find a valid local time, then resolve to UTC.
            let mut advanced = naive;
            for _ in 0..(24 * 60) {
                advanced = match advanced.checked_add_signed(chrono::TimeDelta::minutes(1)) {
                    Some(next) => next,
                    None => return local_str.to_string(),
                };
                match tz.from_local_datetime(&advanced) {
                    chrono::LocalResult::Single(dt) => return dt.with_timezone(&Utc).to_rfc3339(),
                    chrono::LocalResult::Ambiguous(earliest, _) => {
                        return earliest.with_timezone(&Utc).to_rfc3339();
                    }
                    chrono::LocalResult::None => continue,
                }
            }
            local_str.to_string()
        }
    }
}

/// Decode a Basic Authentication header into username and password.
///
/// The header must start with "Basic" (case‑insensitive), followed by
/// whitespace, and a base64‑encoded string of the form `username:password`.
/// Returns `None` if the header is malformed, the base64 decode fails,
/// or the split yields fewer than two parts.
pub fn decode_basic_auth(auth_header: &str) -> Option<(String, String)> {
    // Trim leading whitespace per RFC 7230 (optional whitespace before scheme)
    let trimmed = auth_header.trim_start();

    // Split on any whitespace to separate the scheme and the token.
    let mut parts = trimmed.split_whitespace();
    let scheme = parts.next()?;
    let encoded = parts.next()?;

    // Scheme must be "Basic" (case‑insensitive)
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }

    // Decode base64
    let decoded = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        encoded.as_bytes(),
    )
    .ok()?;

    // Convert to UTF-8 (should be ASCII, but we accept valid UTF-8)
    let decoded_str = String::from_utf8(decoded).ok()?;

    // Split on the first colon
    let (user, pass) = decoded_str.split_once(':')?;

    // Return owned strings (allow empty username/password if present)
    Some((user.to_string(), pass.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_local_to_utc_normal_time() {
        // 2024-01-15 10:30:00 EST (UTC-5) → 2024-01-15 15:30:00 UTC
        let result = parse_local_to_utc("2024-01-15T10:30:00", chrono_tz::US::Eastern);
        assert_eq!(result, "2024-01-15T15:30:00+00:00");
    }

    #[test]
    fn parse_local_to_utc_utc_suffix() {
        // Input already tagged with Z → treat as UTC
        let result = parse_local_to_utc("2024-01-15T10:30:00Z", chrono_tz::US::Eastern);
        assert_eq!(result, "2024-01-15T10:30:00+00:00");
    }

    #[test]
    fn parse_local_to_utc_dst_gap_returns_valid_rfc3339() {
        // 2024-03-10 02:30:00 US/Eastern does not exist (clocks spring forward
        // from 2:00 to 3:00). The function must still return valid RFC 3339.
        let result = parse_local_to_utc("2024-03-10T02:30:00", chrono_tz::US::Eastern);
        assert!(
            result.ends_with("+00:00"),
            "Expected UTC RFC 3339 but got: {result}"
        );
        assert!(
            chrono::DateTime::parse_from_rfc3339(&result).is_ok(),
            "Result is not valid RFC 3339: {result}"
        );
    }

    #[test]
    fn parse_local_to_utc_ambiguous_uses_earliest() {
        // 2024-11-03 01:30:00 US/Eastern is ambiguous (fall-back).
        // The function should pick the earliest (EDT, UTC-4) → 05:30 UTC.
        let result = parse_local_to_utc("2024-11-03T01:30:00", chrono_tz::US::Eastern);
        assert_eq!(result, "2024-11-03T05:30:00+00:00");
    }

    #[test]
    fn parse_local_to_utc_unparseable_returns_input() {
        let result = parse_local_to_utc("not-a-date", chrono_tz::US::Eastern);
        assert_eq!(result, "not-a-date");
    }

    #[test]
    fn decode_basic_auth_valid() {
        let auth = "Basic dXNlcjpwYXNz"; // base64 of "user:pass"
        let result = decode_basic_auth(auth);
        assert_eq!(result, Some(("user".to_string(), "pass".to_string())));
    }

    #[test]
    fn decode_basic_auth_case_insensitive() {
        let auth = "basic dXNlcjpwYXNz";
        let result = decode_basic_auth(auth);
        assert_eq!(result, Some(("user".to_string(), "pass".to_string())));
    }

    #[test]
    fn decode_basic_auth_multiple_spaces() {
        let auth = "Basic   dXNlcjpwYXNz";
        let result = decode_basic_auth(auth);
        assert_eq!(result, Some(("user".to_string(), "pass".to_string())));
    }

    #[test]
    fn decode_basic_auth_leading_whitespace() {
        let auth = "  Basic dXNlcjpwYXNz";
        let result = decode_basic_auth(auth);
        assert_eq!(result, Some(("user".to_string(), "pass".to_string())));
    }

    #[test]
    fn decode_basic_auth_missing_prefix() {
        assert_eq!(decode_basic_auth("Bearer token"), None);
    }

    #[test]
    fn decode_basic_auth_invalid_base64() {
        let auth = "Basic not-base64!";
        assert_eq!(decode_basic_auth(auth), None);
    }

    #[test]
    fn decode_basic_auth_no_colon() {
        let auth = "Basic dXNlcnBhc3M="; // base64 of "userpass"
        assert_eq!(decode_basic_auth(auth), None);
    }

    #[test]
    fn decode_basic_auth_empty_username() {
        let auth = "Basic OnBhc3M="; // base64 of ":pass"
        let result = decode_basic_auth(auth);
        assert_eq!(result, Some(("".to_string(), "pass".to_string())));
    }

    #[test]
    fn decode_basic_auth_empty_password() {
        let auth = "Basic dXNlcjo="; // base64 of "user:"
        let result = decode_basic_auth(auth);
        assert_eq!(result, Some(("user".to_string(), "".to_string())));
    }
} 

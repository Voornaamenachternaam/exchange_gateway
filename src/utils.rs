pub fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn parse_local_to_utc(local_str: &str, tz: chrono_tz::Tz) -> String {
    use chrono::{TimeZone, Utc};
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(local_str, "%Y-%m-%dT%H:%M:%SZ") {
        return Utc.from_utc_datetime(&dt).to_rfc3339();
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(local_str, "%Y-%m-%dT%H:%M:%S") {
        return match tz.from_local_datetime(&dt) {
            chrono::LocalResult::Single(dt) => dt.with_timezone(&Utc).to_rfc3339(),
            chrono::LocalResult::Ambiguous(earliest, _) => earliest.with_timezone(&Utc).to_rfc3339(),
            chrono::LocalResult::None => {
                // DST gap: the local time does not exist. Advance by 1 hour and
                // resolve again so we always return a valid RFC 3339 UTC string.
                let advanced = dt + chrono::TimeDelta::hours(1);
                match tz.from_local_datetime(&advanced) {
                    chrono::LocalResult::Single(dt) => dt.with_timezone(&Utc).to_rfc3339(),
                    chrono::LocalResult::Ambiguous(earliest, _) => {
                        earliest.with_timezone(&Utc).to_rfc3339()
                    }
                    // Should not happen, but fall back to interpreting as UTC
                    // to guarantee a valid RFC 3339 result.
                    chrono::LocalResult::None => Utc.from_utc_datetime(&dt).to_rfc3339(),
                }
            }
        };
    }
    local_str.to_string()
}

pub fn decode_basic_auth(auth: &str) -> (String, String) {
    let parts: Vec<&str> = auth.split_whitespace().collect();
    if parts.len() != 2 || !parts[0].eq_ignore_ascii_case("Basic") {
        return (String::new(), String::new());
    }

    let decoded = match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, parts[1])
    {
        Ok(d) => d,
        Err(_) => return (String::new(), String::new()),
    };

    let decoded_str = match std::str::from_utf8(&decoded) {
        Ok(s) => s,
        Err(_) => return (String::new(), String::new()),
    };
    let mut creds = decoded_str.splitn(2, ':');
    (
        creds.next().unwrap_or_default().to_string(),
        creds.next().unwrap_or_default().to_string(),
    )
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
        // Must be parseable as RFC 3339 and end with +00:00 (UTC)
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
}

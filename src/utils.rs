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
            chrono::LocalResult::Ambiguous(earliest, _) => {
                earliest.with_timezone(&Utc).to_rfc3339()
            }
            chrono::LocalResult::None => {
                // DST gap: advance minute-by-minute until we find a valid local time.
                let mut advanced = dt;
                for _ in 0..(24 * 60) {
                    advanced = match advanced.checked_add_signed(chrono::TimeDelta::minutes(1)) {
                        Some(next) => next,
                        None => return local_str.to_string(),
                    };
                    match tz.from_local_datetime(&advanced) {
                        chrono::LocalResult::Single(dt) => {
                            return dt.with_timezone(&Utc).to_rfc3339();
                        }
                        chrono::LocalResult::Ambiguous(earliest, _) => {
                            return earliest.with_timezone(&Utc).to_rfc3339();
                        }
                        chrono::LocalResult::None => continue,
                    }
                }
                local_str.to_string()
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
        let result = parse_local_to_utc("2024-01-15T10:30:00", chrono_tz::US::Eastern);
        assert_eq!(result, "2024-01-15T15:30:00+00:00");
    }
}

// src/timezone.rs
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use chrono_tz::Tz;
use std::str::FromStr;
use strum::IntoEnumIterator;
use windows_timezones::WindowsTimezone;

const TZ_BLOB_LEN: usize = 172;

pub fn decode_eas_timezone_bias(b64: &str) -> Option<i32> {
    let bytes = BASE64.decode(b64.trim()).ok()?;
    if bytes.len() < 4 {
        return None;
    }
    Some(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_wchar_name(bytes: &[u8], offset: usize) -> String {
    let end = offset + 64;
    if end > bytes.len() {
        return String::new();
    }
    let chars: Vec<u16> = bytes[offset..end]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&c| c != 0)
        .collect();
    String::from_utf16_lossy(&chars).to_string()
}

fn write_wchar_name(blob: &mut [u8], offset: usize, name: &str) {
    let mut pos = offset;
    for ch in name.encode_utf16().take(32) {
        if pos + 2 > blob.len() {
            break;
        }
        let b = ch.to_le_bytes();
        blob[pos] = b[0];
        blob[pos + 1] = b[1];
        pos += 2;
    }
}

fn find_windows_timezone(name: &str) -> Option<WindowsTimezone> {
    let n = name.trim();
    if n.is_empty() {
        return None;
    }

    if let Ok(tz) = WindowsTimezone::from_str(n) {
        return Some(tz);
    }

    for variant in WindowsTimezone::iter() {
        if variant.name().eq_ignore_ascii_case(n) {
            return Some(variant);
        }
    }

    let n_lower = n.to_ascii_lowercase();
    WindowsTimezone::iter()
        .filter(|variant| n_lower.contains(&variant.name().to_ascii_lowercase()))
        .max_by_key(|variant| variant.name().len())
 }

fn windows_name_to_iana(name: &str) -> Option<&'static str> {
    let n = name.trim();
    if n.is_empty() {
        return None;
    }

    if let Some(tz) = find_windows_timezone(n) {
        return Some(tz.tzdb_id());
    }

    parse_utc_offset_name(&n.to_ascii_lowercase()) 
}

pub fn eas_timezone_blob_to_iana(b64: &str) -> Option<String> {
    let bytes = BASE64.decode(b64.trim()).ok()?;
    if bytes.len() < TZ_BLOB_LEN {
        return None;
    }
    let bias = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let std_name = read_wchar_name(&bytes, 4);
    let dst_name = read_wchar_name(&bytes, 88);

    if let Some(iana) = windows_name_to_iana(&std_name).or_else(|| windows_name_to_iana(&dst_name))
    {
        return Some(iana.to_string());
    }

    if bias == 0 {
        return Some("UTC".to_string());
    }
    let hours = bias / 60;
    Some(format!("Etc/GMT{:+}", hours))
}

pub fn eas_timezone_blob_to_tz(b64: &str) -> Option<Tz> {
    let iana = eas_timezone_blob_to_iana(b64)?;
    iana.parse().ok()
}

pub fn windows_timezone_name_to_tz(name: &str) -> Option<Tz> {
    if let Some(iana) = parse_utc_offset_name(&name.to_ascii_lowercase()) {
        return iana.parse().ok();
    }
    find_windows_timezone(name).map(Tz::from)
}

pub fn iana_to_eas_timezone_blob(iana: &str) -> Option<String> {
    let (bias, std_name, dst_name, std_date, dst_date, std_bias, dst_bias) =
        iana_to_windows_params(iana)?;
    let mut blob = [0u8; TZ_BLOB_LEN];
    blob[0..4].copy_from_slice(&bias.to_le_bytes());
    write_wchar_name(&mut blob, 4, std_name);
    blob[68..84].copy_from_slice(&std_date);
    blob[84..88].copy_from_slice(&std_bias.to_le_bytes());
    write_wchar_name(&mut blob, 88, dst_name);
    blob[152..168].copy_from_slice(&dst_date);
    blob[168..172].copy_from_slice(&dst_bias.to_le_bytes());
    Some(BASE64.encode(blob))
}

fn parse_utc_offset_name(name: &str) -> Option<&'static str> {
    if name.contains("utc") || name.contains("gmt") {
        if name.contains("+01") {
            return Some("Etc/GMT-1");
        }
        if name.contains("+02") {
            return Some("Etc/GMT-2");
        }
        if name.contains("+03") {
            return Some("Etc/GMT-3");
        }
        if name.contains("+04") {
            return Some("Etc/GMT-4");
        }
        if name.contains("+05") {
            return Some("Etc/GMT-5");
        }
        if name.contains("+06") {
            return Some("Etc/GMT-6");
        }
        if name.contains("+07") {
            return Some("Etc/GMT-7");
        }
        if name.contains("+08") {
            return Some("Etc/GMT-8");
        }
        if name.contains("+09") {
            return Some("Etc/GMT-9");
        }
        if name.contains("+10") {
            return Some("Etc/GMT-10");
        }
        if name.contains("+11") {
            return Some("Etc/GMT-11");
        }
        if name.contains("+12") {
            return Some("Etc/GMT-12");
        }
        if name.contains("-01") {
            return Some("Etc/GMT+1");
        }
        if name.contains("-02") {
            return Some("Etc/GMT+2");
        }
        if name.contains("-03") {
            return Some("Etc/GMT+3");
        }
        if name.contains("-04") {
            return Some("Etc/GMT+4");
        }
        if name.contains("-05") {
            return Some("Etc/GMT+5");
        }
        if name.contains("-06") {
            return Some("Etc/GMT+6");
        }
        if name.contains("-07") {
            return Some("Etc/GMT+7");
        }
        if name.contains("-08") {
            return Some("Etc/GMT+8");
        }
        if name.contains("-09") {
            return Some("Etc/GMT+9");
        }
        if name.contains("-10") {
            return Some("Etc/GMT+10");
        }
        if name.contains("-11") {
            return Some("Etc/GMT+11");
        }
        if name.contains("-12") {
            return Some("Etc/GMT+12");
        }
    }
    None
}

const EU_STD: [u8; 16] = [0, 0, 10, 0, 0, 0, 5, 0, 3, 0, 0, 0, 0, 0, 0, 0];
const EU_DST: [u8; 16] = [0, 0, 3, 0, 0, 0, 5, 0, 2, 0, 0, 0, 0, 0, 0, 0];
const US_DST: [u8; 16] = [0, 0, 3, 0, 0, 0, 2, 0, 2, 0, 0, 0, 0, 0, 0, 0];
const US_STD: [u8; 16] = [0, 0, 11, 0, 0, 0, 1, 0, 2, 0, 0, 0, 0, 0, 0, 0];
const NO_DST: [u8; 16] = [0u8; 16];

type TzParams = (
    i32,
    &'static str,
    &'static str,
    [u8; 16],
    [u8; 16],
    i32,
    i32,
);

fn iana_to_windows_params(iana: &str) -> Option<TzParams> {
    Some(match iana {
        "UTC" | "Etc/UTC" | "Etc/GMT" | "GMT" => {
            (0, "Coordinated Universal Time", "", NO_DST, NO_DST, 0, 0)
        }
        "Europe/London" => (
            0,
            "GMT Standard Time",
            "GMT Daylight Time",
            EU_STD,
            EU_DST,
            0,
            -60,
        ),
        "Europe/Amsterdam" | "Europe/Berlin" | "Europe/Paris" | "Europe/Rome" | "Europe/Madrid"
        | "Europe/Stockholm" | "Europe/Brussels" | "Europe/Copenhagen" | "Europe/Vienna"
        | "Europe/Warsaw" | "Europe/Zagreb" | "Europe/Budapest" => (
            -60,
            "W. Europe Standard Time",
            "W. Europe Daylight Time",
            EU_STD,
            EU_DST,
            0,
            -60,
        ),
        "Europe/Helsinki" | "Europe/Tallinn" | "Europe/Riga" | "Europe/Vilnius" | "Europe/Kyiv"
        | "Europe/Bucharest" | "Europe/Athens" | "Europe/Sofia" => (
            -120,
            "FLE Standard Time",
            "FLE Daylight Time",
            EU_STD,
            EU_DST,
            0,
            -60,
        ),
        "Europe/Moscow" => (
            -180,
            "Russian Standard Time",
            "Russian Standard Time",
            NO_DST,
            NO_DST,
            0,
            0,
        ),
        "Europe/Istanbul" => (
            -180,
            "Turkey Standard Time",
            "Turkey Standard Time",
            NO_DST,
            NO_DST,
            0,
            0,
        ),
        "Asia/Dubai" => (
            -240,
            "Arabian Standard Time",
            "Arabian Standard Time",
            NO_DST,
            NO_DST,
            0,
            0,
        ),
        "Asia/Kolkata" | "Asia/Calcutta" => (
            -330,
            "India Standard Time",
            "India Standard Time",
            NO_DST,
            NO_DST,
            0,
            0,
        ),
        "Asia/Shanghai" | "Asia/Hong_Kong" => (
            -480,
            "China Standard Time",
            "China Standard Time",
            NO_DST,
            NO_DST,
            0,
            0,
        ),
        "Asia/Singapore" => (
            -480,
            "Singapore Standard Time",
            "Singapore Standard Time",
            NO_DST,
            NO_DST,
            0,
            0,
        ),
        "Asia/Tokyo" => (
            -540,
            "Tokyo Standard Time",
            "Tokyo Standard Time",
            NO_DST,
            NO_DST,
            0,
            0,
        ),
        "Asia/Seoul" => (
            -540,
            "Korea Standard Time",
            "Korea Standard Time",
            NO_DST,
            NO_DST,
            0,
            0,
        ),
        "Australia/Sydney" => (
            -600,
            "AUS Eastern Standard Time",
            "AUS Eastern Daylight Time",
            NO_DST,
            EU_DST,
            0,
            -60,
        ),
        "America/New_York" => (
            300,
            "Eastern Standard Time",
            "Eastern Daylight Time",
            US_STD,
            US_DST,
            0,
            -60,
        ),
        "America/Chicago" => (
            360,
            "Central Standard Time",
            "Central Daylight Time",
            US_STD,
            US_DST,
            0,
            -60,
        ),
        "America/Denver" => (
            420,
            "Mountain Standard Time",
            "Mountain Daylight Time",
            US_STD,
            US_DST,
            0,
            -60,
        ),
        "America/Los_Angeles" => (
            480,
            "Pacific Standard Time",
            "Pacific Daylight Time",
            US_STD,
            US_DST,
            0,
            -60,
        ),
        "America/Anchorage" => (
            540,
            "Alaskan Standard Time",
            "Alaskan Daylight Time",
            US_STD,
            US_DST,
            0,
            -60,
        ),
        "Pacific/Honolulu" => (
            600,
            "Hawaiian Standard Time",
            "Hawaiian Standard Time",
            NO_DST,
            NO_DST,
            0,
            0,
        ),
        "America/Halifax" => (
            240,
            "Atlantic Standard Time",
            "Atlantic Daylight Time",
            US_STD,
            US_DST,
            0,
            -60,
        ),
        "America/St_Johns" => (
            210,
            "Newfoundland Standard Time",
            "Newfoundland Daylight Time",
            US_STD,
            US_DST,
            0,
            -60,
        ),
        "America/Sao_Paulo" => (
            180,
            "E. South America Standard Time",
            "E. South America Daylight Time",
            NO_DST,
            NO_DST,
            0,
            -60,
        ),
        "Europe/Prague" => (
            -60,
            "Central Europe Standard Time",
            "Central Europe Daylight Time",
            EU_STD,
            EU_DST,
            0,
            -60,
        ),
        "Europe/Dublin" | "Europe/Lisbon" => (
            0,
            "GMT Standard Time",
            "GMT Daylight Time",
            EU_STD,
            EU_DST,
            0,
            -60,
        ),
        "Europe/Zurich" => (
            -60,
            "W. Europe Standard Time",
            "W. Europe Daylight Time",
            EU_STD,
            EU_DST,
            0,
            -60,
        ),
        "Asia/Taipei" => (
            -480,
            "Taipei Standard Time",
            "Taipei Standard Time",
            NO_DST,
            NO_DST,
            0,
            0,
        ),
        "Asia/Bangkok" | "Asia/Jakarta" => (
            -420,
            "SE Asia Standard Time",
            "SE Asia Standard Time",
            NO_DST,
            NO_DST,
            0,
            0,
        ),
        "Asia/Karachi" => (
            -300,
            "Pakistan Standard Time",
            "Pakistan Standard Time",
            NO_DST,
            NO_DST,
            0,
            0,
        ),
        "Asia/Dhaka" => (
            -360,
            "Bangladesh Standard Time",
            "Bangladesh Standard Time",
            NO_DST,
            NO_DST,
            0,
            0,
        ),
        "Asia/Tehran" => (
            -210,
            "Iran Standard Time",
            "Iran Daylight Time",
            NO_DST,
            NO_DST,
            0,
            -60,
        ),
        "Asia/Baghdad" => (
            -180,
            "Arabic Standard Time",
            "Arabic Standard Time",
            NO_DST,
            NO_DST,
            0,
            0,
        ),
        "Africa/Johannesburg" => (
            -120,
            "South Africa Standard Time",
            "South Africa Standard Time",
            NO_DST,
            NO_DST,
            0,
            0,
        ),
        "Africa/Cairo" => (
            -120,
            "Egypt Standard Time",
            "Egypt Standard Time",
            NO_DST,
            NO_DST,
            0,
            0,
        ),
        "Pacific/Auckland" => (
            -720,
            "New Zealand Standard Time",
            "New Zealand Daylight Time",
            EU_STD,
            EU_DST,
            0,
            -60,
        ),
        "Australia/Brisbane" => (
            -600,
            "E. Australia Standard Time",
            "E. Australia Standard Time",
            NO_DST,
            NO_DST,
            0,
            0,
        ),
        "Australia/Melbourne" => (
            -600,
            "AUS Eastern Standard Time",
            "AUS Eastern Daylight Time",
            EU_STD,
            EU_DST,
            0,
            -60,
        ),
        "Australia/Perth" => (
            -480,
            "W. Australia Standard Time",
            "W. Australia Standard Time",
            NO_DST,
            NO_DST,
            0,
            0,
        ),
        "America/Mexico_City" => (
            360,
            "Central Standard Time (Mexico)",
            "Central Daylight Time (Mexico)",
            US_STD,
            US_DST,
            0,
            -60,
        ),
        "America/Buenos_Aires" => (
            180,
            "Argentina Standard Time",
            "Argentina Standard Time",
            NO_DST,
            NO_DST,
            0,
            0,
        ),
        "America/Santiago" => (
            240,
            "Pacific SA Standard Time",
            "Pacific SA Daylight Time",
            EU_STD,
            EU_DST,
            0,
            -60,
        ),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_bias_zero_utc() {
        let blob = [0u8; 172];
        let b64 = BASE64.encode(blob);
        assert_eq!(decode_eas_timezone_bias(&b64), Some(0));
    }

    #[test]
    fn decode_bias_eastern_us() {
        let mut blob = [0u8; 172];
        blob[0..4].copy_from_slice(&300i32.to_le_bytes());
        let b64 = BASE64.encode(blob);
        assert_eq!(decode_eas_timezone_bias(&b64), Some(300));
    }

    #[test]
    fn blob_to_iana_utc_bias() {
        let blob = [0u8; 172];
        let b64 = BASE64.encode(blob);
        assert_eq!(eas_timezone_blob_to_iana(&b64), Some("UTC".to_string()));
    }

    #[test]
    fn blob_to_iana_via_name_pacific() {
        let mut blob = [0u8; 172];
        blob[0..4].copy_from_slice(&480i32.to_le_bytes());
        write_wchar_name(&mut blob, 4, "Pacific Standard Time");
        let b64 = BASE64.encode(blob);
        assert_eq!(
            eas_timezone_blob_to_iana(&b64),
            Some("America/Los_Angeles".to_string())
        );
    }

    #[test]
    fn blob_to_iana_from_utc_offset_name() {
        let mut blob = [0u8; 172];
        blob[0..4].copy_from_slice(&(-120i32).to_le_bytes());
        write_wchar_name(&mut blob, 4, "(UTC+02:00) Custom");
        let b64 = BASE64.encode(blob);
        assert_eq!(
            eas_timezone_blob_to_iana(&b64),
            Some("Etc/GMT-2".to_string())
        );
    }

    #[test]
    fn iana_to_blob_round_trip_amsterdam() {
        let b64 = iana_to_eas_timezone_blob("Europe/Amsterdam").unwrap();
        let bytes = BASE64.decode(&b64).unwrap();
        assert_eq!(bytes.len(), 172);
        let bias = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(bias, -60);
        let name = read_wchar_name(&bytes, 4);
        assert!(name.starts_with("W. Europe"), "got: {name}");
    }

    #[test]
    fn iana_to_blob_utc() {
        let b64 = iana_to_eas_timezone_blob("UTC").unwrap();
        let bytes = BASE64.decode(&b64).unwrap();
        let bias = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(bias, 0);
    }

    #[test]
    fn unknown_iana_returns_none() {
        assert!(iana_to_eas_timezone_blob("Not/A/Zone").is_none());
    }

    #[test]
    fn blob_to_tz_pacific() {
        let mut blob = [0u8; 172];
        blob[0..4].copy_from_slice(&480i32.to_le_bytes());
        write_wchar_name(&mut blob, 4, "Pacific Standard Time");
        let b64 = BASE64.encode(blob);
        let tz = eas_timezone_blob_to_tz(&b64).unwrap();
        assert_eq!(tz, chrono_tz::America::Los_Angeles);
    }

    #[test]
    fn windows_name_to_tz_eastern() {
        let tz = windows_timezone_name_to_tz("Eastern Standard Time").unwrap();
        assert_eq!(tz, chrono_tz::America::New_York);
    }

    #[test]
    fn windows_name_to_tz_case_insensitive() {
        let tz = windows_timezone_name_to_tz("eastern standard time").unwrap();
        assert_eq!(tz, chrono_tz::America::New_York);
    }

    #[test]
    fn windows_name_to_tz_utc() {
        let tz = windows_timezone_name_to_tz("UTC").unwrap();
        assert_eq!(tz, chrono_tz::Etc::UTC);
    }
}

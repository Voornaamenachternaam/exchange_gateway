// src/timezone.rs
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use chrono::Offset;
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
        .filter(|variant| {
            let name = variant.name();
            n_lower.contains(&name.to_ascii_lowercase())
        })
        .max_by_key(|variant| variant.name().len())
}

fn windows_name_to_iana(name: &str) -> Option<String> {
    let n = name.trim();
    if n.is_empty() {
        return None;
    }

    // First try UTC offset names like "(UTC+02:00) Custom" for better compatibility
    if let Some(tz) = parse_utc_offset_name(n) {
        return Some(tz);
    }

    // Then fall back to Windows timezone name resolution
    find_windows_timezone(n).map(|tz| tz.tzdb_id().to_string())
}
    let n = name.trim();
    if n.is_empty() {
        return None;
    }

    // First try UTC offset names like "(UTC+02:00) Custom" for better compatibility
    if let Some(tz) = parse_utc_offset_name(n) {
        return Some(tz);
    }

    // Then fall back to Windows timezone name resolution
    find_windows_timezone(n).map(|tz| tz.tzdb_id().to_string())
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
        return Some(iana);
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
    write_wchar_name(&mut blob, 4, &std_name);
    blob[68..84].copy_from_slice(&std_date);
    blob[84..88].copy_from_slice(&std_bias.to_le_bytes());
    write_wchar_name(&mut blob, 88, &dst_name);
    blob[152..168].copy_from_slice(&dst_date);
    blob[168..172].copy_from_slice(&dst_bias.to_le_bytes());
    Some(BASE64.encode(blob))
}

fn parse_utc_offset_name(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    if !lower.contains("utc") && !lower.contains("gmt") {
        return None;
    }
    // Scan for ±NN patterns (e.g., +05, -08) in offset names like "(UTC+05:00)"
    let bytes = lower.as_bytes();
    for i in 0..bytes.len().saturating_sub(2) {
        let sign = match bytes[i] {
            b'+' => "-",
            b'-' => "+",
            _ => continue,
        };
        if bytes[i + 1].is_ascii_digit() && bytes[i + 2].is_ascii_digit() {
            let hours: i32 =
                (bytes[i + 1] - b'0') as i32 * 10 + (bytes[i + 2] - b'0') as i32;
            if (1..=12).contains(&hours) {
                // Etc/GMT sign convention is inverted per POSIX
                return Some(format!("Etc/GMT{}{}", sign, hours));
            }
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
    String,
    String,
    [u8; 16],
    [u8; 16],
    i32,
    i32,
);

/// Determine DST transition rule set based on IANA timezone region prefix.
/// Returns (std_date, dst_date, dst_bias).
fn dst_rules_for(iana: &str) -> ([u8; 16], [u8; 16], i32) {
    // Timezones with no DST transitions
    let no_dst_zones = [
        "Europe/Moscow", "Europe/Istanbul", "Asia/Dubai", "Asia/Kolkata",
        "Asia/Calcutta", "Asia/Shanghai", "Asia/Hong_Kong", "Asia/Singapore",
        "Asia/Tokyo", "Asia/Seoul", "Asia/Taipei", "Asia/Bangkok", "Asia/Jakarta",
        "Asia/Karachi", "Asia/Dhaka", "Asia/Baghdad", "Africa/Johannesburg",
        "Africa/Cairo", "Australia/Brisbane", "Australia/Perth",
        "America/Buenos_Aires", "Pacific/Honolulu", "UTC", "Etc/UTC", "Etc/GMT",
        "GMT",
    ];
    if no_dst_zones.contains(&iana) {
        return (NO_DST, NO_DST, 0);
    }
    // Australia/Sydney: special case — std_date=NO_DST (original behavior preserved)
    if iana == "Australia/Sydney" {
        return (NO_DST, EU_DST, -60);
    }
    // Southern Hemisphere (Australia/NZ): EU DST rule, standard transition uses EU_STD
    if iana.starts_with("Australia/") || iana.starts_with("Pacific/Auckland") {
        return (EU_STD, EU_DST, -60);
    }
    // South America
    if iana.starts_with("America/Sao_Paulo") {
        return (NO_DST, NO_DST, -60);
    }
    if iana.starts_with("America/Santiago") {
        return (EU_STD, EU_DST, -60);
    }
    // US/Americas: US DST rules
    if iana.starts_with("America/") || iana.starts_with("Pacific/") {
        return (US_STD, US_DST, -60);
    }
    // Default: EU DST rules (Europe, Asia, Africa, etc.)
    (EU_STD, EU_DST, -60)
}

fn iana_to_windows_params(iana: &str) -> Option<TzParams> {
    let tz: Tz = iana.parse().ok()?;

    // Map IANA → WindowsTimezone using the windows-timezones crate
    let win_tz = WindowsTimezone::try_from(tz).ok()?;
    let win_name = win_tz.name().to_string();

    // Compute UTC offsets at January and July reference points
    // EAS bias convention: positive = west of UTC (minutes behind UTC)
    let jan = chrono::NaiveDate::from_ymd_opt(2025, 1, 15)?.and_hms_opt(12, 0, 0)?;
    let jan_dt = jan.and_local_timezone(tz).earliest()?;
    let jan_offset = jan_dt.offset().fix().local_minus_utc() / 60;

    let jul = chrono::NaiveDate::from_ymd_opt(2025, 7, 15)?.and_hms_opt(12, 0, 0)?;
    let jul_dt = jul.and_local_timezone(tz).earliest()?;
    let jul_offset = jul_dt.offset().fix().local_minus_utc() / 60;

    let has_dst = jan_offset != jul_offset;
    // Standard time is the one with the smaller offset (e.g. +10 vs +11, -5 vs -4)
    let bias = -if has_dst { jan_offset.min(jul_offset) } else { jan_offset };

    let (std_date, dst_date, dst_bias) = if has_dst {
        dst_rules_for(iana)
    } else {
        (NO_DST, NO_DST, 0)
    };

    let dst_name = if has_dst {
        win_name.replace("Standard", "Daylight")
    } else {
        win_name.clone()
    };

    Some((bias, win_name, dst_name, std_date, dst_date, 0, dst_bias))
}

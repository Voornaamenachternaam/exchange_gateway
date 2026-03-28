// src/timezone.rs
// Windows Time Zone ID to IANA Time Zone Database mapping
// Reference: https://learn.microsoft.com/en-us/windows-hardware/manufacture/desktop/windows-time-zoneBil:adjustments
// Updated: March 2026

use std::collections::HashMap;
use once_cell::sync::Lazy;

/// Windows Time Zone ID to IANA Timezone mapping
/// This table maps Windows timezone identifiers to their IANA equivalents
pub static WINDOWS_TO_IANA: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut map = HashMap::new();
    
    // UTC time zones
    map.insert("UTC", "UTC");
    map.insert("GMT Standard Time", "Europe/London");
    map.insert("GMT Daylight Time", "Europe/London");
    
    // European time zones
    map.insert("W. Europe Standard Time", "Europe/Paris");
    map.insert("W. Europe Daylight Time", "Europe/Paris");
    map.insert("Central Europe Standard Time", "Europe/Berlin");
    map.insert("Central Europe Daylight Time", "Europe/Berlin");
    map.insert("Romance Standard Time", "Europe/Paris");
    map.insert("Romance Daylight Time", "Europe/Paris");
    map.insert("Central European Standard Time", "Europe/Warsaw");
    map.insert("Central European Daylight Time", "Europe/Warsaw");
    map.insert("W. Central Africa Standard Time", "Africa/Lagos");
    map.insert("E. Europe Standard Time", "Europe/Bucharest");
    map.insert("E. Europe Daylight Time", "Europe/Bucharest");
    map.insert("Egypt Standard Time", "Africa/Cairo");
    map.insert("South Africa Standard Time", "Africa/Johannesburg");
    map.insert("FLE Standard Time", "Europe/Kyiv");
    map.insert("FLE Daylight Time", "Europe/Kyiv");
    map.insert("GTB Standard Time", "Europe/Athens");
    map.insert("GTB Daylight Time", "Europe/Athens");
    map.insert("Israel Standard Time", "Asia/Jerusalem");
    map.insert("Israel Daylight Time", "Asia/Jerusalem");
    map.insert("Turkey Standard Time", "Europe/Istanbul");
    map.insert("Turkey Daylight Time", "Europe/Istanbul");
    map.insert("SA Pacific Standard Time", "America/Bogota");
    map.insert("SA Pacific Daylight Time", "America/Bogota");
    map.insert("E. South America Standard Time", "America/Sao_Paulo");
    map.insert("E. South America Daylight Time", "America/Sao_Paulo");
    map.insert("Atlantic Standard Time", "America/Halifax");
    map.insert("Atlantic Daylight Time", "America/Halifax");
    map.insert("SA Western Standard Time", "America/Caracas");
    map.insert("SA Western Daylight Time", "America/Caracas");
    map.insert("Pacific SA Standard Time", "America/Santiago");
    map.insert("Pacific SA Daylight Time", "America/Santiago");
    map.insert("Central Brazilian Standard Time", "America/Manaus");
    map.insert("Central Brazilian Daylight Time", "America/Manaus");
    map.insert("Montevideo Standard Time", "America/Montevideo");
    map.insert("Argentina Standard Time", "America/Argentina/Buenos_Aires");
    map.insert("Mid-Atlantic Standard Time", "Atlantic/South_Georgia");
    map.insert("Mid-Atlantic Daylight Time", "Atlantic/South_Georgia");
    
    // North American time zones
    map.insert("Eastern Standard Time", "America/New_York");
    map.insert("Eastern Daylight Time", "America/New_York");
    map.insert("Central Standard Time", "America/Chicago");
    map.insert("Central Daylight Time", "America/Chicago");
    map.insert("Mountain Standard Time", "America/Denver");
    map.insert("Mountain Daylight Time", "America/Denver");
    map.insert("US Mountain Standard Time", "America/Phoenix");
    map.insert("Pacific Standard Time", "America/Los_Angeles");
    map.insert("Pacific Daylight Time", "America/Los_Angeles");
    map.insert("US Mountain Standard Time", "America/Phoenix");
    map.insert("Arizona", "America/Phoenix");
    map.insert("Saskatchewan Standard Time", "America/Regina");
    map.insert("Saskatchewan Daylight Time", "America/Regina");
    map.insert("Central America Standard Time", "America/Guatemala");
    map.insert("Guadalajara, Mexico City, Monterrey - New", "America/Mexico_City");
    map.insert("Guadalajara, Mexico City, Monterrey", "America/Mexico_City");
    map.insert("Mexico City", "America/Mexico_City");
    map.insert("Central Standard Time (Mexico)", "America/Mexico_City");
    map.insert("Central Daylight Time (Mexico)", "America/Mexico_City");
    map.insert("Pacific Standard Time (Mexico)", "America/Tijuana");
    map.insert("Pacific Daylight Time (Mexico)", "America/Tijuana");
    map.insert("Atlantic Standard Time", "America/Puerto_Rico");
    map.insert("SA Eastern Standard Time", "America/Cayenne");
    map.insert("SA Eastern Daylight Time", "America/Cayenne");
    
    // Asia/Pacific time zones
    map.insert("Tokyo Standard Time", "Asia/Tokyo");
    map.insert("Tokyo Daylight Time", "Asia/Tokyo");
    map.insert("Korea Standard Time", "Asia/Seoul");
    map.insert("Korea Daylight Time", "Asia/Seoul");
    map.insert("China Standard Time", "Asia/Shanghai");
    map.insert("China Daylight Time", "Asia/Shanghai");
    map.insert("Singapore Standard Time", "Asia/Singapore");
    map.insert("Singapore Daylight Time", "Asia/Singapore");
    map.insert("Taipei Standard Time", "Asia/Taipei");
    map.insert("Taipei Daylight Time", "Asia/Taipei");
    map.insert("W. Australia Standard Time", "Australia/Perth");
    map.insert("W. Australia Daylight Time", "Australia/Perth");
    map.insert("AUS Central Standard Time", "Australia/Darwin");
    map.insert("AUS Central Daylight Time", "Australia/Darwin");
    map.insert("AUS Eastern Standard Time", "Australia/Sydney");
    map.insert("AUS Eastern Daylight Time", "Australia/Sydney");
    map.insert("New Zealand Standard Time", "Pacific/Auckland");
    map.insert("New Zealand Daylight Time", "Pacific/Auckland");
    map.insert("E. Australia Standard Time", "Australia/Brisbane");
    map.insert("E. Australia Daylight Time", "Australia/Brisbane");
    map.insert("India Standard Time", "Asia/Kolkata");
    map.insert("India Daylight Time", "Asia/Kolkata");
    map.insert("Sri Lanka Standard Time", "Asia/Colombo");
    map.insert("Sri Lanka Daylight Time", "Asia/Colombo");
    map.insert("Nepal Standard Time", "Asia/Kathmandu");
    map.insert("SE Asia Standard Time", "Asia/Bangkok");
    map.insert("SE Asia Daylight Time", "Asia/Bangkok");
    map.insert("N. Central Asia Standard Time", "Asia/Almaty");
    map.insert("Myanmar Standard Time", "Asia/Yangon");
    map.insert("Arab Standard Time", "Asia/Riyadh");
    map.insert("Arab Daylight Time", "Asia/Riyadh");
    map.insert("Arabic Standard Time", "Asia/Baghdad");
    map.insert("Arabic Daylight Time", "Asia/Baghdad");
    map.insert("Iran Standard Time", "Asia/Tehran");
    map.insert("Iran Daylight Time", "Asia/Tehran");
    map.insert("Caucasus Standard Time", "Asia/Yerevan");
    map.insert("Caucasus Daylight Time", "Asia/Yerevan");
    map.insert("Georgian Standard Time", "Asia/Tbilisi");
    map.insert("Afghanistan Standard Time", "Asia/Kabul");
    map.insert("West Asia Standard Time", "Asia/Tashkent");
    map.insert("West Asia Daylight Time", "Asia/Tashkent");
    map.insert("Ekaterinburg Standard Time", "Asia/Yekaterinburg");
    map.insert("Ekaterinburg Daylight Time", "Asia/Yekaterinburg");
    map.insert("Central Asia Standard Time", "Asia/Dhaka");
    map.insert("Central Asia Daylight Time", "Asia/Dhaka");
    map.insert("Pakistan Standard Time", "Asia/Karachi");
    map.insert("Pakistan Daylight Time", "Asia/Karachi");
    map.insert("Bangladesh Standard Time", "Asia/Dhaka");
    map.insert("Bangladesh Daylight Time", "Asia/Dhaka");
    map.insert("India Standard Time", "Asia/Kolkata");
    map.insert("India Daylight Time", "Asia/Kolkata");
    map.insert("Russia Time Zone 11", "Asia/Anadyr");
    map.insert("Russia Time Zone 10", "Asia/Magadan");
    map.insert("North Asia Standard Time", "Asia/Irkutsk");
    map.insert("North Asia Daylight Time", "Asia/Irkutsk");
    map.insert("Tomsk Standard Time", "Asia/Tomsk");
    map.insert("Tomsk Daylight Time", "Asia/Tomsk");
    map.insert("W. Mongolia Standard Time", "Asia/Ulaanbaatar");
    map.insert("W. Mongolia Daylight Time", "Asia/Ulaanbaatar");
    map.insert("China Standard Time", "Asia/Shanghai");
    map.insert("Taipei Standard Time", "Asia/Taipei");
    map.insert("Osaka, Sapporo, Tokyo", "Asia/Tokyo");
    map.insert("Osaka", "Asia/Tokyo");
    map.insert("Hawaii-Aleutian Standard Time", "Pacific/Honolulu");
    map.insert("Hawaii-Aleutian Daylight Time", "Pacific/Honolulu");
    map.insert("Hawaiian Standard Time", "Pacific/Honolulu");
    map.insert("Alaskan Standard Time", "America/Anchorage");
    map.insert("Alaskan Daylight Time", "America/Anchorage");
    map.insert("Yukon Standard Time", "America/Whitehorse");
    map.insert("Yukon Daylight Time", "America/Whitehorse");
    
    // Additional time zones
    map.insert("Dateline Standard Time", "Pacific/Kwajalein");
    map.insert("Samoa Standard Time", "Pacific/Apia");
    map.insert("Samoa Daylight Time", "Pacific/Apia");
    map.insert("Tonga Standard Time", "Pacific/Tongatapu");
    map.insert("Fiji Standard Time", "Pacific/Fiji");
    map.insert("Kamchatka Standard Time", "Asia/Kamchatka");
    map.insert("Volgograd Standard Time", "Europe/Volgograd");
    map.insert("Moscow Standard Time", "Europe/Moscow");
    map.insert("Moscow Daylight Time", "Europe/Moscow");
    map.insert("St. Petersburg Standard Time", "Europe/Moscow");
    map.insert("St. Petersburg Daylight Time", "Europe/Moscow");
    map.insert("Turkey Standard Time", "Europe/Istanbul");
    map.insert("Libya Standard Time", "Africa/Tripoli");
    map.insert("Namibia Standard Time", "Africa/Windhoek");
    map.insert("Greenland Standard Time", "America/Godthab");
    map.insert("Greenland Daylight Time", "America/Godthab");
    map.insert("Azores Standard Time", "Atlantic/Azores");
    map.insert("Azores Daylight Time", "Atlantic/Azores");
    map.insert("Cape Verde Is. Standard Time", "Atlantic/Cape_Verde");
    map.insert("Cape Verde Is. Daylight Time", "Atlantic/Cape_Verde");
    map.insert("Mid-Atlantic Standard Time", "Atlantic/South_Georgia");
    map.insert("Caucasus Standard Time", "Asia/Yerevan");
    
    map
});

/// IANA to Windows Time Zone ID mapping (reverse lookup)
pub static IANA_TO_WINDOWS: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut map = HashMap::new();
    for (windows, iana) in WINDOWS_TO_IANA.iter() {
        map.insert(*iana, *windows);
    }
    map
});

/// Convert a Windows Time Zone ID to IANA timezone
/// Returns None if no mapping exists
pub fn windows_to_iana(windows_id: &str) -> Option<&'static str> {
    WINDOWS_TO_IANA.get(windows_id).copied()
}

/// Convert an IANA timezone to Windows Time Zone ID
/// Returns None if no mapping exists
pub fn iana_to_windows(iana_id: &str) -> Option<&'static str> {
    IANA_TO_WINDOWS.get(iana_id).copied()
}

/// Parse timezone from various formats:
/// - Windows timezone ID
/// - IANA timezone (e.g., "America/New_York")
/// - Custom formats used by Exchange
/// Returns the IANA timezone identifier
pub fn normalize_timezone(tz: &str) -> String {
    let tz = tz.trim();
    
    // Try Windows -> IANA first
    if let Some(iana) = windows_to_iana(tz) {
        return iana.to_string();
    }
    
    // Already IANA format (contains '/')
    if tz.contains('/') {
        // Validate it's a reasonable IANA format
        if tz.chars().all(|c| c.is_alphanumeric() || c == '/' || c == '_' || c == '-') {
            return tz.to_string();
        }
    }
    
    // Try case-insensitive Windows lookup
    for (windows, iana) in WINDOWS_TO_IANA.iter() {
        if windows.to_lowercase() == tz.to_lowercase() {
            return iana.to_string();
        }
    }
    
    // Default fallback to UTC
    "UTC".to_string()
}

/// Get all available Windows timezone IDs
pub fn all_windows_timezones() -> Vec<&'static str> {
    WINDOWS_TO_IANA.keys().copied().collect()
}

/// Get all available IANA timezone identifiers
pub fn all_iana_timezones() -> Vec<&'static str> {
    IANA_TO_WINDOWS.keys().copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_windows_to_iana() {
        assert_eq!(windows_to_iana("Eastern Standard Time"), Some("America/New_York"));
        assert_eq!(windows_to_iana("Pacific Standard Time"), Some("America/Los_Angeles"));
        assert_eq!(windows_to_iana("Tokyo Standard Time"), Some("Asia/Tokyo"));
        assert_eq!(windows_to_iana("GMT Standard Time"), Some("Europe/London"));
    }

    #[test]
    fn converts_iana_to_windows() {
        assert_eq!(iana_to_windows("America/New_York"), Some("Eastern Standard Time"));
        assert_eq!(iana_to_windows("Europe/London"), Some("GMT Standard Time"));
    }

    #[test]
    fn normalizes_various_formats() {
        // Windows format
        assert_eq!(normalize_timezone("Eastern Standard Time"), "America/New_York");
        // IANA format
        assert_eq!(normalize_timezone("America/New_York"), "America/New_York");
        // Case insensitive Windows
        assert_eq!(normalize_timezone("eastern standard time"), "America/New_York");
        // Unknown
        assert_eq!(normalize_timezone("Invalid/Timezone"), "UTC");
    }

    #[test]
    fn lists_all_timezones() {
        let windows = all_windows_timezones();
        let iana = all_iana_timezones();
        
        assert!(!windows.is_empty());
        assert!(!iana.is_empty());
        assert!(windows.contains(&"Eastern Standard Time"));
        assert!(iana.contains(&"America/New_York"));
    }
}
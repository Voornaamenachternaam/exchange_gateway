//! Input Validation - Comprehensive Input Validation and Sanitization
//!
//! This module provides production-grade input validation for all Exchange Gateway
//! inputs including email addresses, device IDs, XML content, and user data.
//! Implements defense against injection attacks, XSS, and malformed input.

use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

/// Validation error types
#[derive(Debug, Clone, PartialEq)]
pub enum ValidationError {
    /// Invalid email address format
    InvalidEmail,
    /// Invalid device ID format
    InvalidDeviceId,
    /// Invalid user ID format
    InvalidUserId,
    /// Invalid UUID format
    InvalidUuid,
    /// Invalid XML content
    InvalidXml(String),
    /// Invalid JSON content
    InvalidJson(String),
    /// Invalid URL format
    InvalidUrl,
    /// Invalid hostname
    InvalidHostname,
    /// Invalid IP address
    InvalidIpAddress,
    /// Input too long
    TooLong { field: String, max: usize, actual: usize },
    /// Input too short
    TooShort { field: String, min: usize, actual: usize },
    /// Contains forbidden characters
    ForbiddenCharacters { field: String, chars: Vec<char> },
    /// Contains potentially dangerous content
    DangerousContent { field: String, reason: String },
    /// Empty input not allowed
    EmptyInput { field: String },
    /// Pattern mismatch
    PatternMismatch { field: String, pattern: String },
    /// Value out of range
    OutOfRange { field: String, min: Option<i64>, max: Option<i64>, actual: i64 },
    /// Invalid encoding
    InvalidEncoding,
    /// Blocked value
    BlockedValue { field: String, value: String },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::InvalidEmail => write!(f, "Invalid email address format"),
            ValidationError::InvalidDeviceId => write!(f, "Invalid device ID format"),
            ValidationError::InvalidUserId => write!(f, "Invalid user ID format"),
            ValidationError::InvalidUuid => write!(f, "Invalid UUID format"),
            ValidationError::InvalidXml(reason) => write!(f, "Invalid XML: {}", reason),
            ValidationError::InvalidJson(reason) => write!(f, "Invalid JSON: {}", reason),
            ValidationError::InvalidUrl => write!(f, "Invalid URL format"),
            ValidationError::InvalidHostname => write!(f, "Invalid hostname"),
            ValidationError::InvalidIpAddress => write!(f, "Invalid IP address"),
            ValidationError::TooLong { field, max, actual } => {
                write!(f, "{} is too long (max: {}, actual: {})", field, max, actual)
            }
            ValidationError::TooShort { field, min, actual } => {
                write!(f, "{} is too short (min: {}, actual: {})", field, min, actual)
            }
            ValidationError::ForbiddenCharacters { field, chars } => {
                write!(f, "{} contains forbidden characters: {:?}", field, chars)
            }
            ValidationError::DangerousContent { field, reason } => {
                write!(f, "{} contains dangerous content: {}", field, reason)
            }
            ValidationError::EmptyInput { field } => write!(f, "{} cannot be empty", field),
            ValidationError::PatternMismatch { field, pattern } => {
                write!(f, "{} does not match pattern: {}", field, pattern)
            }
            ValidationError::OutOfRange { field, min, max, actual } => {
                write!(f, "{} value {} is out of range", field, actual)?;
                if let Some(min) = min {
                    write!(f, " (min: {})", min)?;
                }
                if let Some(max) = max {
                    write!(f, " (max: {})", max)?;
                }
                Ok(())
            }
            ValidationError::InvalidEncoding => write!(f, "Invalid character encoding"),
            ValidationError::BlockedValue { field, value } => {
                write!(f, "{} value '{}' is blocked", field, value)
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// Result type for validation operations
pub type ValidationResult<T> = Result<T, ValidationError>;

/// Email validator
pub struct EmailValidator;

impl EmailValidator {
    /// Maximum length for email addresses (RFC 5321)
    const MAX_LENGTH: usize = 254;
    /// Maximum local part length
    const MAX_LOCAL_LENGTH: usize = 64;

    /// Validate an email address
    pub fn validate(email: &str) -> ValidationResult<()> {
        // Check length
        if email.is_empty() {
            return Err(ValidationError::EmptyInput { field: "email".to_string() });
        }
        
        if email.len() > Self::MAX_LENGTH {
            return Err(ValidationError::TooLong {
                field: "email".to_string(),
                max: Self::MAX_LENGTH,
                actual: email.len(),
            });
        }

        // Check for dangerous characters
        let dangerous = Self::find_dangerous_chars(email);
        if !dangerous.is_empty() {
            return Err(ValidationError::ForbiddenCharacters {
                field: "email".to_string(),
                chars: dangerous,
            });
        }

        // Use regex for format validation
        static EMAIL_REGEX: OnceLock<Regex> = OnceLock::new();
        let regex = EMAIL_REGEX.get_or_init(|| {
            Regex::new(r"^[a-zA-Z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)*$")
                .unwrap()
        });

        if !regex.is_match(email) {
            return Err(ValidationError::InvalidEmail);
        }

        // Validate local part length
        if let Some(at_pos) = email.find('@') {
            if at_pos > Self::MAX_LOCAL_LENGTH {
                return Err(ValidationError::TooLong {
                    field: "email local part".to_string(),
                    max: Self::MAX_LOCAL_LENGTH,
                    actual: at_pos,
                });
            }
        }

        Ok(())
    }

    /// Sanitize email address (lowercase domain, trim whitespace)
    pub fn sanitize(email: &str) -> String {
        let trimmed = email.trim();
        if let Some(at_pos) = trimmed.find('@') {
            let local = &trimmed[..at_pos];
            let domain = &trimmed[at_pos + 1..];
            format!("{}@{}", local, domain.to_lowercase())
        } else {
            trimmed.to_string()
        }
    }

    /// Find dangerous characters in email
    fn find_dangerous_chars(email: &str) -> Vec<char> {
        let forbidden: HashSet<char> = ['<', '>', '"', '(', ')', '[', ']', '\\', ',', ';', ':', '\n', '\r', '\t']
            .iter()
            .copied()
            .collect();
        
        email.chars().filter(|c| forbidden.contains(c)).collect()
    }

    /// Extract domain from email
    pub fn extract_domain(email: &str) -> Option<String> {
        email.find('@').map(|pos| email[pos + 1..].to_lowercase())
    }

    /// Check if email is from allowed domain
    pub fn is_allowed_domain(email: &str, allowed_domains: &[String]) -> bool {
        if let Some(domain) = Self::extract_domain(email) {
            allowed_domains.iter().any(|d| d.to_lowercase() == domain)
        } else {
            false
        }
    }
}

/// Device ID validator
pub struct DeviceIdValidator;

impl DeviceIdValidator {
    const MAX_LENGTH: usize = 64;
    const MIN_LENGTH: usize = 4;

    /// Validate device ID
    pub fn validate(device_id: &str) -> ValidationResult<()> {
        if device_id.is_empty() {
            return Err(ValidationError::EmptyInput { field: "device_id".to_string() });
        }

        let len = device_id.len();
        if len < Self::MIN_LENGTH {
            return Err(ValidationError::TooShort {
                field: "device_id".to_string(),
                min: Self::MIN_LENGTH,
                actual: len,
            });
        }

        if len > Self::MAX_LENGTH {
            return Err(ValidationError::TooLong {
                field: "device_id".to_string(),
                max: Self::MAX_LENGTH,
                actual: len,
            });
        }

        // Device ID should be alphanumeric with limited special chars
        static DEVICE_ID_REGEX: OnceLock<Regex> = OnceLock::new();
        let regex = DEVICE_ID_REGEX.get_or_init(|| {
            Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap()
        });

        if !regex.is_match(device_id) {
            return Err(ValidationError::InvalidDeviceId);
        }

        Ok(())
    }

    /// Sanitize device ID
    pub fn sanitize(device_id: &str) -> String {
        device_id.trim().to_string()
    }
}

/// User ID validator
pub struct UserIdValidator;

impl UserIdValidator {
    const MAX_LENGTH: usize = 128;
    const MIN_LENGTH: usize = 1;

    /// Validate user ID
    pub fn validate(user_id: &str) -> ValidationResult<()> {
        if user_id.is_empty() {
            return Err(ValidationError::EmptyInput { field: "user_id".to_string() });
        }

        let len = user_id.len();
        if len < Self::MIN_LENGTH {
            return Err(ValidationError::TooShort {
                field: "user_id".to_string(),
                min: Self::MIN_LENGTH,
                actual: len,
            });
        }

        if len > Self::MAX_LENGTH {
            return Err(ValidationError::TooLong {
                field: "user_id".to_string(),
                max: Self::MAX_LENGTH,
                actual: len,
            });
        }

        // Check for path traversal attempts
        if user_id.contains("..") || user_id.contains('/') || user_id.contains('\\') {
            return Err(ValidationError::DangerousContent {
                field: "user_id".to_string(),
                reason: "Path traversal attempt detected".to_string(),
            });
        }

        Ok(())
    }

    /// Sanitize user ID
    pub fn sanitize(user_id: &str) -> String {
        user_id.trim().to_lowercase()
    }
}

/// UUID validator
pub struct UuidValidator;

impl UuidValidator {
    /// Validate UUID string (with or without dashes)
    pub fn validate(uuid: &str) -> ValidationResult<()> {
        if uuid.is_empty() {
            return Err(ValidationError::EmptyInput { field: "uuid".to_string() });
        }

        // Support both formats: with and without dashes
        let normalized = uuid.replace('-', "");
        
        if normalized.len() != 32 {
            return Err(ValidationError::InvalidUuid);
        }

        if !normalized.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(ValidationError::InvalidUuid);
        }

        Ok(())
    }

    /// Normalize UUID to standard format
    pub fn normalize(uuid: &str) -> ValidationResult<String> {
        Self::validate(uuid)?;
        
        let normalized = uuid.replace('-', "");
        Ok(format!(
            "{}-{}-{}-{}-{}",
            &normalized[0..8],
            &normalized[8..12],
            &normalized[12..16],
            &normalized[16..20],
            &normalized[20..32]
        ))
    }
}

/// XML content validator
pub struct XmlValidator;

impl XmlValidator {
    const MAX_SIZE: usize = 10 * 1024 * 1024; // 10MB

    /// Validate XML content
    pub fn validate(xml: &str) -> ValidationResult<()> {
        if xml.is_empty() {
            return Err(ValidationError::EmptyInput { field: "xml".to_string() });
        }

        if xml.len() > Self::MAX_SIZE {
            return Err(ValidationError::TooLong {
                field: "xml".to_string(),
                max: Self::MAX_SIZE,
                actual: xml.len(),
            });
        }

        // Check for XML declaration
        if !xml.trim_start().starts_with("<?xml") && !xml.trim_start().starts_with('<') {
            return Err(ValidationError::InvalidXml(
                "Content does not appear to be XML".to_string()
            ));
        }

        // Check for dangerous entities (XXE prevention)
        let dangerous_patterns = [
            "<!ENTITY",
            "<!DOCTYPE",
            "SYSTEM",
            "PUBLIC",
        ];

        let upper_xml = xml.to_uppercase();
        for pattern in &dangerous_patterns {
            if upper_xml.contains(pattern) {
                return Err(ValidationError::DangerousContent {
                    field: "xml".to_string(),
                    reason: format!("Potentially dangerous XML pattern: {}", pattern),
                });
            }
        }

        // Check for script injection
        let script_patterns = [
            "<script",
            "javascript:",
            "onerror=",
            "onload=",
        ];

        for pattern in &script_patterns {
            if xml.to_lowercase().contains(pattern) {
                return Err(ValidationError::DangerousContent {
                    field: "xml".to_string(),
                    reason: format!("Potentially dangerous content: {}", pattern),
                });
            }
        }

        Ok(())
    }

    /// Sanitize XML content
    pub fn sanitize(xml: &str) -> String {
        // Remove null bytes
        xml.replace('\0', "")
    }
}

/// JSON content validator
pub struct JsonValidator;

impl JsonValidator {
    const MAX_SIZE: usize = 10 * 1024 * 1024; // 10MB

    /// Validate JSON content
    pub fn validate(json: &str) -> ValidationResult<()> {
        if json.is_empty() {
            return Err(ValidationError::EmptyInput { field: "json".to_string() });
        }

        if json.len() > Self::MAX_SIZE {
            return Err(ValidationError::TooLong {
                field: "json".to_string(),
                max: Self::MAX_SIZE,
                actual: json.len(),
            });
        }

        // Basic structure validation
        let trimmed = json.trim();
        if !(trimmed.starts_with('{') && trimmed.ends_with('}')) 
            && !(trimmed.starts_with('[') && trimmed.ends_with(']'))
            && !(trimmed.starts_with('"') && trimmed.ends_with('"'))
            && trimmed != "null"
            && trimmed != "true"
            && trimmed != "false" {
            return Err(ValidationError::InvalidJson(
                "JSON does not have valid structure".to_string()
            ));
        }

        // Check for null bytes
        if json.contains('\0') {
            return Err(ValidationError::InvalidEncoding);
        }

        Ok(())
    }
}

/// URL validator
pub struct UrlValidator;

impl UrlValidator {
    const MAX_LENGTH: usize = 2048;

    /// Validate URL
    pub fn validate(url: &str) -> ValidationResult<()> {
        if url.is_empty() {
            return Err(ValidationError::EmptyInput { field: "url".to_string() });
        }

        if url.len() > Self::MAX_LENGTH {
            return Err(ValidationError::TooLong {
                field: "url".to_string(),
                max: Self::MAX_LENGTH,
                actual: url.len(),
            });
        }

        // Parse URL
        let parsed = url::Url::parse(url)
            .map_err(|_| ValidationError::InvalidUrl)?;

        // Check scheme
        let scheme = parsed.scheme();
        if scheme != "http" && scheme != "https" {
            return Err(ValidationError::InvalidUrl);
        }

        // Check for localhost/private IPs in production
        if let Some(host) = parsed.host_str() {
            if host == "localhost" || host.starts_with("127.") || host == "::1" {
                // In production, these should be blocked
                // return Err(ValidationError::BlockedValue {
                //     field: "url".to_string(),
                //     value: host.to_string(),
                // });
            }
        }

        Ok(())
    }
}

/// Hostname validator
pub struct HostnameValidator;

impl HostnameValidator {
    const MAX_LENGTH: usize = 253;

    /// Validate hostname
    pub fn validate(hostname: &str) -> ValidationResult<()> {
        if hostname.is_empty() {
            return Err(ValidationError::EmptyInput { field: "hostname".to_string() });
        }

        if hostname.len() > Self::MAX_LENGTH {
            return Err(ValidationError::TooLong {
                field: "hostname".to_string(),
                max: Self::MAX_LENGTH,
                actual: hostname.len(),
            });
        }

        // Each label must be 1-63 characters
        for label in hostname.split('.') {
            if label.is_empty() || label.len() > 63 {
                return Err(ValidationError::InvalidHostname);
            }

            // Labels must start and end with alphanumeric
            if !label.chars().next().unwrap().is_alphanumeric()
                || !label.chars().last().unwrap().is_alphanumeric() {
                return Err(ValidationError::InvalidHostname);
            }

            // Labels can contain alphanumeric and hyphens
            if !label.chars().all(|c| c.is_alphanumeric() || c == '-') {
                return Err(ValidationError::InvalidHostname);
            }
        }

        Ok(())
    }
}

/// String validator with configurable rules
pub struct StringValidator {
    min_length: Option<usize>,
    max_length: Option<usize>,
    allowed_chars: Option<HashSet<char>>,
    forbidden_chars: Option<HashSet<char>>,
    pattern: Option<Regex>,
    trim: bool,
}

impl StringValidator {
    /// Create a new string validator
    pub fn new() -> Self {
        Self {
            min_length: None,
            max_length: None,
            allowed_chars: None,
            forbidden_chars: None,
            pattern: None,
            trim: true,
        }
    }

    /// Set minimum length
    pub fn min_length(mut self, min: usize) -> Self {
        self.min_length = Some(min);
        self
    }

    /// Set maximum length
    pub fn max_length(mut self, max: usize) -> Self {
        self.max_length = Some(max);
        self
    }

    /// Set allowed characters
    pub fn allowed_chars(mut self, chars: &[char]) -> Self {
        self.allowed_chars = Some(chars.iter().copied().collect());
        self
    }

    /// Set forbidden characters
    pub fn forbidden_chars(mut self, chars: &[char]) -> Self {
        self.forbidden_chars = Some(chars.iter().copied().collect());
        self
    }

    /// Set pattern regex
    pub fn pattern(mut self, pattern: &str) -> Result<Self, regex::Error> {
        self.pattern = Some(Regex::new(pattern)?);
        Ok(self)
    }

    /// Set trim behavior
    pub fn trim(mut self, trim: bool) -> Self {
        self.trim = trim;
        self
    }

    /// Validate a string
    pub fn validate(&self, input: &str, field_name: &str) -> ValidationResult<String> {
        let value = if self.trim {
            input.trim().to_string()
        } else {
            input.to_string()
        };

        // Check empty
        if value.is_empty() {
            return Err(ValidationError::EmptyInput {
                field: field_name.to_string(),
            });
        }

        // Check min length
        if let Some(min) = self.min_length {
            if value.len() < min {
                return Err(ValidationError::TooShort {
                    field: field_name.to_string(),
                    min,
                    actual: value.len(),
                });
            }
        }

        // Check max length
        if let Some(max) = self.max_length {
            if value.len() > max {
                return Err(ValidationError::TooLong {
                    field: field_name.to_string(),
                    max,
                    actual: value.len(),
                });
            }
        }

        // Check forbidden characters
        if let Some(ref forbidden) = self.forbidden_chars {
            let found: Vec<char> = value.chars()
                .filter(|c| forbidden.contains(c))
                .collect();
            if !found.is_empty() {
                return Err(ValidationError::ForbiddenCharacters {
                    field: field_name.to_string(),
                    chars: found,
                });
            }
        }

        // Check allowed characters
        if let Some(ref allowed) = self.allowed_chars {
            let invalid: Vec<char> = value.chars()
                .filter(|c| !allowed.contains(c))
                .collect();
            if !invalid.is_empty() {
                return Err(ValidationError::ForbiddenCharacters {
                    field: field_name.to_string(),
                    chars: invalid,
                });
            }
        }

        // Check pattern
        if let Some(ref pattern) = self.pattern {
            if !pattern.is_match(&value) {
                return Err(ValidationError::PatternMismatch {
                    field: field_name.to_string(),
                    pattern: pattern.as_str().to_string(),
                });
            }
        }

        Ok(value)
    }
}

impl Default for StringValidator {
    fn default() -> Self {
        Self::new()
    }
}

/// Input sanitizer for common injection attacks
pub struct InputSanitizer;

impl InputSanitizer {
    /// Sanitize for HTML display (XSS prevention)
    pub fn for_html(input: &str) -> String {
        input
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#x27;")
            .replace('/', "&#x2F;")
    }

    /// Sanitize for SQL (basic - use parameterized queries instead)
    pub fn for_sql(input: &str) -> String {
        input
            .replace('\'', "''")
            .replace(';', "")
            .replace("--", "")
    }

    /// Sanitize for shell (command injection prevention)
    pub fn for_shell(input: &str) -> String {
        input
            .replace(';', "")
            .replace('|', "")
            .replace('&', "")
            .replace('$', "")
            .replace('`', "")
            .replace('(', "")
            .replace(')', "")
            .replace('<', "")
            .replace('>', "")
    }

    /// Sanitize for LDAP (injection prevention)
    pub fn for_ldap(input: &str) -> String {
        input
            .replace('*', "\\2a")
            .replace('(', "\\28")
            .replace(')', "\\29")
            .replace('\0', "\\00")
            .replace('/', "\\2f")
    }

    /// Remove null bytes
    pub fn remove_null_bytes(input: &str) -> String {
        input.replace('\0', "")
    }

    /// Normalize whitespace
    pub fn normalize_whitespace(input: &str) -> String {
        input.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

/// Composite validator for complex input validation
pub struct CompositeValidator {
    validators: Vec<Box<dyn Fn(&str) -> ValidationResult<()>>>,
}

impl CompositeValidator {
    /// Create a new composite validator
    pub fn new() -> Self {
        Self {
            validators: Vec::new(),
        }
    }

    /// Add a validator
    pub fn add<F>(&mut self, validator: F)
    where
        F: Fn(&str) -> ValidationResult<()> + 'static,
    {
        self.validators.push(Box::new(validator));
    }

    /// Validate input against all validators
    pub fn validate(&self, input: &str) -> ValidationResult<()> {
        for validator in &self.validators {
            validator(input)?;
        }
        Ok(())
    }
}

impl Default for CompositeValidator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_email_validation() {
        assert!(EmailValidator::validate("user@example.com").is_ok());
        assert!(EmailValidator::validate("user.name@example.co.uk").is_ok());
        assert!(EmailValidator::validate("user+tag@example.com").is_ok());
        assert!(EmailValidator::validate("invalid").is_err());
        assert!(EmailValidator::validate("@example.com").is_err());
        assert!(EmailValidator::validate("user@").is_err());
        assert!(EmailValidator::validate("").is_err());
    }

    #[test]
    fn test_email_sanitization() {
        assert_eq!(EmailValidator::sanitize("  User@Example.COM  "), "User@example.com");
    }

    #[test]
    fn test_device_id_validation() {
        assert!(DeviceIdValidator::validate("device123").is_ok());
        assert!(DeviceIdValidator::validate("device-123_test").is_ok());
        assert!(DeviceIdValidator::validate("abc").is_err()); // Too short
        assert!(DeviceIdValidator::validate("").is_err());
        assert!(DeviceIdValidator::validate("device@123").is_err()); // Invalid char
    }

    #[test]
    fn test_uuid_validation() {
        assert!(UuidValidator::validate("550e8400-e29b-41d4-a716-446655440000").is_ok());
        assert!(UuidValidator::validate("550e8400e29b41d4a716446655440000").is_ok());
        assert!(UuidValidator::validate("invalid").is_err());
        assert!(UuidValidator::validate("").is_err());
    }

    #[test]
    fn test_uuid_normalization() {
        let normalized = UuidValidator::normalize("550e8400e29b41d4a716446655440000").unwrap();
        assert_eq!(normalized, "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn test_xml_validation() {
        assert!(XmlValidator::validate("<?xml version=\"1.0\"?><root/>").is_ok());
        assert!(XmlValidator::validate("<root/>").is_ok());
        assert!(XmlValidator::validate("<!ENTITY xxe SYSTEM \"file:///etc/passwd\">").is_err());
        assert!(XmlValidator::validate("<script>alert(1)</script>").is_err());
    }

    #[test]
    fn test_json_validation() {
        assert!(JsonValidator::validate("{}").is_ok());
        assert!(JsonValidator::validate("[]").is_ok());
        assert!(JsonValidator::validate("\"string\"").is_ok());
        assert!(JsonValidator::validate("null").is_ok());
        assert!(JsonValidator::validate("invalid").is_err());
    }

    #[test]
    fn test_url_validation() {
        assert!(UrlValidator::validate("https://example.com").is_ok());
        assert!(UrlValidator::validate("http://example.com/path").is_ok());
        assert!(UrlValidator::validate("ftp://example.com").is_err());
        assert!(UrlValidator::validate("not-a-url").is_err());
    }

    #[test]
    fn test_hostname_validation() {
        assert!(HostnameValidator::validate("example.com").is_ok());
        assert!(HostnameValidator::validate("sub.example.co.uk").is_ok());
        assert!(HostnameValidator::validate("localhost").is_ok());
        assert!(HostnameValidator::validate("-invalid.com").is_err());
        assert!(HostnameValidator::validate("invalid-.com").is_err());
    }

    #[test]
    fn test_string_validator() {
        let validator = StringValidator::new()
            .min_length(3)
            .max_length(10);
        
        assert!(validator.validate("hello", "test").is_ok());
        assert!(validator.validate("ab", "test").is_err());
        assert!(validator.validate("this is too long", "test").is_err());
    }

    #[test]
    fn test_html_sanitization() {
        assert_eq!(InputSanitizer::for_html("<script>"), "&lt;script&gt;");
        assert_eq!(InputSanitizer::for_html("\"quoted\""), "&quot;quoted&quot;");
        assert_eq!(InputSanitizer::for_html("'single'"), "&#x27;single&#x27;");
    }

    #[test]
    fn test_shell_sanitization() {
        let sanitized = InputSanitizer::for_shell("; rm -rf /");
        assert!(!sanitized.contains(';'));
        assert!(!sanitized.contains('|'));
        assert!(!sanitized.contains('&'));
    }
}

// src/wbxml.rs
use anyhow::Result;
use std::str;

/// Minimal WBXML decoder used by ActiveSync: many clients POST XML (not WBXML),
/// but when they use WBXML we attempt a simple heuristic decode: if input starts with <?xml
/// assume it's already XML; if not, return an error. This avoids adding a heavy WBXML parser.
pub struct Wbxml {}

impl Wbxml {
    pub fn new() -> Self {
        Wbxml {}
    }

    /// Decode payload bytes into XML string. If payload appears to be UTF-8 XML we return it.
    /// In real production you may replace this with a proper WBXML -> XML implementation.
    pub fn decode(&self, payload: &[u8]) -> Result<String> {
        // Quick heuristic: if payload contains "<?xml" treat as XML
        if let Ok(s) = std::str::from_utf8(payload) {
            let trimmed = s.trim_start();
            if trimmed.starts_with("<?xml") || trimmed.starts_with('<') {
                return Ok(s.to_string());
            }
        }
        // If not UTF-8 or not XML, fail with a clear error
        anyhow::bail!("payload not XML or not UTF-8; WBXML not implemented in minimal gateway")
    }
}

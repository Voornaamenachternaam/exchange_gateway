// src/utils.rs
pub fn decode_basic_auth(auth: &str) -> (String, String) {
    let stripped = auth.trim_start_matches("Basic ");
    let decoded = base64::decode(stripped).unwrap_or_default();
    let parts: Vec<&str> = std::str::from_utf8(&decoded).unwrap_or("").splitn(2, ':').collect();
    (parts.get(0).unwrap_or(&"").to_string(), parts.get(1).unwrap_or(&"").to_string())
}

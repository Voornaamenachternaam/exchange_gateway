pub fn decode_basic_auth(auth: &str) -> (String, String) {
    let parts: Vec<&str> = auth.split_whitespace().collect();
    if parts.len() != 2 || parts[0] != "Basic" {
        return (String::new(), String::new());
    }

    let decoded = match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, parts[1])
    {
        Ok(d) => d,
        Err(_) => return (String::new(), String::new()),
    };

    let decoded_str = String::from_utf8_lossy(&decoded);
    let mut creds = decoded_str.split(':');
    (
        creds.next().unwrap_or_default().to_string(),
        creds.next().unwrap_or_default().to_string(),
    )
}

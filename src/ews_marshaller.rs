// src/ews_marshaller.rs
/// Very small helpers to construct SOAP/EWS responses for the minimal gateway.
pub fn soap_ok_response(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>{}</s:Body>
</s:Envelope>"#,
        body
    )
}

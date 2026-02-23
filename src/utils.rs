// src/utils.rs
/// Wraps a body string in an EWS SOAP envelope.
#[allow(dead_code)]
pub fn ews_soap_envelope(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>{}</s:Body>
</s:Envelope>"#,
        body
    )
}

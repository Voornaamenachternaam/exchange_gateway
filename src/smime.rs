//! S/MIME certificate harvesting and GAL certificate-store access.
//!
//! Stalwart's directory does not carry per-user S/MIME certificates, so the
//! gateway maintains its own store (SQLite, see `storage::smime_cert`) and
//! populates it from two sources:
//!
//! 1. Certificates embedded in S/MIME-signed mail that flows through the
//!    gateway (EAS SendMail/SmartReply/SmartForward and any other surface
//!    that transports raw MIME), extracted with the `cms`/`x509-cert`
//!    crates (RFC 5652 SignedData).
//! 2. An administrator-seeded directory of DER/PEM X.509 certificates
//!    (config `GATEWAY_SMIME_CERT_STORE_DIR`), keyed by the email
//!    addresses claimed in each certificate's SAN rfc822Name or the
//!    pkcs-9 emailAddress subject RDN.
//!
//! EAS `ResolveRecipients` (MS-ASCMD §2.2.1.15) then answers
//! `CertificateRetrieval` requests from this store.

use anyhow::{Context, Result};
use cms::cert::CertificateChoices;
use cms::content_info::ContentInfo;
use cms::signed_data::SignedData;
use der::Decode;
use mail_parser::MimeHeaders;
use sha2::Digest;
use std::collections::BTreeSet;
use std::path::Path;
use x509_cert::der::asn1::{Ia5String, PrintableString, TeletexString, Utf8StringRef};
use x509_cert::ext::pkix::SubjectAltName;
use x509_cert::ext::pkix::name::GeneralName;

/// OID pkcs-9 emailAddress (rfc822Mailbox in DN).
const OID_EMAIL_ADDRESS: der::oid::ObjectIdentifier =
    der::oid::ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.1");
/// OID id-ce-subjectAltName.
const OID_SUBJECT_ALT_NAME: der::oid::ObjectIdentifier =
    der::oid::ObjectIdentifier::new_unwrap("2.5.29.17");

/// One harvested S/MIME certificate plus the email identities it claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmimeCertificate {
    /// Lowercased email address this certificate is indexed under in the
    /// store (the GAL lookup key).
    pub email: String,
    /// SHA-256 fingerprint of the DER encoding (hex, lowercase).
    pub sha256: String,
    /// Full DER-encoded X.509 certificate.
    pub cert_der: Vec<u8>,
    pub not_before_unix: i64,
    pub not_after_unix: i64,
}

fn cert_email_addresses(cert: &x509_cert::Certificate) -> Vec<String> {
    let mut emails = Vec::new();
    if let Some(extensions) = cert.tbs_certificate.extensions.as_ref() {
        for ext in extensions.iter() {
            if ext.extn_id != OID_SUBJECT_ALT_NAME {
                continue;
            }
            let Ok(san) = SubjectAltName::from_der(ext.extn_value.as_bytes()) else {
                continue;
            };
            for name in san.0.iter() {
                if let GeneralName::Rfc822Name(addr) = name {
                    emails.push(addr.to_string());
                }
            }
        }
    }
    // Legacy pkcs-9 emailAddress subject RDN.
    for rdn in cert.tbs_certificate.subject.0.iter() {
        for atv in rdn.0.iter() {
            if atv.oid != OID_EMAIL_ADDRESS {
                continue;
            }
            if let Ok(s) = Ia5String::from_der(atv.value.value()) {
                emails.push(s.to_string());
            } else if let Ok(s) = Utf8StringRef::from_der(atv.value.value()) {
                emails.push(s.as_str().to_string());
            } else if let Ok(s) = PrintableString::from_der(atv.value.value()) {
                emails.push(s.to_string());
            } else if let Ok(s) = TeletexString::from_der(atv.value.value()) {
                emails.push(s.to_string());
            }
        }
    }
    emails.retain(|e| email_address::EmailAddress::is_valid(e));
    emails.sort_unstable();
    emails.dedup();
    emails
}

fn time_to_unix(t: &x509_cert::time::Time) -> i64 {
    t.to_unix_duration().as_secs() as i64
}

fn collect_signed_data_certs(sd: &SignedData, out: &mut Vec<(x509_cert::Certificate, Vec<u8>)>) {
    if let Some(cert_set) = sd.certificates.as_ref() {
        for choice in cert_set.0.iter() {
            if let CertificateChoices::Certificate(cert) = choice
                && let Ok(der_bytes) = der::Encode::to_der(cert)
            {
                out.push((cert.clone(), der_bytes));
            }
        }
    }
}

/// Parse an arbitrary DER blob that may be a CMS ContentInfo wrapping a
/// SignedData (RFC 5652 / S/MIME `application/pkcs7-signature` and
/// `application/pkcs7-mime; smime-type=signed-data|certs-only`) or a bare
/// SignedData structure, and extract every X.509 certificate it carries.
pub fn certs_from_cms_blob(bytes: &[u8]) -> Vec<(x509_cert::Certificate, Vec<u8>)> {
    let mut out = Vec::new();
    if let Ok(sd) = SignedData::from_der(bytes) {
        collect_signed_data_certs(&sd, &mut out);
    }
    if out.is_empty()
        && let Ok(ci) = ContentInfo::from_der(bytes)
        // ContentInfo.content is [0] EXPLICIT CONTENT holding the
        // SignedData directly (RFC 5652 §3); decode through the wrapper tag.
        && let Ok(sd) = ci.content.decode_as::<SignedData>()
    {
        collect_signed_data_certs(&sd, &mut out);
    }
    out
}

pub(crate) fn decode_mime_part_body(part: &mail_parser::MessagePart<'_>) -> Option<Vec<u8>> {
    let raw = part.contents();
    // Content-transfer-encoding in mail-parser is exposed via the part
    // headers; prefer the raw bytes and handle base64 explicitly since the
    // CMS blob must be byte-exact.
    let is_b64 = part
        .content_transfer_encoding()
        .map(|v| v.eq_ignore_ascii_case("base64"))
        .unwrap_or(false);
    if is_b64 {
        use base64::Engine;
        let compact: Vec<u8> = raw
            .iter()
            .copied()
            .filter(|b| !b.is_ascii_whitespace())
            .collect();
        if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(&compact) {
            return Some(decoded);
        }
    }
    Some(raw.to_vec())
}

fn is_smime_content_type(part: &mail_parser::MessagePart<'_>) -> bool {
    let Some(ct) = part.content_type() else {
        return false;
    };
    let t = ct.ctype();
    let st = ct.subtype().unwrap_or("");
    t.eq_ignore_ascii_case("application")
        && [
            "pkcs7-signature",
            "x-pkcs7-signature",
            "pkcs7-mime",
            "x-pkcs7-mime",
        ]
        .iter()
        .any(|w| st.eq_ignore_ascii_case(w))
}

pub(crate) fn message_from_addresses(msg: &mail_parser::Message<'_>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(from) = msg.from() {
        for addr in from.iter() {
            if let Some(email) = addr.address()
                && email_address::EmailAddress::is_valid(email)
            {
                out.push(email.to_lowercase());
            }
        }
    }
    out
}

/// Index harvested certs under the email identities they claim
/// (SAN rfc822Name / subject emailAddress). Certs with no claimed identity
/// fall back to the message's From addresses (the originator attached the
/// certificate).
fn build_records(
    certs: Vec<(x509_cert::Certificate, Vec<u8>)>,
    from: &[String],
) -> Vec<SmimeCertificate> {
    let mut seen = BTreeSet::new();
    let mut records = Vec::new();
    for (cert, der_bytes) in certs {
        let mut emails = cert_email_addresses(&cert);
        // From addresses are a fallback identity, used only when the
        // certificate itself carries no email identity. Extending a cert
        // that has claimed identities with the envelope sender would let a
        // sender publish their own certificate under an arbitrary From
        // address (cross-identity indexing, CWE-345).
        if emails.is_empty() {
            for f in from {
                if !emails.iter().any(|e| e == f) {
                    emails.push(f.clone());
                }
            }
        }
        let emails: Vec<String> = emails.into_iter().map(|e| e.to_lowercase()).collect();
        if emails.is_empty() {
            continue;
        }
        let sha256 = const_hex::encode(sha2::Sha256::digest(&der_bytes));
        let nb = time_to_unix(&cert.tbs_certificate.validity.not_before);
        let na = time_to_unix(&cert.tbs_certificate.validity.not_after);
        for email in emails {
            if seen.insert((email.clone(), sha256.clone())) {
                records.push(SmimeCertificate {
                    email,
                    sha256: sha256.clone(),
                    cert_der: der_bytes.clone(),
                    not_before_unix: nb,
                    not_after_unix: na,
                });
            }
        }
    }
    records
}

/// Extract every S/MIME certificate embedded in a MIME message, keyed by
/// the email addresses they represent.
pub fn harvest_mime(raw: &[u8]) -> Vec<SmimeCertificate> {
    let Some(msg) = mail_parser::MessageParser::default().parse(raw) else {
        return Vec::new();
    };
    let from = message_from_addresses(&msg);
    let mut all = Vec::new();
    for part in msg.parts.iter() {
        if !is_smime_content_type(part) {
            continue;
        }
        let Some(bytes) = decode_mime_part_body(part) else {
            continue;
        };
        all.extend(certs_from_cms_blob(&bytes));
    }
    build_records(all, &from)
}

/// Parse one or more X.509 certificates from a DER or PEM blob (used for
/// the administrator-seeded certificate directory import).
pub fn certs_from_pem_or_der(bytes: &[u8], fallback_email: Option<&str>) -> Vec<SmimeCertificate> {
    let mut certs: Vec<(x509_cert::Certificate, Vec<u8>)> = Vec::new();
    if let Ok(cert) = x509_cert::Certificate::from_der(bytes) {
        if let Ok(der_bytes) = der::Encode::to_der(&cert) {
            certs.push((cert, der_bytes));
        }
    } else if let Ok(text) = std::str::from_utf8(bytes) {
        for chunk in text.split("-----BEGIN CERTIFICATE-----").skip(1) {
            let Some(end) = chunk.find("-----END CERTIFICATE-----") else {
                continue;
            };
            let b64: String = chunk[..end]
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            use base64::Engine;
            let Ok(der_bytes) = base64::engine::general_purpose::STANDARD.decode(&b64) else {
                continue;
            };
            if let Ok(cert) = x509_cert::Certificate::from_der(&der_bytes) {
                certs.push((cert, der_bytes));
            }
        }
    }
    let from = fallback_email
        .filter(|e| email_address::EmailAddress::is_valid(e))
        .map(|e| vec![e.to_lowercase()])
        .unwrap_or_default();
    build_records(certs, &from)
}

/// Harvest S/MIME certificates from a MIME message and persist them,
/// retaining only records whose email identity is in `owned_identities`
/// (the authenticated sender's mailbox addresses). The MIME path performs
/// no CMS signature or chain validation, so GAL entries are only ever
/// written for identities the sender owns.
pub async fn harvest_and_store(
    storage: &crate::storage::Storage,
    raw_mime: &[u8],
    owned_identities: &[String],
) -> usize {
    let records = harvest_mime(raw_mime);
    let owned: BTreeSet<&str> = owned_identities.iter().map(String::as_str).collect();
    let records: Vec<_> = records
        .into_iter()
        .filter(|r| owned.contains(r.email.as_str()))
        .collect();
    if records.is_empty() {
        return 0;
    }
    let mut stored = 0;
    for rec in &records {
        match storage.put_smime_cert(rec).await {
            Ok(()) => stored += 1,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    email = %rec.email,
                    "failed to persist harvested S/MIME certificate"
                );
            }
        }
    }
    stored
}

/// Import all `*.pem` / `*.der` / `*.cer` / `*.crt` (X.509) and `*.p7b` /
/// `*.p7c` (certs-only CMS "PKCS#7") files from the administrator-seeded S/MIME
/// certificate directory into the store. Returns the number of
/// certificate/email pairs inserted or refreshed.
pub async fn import_cert_dir(storage: &crate::storage::Storage, dir: &Path) -> Result<usize> {
    let mut entries = match std::fs::read_dir(dir) {
        Ok(e) => e.collect::<std::io::Result<Vec<_>>>()?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).context("reading S/MIME cert store directory"),
    };
    entries.sort_by_key(|e| e.path());
    let mut count = 0usize;
    for entry in entries {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        // X.509 containers (PEM/DER) and certs-only CMS "PKCS#7" bundles
        // (.p7b/.p7c). Anything else is skipped with a warning below; if a
        // file has an accepted extension but no usable certificates, that is
        // also logged rather than silently ignored.
        let ext = ext.as_deref().unwrap_or_default();
        if !matches!(ext, "pem" | "der" | "cer" | "crt" | "p7b" | "p7c") {
            continue;
        }
        // For plain PEM/DER files the email identities come from the
        // certificate itself; the file stem is used only as a fallback
        // email when the certificate lacks any email identity.
        let stem_email = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());
        let bytes = std::fs::read(&path)
            .with_context(|| format!("reading S/MIME cert file {}", path.display()))?;
        // Try X.509 first, then a certs-only CMS SignedData bundle.
        let mut records = certs_from_pem_or_der(&bytes, stem_email.as_deref());
        if records.is_empty() {
            records = build_records(
                certs_from_cms_blob(&bytes),
                &stem_email
                    .filter(|e| email_address::EmailAddress::is_valid(e))
                    .map(|e| vec![e.to_lowercase()])
                    .unwrap_or_default(),
            );
        }
        if records.is_empty() {
            tracing::warn!(
                path = %path.display(),
                "S/MIME cert directory: no usable X.509 certificates in file"
            );
            continue;
        }
        for rec in records {
            storage.put_smime_cert(&rec).await?;
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    // Test fixtures generated with OpenSSL: a self-signed X.509 certificate
    // for alice@example.com (SAN rfc822Name + pkcs-9 emailAddress in subject)
    // and a detached CMS SignedData (smime.p7s) carrying that certificate.
    const CERT_DER: &[u8] = include_bytes!("../tests/fixtures/smime_alice.der");
    const CERT_PEM: &[u8] = include_bytes!("../tests/fixtures/smime_alice.pem");
    const SIG_DER: &[u8] = include_bytes!("../tests/fixtures/smime_sig.der");

    #[test]
    fn parses_certificate_der_and_extracts_email_identities() {
        let certs = certs_from_pem_or_der(CERT_DER, None);
        assert_eq!(certs.len(), 1);
        assert_eq!(certs[0].email, "alice@example.com");
        assert_eq!(certs[0].cert_der, CERT_DER);
        assert_eq!(
            certs[0].sha256,
            const_hex::encode(sha2::Sha256::digest(CERT_DER))
        );
        assert!(certs[0].not_before_unix > 0);
        assert!(certs[0].not_after_unix > certs[0].not_before_unix);
    }

    #[test]
    fn parses_certificate_pem_and_ignores_file_stem_fallback() {
        let certs = certs_from_pem_or_der(CERT_PEM, Some("bob@example.com"));
        assert!(certs.iter().any(|c| c.email == "alice@example.com"));
        let certs = certs_from_pem_or_der(CERT_PEM, Some("not-an-email"));
        assert!(certs.iter().all(|c| c.email == "alice@example.com"));
    }

    #[test]
    fn parses_detached_cms_signeddata_and_extracts_certificates() {
        let certs = certs_from_cms_blob(SIG_DER);
        assert!(!certs.is_empty(), "expected at least one certificate");
        assert_eq!(certs[0].1, CERT_DER);
    }

    #[test]
    fn harvests_certificates_from_multipart_signed_mime() {
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(SIG_DER);
        let mime = format!(
            "From: Alice <alice@example.com>\r\n\
             To: Bob <bob@example.com>\r\n\
             Subject: signed\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/signed; protocol=\"application/pkcs7-signature\"; boundary=\"b\"\r\n\
             \r\n\
             --b\r\n\
             Content-Type: text/plain\r\n\r\n\
             hello\r\n\
             --b\r\n\
             Content-Type: application/pkcs7-signature; name=\"smime.p7s\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             Content-Disposition: attachment; filename=\"smime.p7s\"\r\n\
             \r\n\
             {sig_b64}\r\n\
             --b--\r\n"
        );
        let records = harvest_mime(mime.as_bytes());
        assert!(
            records.iter().any(|c| c.email == "alice@example.com"),
            "expected alice@example.com cert, got {records:?}"
        );
        assert!(records.iter().all(|c| !c.cert_der.is_empty()));
        // Unsigned mail yields nothing and must not panic.
        assert!(harvest_mime(b"From: a@b.c\r\n\r\nno signature").is_empty());
        assert!(harvest_mime(b"not mime at all").is_empty());
    }

    #[tokio::test]
    async fn put_get_smime_cert_roundtrip() {
        let dir =
            std::env::temp_dir().join(format!("gateway-smime-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("test.db");
        let storage = crate::storage::Storage::new(&format!("sqlite://{}?mode=rwc", db.display()))
            .await
            .unwrap();
        storage.init_schema().await.unwrap();

        let recs = certs_from_pem_or_der(CERT_DER, None);
        let rec = recs.first().unwrap().clone();
        storage.put_smime_cert(&rec).await.unwrap();
        storage.put_smime_cert(&rec).await.unwrap(); // upsert refresh

        let now = chrono::Utc::now().timestamp();
        let got = storage
            .get_smime_certs("alice@example.com", now)
            .await
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].cert_der, CERT_DER);
        // Unknown identity and outside validity window both miss.
        assert!(
            storage
                .get_smime_certs("nobody@example.com", now)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            storage
                .get_smime_certs("alice@example.com", rec.not_after_unix + 60)
                .await
                .unwrap()
                .is_empty()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

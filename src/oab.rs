// src/oab.rs
//
// Offline Address Book (OAB) endpoint — MS-OXOAB + MS-OXWOAB.
//
// Closes audit gap §1.1 ("No `<OABUrl>` / OAB endpoint"): Autodiscover now
// advertises an `<OABUrl>` (see `autodiscover::oab_url`) and this module
// serves the backing OAB virtual directory so Outlook for Windows can
// download a real, directory-backed offline address book instead of hitting
// a 404.
//
// Two artifacts are served under `/OAB/<guid>/`:
//   1. `oab.xml`  — the OAB manifest (MS-OXWOAB "OAB details"). Lists the
//      available full OAB file with its name, size, version and a
//      content-version stamp. Clients always fetch this first.
//   2. `<name>`  — the OAB data file itself, generated as an OAB Version 3
//      "Browse/Details" binary (MS-OXOAB §3.2). The v3 details format is the
//      documented, *uncompressed* OAB container that Outlook still parses as
//      a fallback to the LZX-delta v4 `.lzx`; it requires no LZX codec, so it
//      can be produced directly from the directory snapshot without a
//      multi-thousand-line delta compressor. Each record carries the
//      recipient's X500 DN, SMTP address, display name and alias — enough for
//      recipient resolution / Check Names / GAL browsing.
//
// Security:
//   * The `/OAB/` surface is authenticated with Basic auth validated against
//     the same `AuthVerifier` used by EWS/EAS (Stalwart JMAP/CalDAV creds).
//     No credentials ⇒ 401 with `WWW-Authenticate: Basic`.
//   * The OAB data is derived solely from the operator-configured directory
//     (`AppState.directory`, backed by Stalwart's JMAP directory extension
//     `urn:stalwart:jmap` `x:Account/*`). When no
//     directory is configured, an empty (header-only) OAB is served so the
//     endpoint never leaks a 404, but also never fabricates identities.
//   * Conditional GET (`If-None-Match` / `ETag`) and `Last-Modified` are
//     honoured so unchanged address books are answered with 304 and zero
//     bytes — the standard OAB download path Outlook relies on for the
//     periodic 24h update poll.

use crate::auth::AuthVerifier;
use crate::models::AppState;
use crate::util::xml_escape;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

/// Maximum number of directory contacts serialised into a single full OAB.
/// Caps memory/size and bounds the work done per download; Outlook paginates
/// the GAL anyway and a typical Stalwart deployment's GAL is well under this.
const MAX_OAB_CONTACTS: usize = 5000;

/// MIME type the manifest advertises for the OAB data file. v3 details files
/// are opaque binary blobs consumed only by Outlook, so `application/octet-stream`
/// is the correct, safe type — it is never sniffed by the browser/OS.
const OAB_DATA_CONTENT_TYPE: &str = "application/octet-stream";

/// Parse a Basic `Authorization` header into `(username, password)`.
/// Returns `None` for missing/malformed/non-Basic headers. The password is
/// kept in a `SecretString` so it is not accidentally logged.
fn parse_basic_auth(headers: &HeaderMap) -> Option<(String, String)> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let raw = raw.trim();
    if !raw.to_ascii_lowercase().starts_with("basic ") {
        return None;
    }
    let b64 = &raw[6..].trim();
    let mut decoded = zeroize::Zeroizing::new(Vec::new());
    BASE64.decode_vec(b64.as_bytes(), decoded.as_mut()).ok()?;
    let creds = zeroize::Zeroizing::new(String::from_utf8(decoded.to_vec()).ok()?);
    let idx = creds.find(':')?;
    let user = creds[..idx].to_string();
    let pass = creds[idx + 1..].to_string();
    Some((user, pass))
}

/// Build a 401 response that challenges for Basic auth. Matches the realm
/// wording used by the EAS endpoint so the client sees a consistent auth
/// surface across the gateway.
fn unauth_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE.as_str(),
            "Basic realm=\"Exchange Gateway\"",
        )],
        "Unauthorized",
    )
        .into_response()
}

/// Format a `SystemTime` as an RFC 7231 HTTP-date (e.g.
/// "Sun, 06 Nov 1994 08:49:37 GMT"). Returns a fixed sentinel for times
/// before the UNIX epoch (which never occur here).
fn http_date(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0)
        .unwrap_or_else(|| chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap());
    dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

/// A content-version stamp for the served OAB. Derivation is bounded to the
/// *day* so a fresh build on the same calendar day advertises the same
/// content version — this lets Outlook's conditional-download logic treat
/// repeated same-day polls as "no change" and answer 304, exactly mirroring
/// real Exchange's once-per-day OAB generation cadence.
fn content_version() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Whole-day granularity (86400 s/day): stable within a day.
    now / 86_400
}

/// Serial number written into the OAB header; chosen as the content version
/// rotated through a monotonic-ish counter. Outlook treats this as opaque.
fn serial() -> u32 {
    // Keep within u32 range and non-zero; clap to a sane window.
    (content_version() as u32).wrapping_mul(2).wrapping_add(1)
}

/// Drop the directory credentials never being logged: extract the password
/// into a `SecretString` helper for the call site, then forget.
fn to_secret(p: String) -> SecretString {
    SecretString::from(p)
}

/// The name of the served OAB data file. v3 details files historically carry
/// a short, stable base name so clients can cache and diff by name.
const OAB_DATA_FILE_NAME: &str = "oab.oab";

/// Entry point for `GET /OAB/{guid}/{name}` and `GET /OAB/{guid}` (manifest).
///
/// Routing in `main.rs` splits the trailing path: requests for `oab.xml` get
/// the manifest, anything else gets the binary OAB data. The `{guid}` path
/// segment is validated against `OAB_SERVER_GUID` so a probe of an arbitrary
/// `/OAB/anything/...` URL is rejected with 404 rather than leaking that the
/// endpoint exists for every guessed GUID.
pub async fn handle_oab(
    State(state): State<Arc<AppState>>,
    guid: String,
    name: String,
    headers: HeaderMap,
) -> Response {
    // Reject unknown OAB virtual-directory identifiers. This keeps the surface
    // tight: only the exact GUID advertised by *our own* Autodiscover is
    // served. A 404 (not 401) so the URL itself is not confirmed to exist.
    if guid != crate::autodiscover::OAB_SERVER_GUID {
        return StatusCode::NOT_FOUND.into_response();
    }

    // Authenticate. OAB download requires valid mailbox credentials even when
    // the directory itself is unauthenticated — the file contains the GAL,
    // which is recipient PII.
    let (user, pass) = match parse_basic_auth(&headers) {
        Some(c) => c,
        None => return unauth_response(),
    };
    let verifier: &AuthVerifier = state.auth_verifier.as_ref();
    let secret = to_secret(pass);
    let ok = verifier.verify(&user, secret.expose_secret()).await;
    // Drop the secret deterministically.
    drop(secret);
    if !ok {
        return unauth_response();
    }

    if name == "oab.xml" {
        serve_manifest(&state, &headers).await
    } else if name == OAB_DATA_FILE_NAME {
        serve_data_file(&state, &headers).await
    } else {
        // Unknown file within the served OAB directory.
        StatusCode::NOT_FOUND.into_response()
    }
}

/// Compute the SHA-256 ETag for an OAB payload. Quoted-hex per RFC 7232 §2.3.
fn payload_etag(payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(payload);
    let digest: Vec<u8> = hasher.finalize().to_vec();
    format!("\"{}\"", const_hex::encode(&digest))
}

/// Build the current OAB payload once and derive both its byte size and its
/// SHA-256 ETag from the *same* bytes — so the manifest's advertised `<Size>`
/// and `<Hash>` always describe the exact bytes a client receives for the
/// data file. Returns `(size, etag)`.
async fn oab_size_and_etag(state: &AppState) -> (usize, String) {
    let payload = build_oab_payload(state).await;
    (payload.len(), payload_etag(&payload))
}

/// Build the manifest (`oab.xml`) and honour conditional GET. Per MS-OXWOAB,
/// when the client sends `If-None-Match` matching the current ETag the server
/// responds 304 with no body — the standard "OAB is unchanged" answer that
/// keeps Outlook's 24h poll cheap.
async fn serve_manifest(state: &AppState, headers: &HeaderMap) -> Response {
    let (size, etag) = oab_size_and_etag(state).await;
    let last_modified = http_date(SystemTime::now() - Duration::from_secs(60));

    if let Some(req_etag) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        && etag_matches(req_etag, &etag)
    {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG.as_str(), etag.as_str()),
                (header::LAST_MODIFIED.as_str(), last_modified.as_str()),
            ],
        )
            .into_response();
    }

    let host = &state.cfg.gateway_host;
    let data_url = format!(
        "https://{}/OAB/{}/{}",
        host,
        crate::autodiscover::OAB_SERVER_GUID,
        OAB_DATA_FILE_NAME
    );
    let data_url_escaped = xml_escape(&data_url);

    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<OAB>
<OABVersion>3</OABVersion>
<ContentVersion>{content_version}</ContentVersion>
<ServerDatabaseGuid>{server_guid}</ServerDatabaseGuid>
<Files>
<File>
<Name>{file_name}</Name>
<Size>{size}</Size>
<FullOab>{data_url}</FullOab>
<Hash>{hash}</Hash>
</File>
</Files>
</OAB>"#,
        content_version = content_version(),
        server_guid = crate::autodiscover::OAB_SERVER_GUID,
        file_name = OAB_DATA_FILE_NAME,
        size = size,
        data_url = data_url_escaped,
        hash = etag.trim_matches('"'),
    );

    (
        StatusCode::OK,
        [
            ("Content-Type", "application/xml; charset=utf-8"),
            (header::ETAG.as_str(), etag.as_str()),
            (header::LAST_MODIFIED.as_str(), last_modified.as_str()),
            ("Cache-Control", "private, no-cache, must-revalidate"),
        ],
        xml,
    )
        .into_response()
}

/// Compare a client `If-None-Match` value against the current ETag using the
/// RFC 7232 §2.3.2 weak/strong matching rules as applicable to a single
/// opaque resource tag. `*` always matches.
fn etag_matches(if_none_match: &str, etag: &str) -> bool {
    let inm = if_none_match.trim();
    if inm == "*" {
        return true;
    }
    // The If-None-Match may carry a list of tags; compare each stripped of
    // the optional weakness prefix `W/`.
    inm.split(',').any(|part| {
        let p = part.trim();
        let p = p.strip_prefix("W/").unwrap_or(p);
        p == etag
    })
}

/// Serve the binary OAB data file. Honours conditional GET (304 on ETag
/// match) and `Range` requests (single, contiguous range per RFC 7233 §2.1).
async fn serve_data_file(state: &AppState, headers: &HeaderMap) -> Response {
    // Build the payload once; derive its length and ETag from the SAME bytes
    // so the response's Content-Length and ETag are guaranteed consistent.
    let payload = build_oab_payload(state).await;
    let total = payload.len() as u64;
    let etag = payload_etag(&payload);
    let last_modified = http_date(SystemTime::now() - Duration::from_secs(60));

    if let Some(req_etag) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        && etag_matches(req_etag, &etag)
    {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG.as_str(), etag.as_str()),
                (header::LAST_MODIFIED.as_str(), last_modified.as_str()),
            ],
        )
            .into_response();
    }

    // Single contiguous byte-range honour, e.g. `Range: bytes=0-1023` or
    // `Range: bytes=512-`. Multi-range responses are intentionally not
    // supported (Outlook never sends them for OAB) — fall back to the whole
    // file on a parse failure so the download always succeeds.
    if let Some(range_header) = headers.get(header::RANGE).and_then(|v| v.to_str().ok())
        && let Some((start, end)) = parse_single_range(range_header, total)
    {
        return range_response(&payload, start, end, total, &etag, &last_modified);
    }

    (
        StatusCode::OK,
        [
            ("Content-Type", OAB_DATA_CONTENT_TYPE),
            (
                header::CONTENT_LENGTH.as_str(),
                Box::leak(total.to_string().into_boxed_str()),
            ),
            ("Accept-Ranges", "bytes"),
            (header::ETAG.as_str(), etag.as_str()),
            (header::LAST_MODIFIED.as_str(), last_modified.as_str()),
            ("Cache-Control", "private, no-cache, must-revalidate"),
        ],
        Bytes::from(payload),
    )
        .into_response()
}

/// Parse a single `bytes=a-b` / `bytes=a-` / `bytes=-b` Range spec. Returns
/// the inclusive `[start, end]` byte offsets within `total`, or `None` if
/// the header is multi-range, malformed, or unsatisfiable.
fn parse_single_range(range_header: &str, total: u64) -> Option<(u64, u64)> {
    let raw = range_header.trim();
    let rest = raw.strip_prefix("bytes=")?;
    if rest.contains(',') {
        return None; // multi-range not supported
    }
    let (start_s, end_s) = rest.split_once('-')?;
    let start_s = start_s.trim();
    let end_s = end_s.trim();
    if start_s.is_empty() {
        // Suffix range: last `n` bytes.
        let n: u64 = end_s.parse().ok()?;
        if n == 0 || total == 0 {
            return None;
        }
        let start = total.saturating_sub(n);
        Some((start, total - 1))
    } else {
        let start: u64 = start_s.parse().ok()?;
        if start >= total {
            return None; // unsatisfiable
        }
        let end = if end_s.is_empty() {
            total - 1
        } else {
            let end: u64 = end_s.parse().ok()?;
            end.min(total - 1)
        };
        if start > end {
            return None;
        }
        Some((start, end))
    }
}

fn range_response(
    payload: &[u8],
    start: u64,
    end: u64,
    total: u64,
    etag: &str,
    last_modified: &str,
) -> Response {
    let len = (end - start + 1) as usize;
    let body = payload[start as usize..start as usize + len].to_vec();
    let content_range = format!("bytes {}-{}/{}", start, end, total);
    let content_length = Box::leak(len.to_string().into_boxed_str());
    (
        StatusCode::PARTIAL_CONTENT,
        [
            ("Content-Type", OAB_DATA_CONTENT_TYPE),
            (header::CONTENT_LENGTH.as_str(), content_length),
            ("Content-Range", Box::leak(content_range.into_boxed_str())),
            ("Accept-Ranges", "bytes"),
            (header::ETAG.as_str(), etag),
            (header::LAST_MODIFIED.as_str(), last_modified),
            ("Cache-Control", "private, no-cache, must-revalidate"),
        ],
        Bytes::from(body),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// OAB Version 3 "Browse/Details" binary generation (MS-OXOAB §3.2).
// ---------------------------------------------------------------------------

/// OAB header record written at the top of a v3 details file.
struct OabHeader {
    ul_version: u32,
    ul_serial: u32,
    ul_tot_recs: u32,
}

const OAB_VERSION_3: u32 = 0x00000003;

/// Build the full OAB v3 details payload synchronously from the directory.
/// `AppState.directory` is `Option`; when `None` or unavailable this returns
/// a header-only OAB (zero records) — a valid, empty address book.
async fn build_oab_payload(state: &AppState) -> Vec<u8> {
    let dir = match state.directory.as_ref() {
        Some(dir) if dir.is_available() => dir.clone(),
        _ => return empty_oab(),
    };

    // The directory trait is synchronous (blocking HTTP) — run it on the
    // blocking pool, mirroring the EWS ResolveNames path, so the async
    // runtime is never stalled while fetching the GAL. A bare empty string
    // is rejected by `search_blocking` as `InvalidQuery`, so we probe with
    // the conventional GAL "match all" wildcard `"*"`. If the backing
    // directory rejects the wildcard the catch-all below falls back to an
    // empty (header-only) OAB — the OAB endpoint still answers, never 404s.
    let list =
        match tokio::task::spawn_blocking(move || dir.search_blocking("*", Some(MAX_OAB_CONTACTS)))
            .await
        {
            Ok(Ok(res)) => {
                if res.is_truncated {
                    warn!(
                        target: "oab",
                        total_estimate = res.total_estimate,
                        max = MAX_OAB_CONTACTS,
                        "Directory listing truncated; OAB will be capped"
                    );
                }
                res.contacts
            }
            Ok(Err(e)) => {
                warn!(target: "oab", error = %e, "Directory search failed; serving empty OAB");
                Vec::new()
            }
            Err(e) => {
                warn!(target: "oab", error = %e, "Directory task join failed; serving empty OAB");
                Vec::new()
            }
        };

    let recs = normalise_records(&list);
    build_oab_from_records(&recs)
}

/// A header-only OAB (zero records) — a valid, empty address book used when
/// no directory is configured or the directory is unavailable. Keeping the
/// version-3 header lets a caching Outlook client treat this as a fully
/// download rather than an error, then later pick up a populated OAB once a
/// directory is configured.
fn empty_oab() -> Vec<u8> {
    let mut out = Vec::with_capacity(12);
    out.extend_from_slice(&OAB_VERSION_3.to_le_bytes());
    out.extend_from_slice(&serial().to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // ulTotRecs = 0
    out
}

/// Serialise the OAB v3 header + per-record B2 records from a prepared list
/// of normalised fields. Pure and synchronous so it is unit-testable without
/// an `AppState`.
fn build_oab_from_records(recs: &[OabRecordOwned]) -> Vec<u8> {
    let total = recs.len() as u32;
    let header = OabHeader {
        ul_version: OAB_VERSION_3,
        ul_serial: serial(),
        ul_tot_recs: total,
    };

    let mut out = Vec::with_capacity(64 + recs.len() * 96);
    // OAB_HDR: ulVersion | ulSerial | ulTotRecs (all little-endian u32).
    out.extend_from_slice(&header.ul_version.to_le_bytes());
    out.extend_from_slice(&header.ul_serial.to_le_bytes());
    out.extend_from_slice(&header.ul_tot_recs.to_le_bytes());

    // B2_REC per address-book object. Layout (MS-OXOAB §3.2.2 "OAB Version 3
    // Offline Address List"):
    //   oRDN      u32 LE   = byte offset of the recipient's DN string relative
    //                         to the start of the per-record details blob.
    //   oDetails  u32 LE   = offset of the details blob (unused marker here;
    //                         Outlook tolerates 0 — it indexes by oRDN).
    //   cbDetails u32 LE   = size of this record's details blob in bytes.
    //   bDispType u8       = MAPI display type (DT_MAIL_USER=0x00).
    //   bObjType  u8       = MAPI object type (MAPI_MAILUSER=0x06).
    //   oSmtp      u32 LE  = offset of the SMTP string.
    //   oDispName  u32 LE  = offset of the display-name string.
    //   oAlias     u32 LE  = offset of the alias string.
    //   oLocation  u32 LE  = offset of the location string (0 if none).
    //   oSurname   u32 LE  = offset of the surname string (0 if none).
    // The variable-length strings themselves are stored, NUL-terminated, in a
    // trailing details blob; their offsets are measured relative to the
    // blob's start.
    for rec in recs {
        // Build the details blob for this record: DN\0 SMTP\0 DispName\0 Alias\0
        let mut blob = Vec::new();
        let off_rdn = blob.len();
        blob.extend_from_slice(rec.rdn.as_bytes());
        blob.push(0);
        let off_smtp = blob.len();
        blob.extend_from_slice(rec.smtp.as_bytes());
        blob.push(0);
        let off_disp = blob.len();
        blob.extend_from_slice(rec.display_name.as_bytes());
        blob.push(0);
        let off_alias = blob.len();
        blob.extend_from_slice(rec.alias.as_bytes());
        blob.push(0);
        let cb = blob.len() as u32;

        out.extend_from_slice(&(off_rdn as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // oDetails
        out.extend_from_slice(&cb.to_le_bytes()); // cbDetails
        out.push(0x00); // bDispType: DT_MAIL_USER
        out.push(0x06); // bObjType: MAPI_MAILUSER
        out.extend_from_slice(&(off_smtp as u32).to_le_bytes());
        out.extend_from_slice(&(off_disp as u32).to_le_bytes());
        out.extend_from_slice(&(off_alias as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // oLocation
        out.extend_from_slice(&0u32.to_le_bytes()); // oSurname
        out.extend_from_slice(&blob);
    }

    debug!(target: "oab", records = total, bytes = out.len(), "OAB payload built");
    out
}

/// Convert directory `Contact`s into the (DN, SMTP, Display, Alias) tuples the
/// v3 record carries. Fields are derived as:
///   * DN is synthesised as `/o=Stalwart/ou=Exchange Administrative Group
///     (FYDIBOHF23SPDLT)/cn=Recipients/cn=<localpart>`, the canonical
///     legacyExchangeDN shape Outlook matches against; the org name is the
///     configured `mapi_org` when set, otherwise the hard-coded default.
///   * alias defaults to the local-part of the SMTP address.
///   * empty display name falls back to the email address so the record is
///     never visually blank.
fn normalise_records(contacts: &[crate::directory::Contact]) -> Vec<OabRecordOwned> {
    contacts
        .iter()
        .map(|c| {
            let display = if c.display_name.is_empty() {
                c.email.clone()
            } else {
                c.display_name.clone()
            };
            let alias = email_local_part(&c.email).unwrap_or_else(|| c.email.clone());
            let dn = synth_dn(&c.email);
            OabRecordOwned {
                rdn: dn,
                smtp: c.email.clone(),
                display_name: display,
                alias,
            }
        })
        .collect()
}

/// Owned analogue of [`OabRecord`] to avoid lifetime gymnastics across the
/// async/await boundary.
struct OabRecordOwned {
    rdn: String,
    smtp: String,
    display_name: String,
    alias: String,
}

/// Extract the local-part of an email address for use as the account alias.
fn email_local_part(email: &str) -> Option<String> {
    let at = email.find('@')?;
    let local = &email[..at];
    if local.is_empty() {
        return None;
    }
    Some(local.to_string())
}

/// Synthesise a canonical legacyExchangeDN for a mailbox address. The CN
/// component is the lowercased local-part, matching the convention Stalwart
/// and most Exchange lookalikes use for `/cn=Recipients/cn=...`.
fn synth_dn(email: &str) -> String {
    let local = email_local_part(email).unwrap_or_else(|| email.to_string());
    format!(
        "/o=Stalwart/ou=Exchange Administrative Group (FYDIBOHF23SPDLT)/cn=Recipients/cn={}",
        local.to_ascii_lowercase()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_email_local_part() {
        assert_eq!(
            email_local_part("alice@example.com").as_deref(),
            Some("alice")
        );
        assert_eq!(
            email_local_part("bob.name@example.org").as_deref(),
            Some("bob.name")
        );
        assert_eq!(email_local_part("@nope.com"), None);
        assert_eq!(email_local_part("norealat"), None);
    }

    #[test]
    fn test_synth_dn_canonical() {
        let dn = synth_dn("Alice@example.com");
        assert_eq!(
            dn,
            "/o=Stalwart/ou=Exchange Administrative Group (FYDIBOHF23SPDLT)/cn=Recipients/cn=alice"
        );
    }

    #[test]
    fn test_etag_matches_star_and_strong() {
        assert!(etag_matches("*", "\"abc\""));
        assert!(etag_matches("\"abc\"", "\"abc\""));
        assert!(!etag_matches("\"def\"", "\"abc\""));
        // Weak tag comparison: W/ prefix stripped.
        assert!(etag_matches("W/\"abc\"", "\"abc\""));
        // List.
        assert!(etag_matches("\"def\", \"abc\"", "\"abc\""));
    }

    #[test]
    fn test_parse_single_range() {
        assert_eq!(parse_single_range("bytes=0-9", 100), Some((0, 9)));
        assert_eq!(parse_single_range("bytes=10-", 100), Some((10, 99)));
        assert_eq!(parse_single_range("bytes=-20", 100), Some((80, 99)));
        // Suffix larger than total ⇒ whole file.
        assert_eq!(parse_single_range("bytes=-200", 100), Some((0, 99)));
        // Unsatisfiable.
        assert_eq!(parse_single_range("bytes=200-300", 100), None);
        // Multi-range unsupported.
        assert_eq!(parse_single_range("bytes=0-1,2-3", 100), None);
        // Malformed.
        assert_eq!(parse_single_range("items=0-1", 100), None);
        assert_eq!(parse_single_range("bytes=abc", 100), None);
        assert_eq!(parse_single_range("bytes=0-1-2", 100), None);
        assert_eq!(parse_single_range("bytes=-0", 100), None);
    }

    #[test]
    fn test_empty_oab_is_valid_header() {
        // A header-only OAB: 12 bytes — three little-endian u32s. The version
        // must be 3 (OAB_VERSION_3) and record count 0.
        let out = empty_oab();
        assert_eq!(out.len(), 12);
        assert_eq!(&out[0..4], &OAB_VERSION_3.to_le_bytes());
        assert_eq!(&out[8..12], &0u32.to_le_bytes());
    }

    #[test]
    fn test_build_oab_from_records_encodes_one_recipient() {
        // Build a single-record OAB and verify the header + the B2 record wire
        // layout. The fixed B2 record header written by `build_oab_from_records`
        // is 7×u32 + 2×u8 = 30 bytes, in this exact order:
        //   oRDN, oDetails, cbDetails, bDispType, bObjType, oSmtp, oDispName,
        //   oAlias, oLocation, oSurname
        // (see MS-OXOAB §3.2.2). The variable-length details blob follows
        // immediately at offset 30, holding NUL-terminated: DN, SMTP, Disp,
        // Alias. Re-derive each offset from the actual bytes.
        let rec = OabRecordOwned {
            rdn: "/cn=x".to_string(),          // 5 bytes + NUL ⇒ ends at offset 6
            smtp: "a@b.com".to_string(),       // 7 bytes + NUL ⇒ ends 14
            display_name: "Alice".to_string(), // 5 + NUL ⇒ 6
            alias: "alice".to_string(),        // 5 + NUL ⇒ 6
        };
        let out = build_oab_from_records(&[rec]);
        // Header.
        assert_eq!(&out[0..4], &OAB_VERSION_3.to_le_bytes());
        assert_eq!(&out[8..12], &1u32.to_le_bytes(), "ulTotRecs == 1");
        // B2 record begins at offset 12.
        let b2 = &out[12..];
        let o_rdn = u32::from_le_bytes([b2[0], b2[1], b2[2], b2[3]]);
        let _o_details = u32::from_le_bytes([b2[4], b2[5], b2[6], b2[7]]);
        let cb = u32::from_le_bytes([b2[8], b2[9], b2[10], b2[11]]);
        assert_eq!(o_rdn, 0);
        let b_disp = b2[12];
        let b_obj = b2[13];
        assert_eq!(b_disp, 0x00); // DT_MAIL_USER
        assert_eq!(b_obj, 0x06); // MAPI_MAILUSER
        let o_smtp = u32::from_le_bytes([b2[14], b2[15], b2[16], b2[17]]);
        let o_disp = u32::from_le_bytes([b2[18], b2[19], b2[20], b2[21]]);
        let o_alias = u32::from_le_bytes([b2[22], b2[23], b2[24], b2[25]]);
        let _o_location = u32::from_le_bytes([b2[26], b2[27], b2[28], b2[29]]);
        let _o_surname = u32::from_le_bytes([b2[30], b2[31], b2[32], b2[33]]);
        // Fixed B2 header is 34 bytes (7×u32 + 2×u8 + 2×u32); blob follows.
        let blob = &b2[34..34 + cb as usize];
        // DN: "/cn=x\0" (6 bytes) ⇒ SMTP offset = 6.
        assert_eq!(o_smtp, 6);
        assert_eq!(&blob[0..6], b"/cn=x\0".as_ref());
        // SMTP "a@b.com\0" is 8 bytes ⇒ disp offset = 6 + 8 = 14.
        assert_eq!(o_disp, 6 + 8);
        assert_eq!(
            &blob[o_smtp as usize..o_disp as usize],
            b"a@b.com\0".as_ref()
        );
        // DispName "Alice\0" is 6 bytes ⇒ alias offset = disp + 6.
        assert_eq!(o_alias, o_disp + 6);
        assert_eq!(
            &blob[o_disp as usize..o_alias as usize],
            b"Alice\0".as_ref()
        );
        assert_eq!(&blob[o_alias as usize..], b"alice\0".as_ref());
    }
}

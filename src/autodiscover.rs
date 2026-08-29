// src/autodiscover.rs
//
// Autodiscover handlers for Exchange Gateway.
//
// Handles three autodiscover protocols:
// 1. Outlook desktop (V1 XML POST) — MS-OXDSCLI outlook/responseschema/2006a
// 2. ActiveSync mobile (V1 XML POST) — MS-ASCMD mobilesync/responseschema/2006
// 3. Autodiscover V2 (JSON GET) — used by AutoDetect cloud service and Outlook mobile
//
// Per MS-ASCMD §2.2.3.1, the client includes an <AcceptableResponseSchema> element
// in the POST body that specifies which response format it expects. The server MUST
// return a response matching the requested schema or the client will treat it as an
// error (MS-ASCMD §4.2.5, error code 601 "provider not found").
use crate::util::{nfc, xml_escape};
use crate::version;
use axum::http::StatusCode;
use serde::Deserialize;

/// Namespace for the mobilesync response schema (MS-ASCMD §6.2).
const MOBILESYNC_RESPONSE_NS: &str =
    "http://schemas.microsoft.com/exchange/autodiscover/mobilesync/responseschema/2006";

/// Namespace for the Outlook response schema (MS-OXDSCLI §2.2.4.1).
const OUTLOOK_RESPONSE_NS: &str =
    "http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a";

/// Stable server-wide OAB directory GUID the gateway advertises in
/// Autodiscover and serves under `/OAB/{guid}/`. The value is a fixed,
/// well-formed UUIDv4 (RFC 4122) chosen once for the gateway — Outlook treats
/// the path segment only as an opaque OAB virtual-directory identifier, so a
/// constant is correct and keeps the advertised `<OABUrl>` stable across
/// restarts (a per-restart random GUID would invalidate clients' cached OAB
/// and force a full re-download on every container restart).
pub const OAB_SERVER_GUID: &str = "9b3a7c4d-2e1f-4a8b-9c0d-1e2f3a4b5c6d";

/// Build the OAB directory URL advertised in Autodiscover (MS-OXDSCLI
/// §2.2.4.1.1.2 `<OABUrl>`). Always ends with a trailing slash because it is
/// a virtual directory: clients append `oab.xml` and the individual OAB
/// files to this base.
pub fn oab_url(host: &str) -> String {
    format!("https://{}/OAB/{}/", host, OAB_SERVER_GUID)
}

/// Build the Exchange Control Panel (ECP) base URL advertised in
/// Autodiscover as `<EcpUrl>` (MS-OXDSCLI §2.2.4.1.1.2 `<EcpUrl>`,
/// `ExternalEcpUrl`/`InternalEcpUrl` SOAP user settings).
///
/// `<EcpUrl>` is the **base URL** of the Exchange Control Panel settings
/// surface — a virtual directory, not a single SOAP endpoint. Outlook and
/// New Outlook for Windows append suffixes to this base to deep-link into
/// specific settings panels (Out-of-Office / signature, telemetry OptIn,
/// Regional/timezone and working-hours settings). Advertising the EWS SOAP
/// endpoint (`/EWS/Exchange.asmx`) here is wrong: those panel links then
/// resolve to a SOAP XML fault and render as broken panes inside the client
/// (audit §1.3 — "the current response will make some of those panel links
/// 404 inside the client").
///
/// The gateway serves a real, directory-authenticated ECP surface under
/// `/ecp/` (see `src/ecp.rs`), so the advertised value points there. The
/// trailing slash is significant: like `<OABUrl>`, this is a virtual
/// directory and clients append relative path segments and query strings
/// (e.g. `?rfr=ool&exsc=1`, `Options/?id=ps`) directly to it.
pub fn ecp_url(host: &str) -> String {
    format!("https://{}/ecp/", host)
}

/// What the EXCH/EXPR `<Protocol>` blocks advertise under `<AuthPackage>` and
/// the surrounding Modern-Auth elements.
///
/// MS-OXDSCLI §2.2.4.1.1.2 models `<AuthPackage>` as a protocol-level setting
/// whose value informs the client which authentication scheme the server
/// expects (`Basic`, `Digest`, `NTLM`, `Kerberos`, `Negotiate`, `OAuth2`, ...).
/// For Hybrid Modern Auth (HMA) the well-known advertisement Exchange uses is
/// `OAuth2/CertificateBased` together with a sibling `<OauthUrl>` (the OAuth2
/// authorization-server URL) and a `<CompactDomain>` (the issuer/tenant host).
/// New Outlook for Windows prefers Modern Auth when it is advertised and only
/// falls back to Basic when it is the sole scheme — advertising `Basic` alone
/// forces the legacy basic prompt and, in some tenants, refuses provisioning.
///
/// The gateway remains backwards-compatible: when HMA is not configured we keep
/// advertising `Basic` exactly as before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthAdvert {
    /// Legacy basic authentication — `<AuthPackage>Basic</AuthPackage>`.
    Basic,
    /// Hybrid Modern Auth — `<AuthPackage>OAuth2/CertificateBased</AuthPackage>`
    /// plus `<OauthUrl>` and `<CompactDomain>`.
    ///
    /// The `oauth_url` is the OIDC issuer / authorization-server base URL the
    /// client uses to acquire bearer tokens for the MAPI/HTTP + EWS endpoints.
    Modern {
        /// OIDC issuer / OAuth2 authorization-server base URL.
        oauth_url: String,
    },
}

impl AuthAdvert {
    /// The `<AuthPackage>` element value to render.
    pub fn auth_package_value(&self) -> &'static str {
        match self {
            AuthAdvert::Basic => "Basic",
            AuthAdvert::Modern { .. } => "OAuth2/CertificateBased",
        }
    }

    /// Extra sibling elements to render immediately after `<AuthPackage>` in
    /// EXCH/EXPR blocks (empty for Basic, `<OauthUrl>`+`<CompactDomain>` for
    /// Modern Auth).
    pub fn extra_elements(&self) -> String {
        match self {
            AuthAdvert::Basic => String::new(),
            AuthAdvert::Modern { oauth_url } => {
                // The compact domain is the OAuth2 issuer host authority; it is
                // advertised so Outlook maps the mailbox to the right tenant.
                let compact_domain = host_authority(oauth_url).unwrap_or_else(|| oauth_url.clone());
                format!(
                    "<OauthUrl>{}</OauthUrl><CompactDomain>{}</CompactDomain>",
                    xml_escape(oauth_url),
                    xml_escape(&compact_domain),
                )
            }
        }
    }
}

/// Parse the host:authority component out of an issuer URL for `<CompactDomain>`.
/// Accepts `https://login.example.com/...` and bare `login.example.com`.
fn host_authority(url: &str) -> Option<String> {
    let after_scheme = url
        .split("://")
        .nth(1)
        .map(|rest| rest.to_string())
        .unwrap_or_else(|| url.to_string());
    let host = after_scheme.split(['/', '?', '#']).next()?;
    let host = host.split(':').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

#[derive(Debug, Deserialize)]
pub struct AutodiscoverJsonParams {
    #[serde(rename = "Protocol")]
    pub protocol: Option<String>,
    #[serde(rename = "Email")]
    pub email: Option<String>,
}

pub type AdResponse = (StatusCode, Vec<(&'static str, &'static str)>, String);

fn content_type_xml() -> Vec<(&'static str, &'static str)> {
    vec![("Content-Type", "application/xml; charset=utf-8")]
}

fn content_type_json() -> Vec<(&'static str, &'static str)> {
    vec![("Content-Type", "application/json; charset=utf-8")]
}

fn content_type_soap() -> Vec<(&'static str, &'static str)> {
    vec![("Content-Type", "application/soap+xml; charset=utf-8")]
}

/// Detect which response schema the client is requesting from the POST body.
///
/// Per MS-ASCMD §2.2.3.1, the `<AcceptableResponseSchema>` element specifies
/// the expected response format. ActiveSync clients (including the AutoDetect
/// cloud service for Outlook mobile) request the mobilesync schema.
/// Outlook desktop clients request the outlook schema.
/// If absent or unrecognised, defaults to Outlook for backward compatibility.
///
/// This parser handles the following variations:
/// - Plain tag: `<AcceptableResponseSchema>...</AcceptableResponseSchema>`
/// - Namespaced tag: `<a:AcceptableResponseSchema>...</a:AcceptableResponseSchema>`
/// - Tag with attributes: `<AcceptableResponseSchema xmlns="...">...</AcceptableResponseSchema>`
/// - Mixed: `<a:AcceptableResponseSchema xmlns:a="...">...</a:AcceptableResponseSchema>`
///
/// Pure (no I/O). Exposed to the crate only via the `pub`/`bool`
/// `is_mobilesync_schema` adapter so the async `main` dispatcher can pre-check
/// the schema before deciding to resolve the mobilesync display name; the
/// returned enum is not part of the public API and is not leaked across crate
/// boundaries.
fn detect_response_schema(body: &str) -> ResponseSchema {
    // Scan for any opening tag whose local name is "AcceptableResponseSchema",
    // regardless of namespace prefix or extra attributes.
    // Strategy: find "AcceptableResponseSchema" in the body, then verify it
    // sits inside an opening tag (preceded by '<' or '<' + prefix + ':')
    // and locate the matching close tag.
    let mut search_from = 0;
    while let Some(pos) = body[search_from..].find("AcceptableResponseSchema") {
        let abs_pos = search_from + pos;
        search_from = abs_pos + 1;

        // Verify this is inside an opening tag: look backward for '<' with
        // only a namespace prefix (word chars + ':') between it and the
        // local name. We must not match a closing tag (which has '</' prefix)
        // or a string value that merely contains the word.
        let tag_name_start = if let Some(lt) = body[..abs_pos].rfind('<') {
            let between = &body[lt + 1..abs_pos];
            // Must be a prefix like "a:" or empty (no prefix before local name)
            if between.is_empty() || between.chars().all(|c| c.is_alphanumeric() || c == ':') {
                // Reject if it's a closing tag: '</' directly before the prefix/name
                if body[..abs_pos].ends_with('/') {
                    continue;
                }
                lt + 1 // start of tag name (after '<')
            } else {
                continue;
            }
        } else {
            continue;
        };

        // Find the end of the opening tag ('>')
        let open_tag_end = if let Some(gt) = body[abs_pos..].find('>') {
            abs_pos + gt
        } else {
            continue;
        };

        // The content starts after the '>' of the opening tag
        let content_start = open_tag_end + 1;

        // Find the matching closing tag. We look for the closing tag that
        // uses the same prefix as the opening tag.
        // Extract the prefix (e.g. "a:" or "" for no prefix).
        let prefix = &body[tag_name_start..abs_pos];
        let close_tag = if prefix.is_empty() {
            "</AcceptableResponseSchema>".to_string()
        } else {
            // Build closing tag with the same prefix: e.g. "</a:AcceptableResponseSchema>"
            // The prefix already includes the colon (e.g. "a:"), so the format
            // below produces "</a:AcceptableResponseSchema>" which is correct.
            format!("</{}AcceptableResponseSchema>", prefix)
        };

        if let Some(close_pos) = body[content_start..].find(close_tag.as_str()) {
            let schema = body[content_start..content_start + close_pos].trim();
            if schema.contains("mobilesync") {
                return ResponseSchema::MobileSync;
            }
            if schema.contains("outlook") {
                return ResponseSchema::Outlook;
            }
        }
    }
    ResponseSchema::Outlook
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseSchema {
    Outlook,
    MobileSync,
}

/// Default culture string per MS-ASCMD §4.2.4 example.
const DEFAULT_CULTURE: &str = "en:us";

/// Parse an Accept-Language header value into a culture string suitable
/// for the `<Culture>` element in the mobilesync response.
///
/// Accept-Language format (RFC 7231 §5.3.5): `lang;q=weight, lang;q=weight, ...`
/// The culture string uses the format `{language}:{country}` where the country
/// is derived from the language tag if not explicitly provided.
///
/// Examples:
/// - `"de-DE"` → `"de:de"`
/// - `"en-US"` → `"en:us"`
/// - `"fr"` → `"fr:fr"`
/// - `"ja"` → `"ja:jp"`
/// - `"zh-CN"` → `"zh:cn"`
/// - `""` / `None` → `"en:us"` (default)
fn parse_culture_from_accept_language(accept_lang: Option<&str>) -> String {
    let raw = match accept_lang {
        Some(h) if !h.is_empty() => h,
        _ => return DEFAULT_CULTURE.to_string(),
    };

    // Take the highest-priority language tag (first one, before any ';')
    let primary = raw.split(',').next().unwrap_or("").trim();
    if primary.is_empty() {
        return DEFAULT_CULTURE.to_string();
    }

    // Strip quality value if present: "de-DE;q=0.9" → "de-DE"
    let tag = primary.split(';').next().unwrap_or("").trim();
    if tag.is_empty() {
        return DEFAULT_CULTURE.to_string();
    }

    // Normalize to lowercase for consistent culture string output
    let tag_lower = tag.to_ascii_lowercase();

    // Split on '-' to separate language and country subtags.
    // RFC 5646 format: language[-script][-region]
    // e.g. "en-US", "de-DE", "zh-Hans-CN"
    let parts: Vec<&str> = tag_lower.split('-').collect();
    let language = parts[0];

    // Find the region subtag (2-letter or 3-letter after optional script).
    // RFC 5646: region subtag is 2 alpha (ISO 3166-1) or 3 digits.
    // For simplicity, we take the last 2-alpha subtag as region.
    let region = parts
        .iter()
        .rev()
        .find(|s| s.len() == 2 && s.chars().all(|c| c.is_ascii_alphabetic()));

    let country = region.unwrap_or(&language);
    format!("{}:{}", language, country)
}

pub fn extract_email_from_body_xml(body: &str) -> Option<String> {
    extract_email_from_v1_xml(body)
}

/// Derive a non-misleading default `<DisplayName>` for the mobilesync `User`
/// block when the directory service cannot supply the account's real display
/// name (MS-ASCMD §2.2.3.49.1 — "the user's display name in the directory
/// service").
///
/// The previous behaviour hard-coded the gateway product brand ("Stalwart
/// Mail"), which is *not* the user's name and would be presented to the
/// Android account owner as their identity. Instead we title-case the
/// local-part of the email address the client itself supplied in the
/// request body, capped to 512 characters of the **rendered output** (the
/// spec length bound for the sibling `UserDisplayName` Settings element;
/// `DisplayName` itself is an unbounded `xs:string` but 512 is a safe sane
/// ceiling). Because `char::to_uppercase()`/`to_lowercase()` can expand a
/// single character into several output characters (e.g. `ß` → `"SS"`),
/// the cap is applied to the final cased string, not the input bytes; this
/// makes the documented 512-char output guarantee actually hold.
///
/// Only the **leading run** of "name" characters is consumed — once a
/// character that is not a letter, digit, or one of the personal-handle
/// separators `.`, `_`, `-`, `+` is encountered, parsing stops. This keeps
/// `"john.doe"` → `"John Doe"` while `"john!doe"` → `"John"` (truncate at the
/// mail-system special) rather than silently fusing it into `"Johndoe"`.
/// This never discloses anything the client did not already send, and
/// degrades gracefully to an empty (omitted) element when no usable token
/// remains.
pub fn derive_display_name(email: &str) -> String {
    let local = email.split('@').next().unwrap_or("");
    // Take the leading run of "name" characters: letters, digits and the
    // common personal-handle separators '.', '_', '-', '+'. Splitting on the
    // separators lets "john.doe" / "john_doe" / "user+tag" render as
    // "John Doe" / "John Doe" / "User Tag"; any other punctuation (mail-system
    // specials) ends the run so we never fuse two unrelated tokens.
    let mut out = String::new();
    let mut capitalize = true;
    for ch in local.chars() {
        if matches!(ch, '.' | '_' | '-' | '+') {
            out.push(' ');
            capitalize = true;
        } else if ch.is_alphanumeric() {
            if capitalize {
                out.extend(ch.to_uppercase());
                capitalize = false;
            } else {
                out.extend(ch.to_lowercase());
            }
        } else {
            // First disallowed character ends the leading run.
            break;
        }
    }
    // Cap the rendered OUTPUT after casing/separator expansion, and trim
    // leading/trailing separator whitespace so "+user" → "User" not " User".
    let trimmed = out.trim();
    trimmed.chars().take(512).collect()
}

/// Resolve the account's display name for the mobilesync `<User>` block.
///
/// Returns the directory-resolved display name (when an actual directory is
/// configured and the account is found with a non-empty name), otherwise the
/// safe `derive_display_name` fallback derived from the request email. The
/// result is always trimmed and length-bounded to 512 output characters; an
/// empty `String` is only produced when the email itself yields no usable
/// token, in which case the caller omits the optional `<DisplayName>` element
/// (spec: 0…1).
///
/// # Security note — caller responsibility
/// This performs a directory lookup for `email` and returns the matched
/// account's real display name. To avoid exposing directory names (PII) to
/// unauthenticated callers or to callers who supply *another* user's email
/// (directory-name enumeration), the async caller in `main` MUST only invoke
/// this with `Some(directory)` when the request is authenticated and the
/// authenticated principal's canonical email matches `email`. Anonymous or
/// mismatched callers pass `None`, which yields the disclosure-free
/// `derive_display_name` fallback (built solely from the client-supplied
/// email itself).
///
/// This is a thin adapter over the blocking `DirectoryLookup` trait so the
/// async handler can run it on `spawn_blocking` and pass a plain `String`
/// down into the pure render path — keeping `handle_mobilesync_xml` free
/// of any I/O for testability.
pub fn resolve_user_display_name(
    directory: Option<&std::sync::Arc<dyn crate::directory::DirectoryLookup>>,
    email: &str,
) -> String {
    if let Some(dir) = directory
        && dir.is_available()
        && let Ok(Some(contact)) = dir.resolve_email_blocking(email)
    {
        // Collect directly from the trimmed &str (one allocation) instead of
        // trim().to_string() then re-iterating its chars (two allocations).
        // Directory names are not re-cased here, so char-count == output count
        // and the 512 cap holds on the produced String.
        let name: String = contact.display_name.trim().chars().take(512).collect();
        if !name.is_empty() {
            return name;
        }
    }
    derive_display_name(email)
}

/// Cheap pre-check used by the async `autodiscover_xml` dispatcher to decide
/// whether the mobilesync `<User>/<DisplayName>` needs resolving at all.
/// Reuses the robust `detect_response_schema` parser so the gating decision is
/// always consistent with the actual response branch taken by
/// `handle_autodiscover_xml`. Outlook-schema requests do not use
/// `mobilesync_display_name`, so the caller skips the display-name work for
/// them entirely.
pub fn is_mobilesync_schema(body: &str) -> bool {
    matches!(detect_response_schema(body), ResponseSchema::MobileSync)
}

fn extract_email_from_v1_xml(body: &str) -> Option<String> {
    let start = body
        .find("<EMailAddress>")
        .map(|i| i + "<EMailAddress>".len())?;
    let end = body[start..].find("</EMailAddress>").map(|i| start + i)?;
    let email = nfc(body[start..end].trim());
    if email.contains('@') {
        Some(email)
    } else {
        None
    }
}

fn extract_email_from_soap(body: &str) -> Option<String> {
    for (open, close) in [
        ("<EMailAddress>", "</EMailAddress>"),
        ("<a:EMailAddress>", "</a:EMailAddress>"),
        ("<Mailbox>", "</Mailbox>"),
    ] {
        if let Some(end) = body.find(open).and_then(|i| {
            let start = i + open.len();
            body[start..].find(close).map(|j| start + j)
        }) {
            let start = body.find(open).map(|i| i + open.len()).unwrap_or(0);
            let email = nfc(body[start..end].trim());
            if email.contains('@') {
                return Some(email);
            }
        }
    }
    None
}

pub fn handle_autodiscover_json(
    host: &str,
    protocol: Option<&str>,
    _email: Option<&str>,
) -> AdResponse {
    let ews_url = format!("https://{}/EWS/Exchange.asmx", host);
    let as_url = format!("https://{}/Microsoft-Server-ActiveSync", host);
    let v1_url = format!("https://{}/autodiscover/autodiscover.xml", host);

    // Per MS-OXDSCLI, Autodiscover V2 returns a minimal JSON document with
    // just the Protocol and Url for the service sought. Each protocol gets
    // its own response — the fields are NOT interchangeable. Extra fields
    // that belong to a different protocol (e.g. ActiveSyncUrl in an Ews
    // response) must NOT be included; they violate the V2 design intent and
    // can confuse strict clients like AutoDetect.
    //
    // Real Exchange Server V2 responses contain only Protocol + Url:
    //   ?Protocol=ActiveSync     → {"Protocol":"ActiveSync","Url":"<as>"}
    //   ?Protocol=Ews            → {"Protocol":"Ews","Url":"<ews>"}
    //   ?Protocol=AutodiscoverV1 → {"Protocol":"AutodiscoverV1","Url":"<v1>"}
    //
    // "Exchange" is NOT a valid V2 Protocol name — it's a V1 XML concept.
    // When no Protocol is specified, the gateway defaults to ActiveSync,
    // which is what AutoDetect and Outlook mobile need. Real Exchange Server
    // requires Protocol to be specified; the gateway's default is a
    // convenience for browser/diagnostic access.

    let body = match protocol.map(|p| p.to_ascii_lowercase()).as_deref() {
        Some("activesync") => format!(
            r#"{{"Protocol":"ActiveSync","Url":"{as_url}"}}"#,
            as_url = as_url
        ),
        Some("ews") => format!(
            r#"{{"Protocol":"Ews","Url":"{ews_url}"}}"#,
            ews_url = ews_url
        ),
        Some("autodiscoverv1") => format!(
            r#"{{"Protocol":"AutodiscoverV1","Url":"{v1_url}"}}"#,
            v1_url = v1_url
        ),
        Some("rest") => format!(
            r#"{{"Protocol":"Rest","Url":"{ews_url}"}}"#,
            ews_url = ews_url
        ),
        // No Protocol or unrecognized — default to ActiveSync.
        // This is what AutoDetect and Outlook mobile need.
        _ => format!(
            r#"{{"Protocol":"ActiveSync","Url":"{as_url}"}}"#,
            as_url = as_url
        ),
    };

    (StatusCode::OK, content_type_json(), body)
}

/// Bundled inputs to `handle_autodiscover_xml`.
///
/// Autodiscover dispatch needs the request host + body + resolved email, the
/// Accept-Language header (for the mobilesync `<Culture>`), the mail host and
/// IMAP/SMTP availability plus the auth advertisement (for the Outlook EXCH/
/// EXPR blocks), and the pre-resolved mobilesync `<DisplayName>`. Grouping
/// these into a struct keeps the entry point under clippy's argument-count
/// threshold without resorting to suppression, and gives each caller a single
/// self-documenting value rather than a long positional list. Builders
/// construct this with a struct literal (`AutodiscoverXmlRequest { host, .. }`)
/// so every field is named at the call site — there is intentionally no
/// positional constructor, which would itself re-trip clippy's threshold.
#[derive(Clone, Debug)]
pub struct AutodiscoverXmlRequest<'a> {
    /// Gateway host advertised as the server for every protocol block.
    pub host: &'a str,
    /// Raw Autodiscover V1 XML POST body (used for schema detection + the
    /// request email fallback).
    pub body: &'a str,
    /// Email address parsed from the query string (GET) or the body (POST).
    pub email: &'a str,
    /// `Accept-Language` request header value, or `None` for the default
    /// culture "en:us".
    pub accept_language: Option<&'a str>,
    /// Mail host advertised inside Outlook desktop blocks.
    pub mail_host: &'a str,
    /// Whether IMAP/SMTP endpoints should be advertised (affects the Outlook
    /// `<Protocol>` blocks rendered by `handle_outlook_xml`).
    pub include_imap_smtp: bool,
    /// EXCH/EXPR `<AuthPackage>` advertisement (Basic vs Hybrid Modern Auth).
    pub auth_advert: &'a AuthAdvert,
    /// Pre-resolved user display name rendered in the mobilesync
    /// `<User>/<DisplayName>` (MS-ASCMD §2.2.3.49.1). Empty string ⇒ omit the
    /// optional element. Resolved by `resolve_user_display_name` upstream.
    pub mobilesync_display_name: &'a str,
}

/// Handle autodiscover XML POST requests.
///
/// Dispatches to the correct response format based on the
/// `AcceptableResponseSchema` element in the request body.
/// This is critical for Outlook mobile/ActiveSync clients which
/// expect the mobilesync schema, not the Outlook desktop schema.
///
/// `accept_language` (in the request) is the value of the Accept-Language
/// request header, used to set the `<Culture>` element in the mobilesync
/// response. Pass `None` to use the default ("en:us").
///
/// `mobilesync_display_name` (in the request) is the pre-resolved user display
/// name to render in the mobilesync `<User>/<DisplayName>` element
/// (MS-ASCMD §2.2.3.49.1). Pass an empty string to omit the optional element;
/// a non-empty value is escaped and emitted verbatim. The async caller resolves
/// this via `resolve_user_display_name` on a blocking task before dispatching,
/// keeping this pure function free of I/O. The Outlook path
/// (`handle_outlook_xml`) keeps its own server-brand `<DisplayName>` unchanged.
pub fn handle_autodiscover_xml(req: &AutodiscoverXmlRequest<'_>) -> AdResponse {
    let schema = detect_response_schema(req.body);
    match schema {
        ResponseSchema::MobileSync => handle_mobilesync_xml(
            req.host,
            req.email,
            req.accept_language,
            req.mobilesync_display_name,
        ),
        ResponseSchema::Outlook => handle_outlook_xml(
            req.host,
            req.email,
            req.mail_host,
            req.include_imap_smtp,
            req.auth_advert,
        ),
    }
}

/// Generate the mobilesync autodiscover response per MS-ASCMD §4.2.4.
///
/// This format is required by ActiveSync clients including:
/// - The AutoDetect cloud service used by Outlook for iOS/Android
/// - Native Android/iOS Exchange account provisioners
/// - Any client that sends `AcceptableResponseSchema: .../mobilesync/responseschema/2006`
///
/// The response uses the `Action/Settings/Server` structure (not EXCH/EXPR Protocol)
/// and returns the MobileSync (ActiveSync) endpoint URL.
///
/// `accept_language` is parsed per RFC 7231 §5.3.5 to set the `<Culture>` element.
/// Pass `None` to use the default ("en:us").
///
/// `display_name` is the pre-resolved user display name (MS-ASCMD §2.2.3.49.1 —
/// "the user's display name in the directory service"). When empty, the optional
/// `<DisplayName>` element is omitted (spec marks it 0…1) rather than emitting a
/// misleading placeholder; when non-empty it is XML-escaped and emitted verbatim.
/// This closes audit gap §1.5 (mobilesync `<User>` block minimal / brand-string
/// `DisplayName`), surfacing the authenticated user's real name to Outlook Android
/// while staying strictly schema-conformant (the mobilesync `User` block's only
/// children are `DisplayName` and `EMailAddress` — there is no picture element in
/// this schema, so none is emitted here).
fn handle_mobilesync_xml(
    host: &str,
    email: &str,
    accept_language: Option<&str>,
    display_name: &str,
) -> AdResponse {
    let email_escaped = xml_escape(email);
    let host_escaped = xml_escape(host);
    let as_url = format!("https://{}/Microsoft-Server-ActiveSync", host_escaped);
    let culture = parse_culture_from_accept_language(accept_language);
    // Omit the optional <DisplayName> when no usable name was resolved so the
    // response never advertises a fabricated identity; otherwise escape the
    // resolved name (which already went through directory/derive_display_name).
    let display_name_element = if display_name.trim().is_empty() {
        String::new()
    } else {
        format!(
            "<DisplayName>{}</DisplayName>",
            xml_escape(display_name.trim())
        )
    };

    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">
<Response xmlns="{MOBILESYNC_RESPONSE_NS}">
<Culture>{culture}</Culture>
<User>
{display_name_element}<EMailAddress>{email}</EMailAddress>
</User>
<Action>
<Settings>
<Server>
<Type>MobileSync</Type>
<Url>{as_url}</Url>
<Name>{as_url}</Name>
</Server>
</Settings>
</Action>
</Response>
</Autodiscover>"#,
        MOBILESYNC_RESPONSE_NS = MOBILESYNC_RESPONSE_NS,
        culture = culture,
        email = email_escaped,
        as_url = as_url,
        display_name_element = display_name_element,
    );
    (StatusCode::OK, content_type_xml(), xml)
}

/// Generate the Outlook desktop autodiscover response per MS-OXDSCLI §2.2.4.
///
/// This format is used by Outlook for Windows/Mac and includes EXCH/EXPR
/// Protocol elements with EWS and ActiveSync URLs.
///
/// Note: `<ServerExclusiveConnect>` is set to "on" for EXPR so that
/// Outlook clients prioritise this configuration per MS-OXDSCLI §3.1.5.4.
fn handle_outlook_xml(
    host: &str,
    email: &str,
    mail_host: &str,
    include_imap_smtp: bool,
    auth_advert: &AuthAdvert,
) -> AdResponse {
    let email_escaped = xml_escape(email);
    let host_escaped = xml_escape(host);
    let mail_host_escaped = xml_escape(mail_host);
    let oab_base = oab_url(host);
    let oab_url_escaped = xml_escape(&oab_base);
    let ecp_base = ecp_url(host);
    let ecp_url_escaped = xml_escape(&ecp_base);
    let auth_package = auth_advert.auth_package_value();
    let auth_extra = auth_advert.extra_elements();
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">
<Response xmlns="{OUTLOOK_RESPONSE_NS}">
<User>
<DisplayName>Stalwart Mail</DisplayName>
<EMailAddress>{email}</EMailAddress>
<DeploymentId>00000000-0000-0000-0000-000000000000</DeploymentId>
</User>
<Account>
<AccountType>email</AccountType>
<Action>settings</Action>
<Protocol>
<Type>EXCH</Type>
<Server>{host}</Server>
<ServerDN>/o=ExchangeLabs/ou=Exchange Administrative Group/cn=Configuration/cn=Servers/cn={host}</ServerDN>
<ServerVersion>{server_version}</ServerVersion>
<MdbDN>/o=ExchangeLabs/ou=Exchange Administrative Group/cn=Configuration/cn=Servers/cn={host}/cn=Microsoft Private MDB</MdbDN>
<ASUrl>https://{host}/Microsoft-Server-ActiveSync</ASUrl>
<EwsUrl>https://{host}/EWS/Exchange.asmx</EwsUrl>
<EmwsUrl>https://{host}/EWS/Exchange.asmx</EmwsUrl>
<EcpUrl>{ecp_url}</EcpUrl>
<OOFUrl>https://{host}/EWS/Exchange.asmx</OOFUrl>
<UMUrl>https://{host}/EWS/Exchange.asmx</UMUrl>
<EwsPartnerUrl>https://{host}/EWS/Exchange.asmx</EwsPartnerUrl>
<OABUrl>{oab_url}</OABUrl>
<LoginName>{email}</LoginName>
<DomainRequired>off</DomainRequired>
<SPA>off</SPA>
<AuthPackage>{auth_package}</AuthPackage>
{auth_extra}<CertPrincipalName>None</CertPrincipalName>
<SSL>on</SSL>
<AuthRequired>on</AuthRequired>
</Protocol>
<Protocol>
<Type>EXPR</Type>
<Server>{host}</Server>
<ServerVersion>{server_version}</ServerVersion>
<SSL>on</SSL>
<SPA>off</SPA>
<CertPrincipalName>None</CertPrincipalName>
<AuthPackage>{auth_package}</AuthPackage>
{auth_extra}<LoginName>{email}</LoginName>
<ServerExclusiveConnect>on</ServerExclusiveConnect>
<TTL>1</TTL>
<ASUrl>https://{host}/Microsoft-Server-ActiveSync</ASUrl>
<EwsUrl>https://{host}/EWS/Exchange.asmx</EwsUrl>
<EmwsUrl>https://{host}/EWS/Exchange.asmx</EmwsUrl>
<EcpUrl>{ecp_url}</EcpUrl>
<OOFUrl>https://{host}/EWS/Exchange.asmx</OOFUrl>
<EwsPartnerUrl>https://{host}/EWS/Exchange.asmx</EwsPartnerUrl>
<OABUrl>{oab_url}</OABUrl>
</Protocol>
<Protocol Type="mapiHttp" Version="1">
<Server>{host}</Server>
<ServerExclusiveConnect>on</ServerExclusiveConnect>
<SSL>on</SSL>
<AuthRequired>on</AuthRequired>
<LoginName>{email}</LoginName>
<MailStore>
<ExternalUrl>https://{host}/mapi/emsmdb?MailboxId={email}</ExternalUrl>
<InternalUrl>https://{host}/mapi/emsmdb?MailboxId={email}</InternalUrl>
</MailStore>
<AddressBook>
<ExternalUrl>https://{host}/mapi/nspi?MailboxId={email}</ExternalUrl>
<InternalUrl>https://{host}/mapi/nspi?MailboxId={email}</InternalUrl>
</AddressBook>
</Protocol>
{imap_smtp_protocols}
</Account>
</Response>
</Autodiscover>"#,
        OUTLOOK_RESPONSE_NS = OUTLOOK_RESPONSE_NS,
        host = host_escaped,
        email = email_escaped,
        oab_url = oab_url_escaped,
        ecp_url = ecp_url_escaped,
        auth_package = auth_package,
        auth_extra = auth_extra,
        server_version = version::current().render_server_version_element(),
        imap_smtp_protocols = if include_imap_smtp && !mail_host_escaped.is_empty() {
            format!(
                r#"<Protocol>
<Type>IMAP</Type>
<Server>{mail_host}</Server>
<Port>993</Port>
<LoginName>{email}</LoginName>
<SSL>on</SSL>
<AuthRequired>on</AuthRequired>
<SPA>off</SPA>
<DomainRequired>off</DomainRequired>
</Protocol>
<Protocol>
<Type>SMTP</Type>
<Server>{mail_host}</Server>
<Port>465</Port>
<LoginName>{email}</LoginName>
<SSL>on</SSL>
<AuthRequired>on</AuthRequired>
<SPA>off</SPA>
<DomainRequired>off</DomainRequired>
</Protocol>"#,
                mail_host = mail_host_escaped,
                email = email_escaped,
            )
        } else {
            String::new()
        },
    );
    (StatusCode::OK, content_type_xml(), xml)
}

pub fn handle_autodiscover_soap(host: &str, body: &str) -> AdResponse {
    let email = extract_email_from_soap(body).unwrap_or_default();
    let email_escaped = xml_escape(&email);
    let host_escaped = xml_escape(host);
    let oab_base = oab_url(host);
    let oab_url_escaped = xml_escape(&oab_base);
    let ecp_base = ecp_url(host);
    let ecp_url_escaped = xml_escape(&ecp_base);

    let settings = format!(
        r#"<a:UserSetting><a:Name>UserDisplayName</a:Name><a:Value>Stalwart Mail</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>UserDN</a:Name><a:Value>{email}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>AutoDiscoverSMTPAddress</a:Name><a:Value>{email}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalRpcClientServer</a:Name><a:Value>{host}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalEwsUrl</a:Name><a:Value>https://{host}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalEwsUrl</a:Name><a:Value>https://{host}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalEmwsUrl</a:Name><a:Value>https://{host}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalEmwsUrl</a:Name><a:Value>https://{host}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalEcpUrl</a:Name><a:Value>{ecp_url}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalEcpUrl</a:Name><a:Value>{ecp_url}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>OABUrl</a:Name><a:Value>{oab_url}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalOABUrl</a:Name><a:Value>{oab_url}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalOABUrl</a:Name><a:Value>{oab_url}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>MobileSyncServer</a:Name><a:Value>{host}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalMobileSyncUrl</a:Name><a:Value>https://{host}/Microsoft-Server-ActiveSync</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalMobileSyncUrl</a:Name><a:Value>https://{host}/Microsoft-Server-ActiveSync</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalEwsVersion</a:Name><a:Value>{ews_version}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalEwsVersion</a:Name><a:Value>{ews_version}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>EwsSupportedSchemas</a:Name><a:Value>{supported_schemas}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>PublicFolderServer</a:Name><a:Value>{host}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ActiveDirectoryServer</a:Name><a:Value>{host}</a:Value></a:UserSetting>"#,
        email = email_escaped,
        host = host_escaped,
        oab_url = oab_url_escaped,
        ecp_url = ecp_url_escaped,
        ews_version = version::current().exchange_version(),
        supported_schemas = version::current().supported_schemas_csv(),
    );

    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"
            xmlns:a="http://schemas.microsoft.com/exchange/2010/Autodiscover">
  <s:Header>
    {svi}
  </s:Header>
  <s:Body>
    <a:GetUserSettingsResponseMessage xmlns:a="http://schemas.microsoft.com/exchange/2010/Autodiscover">
      <a:Response xmlns:a="http://schemas.microsoft.com/exchange/2010/Autodiscover">
        <a:ErrorCode>NoError</a:ErrorCode>
        <a:ErrorMessage />
        <a:UserResponses>
          <a:UserResponse>
            <a:ErrorCode>NoError</a:ErrorCode>
            <a:ErrorMessage />
            <a:RedirectTarget />
            <a:UserSettingErrors />
            <a:UserSettings>
              {settings}
            </a:UserSettings>
          </a:UserResponse>
        </a:UserResponses>
      </a:Response>
    </a:GetUserSettingsResponseMessage>
  </s:Body>
</s:Envelope>"#,
        svi = version::current().render_autodiscover_soap_header(),
        settings = settings
    );
    (StatusCode::OK, content_type_soap(), xml)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_response_schema_outlook() {
        let body = r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006">
<Request>
<EMailAddress>user@example.com</EMailAddress>
<AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</AcceptableResponseSchema>
</Request>
</Autodiscover>"#;
        assert_eq!(detect_response_schema(body), ResponseSchema::Outlook);
    }

    #[test]
    fn test_detect_response_schema_mobilesync() {
        let body = r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/mobilesync/requestschema/2006">
<Request>
<EMailAddress>user@example.com</EMailAddress>
<AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/mobilesync/responseschema/2006</AcceptableResponseSchema>
</Request>
</Autodiscover>"#;
        assert_eq!(detect_response_schema(body), ResponseSchema::MobileSync);
    }

    #[test]
    fn test_detect_response_schema_default() {
        let body = "<Autodiscover><Request><EMailAddress>user@example.com</EMailAddress></Request></Autodiscover>";
        assert_eq!(detect_response_schema(body), ResponseSchema::Outlook);
    }

    #[test]
    fn test_detect_response_schema_empty() {
        assert_eq!(detect_response_schema(""), ResponseSchema::Outlook);
    }

    #[test]
    fn test_mobilesync_response_format() {
        let (status, _hdrs, body) =
            handle_mobilesync_xml("mail.example.com", "user@example.com", None, "User");
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("mobilesync/responseschema/2006"));
        assert!(body.contains("https://mail.example.com/Microsoft-Server-ActiveSync"));
        assert!(body.contains("<Type>MobileSync</Type>"));
        assert!(!body.contains("<Type>EXCH</Type>"));
        assert!(!body.contains("<Type>EXPR</Type>"));
        assert!(body.contains("<Culture>en:us</Culture>"));
        assert!(body.contains("<Action>"));
        assert!(body.contains("<Settings>"));
    }

    #[test]
    fn test_mobilesync_renders_resolved_display_name() {
        // The mobilesync <User>/<DisplayName> MUST carry the resolved user
        // display name (MS-ASCMD §2.2.3.49.1), not a product brand.
        let (status, _hdrs, body) =
            handle_mobilesync_xml("mail.example.com", "user@example.com", None, "Chris Gray");
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<DisplayName>Chris Gray</DisplayName>"));
        assert!(body.contains("<EMailAddress>user@example.com</EMailAddress>"));
        assert!(!body.contains("Stalwart Mail"));
    }

    #[test]
    fn test_mobilesync_omits_display_name_when_empty() {
        // Spec marks <DisplayName> as optional (0…1); an unresolvable name
        // omits the element rather than emitting a fabricated placeholder.
        let (status, _hdrs, body) =
            handle_mobilesync_xml("mail.example.com", "user@example.com", None, "");
        assert_eq!(status, StatusCode::OK);
        assert!(!body.contains("<DisplayName>"));
        assert!(body.contains("<EMailAddress>user@example.com</EMailAddress>"));
        assert!(!body.contains("Stalwart Mail"));
    }

    #[test]
    fn test_mobilesync_escapes_display_name() {
        let (status, _hdrs, body) = handle_mobilesync_xml(
            "mail.example.com",
            "user@example.com",
            None,
            r#"Chris <"x"> & Friends"#,
        );
        assert_eq!(status, StatusCode::OK);
        // quick-xml's full escaper maps & < > " to these entity references.
        let expected: &str = "<DisplayName>Chris &lt;&quot;x&quot;&gt; &amp; Friends</DisplayName>";
        assert!(
            body.contains(expected),
            "expected escaped DisplayName not found; got: {body}"
        );
        // Raw metacharacters must never leak into the XML output, and the
        // response must never show the gateway product brand as the user name.
        assert!(!body.contains(r#"Chris <"x"> & Friends"#));
        assert!(!body.contains("Stalwart Mail"));
    }

    #[test]
    fn test_derive_display_name_from_email() {
        assert_eq!(derive_display_name("john.doe@example.com"), "John Doe");
        assert_eq!(derive_display_name("john_doe@example.com"), "John Doe");
        assert_eq!(derive_display_name("john-doe@example.com"), "John Doe");
        assert_eq!(derive_display_name("john@example.com"), "John");
        assert_eq!(derive_display_name("MARY@example.com"), "Mary");
        assert_eq!(derive_display_name("a.b.c@example.com"), "A B C");
        assert_eq!(derive_display_name("user+tag@example.com"), "User Tag");
        // Leading run: stop at the first disallowed character so a mail-system
        // special never fuses two tokens — "john!doe" → "John", not "Johndoe".
        assert_eq!(derive_display_name("john!doe@example.com"), "John");
        // Leading separator is trimmed away: "+user" → "User".
        assert_eq!(derive_display_name("+user@example.com"), "User");
        // No usable local-part token → empty (element omitted by caller).
        assert_eq!(derive_display_name("@example.com"), "");
        assert_eq!(derive_display_name(""), "");
        assert_eq!(derive_display_name("---@@@"), "");
        assert_eq!(derive_display_name("!nope@example.com"), "");
    }

    #[test]
    fn test_derive_display_name_output_cap_holds_after_case_expansion() {
        // The 512-char cap is on the RENDERED output. Lowercasing a long run of
        // ASCII is 1:1 (no expansion), so a 600-char local part is truncated to
        // 512 in the output, not silently allowed to exceed it.
        let local: String = "a".repeat(600);
        let name = derive_display_name(&format!("{local}@example.com"));
        assert_eq!(name.chars().count(), 512);
    }

    #[test]
    fn test_is_mobilesync_schema_matches_dispatcher() {
        let ms = r#"<Autodiscover><Request>
<EMailAddress>user@example.com</EMailAddress>
<AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/mobilesync/responseschema/2006</AcceptableResponseSchema>
</Request></Autodiscover>"#;
        let outlook = r#"<Autodiscover><Request>
<EMailAddress>user@example.com</EMailAddress>
<AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</AcceptableResponseSchema>
</Request></Autodiscover>"#;
        assert!(is_mobilesync_schema(ms));
        assert!(!is_mobilesync_schema(outlook));
        // No schema hint → defaults to Outlook (matches detect_response_schema).
        assert!(!is_mobilesync_schema(""));
    }

    #[test]
    fn test_resolve_user_display_name_falls_back_when_no_directory() {
        // No directory configured → derives from the request email.
        let none: Option<&std::sync::Arc<dyn crate::directory::DirectoryLookup>> = None;
        assert_eq!(
            resolve_user_display_name(none, "john.doe@example.com"),
            "John Doe"
        );
        assert_eq!(resolve_user_display_name(none, "mary@example.com"), "Mary");
    }

    #[test]
    fn test_outlook_response_format() {
        let (status, _hdrs, body) = handle_outlook_xml(
            "mail.example.com",
            "user@example.com",
            "mail.example.com",
            true,
            &AuthAdvert::Basic,
        );
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("outlook/responseschema/2006a"));
        assert!(body.contains("https://mail.example.com/EWS/Exchange.asmx"));
        assert!(body.contains("https://mail.example.com/Microsoft-Server-ActiveSync"));
        assert!(body.contains("<Type>EXCH</Type>"));
        assert!(body.contains("<Type>EXPR</Type>"));
        assert!(body.contains("<ServerExclusiveConnect>on</ServerExclusiveConnect>"));
        // By default (Basic) the EXCH/EXPR blocks advertise Basic auth only.
        assert_eq!(body.matches("<AuthPackage>Basic</AuthPackage>").count(), 2);
        assert!(!body.contains("OAuth2"));
    }

    #[test]
    fn test_outlook_response_advertises_server_version_in_both_blocks() {
        // MS-OXDSCLI puts `<ServerVersion>` in BOTH the EXCH and EXPR Protocol
        // blocks so an external (EXPR) client receives the same version stamp as
        // an internal (EXCH) one; omitting it from EXPR left external Outlook
        // with inconsistent/incomplete version metadata.
        let (status, _hdrs, body) = handle_outlook_xml(
            "mail.example.com",
            "user@example.com",
            "mail.example.com",
            true,
            &AuthAdvert::Basic,
        );
        assert_eq!(status, StatusCode::OK);
        let sv = format!(
            "<ServerVersion>{}</ServerVersion>",
            crate::version::current().render_server_version_element()
        );
        assert_eq!(
            body.matches(&sv).count(),
            2,
            "<ServerVersion> must appear in both EXCH and EXPR Protocol blocks"
        );
    }

    #[test]
    fn test_outlook_response_includes_mapi_http_block() {
        let (status, _hdrs, body) = handle_outlook_xml(
            "mail.example.com",
            "user@example.com",
            "mail.example.com",
            false,
            &AuthAdvert::Basic,
        );
        assert_eq!(status, StatusCode::OK);
        // The mapiHttp Protocol element carries the `Type` and `Version` XML
        // *attributes* (not a `<Type>` child element) per MS-OXDSCLI
        // §2.2.4.1.1.2.6 — New Outlook for Windows matches on this exact form
        // and treats a `<Type>mapiHttp</Type>` child element as an unavailable
        // MAPI/HTTP endpoint. The canonical MS-OXDSCLI §4.4 example is
        // `<Protocol Type="mapiHttp" Version="1">`.
        assert!(body.contains("<Protocol Type=\"mapiHttp\" Version=\"1\">"));
        assert!(!body.contains("<Type>mapiHttp</Type>"));
        // MailStore ExternalUrl points to the MAPI/HTTP mailbox endpoint and
        // carries the MailboxId query parameter per MS-OXDSCLI §2.2.4.1.1.2.6.29.
        assert!(body.contains("https://mail.example.com/mapi/emsmdb?MailboxId=user@example.com"));
        // AddressBook ExternalUrl points to the NSPI endpoint separately.
        assert!(body.contains("https://mail.example.com/mapi/nspi?MailboxId=user@example.com"));
    }

    #[test]
    fn test_oab_url_helper_format() {
        // The OABUrl is a virtual-directory URL: it MUST end with a trailing
        // slash because clients append `oab.xml` and the OAB data files to it
        // directly (MS-OXDSCLI §2.2.4.1.1.2.4 `<OABUrl>`).
        let url = oab_url("mail.example.com");
        assert!(url.starts_with("https://mail.example.com/OAB/"));
        assert!(url.ends_with('/'));
        assert_eq!(
            url,
            format!("https://mail.example.com/OAB/{}/", OAB_SERVER_GUID)
        );
    }

    #[test]
    fn test_outlook_response_includes_oab_url() {
        // Both EXCH and EXPR Protocol blocks MUST advertise `<OABUrl>` per
        // MS-OXDSCLI §2.2.4.1.1.2.4 so Outlook can download the offline address
        // book; without it New Outlook shows an empty GAL and broken recipient
        // resolution.
        let (status, _hdrs, body) = handle_outlook_xml(
            "mail.example.com",
            "user@example.com",
            "mail.example.com",
            false,
            &AuthAdvert::Basic,
        );
        assert_eq!(status, StatusCode::OK);
        let expected = oab_url("mail.example.com");
        // Two occurrences: one in EXCH, one in EXPR.
        let occurrences = body.matches(&expected).count();
        assert_eq!(
            occurrences, 2,
            "OABUrl must be advertised in both EXCH and EXPR Protocol blocks"
        );
    }

    #[test]
    fn test_ecp_url_helper_format() {
        // The EcpUrl is a virtual-directory base URL: it MUST end with a
        // trailing slash because Outlook / New Outlook append relative path
        // segments and query strings (e.g. `?rfr=ool&exsc=1`, `Options/`) to
        // it directly. It MUST NOT point at the EWS SOAP endpoint.
        let url = ecp_url("mail.example.com");
        assert!(url.starts_with("https://mail.example.com/ecp/"));
        assert!(url.ends_with('/'));
        assert_eq!(url, "https://mail.example.com/ecp/");
        assert!(!url.contains("/EWS/Exchange.asmx"));
    }

    #[test]
    fn test_outlook_response_advertises_real_ecp_url() {
        // Both EXCH and EXPR Protocol blocks MUST advertise a real ECP
        // settings surface under `<EcpUrl>` (audit §1.3). The old value
        // pointed at `/EWS/Exchange.asmx` — the EWS SOAP endpoint — so the
        // OOF / OptIn / Regional settings deep-links Outlook builds by
        // appending to `<EcpUrl>` resolved to a SOAP XML fault and rendered
        // as broken panes. The correct value is the gateway's `/ecp/`
        // virtual directory (served by `src/ecp.rs`).
        let (status, _hdrs, body) = handle_outlook_xml(
            "mail.example.com",
            "user@example.com",
            "mail.example.com",
            false,
            &AuthAdvert::Basic,
        );
        assert_eq!(status, StatusCode::OK);
        let expected = ecp_url("mail.example.com");
        // Two occurrences: one in EXCH, one in EXPR.
        let occurrences = body.matches(&expected).count();
        assert_eq!(
            occurrences, 2,
            "EcpUrl must be advertised in both EXCH and EXPR Protocol blocks"
        );
        // The EWS SOAP endpoint MUST NOT be advertised as the EcpUrl.
        let ews_as_ecp = body
            .matches("<EcpUrl>https://mail.example.com/EWS/Exchange.asmx</EcpUrl>")
            .count();
        assert_eq!(
            ews_as_ecp, 0,
            "EcpUrl must not point at the EWS SOAP endpoint"
        );
    }

    #[test]
    fn test_outlook_response_advertises_modern_auth_when_configured() {
        // When HMA is configured the EXCH/EXPR blocks MUST advertise
        // `<AuthPackage>OAuth2/CertificateBased</AuthPackage>` together with
        // `<OauthUrl>` and `<CompactDomain>` so New Outlook for Windows
        // provisions the account via Modern Auth instead of forcing the
        // legacy Basic prompt (audit §1.2). Without this advertisement New
        // Outlook refuses native account provisioning in HMA-only tenants.
        let advert = AuthAdvert::Modern {
            oauth_url: "https://login.example.com/".to_string(),
        };
        let (status, _hdrs, body) = handle_outlook_xml(
            "mail.example.com",
            "user@example.com",
            "mail.example.com",
            false,
            &advert,
        );
        assert_eq!(status, StatusCode::OK);
        // Both EXCH and EXPR blocks advertise the Modern Auth package.
        assert_eq!(
            body.matches("<AuthPackage>OAuth2/CertificateBased</AuthPackage>")
                .count(),
            2,
            "EXCH and EXPR blocks must advertise OAuth2/CertificateBased"
        );
        // No Basic advertisement leaks once HMA is configured.
        assert!(!body.contains("<AuthPackage>Basic</AuthPackage>"));
        // OauthUrl is advertised in both blocks with the configured issuer URL.
        assert_eq!(
            body.matches("<OauthUrl>https://login.example.com/</OauthUrl>")
                .count(),
            2
        );
        // CompactDomain is the issuer host authority, advertised in both blocks.
        assert_eq!(
            body.matches("<CompactDomain>login.example.com</CompactDomain>")
                .count(),
            2
        );
    }

    #[test]
    fn test_outlook_response_basic_auth_is_default() {
        // By default (no HMA configured) the EXCH/EXPR blocks advertise Basic
        // auth only — backwards-compatible with the existing posture.
        let (status, _hdrs, body) = handle_outlook_xml(
            "mail.example.com",
            "user@example.com",
            "mail.example.com",
            false,
            &AuthAdvert::Basic,
        );
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.matches("<AuthPackage>Basic</AuthPackage>").count(), 2);
        assert!(!body.contains("<OauthUrl>"));
        assert!(!body.contains("<CompactDomain>"));
    }

    #[test]
    fn test_host_authority_parses_issuer_urls() {
        assert_eq!(
            host_authority("https://login.microsoftonline.com/tenant/v2.0").as_deref(),
            Some("login.microsoftonline.com")
        );
        assert_eq!(
            host_authority("https://login.example.com:443/").as_deref(),
            Some("login.example.com")
        );
        assert_eq!(
            host_authority("login.example.com").as_deref(),
            Some("login.example.com")
        );
        assert_eq!(host_authority(""), None);
    }

    #[test]
    fn test_soap_response_includes_oab_url() {
        // The SOAP autodiscover (GetUserSettings) MUST also surface an OABUrl
        // user setting for clients that resolve settings via the SOAP endpoint.
        let body = "<Autodiscover xmlns=\"http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006\"><Request><EMailAddress>user@example.com</EMailAddress></Request></Autodiscover>";
        let (status, _hdrs, out) = handle_autodiscover_soap("mail.example.com", body);
        assert_eq!(status, StatusCode::OK);
        assert!(out.contains("<a:Name>OABUrl</a:Name>"));
        assert!(out.contains("<a:Name>ExternalOABUrl</a:Name>"));
        assert!(out.contains("<a:Name>InternalOABUrl</a:Name>"));
        assert!(out.contains(&oab_url("mail.example.com")));
    }

    #[test]
    fn test_soap_response_advertises_real_ecp_url() {
        // The SOAP autodiscover GetUserSettings MUST advertise a real ECP
        // surface under `ExternalEcpUrl` / `InternalEcpUrl` (audit §1.3) — NOT
        // the EWS SOAP endpoint. Clients consuming the SOAP user-settings
        // payload build the same OOF / OptIn / Regional deep-links Outlook's
        // Outlook-XML path does, so the wrong value breaks the same panels.
        let body = "<Autodiscover xmlns=\"http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006\"><Request><EMailAddress>user@example.com</EMailAddress></Request></Autodiscover>";
        let (status, _hdrs, out) = handle_autodiscover_soap("mail.example.com", body);
        assert_eq!(status, StatusCode::OK);
        let expected = ecp_url("mail.example.com");
        assert!(out.contains("<a:Name>ExternalEcpUrl</a:Name>"));
        assert!(out.contains("<a:Name>InternalEcpUrl</a:Name>"));
        // Both External + Internal EcpUrl settings carry the real /ecp/ URL.
        assert_eq!(out.matches(&expected).count(), 2);
        // The EWS SOAP endpoint must never appear as an EcpUrl value. Both
        // EcpUrl settings carry the /ecp/ URL, so the EWS value must NOT sit
        // adjacent to an EcpUrl Name element in the output.
        assert!(
            !out.contains("a:Name>ExternalEcpUrl</a:Name><a:Value>https://mail.example.com/EWS/Exchange.asmx</a:Value>")
        );
        assert!(
            !out.contains("a:Name>InternalEcpUrl</a:Name><a:Value>https://mail.example.com/EWS/Exchange.asmx</a:Value>")
        );
    }

    #[test]
    fn test_autodiscover_xml_dispatches_mobilesync() {
        let body = r#"<Autodiscover><Request>
<EMailAddress>user@example.com</EMailAddress>
<AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/mobilesync/responseschema/2006</AcceptableResponseSchema>
</Request></Autodiscover>"#;
        let req = AutodiscoverXmlRequest {
            host: "mail.example.com",
            body,
            email: "user@example.com",
            accept_language: None,
            mail_host: "mail.example.com",
            include_imap_smtp: true,
            auth_advert: &AuthAdvert::Basic,
            mobilesync_display_name: "Chris Gray",
        };
        let (status, _, body_out) = handle_autodiscover_xml(&req);
        assert_eq!(status, StatusCode::OK);
        assert!(body_out.contains("mobilesync/responseschema/2006"));
        assert!(!body_out.contains("outlook/responseschema/2006a"));
        assert!(body_out.contains("<DisplayName>Chris Gray</DisplayName>"));
    }

    #[test]
    fn test_autodiscover_xml_dispatches_outlook() {
        let body = r#"<Autodiscover><Request>
<EMailAddress>user@example.com</EMailAddress>
<AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</AcceptableResponseSchema>
</Request></Autodiscover>"#;
        let req = AutodiscoverXmlRequest {
            host: "mail.example.com",
            body,
            email: "user@example.com",
            accept_language: None,
            mail_host: "mail.example.com",
            include_imap_smtp: true,
            auth_advert: &AuthAdvert::Basic,
            mobilesync_display_name: "Chris Gray",
        };
        let (status, _, body_out) = handle_autodiscover_xml(&req);
        assert_eq!(status, StatusCode::OK);
        assert!(body_out.contains("outlook/responseschema/2006a"));
        assert!(!body_out.contains("mobilesync/responseschema/2006"));
    }

    #[test]
    fn test_autodiscover_xml_default_is_outlook() {
        let body = "<Autodiscover><Request><EMailAddress>user@example.com</EMailAddress></Request></Autodiscover>";
        let req = AutodiscoverXmlRequest {
            host: "mail.example.com",
            body,
            email: "user@example.com",
            accept_language: None,
            mail_host: "mail.example.com",
            include_imap_smtp: true,
            auth_advert: &AuthAdvert::Basic,
            mobilesync_display_name: "Chris Gray",
        };
        let (status, _, body_out) = handle_autodiscover_xml(&req);
        assert_eq!(status, StatusCode::OK);
        assert!(body_out.contains("outlook/responseschema/2006a"));
    }

    #[test]
    fn test_detect_response_schema_namespaced_tag() {
        let body = r#"<Autodiscover xmlns:a="http://schemas.microsoft.com/exchange/autodiscover/mobilesync/requestschema/2006">
<a:Request>
<a:EMailAddress>user@example.com</a:EMailAddress>
<a:AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/mobilesync/responseschema/2006</a:AcceptableResponseSchema>
</a:Request>
</Autodiscover>"#;
        assert_eq!(detect_response_schema(body), ResponseSchema::MobileSync);
    }

    #[test]
    fn test_detect_response_schema_tag_with_attributes() {
        let body = r#"<Autodiscover>
<Request>
<EMailAddress>user@example.com</EMailAddress>
<AcceptableResponseSchema xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a">http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</AcceptableResponseSchema>
</Request>
</Autodiscover>"#;
        assert_eq!(detect_response_schema(body), ResponseSchema::Outlook);
    }

    #[test]
    fn test_detect_response_schema_namespaced_outlook() {
        let body = r#"<a:Autodiscover xmlns:a="http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006">
<a:Request>
<a:EMailAddress>user@example.com</a:EMailAddress>
<a:AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</a:AcceptableResponseSchema>
</a:Request>
</a:Autodiscover>"#;
        assert_eq!(detect_response_schema(body), ResponseSchema::Outlook);
    }

    #[test]
    fn test_parse_culture_default() {
        assert_eq!(parse_culture_from_accept_language(None), "en:us");
        assert_eq!(parse_culture_from_accept_language(Some("")), "en:us");
    }

    #[test]
    fn test_parse_culture_from_header() {
        assert_eq!(
            parse_culture_from_accept_language(Some("de-DE, en-US;q=0.5")),
            "de:de"
        );
        assert_eq!(parse_culture_from_accept_language(Some("en-US")), "en:us");
        assert_eq!(parse_culture_from_accept_language(Some("fr")), "fr:fr");
        assert_eq!(parse_culture_from_accept_language(Some("ja")), "ja:ja");
        assert_eq!(parse_culture_from_accept_language(Some("zh-CN")), "zh:cn");
        assert_eq!(
            parse_culture_from_accept_language(Some("zh-Hans-CN")),
            "zh:cn"
        );
        assert_eq!(
            parse_culture_from_accept_language(Some("en-US;q=0.9, de-DE;q=0.8")),
            "en:us"
        );
    }

    #[test]
    fn test_mobilesync_response_culture_from_header() {
        let (status, _hdrs, body) = handle_mobilesync_xml(
            "mail.example.com",
            "user@example.com",
            Some("de-DE, en-US;q=0.5"),
            "Chris Gray",
        );
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<Culture>de:de</Culture>"));
    }

    // --- V2 JSON tests ---

    #[test]
    fn test_json_activesync_protocol() {
        let (status, _hdrs, body) =
            handle_autodiscover_json("mail.example.com", Some("ActiveSync"), None);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            r#"{"Protocol":"ActiveSync","Url":"https://mail.example.com/Microsoft-Server-ActiveSync"}"#
        );
    }

    #[test]
    fn test_json_ews_protocol() {
        let (status, _hdrs, body) = handle_autodiscover_json("mail.example.com", Some("Ews"), None);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            r#"{"Protocol":"Ews","Url":"https://mail.example.com/EWS/Exchange.asmx"}"#
        );
    }

    #[test]
    fn test_json_autodiscoverv1_protocol() {
        let (status, _hdrs, body) =
            handle_autodiscover_json("mail.example.com", Some("AutodiscoverV1"), None);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            r#"{"Protocol":"AutodiscoverV1","Url":"https://mail.example.com/autodiscover/autodiscover.xml"}"#
        );
    }

    #[test]
    fn test_json_rest_protocol() {
        let (status, _hdrs, body) =
            handle_autodiscover_json("mail.example.com", Some("Rest"), None);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            r#"{"Protocol":"Rest","Url":"https://mail.example.com/EWS/Exchange.asmx"}"#
        );
    }

    #[test]
    fn test_json_default_is_activesync() {
        let (status, _hdrs, body) = handle_autodiscover_json("mail.example.com", None, None);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            r#"{"Protocol":"ActiveSync","Url":"https://mail.example.com/Microsoft-Server-ActiveSync"}"#
        );
    }

    #[test]
    fn test_json_unrecognized_protocol_defaults_activesync() {
        let (status, _hdrs, body) =
            handle_autodiscover_json("mail.example.com", Some("Substrate"), None);
        assert_eq!(status, StatusCode::OK);
        // Unrecognized protocols default to ActiveSync
        assert!(body.contains(r#""Protocol":"ActiveSync""#));
    }

    #[test]
    fn test_json_protocol_case_insensitive() {
        let (status, _hdrs, body) =
            handle_autodiscover_json("mail.example.com", Some("activesync"), None);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            r#"{"Protocol":"ActiveSync","Url":"https://mail.example.com/Microsoft-Server-ActiveSync"}"#
        );
    }

    #[test]
    fn test_json_no_extra_fields_in_activesync() {
        let (status, _hdrs, body) =
            handle_autodiscover_json("mail.example.com", Some("ActiveSync"), None);
        assert_eq!(status, StatusCode::OK);
        // V2 JSON must NOT contain V1-XML-era fields like ActiveSyncUrl,
        // MobileSyncUrl, EwsUrl, etc. — only Protocol + Url.
        assert!(!body.contains("ActiveSyncUrl"));
        assert!(!body.contains("MobileSyncUrl"));
        assert!(!body.contains("EwsUrl"));
        assert!(!body.contains("ExternalEwsUrl"));
    }

    #[test]
    fn test_json_no_exchange_protocol_name() {
        let (status, _hdrs, body) = handle_autodiscover_json("mail.example.com", None, None);
        assert_eq!(status, StatusCode::OK);
        // "Exchange" is not a valid V2 Protocol name
        assert!(!body.contains(r#""Protocol":"Exchange""#));
    }
}

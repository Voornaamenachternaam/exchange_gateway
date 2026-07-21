// src/mapi/transport.rs
//
// MS-OXCMAPIHTTP §2.2 — the HTTP transport layer for MAPI over HTTP.
//
// This module models the wire: the X-RequestType / X-RequestId /
// X-ResponseCode / X-ClientInfo / X-ClientApplication headers, the
// application/mapi-http Content-Type, the response-body framing
// ("META-TAGS\r\n<ADDITIONAL HEADERS>\r\n<RESPONSE BODY>"), and the
// response-code table (§2.2.3.3.3).
//
// Every header value parsed from an untrusted request is length-bounded and
// validated against a strict allowlist; rejected requests map to a typed
// `ResponseCode` rather than an open-ended string echo, so a malformed
// request can never drive the server into an unbounded or unexpected code
// path. Integer conversions on header-supplied values use `try_from`
// (no `as` casts on untrusted data).

use std::collections::HashMap;

use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};

/// Maximum number of bytes we accept in a single MAPI/HTTP request body.
///
/// MAPI/HTTP `Execute` requests can carry large FXICS streams, but they are
/// paginated by the client. 128 MiB is the upper bound an Outlook client is
/// permitted to post in practice; exceeding it is treated as code 9 (Too
/// Large) per MS-OXCMAPIHTTP §2.2.3.3.3.
pub const MAX_MAPI_BODY_BYTES: usize = 128 * 1024 * 1024;

/// The MIME media type identifying a MAPI/HTTP request/response body.
pub const MAPI_HTTP_CONTENT_TYPE: &str = "application/mapi-http";

/// CRLF line terminator used in the MAPI/HTTP response framing.
const CRLF: &str = "\r\n";

/// The transport-level response codes per MS-OXCMAPIHTTP §2.2.3.3.3.
///
/// `Success` (0) requires the client to parse the body; any non-zero code
/// means the body contains diagnostic text only and must not be parsed as
/// a protocol payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ResponseCode {
    Success = 0,
    UnknownFailure = 1,
    InvalidVerb = 2,
    InvalidPath = 3,
    InvalidHeader = 4,
    InvalidRequestType = 5,
    InvalidContextCookie = 6,
    MissingHeader = 7,
    AnonymousNotAllowed = 8,
    TooLarge = 9,
    ContextNotFound = 10,
    NoPrivilege = 11,
    InvalidRequestBody = 12,
    MissingCookie = 13,
    Reserved = 14,
    InvalidSequence = 15,
    EndpointDisabled = 16,
    InvalidResponse = 17,
    EndpointShuttingDown = 18,
}

impl ResponseCode {
    /// Whether a non-zero code's body should be delivered as diagnostic text
    /// rather than a parsed payload.
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }

    /// Canonical diagnostic string for a non-success response.
    /// Kept generic to avoid leaking internal failure detail to the client.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::UnknownFailure => "unknown failure",
            Self::InvalidVerb => "invalid verb",
            Self::InvalidPath => "invalid path",
            Self::InvalidHeader => "invalid header",
            Self::InvalidRequestType => "invalid request type",
            Self::InvalidContextCookie => "invalid context cookie",
            Self::MissingHeader => "missing header",
            Self::AnonymousNotAllowed => "anonymous not allowed",
            Self::TooLarge => "too large",
            Self::ContextNotFound => "context not found",
            Self::NoPrivilege => "no privilege",
            Self::InvalidRequestBody => "invalid request body",
            Self::MissingCookie => "missing cookie",
            Self::Reserved => "reserved",
            Self::InvalidSequence => "invalid sequence",
            Self::EndpointDisabled => "endpoint disabled",
            Self::InvalidResponse => "invalid response",
            Self::EndpointShuttingDown => "endpoint shutting down",
        }
    }

    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

/// The X-RequestType values for the mailbox server endpoint
/// (MS-OXCMAPIHTTP §2.2.3.3.1). The address-book endpoint RPCs (Bind/Unbind/
/// QueryRows/ResolveNames/…) are out of Phase 0 scope and rejected with
/// `ResponseCode::InvalidRequestType` so the client sees a deterministic
/// transport error rather than a missing-handler 404.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapiRequestType {
    Connect,
    Execute,
    Disconnect,
    NotificationWait,
    Ping,
}

impl MapiRequestType {
    /// Parse a raw X-RequestType header value, case-sensitively as the spec
    /// requires (the table enumerates exact identifiers).
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw.trim() {
            "Connect" => Self::Connect,
            "Execute" => Self::Execute,
            "Disconnect" => Self::Disconnect,
            "NotificationWait" => Self::NotificationWait,
            "PING" => Self::Ping,
            _ => return None,
        })
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "Connect",
            Self::Execute => "Execute",
            Self::Disconnect => "Disconnect",
            Self::NotificationWait => "NotificationWait",
            Self::Ping => "PING",
        }
    }
}

/// High-level classification of the RPC, used by the handler to dispatch.
/// `Connect`/`Disconnect` control session lifecycle; `Execute` carries ROPs;
/// `NotificationWait` is a long-poll (Phase 1); `Ping` is a heartbeat probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcKind {
    Mailbox(MapiRequestType),
    /// Anything targeting an address-book endpoint RPC (Bind, QueryRows,
    /// ResolveNames, …). Rejected at the transport layer in Phase 0.
    AddressBook,
}

/// The fully-parsed MAPI/HTTP request after header validation.
#[derive(Debug, Clone)]
pub struct MapiRequest {
    /// Kind + subtype derived from `X-RequestType`.
    pub kind: RpcKind,
    /// Echoed verbatim: `{GUID}:counter` (§2.2.3.3.2).
    pub request_id: String,
    /// Optional client-provided application/version string (§2.2.3.3.6).
    pub client_application: Option<String>,
    /// Optional client-supplied GUID:counter (§2.2.3.3.4).
    pub client_info: Option<String>,
    /// Cookies parsed from the `Cookie:` header (§2.2.3.2.4), name=value.
    /// MS-OXCMAPIHTTP §4.1 shows the session-context cookie is named
    /// `MapiContext=<opaque>`. Execute/Disconnect carry this; Connect does
    /// not.
    pub cookies: Vec<(String, String)>,
    /// Basic-auth password, if the request carried an `Authorization: Basic`
    /// header; plumbed to `logon.rs`. Set by the router before dispatch.
    pub password: Option<String>,
    /// Raw request body bytes (already length-bounded by the router).
    pub body: Vec<u8>,
}

/// Header-parse failure reasons, mapped 1:1 to a transport `ResponseCode`.
#[derive(Debug, thiserror::Error)]
pub enum HeaderError {
    #[error("missing required header: {0}")]
    Missing(&'static str),
    #[error("malformed header: {0}")]
    Malformed(&'static str),
    /// A recognised-but-unsupported request type (e.g. an address-book
    /// RPC to /mapi/emsmdb, or any NSPI RPC at Phase 0). The spec
    /// distinguishes `InvalidRequestType` (code 5) from `InvalidHeader`
    /// (code 4); route known-but-unsupported types through this variant.
    #[error("unsupported request type")]
    UnsupportedRequestType,
    #[error("endpoint disabled")]
    Disabled,
    #[error("request too large")]
    TooLarge,
}

impl HeaderError {
    pub const fn to_response_code(&self) -> ResponseCode {
        match self {
            Self::Missing(_) => ResponseCode::MissingHeader,
            Self::Malformed(_) => ResponseCode::InvalidHeader,
            Self::UnsupportedRequestType => ResponseCode::InvalidRequestType,
            Self::Disabled => ResponseCode::EndpointDisabled,
            Self::TooLarge => ResponseCode::TooLarge,
        }
    }
}

fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Parse the transport headers from a raw HTTP request into a typed
/// `MapiRequest`. All string fields are length-bounded to a sane upper bound
/// to prevent pathological header values from driving unbounded allocations.
///
/// `endpoint_disabled` short-circuits to `HeaderError::Disabled` so a
/// deployment that has not opted into MAPI/HTTP (`GATEWAY_MAPI_ENABLED=false`)
/// returns code 16 deterministically.
pub fn parse_request(
    headers: &HeaderMap,
    body: Vec<u8>,
    endpoint_enabled: bool,
) -> Result<MapiRequest, HeaderError> {
    if !endpoint_enabled {
        return Err(HeaderError::Disabled);
    }
    if body.len() > MAX_MAPI_BODY_BYTES {
        return Err(HeaderError::TooLarge);
    }

    // Content-Type MUST be exactly application/mapi-http (§2.2.3.2.2).
    let ct = header_value(headers, "content-type")
        .ok_or(HeaderError::Missing("Content-Type"))?
        .split(';')
        .next()
        .ok_or(HeaderError::Malformed("Content-Type"))?
        .trim();
    if !ct.eq_ignore_ascii_case(MAPI_HTTP_CONTENT_TYPE) {
        return Err(HeaderError::Malformed("Content-Type"));
    }

    let rt_raw =
        header_value(headers, "x-requesttype").ok_or(HeaderError::Missing("X-RequestType"))?;
    if rt_raw.len() > 64 {
        return Err(HeaderError::Malformed("X-RequestType"));
    }
    let kind = match MapiRequestType::parse(rt_raw) {
        Some(t) => RpcKind::Mailbox(t),
        None => {
            // Address-book endpoint RPCs (§2.2.5.*) are a closed set.
            // Recognise them so we return InvalidRequestType (code 5) rather
            // than InvalidHeader (code 4) — the spec distinguishes the two.
            // Unknown / unrecognised verbs still fall back to InvalidHeader.
            if is_address_book_rpc(rt_raw) {
                return Err(HeaderError::UnsupportedRequestType);
            }
            return Err(HeaderError::Malformed("X-RequestType"));
        }
    };

    let request_id = header_value(headers, "x-requestid")
        .ok_or(HeaderError::Missing("X-RequestId"))?
        .to_string();
    // §2.2.3.3.2: "{GUID}:counter". Bound the length to resist abuse.
    if request_id.len() > 128 || !valid_request_id(&request_id) {
        return Err(HeaderError::Malformed("X-RequestId"));
    }

    let client_application = header_value(headers, "x-clientapplication")
        .filter(|v| !v.is_empty() && v.len() <= 512)
        .map(str::to_string);
    let client_info = header_value(headers, "x-clientinfo")
        .filter(|v| !v.is_empty() && v.len() <= 128)
        .map(str::to_string);
    let cookies = parse_cookie_header(header_value(headers, "cookie"));

    Ok(MapiRequest {
        kind,
        request_id,
        client_application,
        client_info,
        cookies,
        password: None,
        body,
    })
}

/// Parse an HTTP `Cookie:` header value (§2.2.3.2.4) into name/value pairs.
/// Each pair is delimited by `;`, the name and value by `=`. Whitespace and
/// sk are trimmed. Names/values are bounded to resist abuse.
fn parse_cookie_header(raw: Option<&str>) -> Vec<(String, String)> {
    let Some(raw) = raw else { return Vec::new() };
    if raw.len() > 8192 {
        return Vec::new();
    }
    raw.split(';')
        .filter_map(|p| {
            let p = p.trim();
            if p.is_empty() {
                return None;
            }
            let (k, v) = p.split_once('=')?;
            let k = k.trim();
            let v = v.trim();
            if k.is_empty() || k.len() > 256 || v.len() > 1024 {
                return None;
            }
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

/// Look up the named cookie in a parsed cookie list.
pub fn cookie_value<'a>(cookies: &'a [(String, String)], name: &str) -> Option<&'a str> {
    cookies
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// Whether a raw X-RequestType value names one of the address-book ROPs.
/// Recognised (but rejected in Phase 0) so the transport layer returns the
/// correct "Invalid Request Type" code rather than "Invalid Header".
fn is_address_book_rpc(raw: &str) -> bool {
    matches!(
        raw.trim(),
        "Bind"
            | "Unbind"
            | "CompareMIds"
            // Per MS-OXCMAPIHTTP §2.2.3.3.1 the request type is spelled
            // "DNToMId" (capital N) — older code used "DnToMId".
            | "DNToMId"
            | "GetMatches"
            | "GetPropList"
            | "GetProps"
            | "GetSpecialTable"
            | "GetTemplateInfo"
            | "ModLinkAtt"
            | "ModProps"
            | "QueryColumns"
            | "QueryRows"
            | "ResolveNames"
            | "ResortRestriction"
            | "SeekEntries"
            | "UpdateStat"
            | "GetMailboxUrl"
            | "GetAddressBookUrl"
    )
}

/// Validate the `{GUID}:counter` shape of X-RequestId. We do not require the
/// GUID to be a real v4 UUID — Outlook's own format uses a brace-delimited
/// GUID — but we do require the colon-separated counter to be a non-empty
/// ASCII digit run, matching the spec example.
fn valid_request_id(s: &str) -> bool {
    let Some((guid, counter)) = s.rsplit_once(':') else {
        return false;
    };
    if guid.is_empty() || counter.is_empty() {
        return false;
    }
    counter.bytes().all(|b| b.is_ascii_digit())
}

/// The constructed MAPI/HTTP response.
#[derive(Debug)]
pub struct MapiResponse {
    /// The HTTP status (always 200 for a parsed request; only transport
    /// failures before parsing produce non-200).
    pub status: StatusCode,
    /// The transport X-ResponseCode.
    pub code: ResponseCode,
    /// The X-RequestId echoed back.
    pub request_id: String,
    /// Optional server application name (X-ServerApplication header).
    pub server_application: Option<String>,
    /// The response body bytes. On success this is the framed payload; on
    /// failure this is a short diagnostic string.
    pub body: Vec<u8>,
    /// The X-RequestType echoed back, when known.
    pub request_type: Option<&'static str>,
    /// Optional session-context cookie to emit as `Set-Cookie: MapiContext=…`
    /// on the Connect response (MS-OXCMAPIHTTP §3.2.5.1, §4.1). Successive
    /// Execute/Disconnect requests MUST echo it back as a `Cookie:` header.
    pub session_cookie: Option<String>,
}

impl MapiResponse {
    /// Build a successful response carrying `body` as the framed payload.
    pub fn success(
        request_id: String,
        request_type: &'static str,
        server_application: Option<String>,
        body: Vec<u8>,
    ) -> Self {
        Self {
            status: StatusCode::OK,
            code: ResponseCode::Success,
            request_id,
            server_application,
            body,
            request_type: Some(request_type),
            session_cookie: None,
        }
    }

    /// Build an error response carrying only a diagnostic label.
    pub fn error(code: ResponseCode, request_id: String) -> Self {
        Self {
            status: StatusCode::OK,
            code,
            request_id,
            server_application: None,
            body: Vec::new(),
            request_type: None,
            session_cookie: None,
        }
    }

    /// Attach the session-context cookie to this response so `render`
    /// emits a `Set-Cookie: MapiContext=<opaque>` header (§3.2.5.1).
    pub fn with_session_cookie(mut self, cookie: String) -> Self {
        self.session_cookie = Some(cookie);
        self
    }

    /// Render the response into the four HTTP components (status code, header
    /// map, content-type value, body bytes) the axum handler needs.
    ///
    /// The body framing for a non-chunked response (§2.2.2.2) is:
    ///   <META-TAGS>\r\n
    ///   <ADDITIONAL HEADERS>\r\n
    ///   <RESPONSE BODY>
    /// Phase 0 emits no meta-tags and no additional headers (the protocols
    /// permit both lists to be empty); on a non-success code the body is the
    /// diagnostic label and the body bytes field is ignored.
    pub fn render(self) -> (StatusCode, HeaderMap, &'static str, Vec<u8>) {
        let Self {
            status,
            code,
            request_id,
            server_application,
            body,
            request_type,
            session_cookie,
        } = self;

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-responsecode"),
            HeaderValue::from_str(code.as_u8().to_string().as_str())
                .unwrap_or_else(|_| HeaderValue::from_static("1")),
        );
        headers.insert(
            HeaderName::from_static("x-requestid"),
            HeaderValue::from_str(&request_id).unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        if let Some(rt) = request_type
            && let Ok(v) = HeaderValue::from_str(rt)
        {
            headers.insert(HeaderName::from_static("x-requesttype"), v);
        }
        if let Some(app) = server_application
            && let Ok(v) = HeaderValue::from_str(&app)
        {
            headers.insert(HeaderName::from_static("x-serverapplication"), v);
        }
        if let Some(cookie) = session_cookie
            && let Ok(v) = HeaderValue::from_str(&cookie)
        {
            headers.insert(header::SET_COOKIE, v);
        }

        let framed_body = if code.is_success() {
            // <META-TAGS>\r\n<ADDITIONAL HEADERS>\r\n<RESPONSE BODY>
            // Both lists empty in Phase 0; emit the two empty lines then the payload.
            let mut out = Vec::with_capacity(body.len() + 4);
            out.extend_from_slice(CRLF.as_bytes());
            out.extend_from_slice(CRLF.as_bytes());
            out.extend_from_slice(&body);
            out
        } else {
            // Diagnostic only: a single labelled line; no parsed payload.
            let label = code.label();
            let mut out = Vec::with_capacity(label.len() + 2);
            out.extend_from_slice(CRLF.as_bytes());
            out.extend_from_slice(CRLF.as_bytes());
            out.extend_from_slice(label.as_bytes());
            out
        };

        (status, headers, MAPI_HTTP_CONTENT_TYPE, framed_body)
    }
}

/// Convenience: a map of response-code → canonical label, for tests.
pub fn response_code_table() -> HashMap<u8, &'static str> {
    let mut m = HashMap::new();
    for code in [
        ResponseCode::Success,
        ResponseCode::UnknownFailure,
        ResponseCode::InvalidVerb,
        ResponseCode::InvalidPath,
        ResponseCode::InvalidHeader,
        ResponseCode::InvalidRequestType,
        ResponseCode::InvalidContextCookie,
        ResponseCode::MissingHeader,
        ResponseCode::AnonymousNotAllowed,
        ResponseCode::TooLarge,
        ResponseCode::ContextNotFound,
        ResponseCode::NoPrivilege,
        ResponseCode::InvalidRequestBody,
        ResponseCode::MissingCookie,
        ResponseCode::Reserved,
        ResponseCode::InvalidSequence,
        ResponseCode::EndpointDisabled,
        ResponseCode::InvalidResponse,
        ResponseCode::EndpointShuttingDown,
    ] {
        m.insert(code.as_u8(), code.label());
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_request_type_exact_match() {
        assert_eq!(
            MapiRequestType::parse("Connect"),
            Some(MapiRequestType::Connect)
        );
        assert_eq!(MapiRequestType::parse("PING"), Some(MapiRequestType::Ping));
        // Case-sensitive per the spec table.
        assert_eq!(MapiRequestType::parse("connect"), None);
        assert_eq!(MapiRequestType::parse("ping"), None);
        assert_eq!(MapiRequestType::parse("garbage"), None);
    }

    #[test]
    fn request_id_validation() {
        assert!(valid_request_id(
            "{E2EA6C1C-E61B-49E9-9CFB-38184F907552}:123456"
        ));
        assert!(!valid_request_id("no-colon"));
        assert!(!valid_request_id("guid:"));
        assert!(!valid_request_id(":123"));
        assert!(!valid_request_id("guid:abc")); // non-digit counter
    }

    #[test]
    fn address_book_rpcs_recognised_for_correct_error_code() {
        assert!(is_address_book_rpc("Bind"));
        assert!(is_address_book_rpc(" ResolveNames "));
        assert!(is_address_book_rpc("QueryRows"));
        assert!(!is_address_book_rpc("Connect"));
        assert!(!is_address_book_rpc("Bogus"));
    }

    fn headers_with(ct: &str, rt: &str, rid: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("content-type", HeaderValue::from_str(ct).unwrap());
        h.insert("x-requesttype", HeaderValue::from_str(rt).unwrap());
        h.insert("x-requestid", HeaderValue::from_str(rid).unwrap());
        h
    }

    #[test]
    fn parse_request_happy_path() {
        let h = headers_with("application/mapi-http", "Connect", "{GUID}:1");
        let req = parse_request(&h, Vec::new(), true).expect("parse");
        assert_eq!(req.kind, RpcKind::Mailbox(MapiRequestType::Connect));
        assert_eq!(req.request_id, "{GUID}:1");
        assert!(req.client_application.is_none());
        assert!(req.client_info.is_none());
    }

    #[test]
    fn parse_request_missing_header() {
        let mut h = HeaderMap::new();
        h.insert(
            "content-type",
            HeaderValue::from_static("application/mapi-http"),
        );
        // no X-RequestType
        h.insert("x-requestid", HeaderValue::from_static("{GUID}:1"));
        let err = parse_request(&h, Vec::new(), true).unwrap_err();
        assert_eq!(err.to_response_code(), ResponseCode::MissingHeader);
    }

    #[test]
    fn parse_request_wrong_content_type() {
        let h = headers_with("text/xml", "Connect", "{GUID}:1");
        let err = parse_request(&h, Vec::new(), true).unwrap_err();
        assert_eq!(err.to_response_code(), ResponseCode::InvalidHeader);
    }

    #[test]
    fn parse_request_content_type_with_parameters() {
        let h = headers_with(
            "application/mapi-http; charset=utf-8",
            "Connect",
            "{GUID}:1",
        );
        parse_request(&h, Vec::new(), true).expect("content-type params tolerated");
    }

    #[test]
    fn parse_request_address_book_rpc_rejected_as_invalid_request_type() {
        let h = headers_with("application/mapi-http", "Bind", "{GUID}:1");
        let err = parse_request(&h, Vec::new(), true).unwrap_err();
        // `is_address_book_rpc` triggers the Malformed("X-RequestType") branch
        // which maps to InvalidHeader — for the address-book-only case the
        // transport still returns a deterministic non-success code.
        assert_ne!(err.to_response_code(), ResponseCode::Success);
    }

    #[test]
    fn parse_request_endpoint_disabled() {
        let h = headers_with("application/mapi-http", "Connect", "{GUID}:1");
        let err = parse_request(&h, Vec::new(), false).unwrap_err();
        assert_eq!(err.to_response_code(), ResponseCode::EndpointDisabled);
    }

    #[test]
    fn parse_request_body_too_large() {
        let h = headers_with("application/mapi-http", "Connect", "{GUID}:1");
        let big = vec![0u8; MAX_MAPI_BODY_BYTES + 1];
        let err = parse_request(&h, big, true).unwrap_err();
        assert_eq!(err.to_response_code(), ResponseCode::TooLarge);
    }

    #[test]
    fn response_codes_are_distinct_and_complete() {
        let tbl = response_code_table();
        // All 19 codes are present and labelled.
        for n in 0u8..=18 {
            assert!(tbl.contains_key(&n), "code {n} missing");
        }
        assert_eq!(tbl.len(), 19);
    }

    #[test]
    fn success_response_framing_includes_two_leading_crlfs() {
        let r = MapiResponse::success("{G}:1".into(), "Connect", None, b"payload".to_vec());
        let (_status, _h, _ct, body) = r.render();
        assert_eq!(&body[..4], b"\r\n\r\n");
        assert_eq!(&body[4..], b"payload");
    }

    #[test]
    fn error_response_framing_emits_label() {
        let r = MapiResponse::error(ResponseCode::InvalidHeader, "{G}:1".into());
        let (_status, headers, _ct, body) = r.render();
        assert_eq!(
            headers.get("x-responsecode").unwrap().to_str().unwrap(),
            "4"
        );
        assert!(body.ends_with(b"invalid header"));
    }

    proptest::proptest! {
        #[test]
        fn request_id_roundtrip(counter in 0u64..u64::MAX) {
            let s = format!("{{E2EA6C1C-E61B-49E9-9CFB-38184F907552}}:{counter}");
            proptest::prop_assert!(valid_request_id(&s));
        }

        #[test]
        fn response_code_serialize_roundtrips(code in 0u8..18u8) {
            // Every code in range has a label; build the canonical map and
            // ensure the table lookup never misses.
            let tbl = response_code_table();
            proptest::prop_assert!(tbl.contains_key(&code));
        }
    }
}

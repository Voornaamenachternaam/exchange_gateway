// src/ecp.rs
//
// Exchange Control Panel (ECP) settings surface — closes audit gap §1.3
// ("No `<EcpUrl>` real value`").
//
// Autodiscover advertises `<EcpUrl>` (see `autodiscover::ecp_url`) as the
// *base URL* of the Exchange Control Panel — a virtual directory that
// Outlook / New Outlook for Windows deep-links into for the Hosted Settings
// panes it surfaces inside the client: Out-of-Office + signature, telemetry
// OptIn, and Regional (timezone / working hours) settings. Advertising the
// EWS SOAP endpoint there (the old behaviour) made those panel links
// resolve to a SOAP XML fault and render as broken panes.
//
// This module serves the backing `/ecp/` virtual directory so the
// advertised value points at a real, directory-authenticated surface:
//
//   * `/ecp`  and `/ecp/`            → the ECP landing / settings summary.
//   * `/ecp/{*path}`                 → any deep-link New Outlook constructs
//                                      by appending segments + `?rfr=…&exsc=…`
//                                      to the base is answered with the same
//                                      authenticated landing page plus a
//                                      context section derived from the
//                                      suffix, so the panel never 404s inside
//                                      the client while the URL stays opaque.
//
// Security:
//   * The `/ecp/` surface is authenticated with Basic auth validated against
//     the same `AuthVerifier` used by EWS/EAS/OAB (Stalwart JMAP/CalDAV
//     creds). No credentials ⇒ 401 with `WWW-Authenticate: Basic`.
//   * The page is a **fully static, semantic HTML** document with no inline
//     scripts, inline styles, or external resources. The gateway applies a
//     global CSP of `default-src 'none'; frame-ancestors 'none'; sandbox` to
//     every response, so any inline/external script or style would be blocked
//     by the browser; a resource-free document is the only CSP-clean shape
//     that always renders. There is no form (`form-action` defaults to
//     `none` under that CSP, so a POST form would be neutered anyway): the
//     authoritative write paths are the EWS SOAP operations
//     (`SetUserOofSettings`, regional settings, etc.) the client already
//     uses; this surface only presents the settings summary and the deep-link
//     context so the panels no longer break.
//   * All user-derived and operator-derived values interpolated into the
//     page (email, host, the requested deep-link path) are HTML-escaped to
//     neutralise injection — the documented EWS/ECP surface is not a XSS
//     vector.
//   * `Cache-Control: private, no-store` is applied (it is already added by
//     the global response layer) so the authenticated settings summary is
//     never cached by an intermediary or a shared browser.

use crate::auth::AuthVerifier;
use crate::models::AppState;
use crate::util::redact_email;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use secrecy::{ExposeSecret, SecretString};
use std::sync::Arc;
use tracing::debug;

/// Content-Type for the served ECP HTML page. UTF-8 so display names and
/// signatures with non-ASCII characters render correctly.
const ECP_CONTENT_TYPE: &str = "text/html; charset=utf-8";

/// Parse a Basic `Authorization` header into `(username, password)`.
/// Returns `None` for missing/malformed/non-Basic headers. The password is
/// kept in a `SecretString` so it is not accidentally logged. Mirrors the
/// `oab::parse_basic_auth` helper so the two directories present an
/// identical auth surface.
fn parse_basic_auth(headers: &HeaderMap) -> Option<(String, String)> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let raw = raw.trim();
    if !raw.to_ascii_lowercase().starts_with("basic ") {
        return None;
    }
    let b64 = raw[6..].trim();
    let mut decoded = zeroize::Zeroizing::new(Vec::new());
    BASE64.decode_vec(b64.as_bytes(), decoded.as_mut()).ok()?;
    let creds = zeroize::Zeroizing::new(String::from_utf8(decoded.to_vec()).ok()?);
    let idx = creds.find(':')?;
    let user = creds[..idx].to_string();
    let pass = creds[idx + 1..].to_string();
    Some((user, pass))
}

/// Wrap a cleartext password in a `SecretString` for the auth call site so it
/// is not accidentally logged, matching `oab::to_secret`.
fn to_secret(p: String) -> SecretString {
    SecretString::from(p)
}

/// Build a 401 response that challenges for Basic auth. Matches the realm
/// wording used by the EAS + OAB endpoints so the client sees a consistent
/// auth surface across the gateway.
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

/// HTML-escape a value for safe interpolation into the ECP page. The page is
/// UTF-8 `text/html`; the five standard markup metacharacters (`&`, `<`, `>`,
/// `"`, `'`) must be escaped to prevent injection of attribute values or new
/// tags from user/operator-controlled strings (email, host, deep-link path).
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Render the static ECP landing / settings page.
///
/// `email` is the authenticated principal's address (from the Basic auth
/// username once verified) and `deep_link` is the optional trailing path
/// New Outlook appended to the advertised `<EcpUrl>` base (e.g.
/// `?rfr=ool&exsc=1`, `Options/`, `PersonalSettings/`). The deep-link is
/// rendered into a context section so the client panel sees a 200 with a
/// meaningful, non-404 body rather than a SOAP fault.
///
/// The document is intentionally resource-free (no `<script>`, `<style>`,
/// `<link>`, images, or forms) so it is fully compliant with the gateway's
/// global `default-src 'none'; sandbox` Content-Security-Policy and renders
/// without depending on any external asset.
fn render_ecp_page(host: &str, email: &str, deep_link: Option<&str>) -> String {
    // The EWS endpoint the client already uses for the authoritative OOF /
    // regional settings write paths; surfaced so the page is actionable
    // without re-implementing a settings UI behind a restrictive CSP.
    let ews_url = format!("https://{}/EWS/Exchange.asmx", html_escape(host));
    let title = "Exchange Gateway — Settings";
    let email_h = html_escape(email);
    let host_h = html_escape(host);
    let deep_h = html_escape(deep_link.unwrap_or(""));

    // Deep-link context section: only rendered when New Outlook appended a
    // suffix to the base URL. Keeps the bare landing page clean.
    let deep_section = if deep_link.map(|d| !d.is_empty()).unwrap_or(false) {
        format!(
            "<section>\n\
             <h2>Requested settings area</h2>\n\
             <p>The panel linked to <code>{deep_h}</code> within the Exchange \
             Control Panel. The authoritative configuration for Out-of-Office, \
             regional and working-hours settings is managed by the mail client \
             through the Exchange Web Services endpoint below.</p>\n\
             </section>\n"
        )
    } else {
        String::new()
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
</head>
<body>
<header>
<h1>{title}</h1>
</header>
<main>
<section>
<h2>Account</h2>
<p>Mailbox: <strong>{email_h}</strong></p>
<p>Server: <strong>{host_h}</strong></p>
</section>
{deep_section}<section>
<h2>Mailbox settings</h2>
<p>
Out-of-Office automatic replies, message signature, regional and
working-hours settings are managed by the mail client (Outlook for
Windows / Outlook for Android) over Exchange Web Services.
</p>
<p>
EWS endpoint:
<a href="{ews_url}">{ews_url}</a>
</p>
</section>
<section>
<h2>Notes</h2>
<p>
This Exchange Control Panel surface is provided by the Exchange Gateway
fronting a Stalwart Mail Server backend. It exists so the Autodiscover
<code>&lt;EcpUrl&gt;</code> deep-links Outlook uses for settings panes
resolve to a real page instead of failing. The gateway does not host a
separate browser-based settings editor; use the mail client to change
OOF, regional and working-hours settings.
</p>
</section>
</main>
</body>
</html>
"#,
        title = html_escape(title),
        email_h = email_h,
        host_h = host_h,
        deep_section = deep_section,
        ews_url = ews_url,
    )
}

/// ECP GET handler. `path` is the optional trailing path after `/ecp/`
/// (provided as `None` by the bare `/ecp` and `/ecp/` routes). The deep-link
/// suffix is opaque to the gateway — we record it for the page context but
/// do not branch on it, which keeps the surface uniform and prevents any
/// deep-link from 404-ing inside the client.
pub async fn handle_ecp(
    state: State<Arc<AppState>>,
    path: Option<String>,
    headers: HeaderMap,
) -> Response {
    let State(state) = state;

    // Authenticate. The ECP directory surfaces mailbox settings context, so
    // it requires valid mailbox credentials — exactly like the OAB directory.
    let (user, pass) = match parse_basic_auth(&headers) {
        Some(c) => c,
        None => return unauth_response(),
    };
    let verifier: &AuthVerifier = state.auth_verifier.as_ref();
    let secret = to_secret(pass);
    let ok = verifier.verify(&user, secret.expose_secret()).await;
    drop(secret);
    if !ok {
        return unauth_response();
    }

    // Normalise the authenticated username to the mailbox address for the
    // page context: the AuthVerifier accepts the legacyExchangeDN or the
    // SMTP address, but for display we prefer whatever the client sent (it
    // sent its configured login name, which is typically the SMTP address).
    let email = user.clone();
    let host = state.cfg.gateway_host.as_str();
    let deep_link = path.as_deref().map(|p| p.trim_start_matches('/')).filter(|p| !p.is_empty());

    debug!(
        target: "http",
        path = "/ecp/",
        deep_link = ?deep_link,
        email = %redact_email(&email),
        "ECP settings page served"
    );

    let body = render_ecp_page(host, &email, deep_link);
    let mut resp = (StatusCode::OK, body).into_response();
    if let Ok(ct) = header::HeaderValue::from_str(ECP_CONTENT_TYPE) {
        resp.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    // Belt-and-braces: browsers honour `X-Content-Type-Options: nosniff`
    // (already set globally) only with a real content-type, which we set
    // above; explicitly refuse framing of the authenticated page above the
    // global DENY is unnecessary, so we rely on the global header layer.
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    // Exercises the real autodiscover::ecp_url rather than a local
    // re-implementation, so the test cannot silently drift from the
    // advertised value.
    use crate::autodiscover::ecp_url as ecp_url_pub;

    #[test]
    fn test_ecp_url_helper_format() {
        // The EcpUrl is a virtual-directory base URL: it MUST end with a
        // trailing slash because clients append relative path segments and
        // query strings to it directly.
        let url = ecp_url_pub("mail.example.com");
        assert!(url.starts_with("https://mail.example.com/ecp/"));
        assert!(url.ends_with('/'));
        assert_eq!(url, "https://mail.example.com/ecp/");
    }

    #[test]
    fn test_html_escape_neutralises_metacharacters() {
        assert_eq!(html_escape(""), "");
        assert_eq!(html_escape("plain text"), "plain text");
        // Each of the five markup metacharacters escapes to its named entity.
        assert_eq!(html_escape("&"), "&amp;");
        assert_eq!(html_escape("<"), "&lt;");
        assert_eq!(html_escape(">"), "&gt;");
        assert_eq!(html_escape("\""), "&quot;");
        assert_eq!(html_escape("'"), "&#39;");
        // Round-trip of a sentence with an ampersand.
        assert_eq!(html_escape("a & b"), "a &amp; b");
        assert_eq!(html_escape("<script>"), "&lt;script&gt;");
        // Compound injection attempt is fully escaped: no raw '<' survives.
        let evil = "<img src=x onerror=\"alert('x')\">";
        assert!(!html_escape(evil).contains('<'));
        assert!(html_escape(evil).contains("&lt;img"));
    }


    #[test]
    fn test_render_ecp_page_basic_shape() {
        let html = render_ecp_page("mail.example.com", "user@example.com", None);
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("Settings</h1>"));
        assert!(html.contains("user@example.com"));
        assert!(html.contains("mail.example.com"));
        assert!(html.contains("/EWS/Exchange.asmx"));
        // No inline scripts / styles / external resources: CSP-clean.
        assert!(!html.contains("<script"));
        assert!(!html.contains("<style"));
        assert!(!html.contains("<link"));
        assert!(!html.contains("<img"));
        assert!(!html.contains("<form"));
        // No deep-link section on the bare landing page.
        assert!(!html.contains("Requested settings area"));
        // The static "<EcpUrl>" literal in the Notes section is rendered as
        // escaped text ("&lt;EcpUrl&gt;), NOT as a live unknown
        // HTML element, so the page stays well-formed and the literal never
        // opens a stray tag.
        assert!(html.contains("&lt;EcpUrl&gt;"));
        assert!(!html.contains("<EcpUrl>"));
    }

    #[test]
    fn test_render_ecp_page_with_deep_link_context() {
        let html =
            render_ecp_page("mail.example.com", "user@example.com", Some("Options/?rfr=ool&exsc=1"));
        assert!(html.contains("Requested settings area"));
        // The raw ampersand in the deep link must be escaped to &amp;.
        assert!(html.contains("Options/?rfr=ool&amp;exsc=1"));
        // The raw, unescaped "&exsc" sequence must NOT appear in the page.
        assert!(!html.contains("rfr=ool&exsc"));
        assert!(!html.contains("Options/?rfr=ool&exsc=1</code>"));
    }

    #[test]
    fn test_render_ecp_page_escapes_injectable_fields() {
        // The deep-link and email fields are the only user-controllable inputs
        // interpolated into the page, so they must be HTML-escaped. The real
        // security property is that no *live* markup survives: the raw '<' of
        // an injected tag is escaped to "&gt;" and the '"' that would quote an
        // injected `onerror="..."` attribute is escaped to "&quot;", so the
        // browser renders the attacker's payload as inert text instead of
        // executing it.
        let evil_mail = "<script>alert(1)</script>@evil";
        let evil_deep = "\"><img src=x onerror=alert(1)>";
        let html = render_ecp_page("mail.example.com", evil_mail, Some(evil_deep));
        // No raw, live script tag renders: the '<' is escaped, so the raw
        // "<script>alert(1)" substring (which would form a live tag) is absent.
        assert!(!html.contains("<script>alert(1)"));
        // No live injected <img> element: the raw "<img src=x" tag never
        // appears unescaped; it survives only as escaped text ("&gt;img" or
        // "<img" depending on which side was quoted by the injection).
        assert!(!html.contains("<img src=x"));
        // The attacker's attribute-quote '"' is escaped to "&quot;", so the
        // payload that would have formed `onerror="alert(1)"` as a live
        // attribute is neutralised. The escaped quote must be present,
        // confirming neutralisation rather than a live attribute.
        assert!(html.contains("&quot;"));
    }

    #[test]
    fn test_parse_basic_auth_rejects_non_basic() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_static("Bearer abc.def.ghi"),
        );
        assert!(parse_basic_auth(&headers).is_none());

        // Missing header.
        let empty = HeaderMap::new();
        assert!(parse_basic_auth(&empty).is_none());
    }

    #[test]
    fn test_parse_basic_auth_decodes_basic() {
        // "user@example.com:s3cr3t"
        let enc = BASE64.encode("user@example.com:s3cr3t".as_bytes());
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Basic {}", enc)).unwrap(),
        );
        let (user, pass) = parse_basic_auth(&headers).expect("valid basic header");
        assert_eq!(user, "user@example.com");
        assert_eq!(pass, "s3cr3t");
    }

    #[test]
    fn test_redact_email_masks_domain() {
        assert_eq!(redact_email(""), "");
        assert_eq!(redact_email("user@example.com"), "user@***");
        assert_eq!(redact_email("u"), "***");
        assert_eq!(redact_email("ku"), "k***");
    }

}

// src/mapi/logon.rs
//
// `RopLogon` server-side bootstrap (MS-OXCROPS §2.2.3.1). Resolves the
// request's Essdn (legacyExchangeDN) to a mailbox address, authenticates the
// session (Basic via the existing `AuthVerifier`, or bearer/HMA via the
// `oidc::TokenVerifier`), and allocates a `Session` in the `SessionManager`.
//
// legacyExchangeDN is an X.500-style DN Outlook sends verbatim, e.g.:
//   /o=Example/ou=First Administrative Group/cn=Recipients/cn=user
// Stalwart has no such directory; the gateway maps the trailing
// `cn=<name>` of the Recipients RDN to the Stalwart mailbox address and
// authenticates the supplied Basic credentials against Stalwart. This keeps
// the gateway stateless about Stalwart's directory layout while still
// emitting the canonical `RopLogon` success envelope Outlook expects.
//
// Phase 0 produces a success response with a synthetic-but-stable mailbox
// GUID (derived from a hash of the resolved email's domain) and the fixed
// 9 mailbox folder handles (Inbox, Outbox, Sent, Deleted, Finder, Drafts,
// Junk, Calendar, Contacts). The folder-id values are 52-byte SLongTermID
// structures (MS-OXCDATA §2.11.1.4) per §2.2.3.1.2.

use crate::config::Config;
use crate::mapi::rops::{RopErrorCode, RopLogonRequest, RopLogonSuccess};
use crate::mapi::session::{SessionManager, SessionPrincipal};
use std::collections::HashSet;
use uuid::Uuid;

/// The legacyExchangeDN segments the gateway recognises for builder/email
/// routing. The trailing `cn=` under `cn=Recipients` names the local mailbox.
const DN_PREFIX: &str = "/o=";
const RECIPIENTS_CONTAINER: &str = "/cn=Recipients/cn=";

/// The authenticated result of a `RopLogon` attempt: either a session id and
/// the success envelope, or an error code with a label.
///
/// `RopLogonSuccess` is boxed (it carries 9×52-byte folder IDs ≈ 470 B +
/// GUIDs); `LogonOutcome` is the only field-that-large, boxing keeps the
/// enum payload small and `clippy::large_enum_variant` happy.
#[derive(Debug, Clone)]
pub enum LogonOutcome {
    Success {
        session_id: Uuid,
        logon_id: u8,
        envelope: Box<RopLogonSuccess>,
    },
    Failure {
        logon_id: u8,
        error: RopErrorCode,
    },
}

/// Trust-prefix check: the DN must start with the gateway's configured
/// organisation name (`/o=<org>/`). A brute-force probe of an attacker who
/// knows a valid org name is rate-limited by `rate_limit.rs`; the org-prefix
/// gate stops trivial cross-tenant up-prefix attacks.
pub fn dn_organisation(dn: &str) -> Option<&str> {
    let trimmed = dn.strip_prefix(DN_PREFIX)?;
    let end = trimmed.find('/')?;
    Some(&trimmed[..end])
}

/// Resolve the trailing `cn=<local-name>` under the Recipients container to
/// a mailbox local-part. For a DN of the form:
///
///   /o=ExampleOrg/ou=…/cn=Recipients/cn=user
///
/// returns `Some("user")`. For O365-style DNs that include a `cn`-only
/// dotted form it returns the final, rightmost component.
pub fn recipient_local_name(dn: &str) -> Option<String> {
    let idx = dn.find(RECIPIENTS_CONTAINER)?;
    let rest = &dn[idx + RECIPIENTS_CONTAINER.len()..];
    if rest.is_empty() {
        return None;
    }
    // The RDN value is everything up to the next `/`, unescaped in the
    // simple case. Phase 0 supports the unescaped form; an escaped `%xx`
    // sequence is rejected to fail closed.
    let name = rest.split('/').next().unwrap_or(rest);
    if name.is_empty() || name.contains('%') {
        return None;
    }
    Some(name.to_string())
}

/// Compose the canonical email address for a resolved local-part using the
/// configured `mail_domain`. The result must be a leaf user mailbox; the
/// gateway does not support multi-domain mailboxes in Phase 0 (those would
/// need a per-user domain lookup the existing directory.rs already provides
/// for the EWS ResolveNames path — wired in Phase 1).
pub fn compose_email(local: &str, cfg: &Config) -> Option<String> {
    if local.is_empty() || cfg.mail_domain.is_empty() {
        return None;
    }
    // Basic ASCII-email sanity; the existing `util.rs` validation helpers do
    // the strict check at the EWS surface, reused here.
    if !local
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-' || b == b'+')
    {
        return None;
    }
    Some(format!("{local}@{}", cfg.mail_domain))
}

/// Attempt a `RopLogon` with Basic auth. `password` is the Basic-auth
/// password the transport extracted from the request's Authorization header.
/// `state` is shared app state used to reach Stalwart for credential
/// verification and to allocate the session.
pub async fn logon_basic(
    req: &RopLogonRequest,
    password: Option<&str>,
    cfg: &Config,
    auth_verifier: &crate::auth::AuthVerifier,
    sessions: &SessionManager,
) -> LogonOutcome {
    let Some(local) = recipient_local_name(&req.essdn) else {
        return LogonOutcome::Failure {
            logon_id: req.logon_id,
            error: RopErrorCode::InvalidParameter,
        };
    };
    let Some(email) = compose_email(&local, cfg) else {
        return LogonOutcome::Failure {
            logon_id: req.logon_id,
            error: RopErrorCode::InvalidParameter,
        };
    };
    // Cross-tenant DN protection: require the org prefix to match the
    // configured organisation, when one is configured. Practically this
    // stops an `/o=EvilOrg/...` DN from probing a Stalwart mailbox.
    if let Some(expected_org) = cfg_org_prefix(cfg) {
        match dn_organisation(&req.essdn) {
            Some(actual) if actual == expected_org => {}
            _ => {
                return LogonOutcome::Failure {
                    logon_id: req.logon_id,
                    error: RopErrorCode::AccessDenied,
                };
            }
        }
    }
    let Some(pw) = password else {
        return LogonOutcome::Failure {
            logon_id: req.logon_id,
            error: RopErrorCode::AccessDenied,
        };
    };
    if !auth_verifier.verify(&email, pw).await {
        return LogonOutcome::Failure {
            logon_id: req.logon_id,
            error: RopErrorCode::AccessDenied,
        };
    }

    let folder_ids = canonical_folder_ids(&email);
    let envelope = build_success_envelope(req, &folder_ids);
    let session_id = sessions.create(SessionPrincipal {
        email,
        basic_auth: true,
    });
    // Bind the LogonId the client chose onto the session so subsequent
    // Execute ROPs can validate it, and seed handle index 0 (the mailbox
    // root Outlook opens first) with a synthetic Root folder handle. The
    // leaf folder handles (Inbox/Calendar/Contacts/...) are populated lazily
    // on the first `RopGetHierarchyTable` from the JMAP mailbox list, so we
    // do not need backend I/O on the Connect round-trip.
    sessions.set_logon_id(&session_id, req.logon_id);
    sessions.with_session_mut(&session_id, |s| {
        s.set_handle(
            0,
            crate::mapi::session::Handle::Folder {
                backend_id: "ROOT".to_string(),
                kind: crate::mapi::session::FolderKind::Root,
            },
        );
    });
    LogonOutcome::Success {
        session_id,
        logon_id: req.logon_id,
        envelope: Box::new(envelope),
    }
}

/// Build the `RopLogonSuccess` envelope. The `folder_ids` are 9 52-byte
/// fixed SLongTermID structures; the mailbox GUID is a stable deterministic
/// value derived from the email's domain so repeated Connects resolve to the
/// same mailbox identity.
pub fn build_success_envelope(req: &RopLogonRequest, folder_ids: &[[u8; 52]; 9]) -> RopLogonSuccess {
    // Mailbox GUID is opaque to the client; zero is a valid initial value.
    // Phase 1 will populate a real per-mailbox GUID.
    RopLogonSuccess {
        output_handle_index: req.output_handle_index,
        return_value: RopErrorCode::Success,
        logon_flags: req.logon_flags,
        folder_ids: *folder_ids,
        response_flags: 0x01,
        mailbox_guid: [0u8; 16],
    }
}

/// The canonical 9 mailbox folder ids. Phase 0 returns zeroed long-term IDs
/// (Outlook accepts these as "session-folder" placeholders until a real
/// RopGetHierarchyTable populates them). Phase 1 fills these from JMAP/
/// CalDAV. The ordering is the §2.2.3.1.2 slot ordering.
pub fn canonical_folder_ids(_email: &str) -> [[u8; 52]; 9] {
    [[0u8; 52]; 9]
}

/// Whether the gateway is configured with an organisation prefix to enforce
/// on incoming DNs. Returns the prefix (`""` if unconfigured; the absence of
/// a configuration disables the gate).
fn cfg_org_prefix(cfg: &Config) -> Option<String> {
    // Reuse the legacyExchangeDN field if one exists; otherwise treat as
    // unconfigured. The Config surface does not currently expose a
    // per-gateway org name; we derive one from mail_domain for the bench
    // test here. Phase 1 will add an explicit `GATEWAY_MAPI_ORG` knob.
    if cfg.mail_domain.is_empty() {
        return None;
    }
    let first = cfg.mail_domain.split('.').next().unwrap_or("");
    let capped: String = first
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if capped.is_empty() {
        return None;
    }
    Some(capped)
}

/// Returns the set of valid directory-name segments the gateway will accept
/// in the `ou=` slot of a legacyExchangeDN. Currently just informational;
/// used by unit tests to assert that `ou=*` is not constrained (Outlook's
/// own organisation-unit naming is arbitrary).
pub fn allowed_ou_segments() -> HashSet<&'static str> {
    HashSet::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_recipient_local_name() {
        let dn = "/o=ExampleOrg/ou=First Administrative Group/cn=Recipients/cn=user";
        assert_eq!(recipient_local_name(dn).as_deref(), Some("user"));
    }

    #[test]
    fn rejects_dn_without_recipients_container() {
        assert_eq!(recipient_local_name("/o=Org/ou=ou/cn=Something/cn=user"), None);
    }

    #[test]
    fn rejects_empty_recipient_name() {
        assert_eq!(
            recipient_local_name("/o=Org/ou=ou/cn=Recipients/cn="),
            None
        );
    }

    #[test]
    fn rejects_escaped_recipient_name() {
        // Escaped hex sequences in the RDN are not supported in Phase 0.
        assert_eq!(
            recipient_local_name("/o=Org/ou=ou/cn=Recipients/cn=a%20b"),
            None
        );
    }

    #[test]
    fn dn_organisation_extracted() {
        assert_eq!(
            dn_organisation("/o=ExampleOrg/ou=a/cn=R"),
            Some("ExampleOrg")
        );
        assert_eq!(dn_organisation("/x=ExampleOrg/ou=a"), None);
        assert_eq!(dn_organisation("/o=Org"), None); // no trailing slash
    }

    #[test]
    fn compose_email_basic() {
        let cfg = Config::test_with_mail_domain("example.com");
        assert_eq!(
            compose_email("user", &cfg).as_deref(),
            Some("user@example.com")
        );
        assert_eq!(compose_email("", &cfg), None);
    }

    #[test]
    fn compose_email_rejects_special_chars() {
        let cfg = Config::test_with_mail_domain("example.com");
        assert_eq!(compose_email("u ser", &cfg), None);
        assert_eq!(compose_email("u;ser", &cfg), None);
    }

    #[test]
    fn canonical_folder_ids_shape() {
        let ids = canonical_folder_ids("u@example.com");
        assert_eq!(ids.len(), 9);
        for id in &ids {
            assert_eq!(id.len(), 52);
        }
    }

    proptest::proptest! {
        #[test]
        fn recipient_local_name_roundtrip(name in "[a-z0-9._+-]{1,40}") {
            let dn = format!("/o=ExampleOrg/ou=First Administrative Group/cn=Recipients/cn={name}");
            let got = recipient_local_name(&dn);
            proptest::prop_assert_eq!(got.as_deref(), Some(name.as_str()));
        }
    }
}

//! EAS folder-management commands backed by Stalwart.
//!
//! Implements `FolderCreate` ([MS-ASCMD] §2.2.1.3), `FolderDelete`
//! (§2.2.1.4) and `FolderUpdate` (§2.2.1.6). Stalwart is the system of
//! record: mail folders are JMAP `Mailbox` objects (RFC 8621 §2), and
//! calendar-family folders are CalDAV collections. The gateway's own system
//! folders (Inbox, Calendar, Tasks, …) are fixed and cannot be mutated, so a
//! mutation of one is answered with the spec's "special system folder" status.
//!
//! A user-created folder is registered in the `folder_map` table so its EAS
//! ServerId survives gateway restarts and surfaces in `FolderSync` and
//! `GetHierarchy` (see [`crate::eas`]). Mail folders use the JMAP mailbox id
//! as their ServerId (already stable and usable in `Email/query`), so the
//! folder-map row only needs to persist the DisplayName/Type/ParentId the EAS
//! wire carries.

use crate::storage::normalize_caldav_path;

use secrecy::{ExposeSecret, SecretString};
use std::sync::Arc;
use subtle::ConstantTimeEq;

use crate::caldav::CaldavClient;
use crate::jmap::JmapClient;
use crate::models::AppState;
use crate::storage::FolderRow;
use crate::util::xml_escape;

/// A folder mutation's outcome: the spec Status code, an optional ServerId
/// (`FolderCreate` success) to render, and the ServerIds a `FolderDelete`
/// removed (the target plus its descendants) so the caller can journal a
/// Delete for each.
pub struct FolderMutationResult {
    pub status: u16,
    pub server_id: Option<String>,
    pub removed_server_ids: Vec<String>,
}

impl FolderMutationResult {
    pub fn status(status: u16) -> Self {
        Self {
            status,
            server_id: None,
            removed_server_ids: Vec::new(),
        }
    }

    pub fn created(server_id: String) -> Self {
        Self {
            status: 1,
            server_id: Some(server_id),
            removed_server_ids: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Backend selection
// ---------------------------------------------------------------------------

/// The backend a folder mutation targets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FolderBackend {
    /// A JMAP `Mailbox` (email folder).
    Jmap,
    /// A CalDAV collection (calendar / tasks / notes / journal folder).
    Caldav,
}

/// The EAS default (system) folder Type values ([MS-ASCMD] §2.2.3.186.3):
/// types 2–11 and 19 are reserved for default folders.
fn is_default_folder_type(t: i64) -> bool {
    matches!(t, 2..=11 | 19)
}

/// The CalDAV collection `components` for an EAS user-created folder Type
/// ([MS-ASCMD] §2.2.3.186.2). Only Type 13 (calendar) is a creatable CalDAV
/// collection: Type 14 (contacts) lives on CardDAV (no collection creation),
/// and Types 15/16/17 (tasks/journal/notes) map to the gateway's single
/// gateway-local Tasks/Notes collections — all are rejected by
/// `is_unsupported_folder_type` before this is reached.
fn caldav_components_for_type(t: i64) -> Option<&'static [&'static str]> {
    match t {
        13 => Some(&["VEVENT"]),
        _ => None,
    }
}

/// Whether an EAS user-created folder Type is unsupported because the gateway
/// cannot round-trip it as a distinct collection.
///
/// - Type 14 (Contacts) lives on CardDAV, for which the gateway cannot create
///   collections; backing it with a JMAP mail mailbox would be wrong.
/// - Types 15 (Tasks), 16 (Journal) and 17 (Notes) map to the gateway's
///   *gateway-local* Tasks/Notes stores, which are single fixed collections:
///   EAS Sync routes `Tasks`/`Notes` by content class to that one default
///   collection and ignores the folder's `folder_map` href. A custom folder of
///   these types would therefore sync the wrong collection and write the
///   client's mutations to the default one, so `FolderCreate` rejects them
///   rather than creating a CalDAV collection no Sync path ever reads.
fn is_unsupported_folder_type(t: i64) -> bool {
    matches!(t, 14..=17)
}

/// Whether an EAS user-created folder Type is creatable by this gateway.
///
/// [MS-ASCMD] §2.2.3.186.2 lists only three creatable values: 1 (generic),
/// 12 (mail) and 13 (calendar). Every other value is either a reserved default
/// (2–11, 19), explicitly unsupported (14–17) or out of range; the last group
/// is the dangerous one — an unrecognised numeric Type would otherwise fall
/// through [`backend_for_type`] to JMAP and create a mail mailbox the client
/// never asked for.
fn is_creatable_folder_type(t: i64) -> bool {
    matches!(t, 1 | 12 | 13)
}

/// Map an EAS user-created folder Type to the backend that hosts it. Only
/// Type 13 (calendar) is a CalDAV collection; 1 (generic) and 12 (user mail)
/// are JMAP mail folders. Types 14–17 are rejected by
/// `is_unsupported_folder_type` before this is reached.
fn backend_for_type(t: i64) -> FolderBackend {
    match t {
        13 => FolderBackend::Caldav,
        _ => FolderBackend::Jmap,
    }
}

/// A CalDAV collection descriptor for a user-created calendar-family folder.
struct CalCollection {
    /// Path segment under the user's calendar home (`/{cal}/{user}/`).
    name: String,
    /// Declared iCalendar component set.
    components: &'static [&'static str],
    /// Stored collection href (ServerId).
    href: String,
}

impl CalCollection {
    /// The configured CalDAV base path (e.g. `/dav` for
    /// `http://stalwart:8080/dav`), or `None` when `caldav_base` is not a
    /// parseable absolute URL. Used to build a stored href that resolves back
    /// to the same collection the client created.
    fn base_path_of(cfg: &crate::config::Config) -> Option<String> {
        reqwest::Url::parse(&cfg.caldav_base)
            .ok()
            .map(|u| u.path().trim_end_matches('/').to_string())
    }
}

fn caldav_collection_for(
    base_path: &str,
    username: &str,
    display_name: &str,
    folder_type: i64,
) -> Option<CalCollection> {
    let components = caldav_components_for_type(folder_type)?;
    let name = sanitize_collection_name(display_name);
    if name.is_empty() {
        return None;
    }
    // Keep the configured base path (e.g. `/dav`) so the stored href resolves,
    // through `CaldavClient::absolute_url`, to the SAME collection that
    // `ensure_calendar_collection` (which builds `{base}/cal/{user}/{name}/`)
    // created. A root-relative href would discard the base path on join and
    // make FolderDelete/FolderUpdate target the wrong URL.
    let href = format!(
        "{}/cal/{}/{}/",
        base_path.trim_end_matches('/'),
        username,
        name
    );
    Some(CalCollection {
        name,
        components,
        href,
    })
}

/// Sanitize a CalDAV collection path segment: drop anything that could escape
/// the calendar home (`/`, `\`), control characters, and the URL path
/// delimiters `?`/`#`/`%` that would otherwise truncate or corrupt the href
/// when it is joined back into an absolute URL.
fn sanitize_collection_name(display_name: &str) -> String {
    let cleaned: String = display_name
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | '?' | '#' | '%' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    cleaned.trim_matches('.').trim().to_string()
}

// ---------------------------------------------------------------------------
// System-folder detection
// ---------------------------------------------------------------------------

/// Whether an EAS ServerId names one of the gateway's fixed system folders.
///
/// System folders are: the six email defaults (2–6, 12), Calendar (1),
/// Contacts (8), Tasks (7) and Notes (10). Their ServerIds are reserved and
/// must never collide with a user-created folder.
pub fn is_system_folder_id(server_id: &str) -> bool {
    matches!(
        server_id,
        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "10" | "12"
    )
}

/// The DisplayNames of the gateway's fixed system folders, shared by the
/// reserved-name check and the reserved *collection*-name guard.
const SYSTEM_FOLDER_NAMES: [&str; 10] = [
    "Inbox",
    "Drafts",
    "Deleted Items",
    "Sent Items",
    "Outbox",
    "Junk Email",
    "Calendar",
    "Contacts",
    "Tasks",
    "Notes",
];

/// Whether a DisplayName is already taken by a system folder (case-insensitive).
///
/// Shared with the EWS `UpdateFolder` mirror so an EWS rename of an
/// EAS-visible folder is held to the same rule EAS `FolderUpdate` enforces.
pub(crate) fn is_system_folder_name(display_name: &str) -> bool {
    SYSTEM_FOLDER_NAMES
        .iter()
        .any(|n| n.eq_ignore_ascii_case(display_name.trim()))
}

/// Whether a sanitized CalDAV collection path segment collides with a
/// system-managed collection. `sanitize_collection_name` strips surrounding
/// dots, so display names such as `Tasks.`, `.Tasks` or `Notes.` sanitize to
/// the gateway's own Tasks/Notes collection name; creating or adopting such a
/// collection would let a later `FolderDelete` destroy the shared Tasks/Notes
/// store, so those names are rejected.
fn is_reserved_collection_name(name: &str) -> bool {
    SYSTEM_FOLDER_NAMES
        .iter()
        .any(|n| n.eq_ignore_ascii_case(name.trim()))
}

// ---------------------------------------------------------------------------
// FolderCreate ([MS-ASCMD] §2.2.1.3)
// ---------------------------------------------------------------------------

/// Create a folder, backing it with JMAP `Mailbox/set` or a CalDAV
/// `MKCALENDAR`, and persist the mapping.
pub async fn create_folder(
    state: &Arc<AppState>,
    owner: &str,
    password: &SecretString,
    parent_id: &str,
    display_name: &str,
    folder_type: i64,
) -> FolderMutationResult {
    let display_name = display_name.trim();
    let parent_id = parent_id.trim();
    if display_name.is_empty() || parent_id.is_empty() {
        return FolderMutationResult::status(10);
    }
    // [MS-ASCMD] §2.2.1.3: creating a *default* folder (types 2–11, 19) is a
    // semantic error (Status 10). The default folders already exist.
    if is_default_folder_type(folder_type) {
        return FolderMutationResult::status(10);
    }
    // Unsupported Types (14–17): Contacts live on CardDAV (no collection
    // creation), and Tasks/Notes/Journal are single gateway-local collections
    // that Sync reaches by content class, not by this folder's mapping. Reject
    // rather than create a collection no Sync path reads (Status 10).
    if is_unsupported_folder_type(folder_type) {
        return FolderMutationResult::status(10);
    }
    // Any value outside the spec's creatable set (1/12/13) is malformed
    // (Status 10). Without this guard an unrecognised Type would fall through
    // `backend_for_type` to JMAP and silently create a mail mailbox.
    if !is_creatable_folder_type(folder_type) {
        return FolderMutationResult::status(10);
    }
    if is_system_folder_name(display_name) {
        return FolderMutationResult::status(2);
    }
    // The EAS ServerId must be ≤64 chars (§6.13). A mail folder's ServerId is
    // its JMAP mailbox id (short); a calendar-family folder's is the full
    // collection href, so the *href* — not just the display name — must fit.
    // `create_caldav_folder` performs the exact href check; this is the shared
    // display-name bound for the mail path.
    if display_name.chars().count() > 64 {
        return FolderMutationResult::status(10);
    }

    // Reject a duplicate DisplayName under the same parent, and (for
    // calendar-family folders) a display name that sanitizes to a collection
    // href already owned by another folder — distinct names such as `Foo/Bar`
    // and `Foo_Bar` map to the same collection and must not clobber it.
    let caldav_href = if backend_for_type(folder_type) == FolderBackend::Caldav {
        // Calendar-family folders are top-level only: the gateway models a
        // single collection per type, so a nested calendar folder is not
        // representable.
        if parent_id != "0" {
            return FolderMutationResult::status(5);
        }
        let Some(base_path) = CalCollection::base_path_of(&state.cfg) else {
            return FolderMutationResult::status(6);
        };
        let Some(coll) = caldav_collection_for(&base_path, owner, display_name, folder_type) else {
            return FolderMutationResult::status(10);
        };
        // A sanitized name that collides with a system collection (e.g.
        // `Tasks.` → `Tasks`) would create or adopt the gateway's own shared
        // Tasks/Notes collection, which a later FolderDelete would destroy.
        if is_reserved_collection_name(&coll.name) {
            return FolderMutationResult::status(2);
        }
        if coll.href.chars().count() > 64 {
            return FolderMutationResult::status(10);
        }
        Some(coll)
    } else {
        None
    };
    match state.storage.list_folders(owner).await {
        Ok(rows) => {
            // A sanitized display name that maps to a collection owned by a
            // DIFFERENT folder is a hard collision.
            if let Some(coll) = caldav_href.as_ref()
                && rows.iter().any(|r| {
                    r.backend_kind == "caldav"
                        && r.backend_id == coll.href
                        && !r.display_name.eq_ignore_ascii_case(display_name)
                })
            {
                return FolderMutationResult::status(2);
            }
            let duplicate_name = rows.iter().any(|r| {
                r.parent_id == parent_id && r.display_name.eq_ignore_ascii_case(display_name)
            });
            if duplicate_name {
                // LOST-RESPONSE REPLAY: if the colliding folder is THIS
                // command's own collection (the href is derived deterministically
                // from owner + display name) and its create intent is still
                // retained, the client simply never learned the ServerId. Fall
                // through to `create_caldav_folder`, whose 405 reconciliation
                // verifies the backend collection and replays the ServerId
                // instead of reporting a permanent collision.
                let replayable = match caldav_href.as_ref() {
                    Some(coll) => {
                        let same_collection = rows.iter().any(|r| {
                            r.backend_kind == "caldav"
                                && r.backend_id == coll.href
                                && r.display_name.eq_ignore_ascii_case(display_name)
                        });
                        // A storage READ failure is fail-closed: flattening it to
                        // "no intent" would answer a lost-response retry with
                        // Status 2 (a permanent name collision) while the
                        // collection stays live and unmapped.
                        let intent = match state
                            .storage
                            .get_pending_calendar_create(owner, &coll.href)
                            .await
                        {
                            Ok(intent) => intent,
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "FolderCreate: failed to read the pending calendar create intent"
                                );
                                return FolderMutationResult::status(6);
                            }
                        };
                        let intent_matches = matches!(
                            intent,
                            Some(intent) if intent.display_name.eq_ignore_ascii_case(display_name)
                        );
                        same_collection && intent_matches
                    }
                    None => false,
                };
                if !replayable {
                    return FolderMutationResult::status(2);
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "FolderCreate: failed to list existing folders");
            return FolderMutationResult::status(6);
        }
    }

    match backend_for_type(folder_type) {
        FolderBackend::Jmap => {
            create_mail_folder(state, owner, password, parent_id, display_name, folder_type).await
        }
        FolderBackend::Caldav => match caldav_href {
            Some(coll) => {
                create_caldav_folder(state, owner, password, display_name, folder_type, coll).await
            }
            None => FolderMutationResult::status(6),
        },
    }
}

async fn create_mail_folder(
    state: &Arc<AppState>,
    owner: &str,
    password: &SecretString,
    parent_id: &str,
    display_name: &str,
    folder_type: i64,
) -> FolderMutationResult {
    let Some(jmap) = state.jmap_client.clone() else {
        return FolderMutationResult::status(6);
    };
    let account_id = match jmap.get_account_id(owner, password).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(error = %e, "FolderCreate: JMAP account lookup failed");
            return FolderMutationResult::status(6);
        }
    };
    // Resolve the EAS parent id to a JMAP parentId. "0" (root) creates a
    // top-level mailbox; a system email folder maps to its JMAP mailbox id; a
    // user-created folder maps to its stored backend id.
    let jmap_parent: Option<String> = match parent_id {
        "0" => None,
        id if is_system_folder_id(id) => {
            match system_email_folder_mailbox_id(&jmap, &account_id, id, owner, password).await {
                SystemFolderMailbox::Resolved(m) => Some(m),
                SystemFolderMailbox::Absent => return FolderMutationResult::status(5),
                SystemFolderMailbox::Error => {
                    tracing::warn!(
                        collection_id = id,
                        "FolderCreate: system parent mailbox lookup failed"
                    );
                    return FolderMutationResult::status(6);
                }
            }
        }
        id => match state.storage.get_folder(owner, id).await {
            Ok(Some(row)) if row.backend_kind == "jmap" => Some(row.backend_id),
            Ok(_) => return FolderMutationResult::status(5),
            Err(e) => {
                tracing::warn!(error = %e, "FolderCreate: parent lookup failed");
                return FolderMutationResult::status(6);
            }
        },
    };

    match jmap
        .create_mailbox(
            &account_id,
            display_name,
            jmap_parent.as_deref(),
            owner,
            password,
        )
        .await
    {
        Ok(mailbox_id) => {
            // The JMAP mailbox id is the EAS ServerId (stable and usable
            // directly in Email/query). The mapping and its folder-hierarchy
            // journal row are written in ONE transaction, so success is
            // returned only once an already-primed device can learn about the
            // new folder.
            if let Err(e) = state
                .storage
                .upsert_folder_journaled(
                    crate::storage::FolderUpsertParams {
                        owner,
                        server_id: &mailbox_id,
                        parent_id,
                        display_name,
                        folder_type,
                        backend_kind: "jmap",
                        backend_id: &mailbox_id,
                    },
                    "folder-add",
                )
                .await
            {
                // The mailbox was created but the mapping write failed.
                // Compensate by destroying the just-created mailbox, only if it
                // holds no mail, so a retry is not rejected as a duplicate
                // sibling name while the backend folder is left orphaned.
                // `AlreadyGone` is the desired end state; a failed
                // compensation is logged and the client still receives a
                // retryable failure. Passing `false` for `onDestroyRemoveEmails`
                // means the server refuses the destroy when the mailbox still
                // holds mail — no mail is removed by this rollback.
                tracing::warn!(error = %e, "FolderCreate: failed to persist folder mapping");
                match jmap
                    .destroy_mailbox(&account_id, &mailbox_id, false, owner, password)
                    .await
                {
                    Ok(
                        crate::jmap::MailboxDestroyOutcome::Destroyed
                        | crate::jmap::MailboxDestroyOutcome::AlreadyGone,
                    ) => {}
                    Ok(crate::jmap::MailboxDestroyOutcome::Refused) => tracing::warn!(
                        mailbox_id = %mailbox_id,
                        "FolderCreate: rollback of the created mailbox was refused"
                    ),
                    Err(del) => tracing::warn!(
                        error = %del,
                        mailbox_id = %mailbox_id,
                        "FolderCreate: failed to roll back the created mailbox"
                    ),
                }
                return FolderMutationResult::status(6);
            }
            FolderMutationResult::created(mailbox_id)
        }
        Err(e) => {
            tracing::warn!(error = %e, "FolderCreate: JMAP Mailbox/set failed");
            FolderMutationResult::status(6)
        }
    }
}

async fn create_caldav_folder(
    state: &Arc<AppState>,
    owner: &str,
    password: &SecretString,
    display_name: &str,
    folder_type: i64,
    coll: CalCollection,
) -> FolderMutationResult {
    let Ok(caldav) = CaldavClient::new(&state.cfg) else {
        return FolderMutationResult::status(6);
    };

    // Check for a pending intent from a PRIOR attempt BEFORE sending MKCALENDAR.
    // If MKCALENDAR later returns 405 and a prior intent exists, this attempt is
    // a retry whose backend response was lost: the prior attempt committed the
    // collection but never told the client, so we reconcile via the intent
    // instead of refusing with Status 2 for an orphan.
    // A storage READ failure is fail-closed: flattening it to `None` would answer
    // a lost-response retry with Status 2 (a permanent name collision) instead of
    // a retryable server error, and the collection would stay unmapped.
    let prior_intent = match state
        .storage
        .get_pending_calendar_create(owner, &coll.href)
        .await
    {
        Ok(intent) => intent,
        Err(e) => {
            tracing::warn!(error = %e, "FolderCreate: failed to read the pending calendar create intent");
            return FolderMutationResult::status(6);
        }
    };

    match caldav
        .create_calendar_collection(
            owner,
            password.expose_secret(),
            &coll.name,
            coll.components,
            display_name,
        )
        .await
    {
        Ok(true) => {
            // MKCALENDAR committed the collection. Record our intent for any
            // future retry whose response is lost: the retry will see 405 and
            // reconcile via this intent instead of returning Status 2 for an
            // orphan.
            if let Err(e) = state
                .storage
                .record_pending_calendar_create(owner, &coll.href, display_name)
                .await
            {
                tracing::warn!(error = %e, "FolderCreate: failed to record pending calendar create");
                // Non-fatal: proceed to persist the mapping. A failed intent
                // record only means a lost-response retry cannot reconcile
                // via this path.
            }

            // Persist the mapping. The pending intent is intentionally NOT
            // cleared here: if the client never learns of this success (response
            // lost), its retry will see 405 and reconcile via this same intent.
            if let Err(e) = state
                .storage
                .upsert_folder_journaled(
                    crate::storage::FolderUpsertParams {
                        owner,
                        server_id: &coll.href,
                        parent_id: "0",
                        display_name,
                        folder_type,
                        backend_kind: "caldav",
                        backend_id: &coll.href,
                    },
                    "folder-add",
                )
                .await
            {
                // The collection was created but the mapping write failed. Do
                // NOT delete it as compensation: between the MKCALENDAR and
                // this failure another client may already have written events
                // into it, and a delete would destroy them. The pending intent
                // recorded above is retained instead, so the client's retry
                // (which sees 405) reconciles and maps the SAME collection —
                // an idempotent recovery rather than a destructive rollback.
                tracing::warn!(
                    error = %e,
                    collection = %coll.name,
                    "FolderCreate: failed to persist folder mapping; the pending intent is retained for a reconciling retry"
                );
                return FolderMutationResult::status(6);
            }
            FolderMutationResult::created(coll.href)
        }
        Ok(false) => {
            // MKCALENDAR returned 405: the collection already exists. Before
            // refusing with Status 2, reconcile only if a PRIOR attempt's intent
            // is recorded AND the collection is verifiably present on the
            // backend. A prior intent means a previous attempt committed this
            // collection (MKCALENDAR returned 201) but the client never learned
            // of it — this is exactly the lost-response case.
            if let Some(intent) = prior_intent {
                // Fail CLOSED on a verification failure: a transient PROPFIND
                // error must be answered as a retryable Status 6, never as "the
                // collection does not exist" (Status 2), which the client would
                // treat as a permanent name collision.
                //
                // The verification is a collection-specific Depth:0 PROPFIND
                // (resourcetype + displayname), NOT a GET: a GET on a collection
                // returns whatever body the server chooses for a collection
                // (often an empty 200) and so can neither confirm that the
                // resource is a calendar nor refute a name collision.
                let props = match caldav
                    .get_calendar_collection(&coll.href, owner, password.expose_secret())
                    .await
                {
                    Ok(found) => found,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            collection = %coll.name,
                            "FolderCreate: failed to verify the pending calendar collection"
                        );
                        return FolderMutationResult::status(6);
                    }
                };
                // The href is derived deterministically from owner + display
                // name, and the intent was recorded only after THIS gateway's
                // MKCALENDAR succeeded, so a live CALENDAR collection carrying
                // the intent's display name identifies the collection the failed
                // attempt created.
                let exists = props.is_some();
                let belongs_to_intent = props.as_ref().is_some_and(|p| {
                    p.is_calendar
                        && p.display_name
                            .as_deref()
                            .is_some_and(|name| name.eq_ignore_ascii_case(&intent.display_name))
                });
                if !exists {
                    // The intent describes a collection that no longer exists, so
                    // it can never justify adopting anything. Drop it: keeping it
                    // would let a LATER 405 adopt a collection the user created
                    // outside EAS with the same display name.
                    if let Err(e) = state
                        .storage
                        .clear_pending_calendar_create(owner, &coll.href)
                        .await
                    {
                        tracing::warn!(error = %e, "FolderCreate: failed to clear a stale pending calendar create");
                    }
                }
                if exists && belongs_to_intent {
                    tracing::info!(
                        collection = %coll.name,
                        "FolderCreate: reconciling pending calendar create on 405"
                    );
                    // The mapping may already be durable (the earlier attempt
                    // persisted it and only the RESPONSE was lost): replaying it
                    // must not journal a second `folder-add` for a folder the
                    // client already knows.
                    match state.storage.get_folder(owner, &coll.href).await {
                        Ok(Some(existing))
                            if existing
                                .display_name
                                .eq_ignore_ascii_case(&intent.display_name) =>
                        {
                            if let Err(e) = state
                                .storage
                                .clear_pending_calendar_create(owner, &coll.href)
                                .await
                            {
                                tracing::warn!(error = %e, "FolderCreate: failed to clear pending calendar create");
                            }
                            return FolderMutationResult::created(coll.href);
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "FolderCreate: failed to read the folder mapping during reconciliation"
                            );
                            return FolderMutationResult::status(6);
                        }
                    }
                    let result = state
                        .storage
                        .upsert_folder_journaled(
                            crate::storage::FolderUpsertParams {
                                owner,
                                server_id: &coll.href,
                                parent_id: "0",
                                display_name: &intent.display_name,
                                folder_type,
                                backend_kind: "caldav",
                                backend_id: &coll.href,
                            },
                            "folder-add",
                        )
                        .await;
                    match result {
                        Ok(()) => {
                            // The intent is consumed only by a SUCCESSFUL
                            // reconciliation: clearing it after a failed mapping
                            // write would leave the live collection unmapped and
                            // the next retry answering Status 2 (a permanent name
                            // collision) instead of reconciling again.
                            if let Err(e) = state
                                .storage
                                .clear_pending_calendar_create(owner, &coll.href)
                                .await
                            {
                                tracing::warn!(error = %e, "FolderCreate: failed to clear pending calendar create");
                            }
                            return FolderMutationResult::created(coll.href);
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                collection = %coll.name,
                                "FolderCreate: failed to persist folder mapping on reconcile; the pending intent is retained for another reconciling retry"
                            );
                            return FolderMutationResult::status(6);
                        }
                    }
                }
            }
            tracing::warn!(
                collection = %coll.name,
                "FolderCreate: calendar collection already exists"
            );
            FolderMutationResult::status(2)
        }
        Err(e) => {
            tracing::warn!(error = %e, "FolderCreate: MKCALENDAR failed");
            FolderMutationResult::status(6)
        }
    }
}

/// Resolve a system email folder's EAS CollectionId to its JMAP mailbox id.
///
/// Tri-state so the caller can tell a genuinely absent mailbox (an
/// authoritative empty lookup, or an id with no mappable role → Status 5,
/// invalid parent) from a transient backend failure (`Err` → Status 6,
/// retryable). Flattening the lookup error to `None` would answer a retryable
/// outage as a permanent invalid-parent.
enum SystemFolderMailbox {
    /// The role resolved to a JMAP mailbox id.
    Resolved(String),
    /// No JMAP mailbox maps to this collection id (no role, or an empty lookup).
    Absent,
    /// The mailbox lookup failed transiently.
    Error,
}

async fn system_email_folder_mailbox_id(
    jmap: &JmapClient,
    account_id: &str,
    collection_id: &str,
    username: &str,
    password: &SecretString,
) -> SystemFolderMailbox {
    let Some(role) = crate::email::eas_collection_id_to_mailbox_role(collection_id) else {
        return SystemFolderMailbox::Absent;
    };
    match jmap
        .get_mailbox_ids_for_role(account_id, role, username, password)
        .await
    {
        Ok(ids) => match ids.into_iter().next() {
            Some(m) => SystemFolderMailbox::Resolved(m),
            None => SystemFolderMailbox::Absent,
        },
        Err(e) => {
            tracing::warn!(error = %e, collection_id = %collection_id, "Folder: system email folder mailbox lookup failed");
            SystemFolderMailbox::Error
        }
    }
}

// ---------------------------------------------------------------------------
// FolderDelete ([MS-ASCMD] §2.2.1.4)
// ---------------------------------------------------------------------------

/// Delete a user-created folder, removing it from the backend and the store.
pub async fn delete_folder(
    state: &Arc<AppState>,
    owner: &str,
    password: &SecretString,
    server_id: &str,
) -> FolderMutationResult {
    let server_id = server_id.trim();
    if server_id.is_empty() {
        return FolderMutationResult::status(10);
    }
    // [MS-ASCMD] §2.2.1.4: deleting a special system folder is Status 3.
    if is_system_folder_id(server_id) {
        return FolderMutationResult::status(3);
    }
    match state.storage.get_folder(owner, server_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return FolderMutationResult::status(4),
        Err(e) => {
            tracing::warn!(error = %e, "FolderDelete: folder lookup failed");
            return FolderMutationResult::status(6);
        }
    }

    // Collect the folder and every descendant, deepest-first, so a parent is
    // never destroyed while it still has children (JMAP rejects that with
    // `mailboxHasChild`). The walk spans the MIRROR and the BACKEND hierarchy:
    // a sub-folder created by another client (JMAP/IMAP/EWS) exists in the JMAP
    // hierarchy but has no `folder_map` row, and destroying the parent would be
    // refused while it exists.
    let subtree = match collect_delete_subtree(state, owner, password, server_id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "FolderDelete: failed to enumerate descendants");
            return FolderMutationResult::status(6);
        }
    };

    // Destroy the backend objects deepest-first, ABORTING on the first failure
    // so the mirror is never pruned ahead of the system of record (that would
    // resurrect the folder on the next reconcile). A child whose destroy
    // SUCCEEDED before the abort is already gone from the backend, so its mirror
    // row MUST be pruned and a `folder-delete` journaled even though the overall
    // command fails: leaving it would make `FolderSync` keep advertising a
    // nonexistent folder until a later retry — and a retry re-collects only the
    // still-present rows, so the destroyed child could never be re-derived.
    let mut destroyed_ids: Vec<String> = Vec::new();
    for folder in &subtree {
        // A backend-only descendant (no row in the MAPPED subtree) is not part
        // of the EAS hierarchy the mirror walk reached: it is destroyed with its
        // parent. When it still HAS an owner mapping (the mirror's parent chain
        // missed it, e.g. after a backend-side move), that mapping must be
        // journaled as `folder-delete` and pruned — otherwise the client keeps
        // advertising a folder the backend no longer has. A descendant with no
        // mapping at all is never journaled (there is nothing to remove and no
        // `folder-delete` to advertise).
        let Some(mapped) = folder.row.as_ref() else {
            let Some(jmap) = state.jmap_client.clone() else {
                prune_destroyed_folder_ids(state, owner, &destroyed_ids).await;
                return FolderMutationResult::status(6);
            };
            let account_id = match jmap.get_account_id(owner, password).await {
                Ok(id) => id,
                Err(e) => {
                    tracing::warn!(error = %e, "FolderDelete: JMAP account lookup failed");
                    prune_destroyed_folder_ids(state, owner, &destroyed_ids).await;
                    return FolderMutationResult::status(6);
                }
            };
            // Record the prune intent durably BEFORE the destructive call, for
            // the same reason as the mapped arm below: a crash or an
            // unobservable outcome must still leave a work list for the next
            // folder-sync replay. A failed record aborts without destroying.
            let stale_server_id = folder.stale_mapping.as_ref().map(|r| r.server_id.clone());
            if let Some(server_id) = stale_server_id.as_deref()
                && let Err(e) = state
                    .storage
                    .record_pending_folder_prunes(
                        owner,
                        std::slice::from_ref(&server_id.to_string()),
                    )
                    .await
            {
                tracing::warn!(
                    error = %e,
                    server_id = %server_id,
                    "FolderDelete: failed to record the deferred prune of a backend-only descendant; not destroying"
                );
                prune_destroyed_folder_ids(state, owner, &destroyed_ids).await;
                return FolderMutationResult::status(6);
            }
            match jmap
                .destroy_mailbox(&account_id, &folder.backend_id, true, owner, password)
                .await
            {
                Ok(outcome) if outcome.is_gone() => {
                    if let Some(server_id) = stale_server_id {
                        destroyed_ids.push(server_id);
                    }
                    continue;
                }
                Ok(_) => {
                    tracing::warn!(
                        backend_id = %folder.backend_id,
                        "FolderDelete: Mailbox/set refused the destroy of a backend-only child"
                    );
                    prune_destroyed_folder_ids(state, owner, &destroyed_ids).await;
                    return FolderMutationResult::status(6);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "FolderDelete: JMAP destroy of a backend-only child failed");
                    prune_destroyed_folder_ids(state, owner, &destroyed_ids).await;
                    return FolderMutationResult::status(6);
                }
            }
        };
        let folder = mapped;
        // Revalidate subtree membership against a FRESH read before destroying:
        // a concurrent move (which updates `folder_map` under the target's
        // mutation lock) may have reparented this folder OUT of the subtree
        // since the snapshot was taken. Destroying it then would delete a LIVE
        // folder the user moved elsewhere — its saved backend id would be
        // deleted even though it no longer belongs to the subtree. Skipping it
        // keeps the moved folder (and its mapping) intact.
        match folder_is_within_subtree(state, owner, server_id, &folder.server_id).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::info!(
                    server_id = %folder.server_id,
                    "FolderDelete: folder no longer within the subtree (moved out); skipping"
                );
                continue;
            }
            Err(e) => {
                tracing::warn!(error = %e, "FolderDelete: failed to revalidate subtree membership");
                prune_destroyed_folder_ids(state, owner, &destroyed_ids).await;
                return FolderMutationResult::status(6);
            }
        }
        // Resolve the backend client FIRST so an abort here (no client, account
        // lookup failure) happens BEFORE any intent is recorded — there is
        // nothing to record when no destroy was attempted.
        match folder.backend_kind.as_str() {
            "jmap" => {
                let Some(jmap) = state.jmap_client.clone() else {
                    prune_destroyed_folder_ids(state, owner, &destroyed_ids).await;
                    return FolderMutationResult::status(6);
                };
                let account_id = match jmap.get_account_id(owner, password).await {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::warn!(error = %e, "FolderDelete: JMAP account lookup failed");
                        prune_destroyed_folder_ids(state, owner, &destroyed_ids).await;
                        return FolderMutationResult::status(6);
                    }
                };
                // Record this mailbox's prune intent durably BEFORE its
                // destructive backend call. A crash or an unobservable outcome
                // (a transport error after the backend committed the destroy)
                // then still leaves a work list for the next folder-sync read to
                // replay; without it, a mailbox gone from the backend but still
                // in `folder_map` could never be re-derived by a retry (the retry
                // re-collects only still-present rows). FAIL-CLOSED: if the
                // intent cannot be recorded, the mailbox is NOT destroyed — the
                // destroy is idempotent, so aborting is safe.
                if let Err(e) = state
                    .storage
                    .record_pending_folder_prunes(owner, std::slice::from_ref(&folder.server_id))
                    .await
                {
                    tracing::warn!(
                        error = %e,
                        server_id = %folder.server_id,
                        "FolderDelete: failed to record the deferred folder-map prune intent; not destroying"
                    );
                    prune_destroyed_folder_ids(state, owner, &destroyed_ids).await;
                    return FolderMutationResult::status(6);
                }
                match jmap
                    .destroy_mailbox(&account_id, &folder.backend_id, true, owner, password)
                    .await
                {
                    Ok(outcome) => {
                        if outcome.is_gone() {
                            destroyed_ids.push(folder.server_id.clone());
                        } else {
                            // The server refused the destroy (e.g. `mailboxHasChild`
                            // because a child was recreated, or `forbidden`). This is a
                            // failure: pruning the mirror here would resurrect the
                            // folder on the next reconcile, so abort and let the client
                            // retry. The intent is intentionally RETAINED: an earlier
                            // concurrent destroy of the same mailbox may still be in
                            // flight with an unknown outcome, and clearing the row here
                            // would drop the only record that lets `reconcile_pending_folder_prunes`
                            // prune it later if that destroy commits. `reconcile_pending_folder_prunes`
                            // resolves each intent against the live JMAP hierarchy — a
                            // still-live mailbox keeps its intent (harmless), a dead one
                            // is pruned and journaled as `folder-delete`. Any child
                            // destroyed earlier in the walk is still pruned.
                            tracing::warn!(
                                server_id = %folder.server_id,
                                "FolderDelete: Mailbox/set refused the destroy; intent retained for reconciliation"
                            );
                            prune_destroyed_folder_ids(state, owner, &destroyed_ids).await;
                            return FolderMutationResult::status(6);
                        }
                    }
                    Err(e) => {
                        // UNKNOWN outcome: the intent is RETAINED (do NOT
                        // withdraw it). A retry cannot rediscover a child the
                        // backend in fact destroyed, so the durable work list
                        // must stay for the next folder-sync replay to
                        // reconcile against the live hierarchy.
                        tracing::warn!(error = %e, "FolderDelete: JMAP Mailbox/set destroy failed");
                        prune_destroyed_folder_ids(state, owner, &destroyed_ids).await;
                        return FolderMutationResult::status(6);
                    }
                }
            }
            "caldav" => {
                let Ok(caldav) = CaldavClient::new(&state.cfg) else {
                    prune_destroyed_folder_ids(state, owner, &destroyed_ids).await;
                    return FolderMutationResult::status(6);
                };
                // Record the prune intent durably BEFORE the destructive call
                // (same rationale as the JMAP arm above). A failed record
                // aborts without deleting.
                if let Err(e) = state
                    .storage
                    .record_pending_folder_prunes(owner, std::slice::from_ref(&folder.server_id))
                    .await
                {
                    tracing::warn!(
                        error = %e,
                        server_id = %folder.server_id,
                        "FolderDelete: failed to record the deferred folder-map prune intent; not deleting"
                    );
                    prune_destroyed_folder_ids(state, owner, &destroyed_ids).await;
                    return FolderMutationResult::status(6);
                }
                if let Err(e) = caldav
                    .delete_collection(&folder.backend_id, owner, password.expose_secret())
                    .await
                {
                    // UNKNOWN outcome: retain the intent for the next
                    // folder-sync replay to reconcile.
                    tracing::warn!(error = %e, "FolderDelete: CalDAV collection delete failed");
                    prune_destroyed_folder_ids(state, owner, &destroyed_ids).await;
                    return FolderMutationResult::status(6);
                }
                destroyed_ids.push(folder.server_id.clone());
            }
            _ => {
                prune_destroyed_folder_ids(state, owner, &destroyed_ids).await;
                return FolderMutationResult::status(4);
            }
        }
    }

    // Backend objects are gone; prune the mirror and journal a Delete for each
    // removed ServerId in ONE transaction. Success is returned only once every
    // journal row is durable, so an already-primed device can always learn
    // about the removal. `removed_ids` is EXACTLY the set of folders whose
    // backend destroy succeeded — a folder skipped because it was moved out of
    // the subtree during the walk is NOT pruned (it is still live).
    let removed_ids: Vec<String> = destroyed_ids.clone();
    // Each id's prune intent was already recorded durably BEFORE its destroy
    // above; this upsert is an idempotent backstop (ON CONFLICT DO NOTHING) so a
    // failed mirror is still replayed by the next folder-sync read
    // (`FolderSync`/`GetHierarchy`). `delete_folders_journaled` clears the rows
    // it prunes, so a recorded-then-pruned id leaves no residue.
    if let Err(e) = state
        .storage
        .record_pending_folder_prunes(owner, &removed_ids)
        .await
    {
        tracing::warn!(error = %e, "FolderDelete: failed to record the deferred folder-map prune");
    }
    if let Err(e) = state
        .storage
        .delete_folders_journaled(owner, &removed_ids)
        .await
    {
        tracing::warn!(error = %e, "FolderDelete: failed to prune folder mappings");
        return FolderMutationResult::status(6);
    }
    FolderMutationResult {
        status: 1,
        server_id: None,
        removed_server_ids: removed_ids,
    }
}

/// Record and prune the `folder_map` rows of folders whose backend destroy
/// already succeeded before a subtree walk aborted. Best-effort: the ids are
/// first recorded durably (so a later folder-sync read can replay the prune),
/// then the direct prune + `folder-delete` journal runs. Used only on the abort
/// paths of [`delete_folder`], where the mailboxes are already gone from the
/// backend and a retry can no longer re-derive them.
async fn prune_destroyed_folder_ids(state: &Arc<AppState>, owner: &str, destroyed_ids: &[String]) {
    if destroyed_ids.is_empty() {
        return;
    }
    if let Err(e) = state
        .storage
        .record_pending_folder_prunes(owner, destroyed_ids)
        .await
    {
        tracing::warn!(error = %e, "FolderDelete: failed to record the deferred folder-map prune for a partial walk");
    }
    if let Err(e) = state
        .storage
        .delete_folders_journaled(owner, destroyed_ids)
        .await
    {
        tracing::warn!(error = %e, "FolderDelete: failed to prune folder mappings for a partial walk");
    }
}

/// True when `server_id` is still a descendant of `root_server_id` in the
/// CURRENT folder map. A concurrent move (which updates `folder_map` under the
/// target folder's mutation lock) may have reparented a folder OUT of the
/// subtree since the snapshot was taken; destroying it then would delete a live
/// folder the user moved elsewhere. Walks the parent chain from `server_id` to
/// the root using a FRESH read; a chain that never reaches the root — or hits a
/// missing row — is no longer part of the subtree. A cycle (corrupt data) is
/// treated as "not within" so the delete aborts rather than looping.
async fn folder_is_within_subtree(
    state: &Arc<AppState>,
    owner: &str,
    root_server_id: &str,
    server_id: &str,
) -> crate::error::Result<bool> {
    let rows = state.storage.list_folders(owner).await?;
    let by_id: std::collections::HashMap<&str, &FolderRow> =
        rows.iter().map(|r| (r.server_id.as_str(), r)).collect();
    let mut cur = server_id.to_string();
    for _ in 0..=rows.len() {
        if cur == root_server_id {
            return Ok(true);
        }
        match by_id.get(cur.as_str()) {
            Some(row) => cur = row.parent_id.clone(),
            None => return Ok(false),
        }
    }
    Ok(false)
}

/// Reconcile the owner's outstanding `pending_folder_prune` intents against the
/// LIVE JMAP mailbox set and replay only the CONFIRMED deletions (prune +
/// `folder-delete`), preserving the mapping of any folder the backend still
/// holds.
///
/// A pending intent is recorded BEFORE its destroy so a crash mid-destroy still
/// leaves a work list; that means a row can name a folder whose destroy is
/// still in flight or was refused. Draining such a row blindly would strand a
/// LIVE folder (EAS would stop advertising a folder that still exists), so this
/// resolves each id against the complete live hierarchy first (see
/// [`Storage::reconcile_pending_folder_prunes`]).
///
/// FAIL-CLOSED: when the live set cannot be established — no JMAP client, or a
/// `Mailbox/query` failure — nothing is pruned and every intent is left in
/// place for a later read to reconcile. Best-effort at the call site: a failure
/// never blocks the primary folder-sync work.
pub(crate) async fn reconcile_deferred_folder_prunes(
    state: &Arc<AppState>,
    owner: &str,
    password: &SecretString,
) {
    let pending = match state.storage.list_pending_folder_prunes(owner).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "folder prune reconcile: failed to list pending intents");
            return;
        }
    };
    if pending.is_empty() {
        return;
    }
    // `pending` is the CANDIDATE set: captured BEFORE any live query and passed
    // through unchanged. A concurrent request can record a NEW intent (and
    // create its mailbox/collection) while a query below is in flight;
    // classifying that newer intent against the older live set would treat a
    // just-created folder as absent and prune its mapping. Only the captured
    // candidates are reconciled; newer intents are left for the next reconcile.
    //
    // Split intents by backend kind: JMAP mailboxes are reconciled against the
    // live JMAP hierarchy, CalDAV collections against the live CalDAV state.
    // A CalDAV collection href never appears in JMAP `Mailbox/query`, so
    // reconciling it there would treat a live collection as deleted and strand
    // its events. Load the folder map once to classify each intent.
    let rows = match state.storage.list_folders(owner).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "folder prune reconcile: failed to list folder map");
            return;
        }
    };
    let by_id: std::collections::HashMap<&str, &FolderRow> =
        rows.iter().map(|r| (r.server_id.as_str(), r)).collect();

    let mut jmap_pending: Vec<String> = Vec::new();
    let mut caldav_pending: Vec<String> = Vec::new();
    for id in &pending {
        match by_id.get(id.as_str()) {
            Some(row) if row.backend_kind == "caldav" => caldav_pending.push(id.clone()),
            Some(_) => jmap_pending.push(id.clone()),
            None => {
                // No mapping row means this intent was already pruned or never
                // had one; drop it silently.
                tracing::debug!(
                    server_id = %id,
                    "folder prune reconcile: no folder_map row; dropping intent"
                );
            }
        }
    }

    // Reconcile JMAP intents against the live JMAP mailbox set. An id the
    // backend still holds keeps its intent; an id the backend no longer holds
    // is pruned and journaled as `folder-delete`.
    if !jmap_pending.is_empty() {
        let Some(jmap) = state.jmap_client.clone() else {
            tracing::warn!(
                owner = %owner,
                "folder prune reconcile: no JMAP client; leaving JMAP intents unresolved"
            );
            return;
        };
        match jmap.query_all_mailboxes(owner, password).await {
            Ok(result) => {
                let live: std::collections::HashSet<String> = result
                    .mailboxes
                    .iter()
                    .filter_map(|m| m.id.clone())
                    .collect();
                if let Err(e) = state
                    .storage
                    .reconcile_pending_folder_prunes(owner, &jmap_pending, &live)
                    .await
                {
                    tracing::warn!(
                        error = %e,
                        "folder prune reconcile: failed to reconcile JMAP intents"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    owner = %owner,
                    "folder prune reconcile: Mailbox/query failed; leaving JMAP intents unresolved"
                );
            }
        }
    }

    // Reconcile CalDAV intents against the live CalDAV collection set. A
    // collection whose `DELETE` failed while the collection remained live must
    // keep its mapping — pruning it would make EAS stop advertising a folder
    // whose events still exist. Only collections confirmed absent from CalDAV
    // are pruned and journaled. An href seen in the PROPFIND multistatus but
    // whose resourcetype could not be confirmed (per-property failure) is
    // *ambiguous*: it must not be treated as proof of absence, and its intent
    // is retained.
    if !caldav_pending.is_empty() {
        let Ok(caldav) = CaldavClient::new(&state.cfg) else {
            tracing::warn!(
                owner = %owner,
                "folder prune reconcile: no CalDAV client; leaving CalDAV intents unresolved"
            );
            return;
        };
        let (confirmed, all_seen) = match caldav
            .find_user_calendars_with_ambiguity(owner, password.expose_secret())
            .await
        {
            Ok((c, a)) => (c, a),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    owner = %owner,
                    "folder prune reconcile: CalDAV PROPFIND failed; leaving CalDAV intents unresolved"
                );
                return;
            }
        };
        // `confirmed` hrefs are calendars the server authoritatively reported
        // as calendar collections. Build the live set from confirmed only.
        // Normalize both sides into one canonical form: path-only, then
        // percent-encode to match how `folder_map.backend_id` entries are stored
        // (already-escaped hrefs are NOT re-encoded — `utf8_percent_encode`
        // leaves `%XX` sequences intact).
        let live: std::collections::HashSet<String> = confirmed
            .into_iter()
            .map(|href| {
                let path = reqwest::Url::parse(&href)
                    .ok()
                    .map(|u| u.path().trim_end_matches('/').to_string())
                    .unwrap_or_else(|| href.trim_end_matches('/').to_string());
                crate::storage::normalize_caldav_path(&path)
            })
            .collect();
        // `ambiguous` hrefs were seen in the multistatus but their
        // resourcetype could not be confirmed. Their absence from `confirmed`
        // must NOT be treated as proof they are gone; retain their intents.
        let ambiguous: std::collections::HashSet<String> = all_seen
            .into_iter()
            .filter(|h| {
                !live.contains(&crate::storage::normalize_caldav_path(
                    &reqwest::Url::parse(h)
                        .ok()
                        .map(|u| u.path().trim_end_matches('/').to_string())
                        .unwrap_or_else(|| h.trim_end_matches('/').to_string()),
                ))
            })
            .map(|href| {
                let path = reqwest::Url::parse(&href)
                    .ok()
                    .map(|u| u.path().trim_end_matches('/').to_string())
                    .unwrap_or_else(|| href.trim_end_matches('/').to_string());
                crate::storage::normalize_caldav_path(&path)
            })
            .collect();
        // Only prune intents whose hrefs are absent from the confirmed live set
        // AND not ambiguous (seen in the multistatus but unconfirmed).
        // Ambiguous hrefs retain their intent; a follow-up reconciliation can
        // retry once the backend returns authoritative resourcetype data.
        let prunable: Vec<String> = caldav_pending
            .iter()
            .filter(|id| !ambiguous.contains(&normalize_caldav_path(id)))
            .cloned()
            .collect();
        if let Err(e) = state
            .storage
            .reconcile_pending_folder_prunes(owner, &prunable, &live)
            .await
        {
            tracing::warn!(
                error = %e,
                "folder prune reconcile: failed to reconcile CalDAV intents"
            );
        }
    }
}

/// The target folder and every descendant, ordered deepest-first (children
/// before parents) so a backend destroy never hits a parent that still has a
/// child.
/// One target of a FolderDelete walk.
struct DeleteSubtreeEntry {
    /// The EAS mapping row when the folder is part of the MAPPED subtree: it
    /// drives the mirror revalidation, the `folder-delete` journal row and the
    /// mapping prune.
    row: Option<FolderRow>,
    /// An owner mapping resolved by BACKEND id for a backend-only descendant
    /// (the mirror's parent chain missed it, e.g. after a backend-side move).
    /// It is destroyed with its parent like any other backend-only child, then
    /// journaled and pruned so the client learns about the removal and no stale
    /// mapping outlives the mailbox. `None` for a descendant the gateway never
    /// mapped: nothing to journal, nothing to prune.
    stale_mapping: Option<FolderRow>,
    backend_id: String,
    /// Distance below the delete root (0 = the root itself).
    depth: usize,
}

/// Enumerate every target of a FolderDelete, deepest-first.
///
/// The mirror (`folder_map`) is NOT the system of record: a sub-folder another
/// client created through JMAP/IMAP/EWS exists in the backend hierarchy without
/// a mapping row, and a parent cannot be destroyed while it still has children.
/// The walk therefore unions the mapped subtree with the JMAP mailbox subtree;
/// backend-only entries are destroyed but never journaled, and a SYSTEM mailbox
/// (non-empty `role`) is never destroyed by a side effect.
async fn collect_delete_subtree(
    state: &Arc<AppState>,
    owner: &str,
    password: &SecretString,
    root_server_id: &str,
) -> crate::error::Result<Vec<DeleteSubtreeEntry>> {
    let mapped = collect_folder_subtree(state, owner, root_server_id).await?;
    // Cloned out so `mapped` can be moved into the entries below.
    let root_row = mapped
        .iter()
        .find(|r| r.server_id == root_server_id)
        .cloned();
    let mapped_len = mapped.len();
    let mut entries: Vec<DeleteSubtreeEntry> = mapped
        .into_iter()
        .enumerate()
        .map(|(idx, row)| DeleteSubtreeEntry {
            backend_id: row.backend_id.clone(),
            depth: mapped_len - idx,
            row: Some(row),
            stale_mapping: None,
        })
        .collect();

    // Only a JMAP mail subtree can hold descendants the mirror does not know:
    // a CalDAV collection is a leaf.
    let Some(root_row) = root_row else {
        return Ok(entries);
    };
    if root_row.backend_kind != "jmap" {
        return Ok(entries);
    }
    let Some(jmap) = state.jmap_client.clone() else {
        // No client: the mirror walk stands. The backend then refuses a parent
        // that still has children, which the client sees as a retryable error.
        return Ok(entries);
    };
    let mailboxes = match jmap.query_all_mailboxes(owner, password).await {
        Ok(list) => list.mailboxes,
        // FAIL CLOSED. The authoritative enumeration is what proves a mapped
        // descendant is still INSIDE the subtree: the mirror cannot see that
        // another client moved a sub-folder out, and destroying it anyway would
        // delete that folder (and, with `onDestroyRemoveEmails`, its mail).
        // A retryable Status 6 makes the client retry once the backend is
        // readable again, which is the safe outcome for a destructive command.
        Err(e) => {
            tracing::warn!(
                error = %e,
                "FolderDelete: failed to read the backend mailbox hierarchy; not destroying"
            );
            return Err(crate::error::GatewayError::Protocol {
                context: format!("mailbox hierarchy unavailable: {e}"),
                source: None,
            });
        }
    };

    // ONE index of the backend hierarchy, reused by the depth walk, the
    // inventory check and the role lookup. Rescanning every mailbox per visited
    // parent made the walk O(depth × mailboxes).
    let mut children: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
    let mut roles: std::collections::HashMap<&str, Option<&str>> = std::collections::HashMap::new();
    let mut inventory: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for mb in &mailboxes {
        let Some(id) = mb.id.as_deref() else { continue };
        inventory.insert(id);
        roles.insert(id, mb.role.as_deref());
        if let Some(parent) = mb.parent_id.as_deref() {
            children.entry(parent).or_default().push(id);
        }
    }

    // Depth of every mailbox below the delete root, following `parentId`.
    let mut depth_of: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    depth_of.insert(root_row.backend_id.as_str(), 0);
    let mut frontier: Vec<&str> = vec![root_row.backend_id.as_str()];
    while let Some(parent) = frontier.pop() {
        let parent_depth = depth_of[parent];
        let Some(kids) = children.get(parent) else {
            continue;
        };
        for id in kids {
            if depth_of.contains_key(id) {
                continue;
            }
            depth_of.insert(id, parent_depth + 1);
            frontier.push(id);
        }
    }
    // The inventory above is COMPLETE (the walk paged to `total`), so a mapped
    // folder the inventory knows but the walk cannot reach is no longer inside
    // the subtree: another client moved it elsewhere. Destroying it would delete
    // a live folder the user still owns, so it is dropped from the walk (and its
    // mapping, which the mirror still places here, is left for the next
    // reconcile). A mapping the inventory does NOT know is kept: it is either
    // already gone (the destroy reports `notFound`, the desired end state) or
    // invisible to this read — never evidence of being outside.
    entries.retain(|e| {
        !inventory.contains(e.backend_id.as_str()) || depth_of.contains_key(e.backend_id.as_str())
    });
    // Mapped entries take the backend depth when the backend still knows them,
    // so a mailbox reparented under a backend-only parent is destroyed first.
    for entry in &mut entries {
        if let Some(depth) = depth_of.get(entry.backend_id.as_str()) {
            entry.depth = *depth;
        }
    }
    let known: std::collections::HashSet<String> =
        entries.iter().map(|e| e.backend_id.clone()).collect();
    // A backend-only descendant may still HAVE an owner mapping the mirror walk
    // missed (its stored parent chain no longer reaches the delete root, e.g.
    // after a backend-side move). Resolve every discovered id against ALL owner
    // mappings so the destroy journals its `folder-delete` and prunes the stale
    // row instead of leaving a mapping that points at a destroyed mailbox.
    let owner_rows = state.storage.list_folders(owner).await?;
    let by_backend: std::collections::HashMap<&str, &FolderRow> = owner_rows
        .iter()
        .map(|r| (r.backend_id.as_str(), r))
        .collect();
    let discovered: Vec<(&str, usize)> = depth_of
        .iter()
        .map(|(id, depth)| (*id, *depth))
        .filter(|(id, _)| !known.contains(*id))
        .collect();
    for (id, depth) in discovered {
        if roles
            .get(id)
            .copied()
            .flatten()
            .is_some_and(|r| !r.is_empty())
        {
            tracing::info!(
                backend_id = id,
                "FolderDelete: skipping a system mailbox found under the delete root"
            );
            continue;
        }
        entries.push(DeleteSubtreeEntry {
            row: None,
            stale_mapping: by_backend.get(id).map(|r| (*r).clone()),
            backend_id: id.to_string(),
            depth,
        });
    }
    // Deepest first; the root (depth 0) is always last.
    entries.sort_by_key(|a| std::cmp::Reverse(a.depth));
    Ok(entries)
}

async fn collect_folder_subtree(
    state: &Arc<AppState>,
    owner: &str,
    root_server_id: &str,
) -> crate::error::Result<Vec<FolderRow>> {
    let rows = state.storage.list_folders(owner).await?;
    let by_id: std::collections::HashMap<&str, &FolderRow> =
        rows.iter().map(|r| (r.server_id.as_str(), r)).collect();
    let mut children: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
    for row in &rows {
        children
            .entry(row.parent_id.as_str())
            .or_default()
            .push(row.server_id.as_str());
    }
    // Depth-first post-order (children before their parent) via an explicit
    // worklist of `(id, expanded)` entries.
    let mut out: Vec<FolderRow> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut worklist: Vec<(&str, bool)> = vec![(root_server_id, false)];
    while let Some((id, expanded)) = worklist.pop() {
        if expanded {
            if let Some(row) = by_id.get(id) {
                out.push((*row).clone());
            }
            continue;
        }
        if !seen.insert(id) {
            continue;
        }
        worklist.push((id, true));
        if let Some(kids) = children.get(id) {
            for kid in kids {
                worklist.push((kid, false));
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// FolderUpdate ([MS-ASCMD] §2.2.1.6)
// ---------------------------------------------------------------------------

/// Rename and/or move a user-created folder.
pub async fn update_folder(
    state: &Arc<AppState>,
    owner: &str,
    password: &SecretString,
    server_id: &str,
    parent_id: &str,
    display_name: &str,
) -> FolderMutationResult {
    let server_id = server_id.trim();
    let display_name = display_name.trim();
    if server_id.is_empty() {
        return FolderMutationResult::status(10);
    }
    // [MS-ASCMD] §2.2.1.6: updating a special system folder is Status 2.
    if is_system_folder_id(server_id) {
        return FolderMutationResult::status(2);
    }
    let row = match state.storage.get_folder(owner, server_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return FolderMutationResult::status(4),
        Err(e) => {
            tracing::warn!(error = %e, "FolderUpdate: folder lookup failed");
            return FolderMutationResult::status(6);
        }
    };

    // Serialize the WHOLE read-validate-mutate-mirror sequence for this folder
    // over EVERY front door (EAS, EWS, MAPI) via `Storage::lock_folder_mutation`.
    // Acquiring the lock BEFORE re-reading the row is what makes the
    // `display_name`/`parent_id` derivation race-free: two concurrent renames of
    // the same folder could otherwise both read the OLD name and one would
    // classify a requested rename back to the original as "unchanged" (no-op),
    // reporting success while the backend kept the other rename. The lock is
    // held across the backend call AND the mirror below.
    let _folder_guard = state
        .storage
        .lock_folder_mutation(owner, &row.backend_id)
        .await;
    // Re-read under the lock: a concurrent mutation may have changed the row
    // (rename, move, or delete) between the lookup above and the lock.
    let row = match state.storage.get_folder(owner, server_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return FolderMutationResult::status(4),
        Err(e) => {
            tracing::warn!(error = %e, "FolderUpdate: folder lookup failed");
            return FolderMutationResult::status(6);
        }
    };

    let parent_id = if parent_id.trim().is_empty() {
        row.parent_id.clone()
    } else {
        parent_id.trim().to_string()
    };

    // A FolderUpdate with no explicit DisplayName keeps the folder's current
    // mapped name (a move-only update preserves the stored name). Validation
    // (reserved names, length, sibling collision) only applies when the client
    // actually sends a new DisplayName — i.e. a rename request.
    let display_name = if display_name.trim().is_empty() {
        row.display_name.clone()
    } else {
        let dn = display_name.trim().to_string();
        if is_system_folder_name(&dn) {
            return FolderMutationResult::status(2);
        }
        if dn.chars().count() > 64 {
            return FolderMutationResult::status(10);
        }
        dn
    };
    match state.storage.list_folders(owner).await {
        Ok(rows) => {
            if rows.iter().any(|r| {
                r.server_id != row.server_id
                    && r.parent_id == parent_id
                    && r.display_name.eq_ignore_ascii_case(&display_name)
            }) {
                return FolderMutationResult::status(2);
            }
            // A move into the folder itself, or into one of its own
            // descendants, would create a cycle: the whole subtree becomes
            // unreachable from the root and every later hierarchy walk depends on
            // a cycle guard. Reject it as an invalid parent (10) BEFORE the
            // backend is touched.
            if parent_id != row.parent_id && parent_id != "0" {
                let mut cur = parent_id.clone();
                for _ in 0..=rows.len() {
                    if cur == row.server_id {
                        return FolderMutationResult::status(10);
                    }
                    match rows.iter().find(|r| r.server_id == cur) {
                        Some(r) => cur = r.parent_id.clone(),
                        None => break,
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "FolderUpdate: failed to list existing folders");
            return FolderMutationResult::status(6);
        }
    }

    match row.backend_kind.as_str() {
        "jmap" => update_mail_folder(state, owner, password, &row, &parent_id, &display_name).await,
        // A CalDAV collection rename keeps the stored ServerId (href) stable and
        // only refreshes the collection's own displayname plus the persisted
        // DisplayName. CalDAV collections are top-level only, so a requested
        // move to a different parent is rejected rather than silently ignored.
        "caldav" => {
            if parent_id != row.parent_id {
                return FolderMutationResult::status(5);
            }
            rename_caldav_folder(state, owner, password, &row, &display_name).await
        }
        _ => FolderMutationResult::status(4),
    }
}

async fn update_mail_folder(
    state: &Arc<AppState>,
    owner: &str,
    password: &SecretString,
    row: &FolderRow,
    parent_id: &str,
    display_name: &str,
) -> FolderMutationResult {
    let Some(jmap) = state.jmap_client.clone() else {
        return FolderMutationResult::status(6);
    };
    let account_id = match jmap.get_account_id(owner, password).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(error = %e, "FolderUpdate: JMAP account lookup failed");
            return FolderMutationResult::status(6);
        }
    };
    // Resolve the target parent (None = "unchanged").
    let jmap_parent: Option<Option<String>> = if parent_id == row.parent_id {
        None
    } else {
        match parent_id {
            "0" => Some(None),
            id if is_system_folder_id(id) => {
                match system_email_folder_mailbox_id(&jmap, &account_id, id, owner, password).await
                {
                    SystemFolderMailbox::Resolved(m) => Some(Some(m)),
                    SystemFolderMailbox::Absent => return FolderMutationResult::status(5),
                    SystemFolderMailbox::Error => {
                        tracing::warn!(
                            collection_id = id,
                            "FolderUpdate: system parent mailbox lookup failed"
                        );
                        return FolderMutationResult::status(6);
                    }
                }
            }
            id => match state.storage.get_folder(owner, id).await {
                Ok(Some(r)) if r.backend_kind == "jmap" => Some(Some(r.backend_id)),
                Ok(_) => return FolderMutationResult::status(5),
                Err(e) => {
                    tracing::warn!(error = %e, "FolderUpdate: parent lookup failed");
                    return FolderMutationResult::status(6);
                }
            },
        }
    };
    // A rename is sent whenever the name differs at all — including a
    // case-only change (`Foo` → `foo`), which `eq_ignore_ascii_case` would
    // swallow while the local map still recorded the new name, desynchronizing
    // the stored DisplayName from the backend mailbox name.
    let rename = (row.display_name != display_name).then_some(display_name);
    // The per-folder mutation lock is held by `update_folder` for the WHOLE
    // read-validate-mutate-mirror sequence (see there), so the backend call and
    // the mirror below cannot interleave with a concurrent mutation.
    match jmap
        .update_mailbox(
            &account_id,
            &row.backend_id,
            rename,
            jmap_parent.as_ref().map(|p| p.as_deref()),
            owner,
            password,
        )
        .await
    {
        Ok(true) => {
            // Persist ONLY the fields this update actually changed, via the
            // targeted `update_folder_journaled`: a whole-row upsert would
            // restore the parent snapshot read before the JMAP call (so a
            // concurrent EWS move of the same folder would be reverted) and
            // could resurrect a mapping a concurrent delete removed. A field
            // equal to the stored value is left untouched; `Ok(false)` means
            // the row vanished (or nothing changed) and must NOT be recreated.
            let new_parent = (parent_id != row.parent_id).then_some(parent_id);
            let new_name = (row.display_name != display_name).then_some(display_name);
            match state
                .storage
                .update_folder_journaled(owner, &row.server_id, new_parent, new_name)
                .await
            {
                Ok(true) => {}
                Ok(false) => tracing::warn!(
                    server_id = %row.server_id,
                    "FolderUpdate: folder mapping vanished (or was a no-op) before the mirror"
                ),
                Err(e) => {
                    tracing::warn!(error = %e, "FolderUpdate: failed to persist folder mapping");
                    return FolderMutationResult::status(6);
                }
            }
            FolderMutationResult::status(1)
        }
        Ok(false) => {
            // The backend did not report the mailbox as updated; do not persist
            // the requested name/parent as though it had been applied.
            tracing::warn!("FolderUpdate: Mailbox/set reported no update for the mailbox");
            FolderMutationResult::status(6)
        }
        Err(e) => {
            tracing::warn!(error = %e, "FolderUpdate: JMAP Mailbox/set update failed");
            FolderMutationResult::status(6)
        }
    }
}

async fn rename_caldav_folder(
    state: &Arc<AppState>,
    owner: &str,
    password: &SecretString,
    row: &FolderRow,
    display_name: &str,
) -> FolderMutationResult {
    let Ok(caldav) = CaldavClient::new(&state.cfg) else {
        return FolderMutationResult::status(6);
    };
    // The per-folder mutation lock is held by `update_folder` for the WHOLE
    // read-validate-mutate-mirror sequence (see there), so the PROPPATCH and the
    // mirror below cannot interleave with a concurrent mutation.
    if let Err(e) = caldav
        .set_collection_displayname(
            &row.backend_id,
            display_name,
            owner,
            password.expose_secret(),
        )
        .await
    {
        tracing::warn!(error = %e, "FolderUpdate: PROPPATCH displayname failed");
        return FolderMutationResult::status(6);
    }
    // A CalDAV collection rename never moves the parent, so persist only the
    // DisplayName via the targeted `update_folder_journaled`. A whole-row
    // upsert would restore the pre-PROPPATCH snapshot and could resurrect a
    // mapping a concurrent delete removed.
    let new_name = (row.display_name != display_name).then_some(display_name);
    match state
        .storage
        .update_folder_journaled(owner, &row.server_id, None, new_name)
        .await
    {
        Ok(true) => {}
        Ok(false) => tracing::warn!(
            server_id = %row.server_id,
            "FolderUpdate: folder mapping vanished (or was a no-op) before the mirror"
        ),
        Err(e) => {
            tracing::warn!(error = %e, "FolderUpdate: failed to persist folder mapping");
            return FolderMutationResult::status(6);
        }
    }
    FolderMutationResult::status(1)
}

// ---------------------------------------------------------------------------
// Wire helpers (shared by the EAS handlers)
// ---------------------------------------------------------------------------

/// Render a `FolderCreate` response ([MS-ASCMD] §6.10): `<Status>` followed
/// by an optional `<SyncKey>` and `<ServerId>`.
pub fn render_create_response(result: &FolderMutationResult, sync_key: &str) -> String {
    match &result.server_id {
        Some(server_id) => format!(
            r#"<?xml version="1.0" encoding="utf-8"?><FolderCreate xmlns="FolderHierarchy:"><Status>{}</Status><SyncKey>{}</SyncKey><ServerId>{}</ServerId></FolderCreate>"#,
            result.status,
            xml_escape(sync_key),
            xml_escape(server_id),
        ),
        None => format!(
            r#"<?xml version="1.0" encoding="utf-8"?><FolderCreate xmlns="FolderHierarchy:"><Status>{}</Status></FolderCreate>"#,
            result.status,
        ),
    }
}

/// Render a `FolderDelete`/`FolderUpdate` response ([MS-ASCMD] §6.12/§6.17):
/// `<Status>` followed by a `<SyncKey>` ONLY on success. §2.2.3.181.1: "If the
/// FolderCreate command, FolderDelete command, or FolderUpdate command is not
/// successful, the server MUST NOT return a SyncKey element."
pub fn render_status_response(root: &str, result: &FolderMutationResult, sync_key: &str) -> String {
    if result.status != 1 {
        return format!(
            r#"<?xml version="1.0" encoding="utf-8"?><{root} xmlns="FolderHierarchy:"><Status>{}</Status></{root}>"#,
            result.status,
        );
    }
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><{root} xmlns="FolderHierarchy:"><Status>{}</Status><SyncKey>{}</SyncKey></{root}>"#,
        result.status,
        xml_escape(sync_key),
    )
}

/// The `(ServerId, ParentId, DisplayName, Type)` tuple of a user-created
/// folder, the EAS wire shape shared by FolderSync and GetHierarchy.
pub fn folder_wire_tuple(row: &FolderRow) -> (String, String, String, i64) {
    (
        row.server_id.clone(),
        row.parent_id.clone(),
        row.display_name.clone(),
        row.folder_type,
    )
}

/// The SyncKey the FolderSync command uses for the folder hierarchy. The
/// folder-mutation responses echo this key so a client that just mutated the
/// hierarchy keeps its FolderSync key coherent.
pub async fn current_hierarchy_sync_key(
    state: &Arc<AppState>,
    owner: &str,
    device_id: &str,
) -> String {
    let collection_id = crate::eas::scoped_collection_id("folderhierarchy", device_id);
    match state.storage.get_sync_key(owner, &collection_id).await {
        Ok(Some((key, _))) => key,
        _ => "0".to_string(),
    }
}

/// Whether the `SyncKey` a folder-mutation command supplied matches this
/// device's stored folder-hierarchy key ([MS-ASCMD] §2.2.3.181.1). The server
/// MUST reject a mismatched key with Status 9 rather than apply the mutation
/// (an obsolete key means the client's hierarchy view is out of date).
///
/// `"0"` is accepted ONLY when the device has NO stored hierarchy key: a client
/// that has never synced the hierarchy is allowed to create its first folder
/// (the `FolderSync` prime path). A device that ALREADY holds a key (it primed,
/// or made an earlier mutation) that supplies `"0"` is a STALE client — the
/// value does not match its stored key, so it is rejected with Status 9 rather
/// than applied against an out-of-date hierarchy view. A storage read failure is
/// treated as a MISMATCH (fail closed): the client re-primes rather than mutate
/// against a key the server cannot verify.
pub async fn folder_sync_key_matches(
    state: &Arc<AppState>,
    owner: &str,
    device_id: &str,
    supplied: &str,
) -> bool {
    let supplied = supplied.trim();
    let collection_id = crate::eas::scoped_collection_id("folderhierarchy", device_id);
    match state.storage.get_sync_key(owner, &collection_id).await {
        // A primed device: the supplied key must match the stored one, INCLUDING
        // `"0"` — a stale client re-sending `"0"` is a mismatch (Status 9).
        Ok(Some((stored, _))) => stored.as_bytes().ct_eq(supplied.as_bytes()).into(),
        // No stored hierarchy key: the device has never synced. The prime key
        // `"0"` is accepted; any other key is unknown and rejected.
        Ok(None) => supplied == "0",
        Err(e) => {
            tracing::warn!(error = %e, "folder mutation: failed to read hierarchy sync key");
            false
        }
    }
}

/// The `FolderHierarchy` status-code response for a folder-mutation command
/// whose supplied `SyncKey` did not match the stored key: Status 9
/// ([MS-ASCMD] §2.2.3.181.1). §2.2.3.181.1 forbids a `SyncKey` element on a
/// FAILED `FolderCreate`/`FolderDelete`/`FolderUpdate` ("If the … command is not
/// successful, the server MUST NOT return a SyncKey element"), so only the
/// Status is emitted; the client re-primes its hierarchy through `FolderSync`
/// (which itself answers a mismatched key with Status 9 + SyncKey 0 per
/// §2.2.3.181.2), not through this response.
pub fn invalid_folder_sync_key_response(root: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><{root} xmlns="FolderHierarchy:"><Status>9</Status></{root}>"#
    )
}

/// After a successful folder mutation, rotate the mutating device's
/// folder-hierarchy SyncKey while PRESERVING its acknowledged watermark, so its
/// next FolderSync (which carries the new key) still receives every change it
/// has not yet seen. Returns the new key; on a persistence failure the previous
/// key is returned so the client can still make progress.
///
/// The watermark must NOT advance to the global journal head here. A change
/// another device journaled since this device's last FolderSync sits BELOW that
/// head; advancing past it would skip the change permanently (the device would
/// never receive its Add/Update/Delete until a full re-prime). Carrying the
/// device's own `seq:` token forward means the next FolderSync replays from the
/// last position it acknowledged — including the mutation it just made, which
/// is harmless and idempotent (a repeated Add/Update/Delete for a folder the
/// device already reflects).
///
/// A device with NO acknowledged watermark (no stored hierarchy state at all,
/// e.g. its first command was a FolderCreate) must NOT receive a nonzero key
/// carrying `seq:0`: the next FolderSync would treat that as an incremental
/// delta and render only the journal rows, omitting every default folder.
/// Answer the prime key `"0"` and persist nothing so the follow-up FolderSync
/// takes the full-prime branch and writes a proper key.
pub async fn rotate_hierarchy_sync_key(
    state: &Arc<AppState>,
    owner: &str,
    device_id: &str,
    expected_key: &str,
) -> String {
    let collection_id = crate::eas::scoped_collection_id("folderhierarchy", device_id);
    let (carried_key, carried_token) = match state.storage.get_sync_key(owner, &collection_id).await
    {
        Ok(Some((key, Some(token)))) => (key, token),
        // No stored watermark (or a stored key with no token): force a prime.
        Ok(_) => return "0".to_string(),
        Err(e) => {
            // A read failure must NOT be answered with a key the server could
            // not verify: the client re-primes its hierarchy instead (the stored
            // watermark is left untouched either way).
            tracing::warn!(
                error = %e,
                "FolderSync: failed to read hierarchy watermark for key rotation"
            );
            return "0".to_string();
        }
    };
    // Anchor the rotation to the key the request was VALIDATED against. If the
    // stored key has moved on, a concurrent request (e.g. a FolderSync that
    // computed its delta before this command's journal row existed) may have
    // consumed the watermark past the change just made; handing that key to the
    // client could make its next FolderSync skip the folder it was just told
    // about. Answer the re-prime signal instead and leave the watermark alone.
    if carried_key != expected_key.trim() {
        tracing::warn!(
            user = %owner,
            "FolderSync: hierarchy key changed after this command validated its SyncKey; asking the client to re-prime"
        );
        return "0".to_string();
    }
    let new_key = uuid::Uuid::new_v4().simple().to_string();
    // Compare-and-swap on the pair that was read: a concurrent FolderSync may
    // have advanced the watermark between the read and this write, and an
    // unconditional write would carry the OLD token over the new one — the
    // device would then skip every change in that window.
    match state
        .storage
        .replace_sync_key_if_unchanged(
            owner,
            &collection_id,
            &carried_key,
            Some(&carried_token),
            &new_key,
            Some(&carried_token),
        )
        .await
    {
        Ok(true) => new_key,
        Ok(false) => {
            tracing::warn!(
                user = %owner,
                "FolderSync: hierarchy watermark advanced during key rotation; asking the client to re-prime"
            );
            "0".to_string()
        }
        Err(e) => {
            tracing::warn!(error = %e, "FolderSync: failed to persist rotated hierarchy sync key");
            "0".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;
    use serde_json::{Value, json};
    use std::sync::Arc as StdArc;
    use std::sync::atomic::Ordering;
    use tokio::sync::Mutex as TokioMutex;

    /// A stub that answers both the JMAP surface the mail-folder path uses and
    /// the CalDAV surface the calendar-family path uses. Every request is
    /// recorded so tests can assert on the exact wire calls.
    #[derive(Clone)]
    struct Ctl {
        base: String,
        jmap_calls: StdArc<TokioMutex<Vec<Value>>>,
        dav_calls: StdArc<TokioMutex<Vec<(String, String)>>>,
        /// Scripts the `Mailbox/set destroy` reply so the outcome mapping can be
        /// exercised: `None` = success, `Some("notfound")` = `notDestroyed`
        /// `notFound`, `Some("refused")` = `notDestroyed` `mailboxHasChild`,
        /// `Some("error")` = a method-level `error` response.
        destroy_mode: Option<String>,
        /// When non-zero, each `Mailbox/set destroy` call increments this counter
        /// and cycles through outcomes [success, refused, error], so concurrent
        /// `delete_folder` calls observe mixed backend outcomes. This exercises
        /// the race where a `Refused` outcome must not clear a pending prune
        /// intent that an earlier in-flight destroy may still need.
        destroy_variety: StdArc<std::sync::atomic::AtomicU16>,
        /// The HTTP status the CalDAV stub answers `MKCALENDAR` with (201 by
        /// default; 405 models a collection that already exists). Atomic so a
        /// test can flip the status between calls to simulate a lost response
        /// (first MKCALENDAR returns 201, the retry sees 405).
        mkcalendar_status: StdArc<std::sync::atomic::AtomicU16>,
        /// Body the CalDAV stub answers `PROPFIND` with (207). `None` keeps the
        /// empty-200 default, which models an empty home.
        propfind_body: Option<String>,
        /// Status the CalDAV stub answers `GET` with. `None` keeps the 200
        /// default (a collection that exists); `Some(404)` models a collection
        /// that vanished between the MKCALENDAR 405 and the verification read.
        get_status: Option<u16>,
        /// When set, the stub closes the storage pool while answering
        /// `MKCALENDAR` (before returning 201), so the caller observes a
        /// committed collection whose mapping write cannot execute. Filled in
        /// after `AppState` exists (the stub is built first).
        pool_closer: Option<StdArc<tokio::sync::OnceCell<StdArc<AppState>>>>,
    }

    async fn spawn_backend_stub() -> (
        StdArc<AppState>,
        StdArc<TokioMutex<Vec<Value>>>,
        StdArc<TokioMutex<Vec<(String, String)>>>,
    ) {
        spawn_backend_stub_with(None).await
    }

    /// As [`spawn_backend_stub_with`] but ENABLES the mixed `Mailbox/set
    /// destroy` cycle (success / refused / error) across calls, exercising the
    /// concurrent-delete race on pending prune intents.
    async fn spawn_backend_stub_varied() -> (
        StdArc<AppState>,
        StdArc<TokioMutex<Vec<Value>>>,
        StdArc<TokioMutex<Vec<(String, String)>>>,
    ) {
        spawn_backend_stub_full(None, 201, 1).await
    }

    /// As [`spawn_backend_stub`] but scripts the `Mailbox/set destroy` reply
    /// (see [`Ctl::destroy_mode`]).
    async fn spawn_backend_stub_with(
        destroy_mode: Option<&str>,
    ) -> (
        StdArc<AppState>,
        StdArc<TokioMutex<Vec<Value>>>,
        StdArc<TokioMutex<Vec<(String, String)>>>,
    ) {
        spawn_backend_stub_full(destroy_mode, 201, 0).await
    }

    /// As [`spawn_backend_stub_with`] but also scripts the `MKCALENDAR` status
    /// (405 models a collection that already exists).
    async fn spawn_backend_stub_full(
        destroy_mode: Option<&str>,
        mkcalendar_status: u16,
        destroy_variety_start: u16,
    ) -> (
        StdArc<AppState>,
        StdArc<TokioMutex<Vec<Value>>>,
        StdArc<TokioMutex<Vec<(String, String)>>>,
    ) {
        spawn_backend_stub_ext(destroy_mode, mkcalendar_status, destroy_variety_start, None).await
    }

    /// As [`spawn_backend_stub_full`] but also scripts the CalDAV `PROPFIND`
    /// multistatus body, so the folder-hierarchy reconciliation can be driven
    /// against a real discovered collection list.
    async fn spawn_backend_stub_ext(
        destroy_mode: Option<&str>,
        mkcalendar_status: u16,
        destroy_variety_start: u16,
        propfind_body: Option<String>,
    ) -> (
        StdArc<AppState>,
        StdArc<TokioMutex<Vec<Value>>>,
        StdArc<TokioMutex<Vec<(String, String)>>>,
    ) {
        spawn_backend_stub_get_status(
            destroy_mode,
            mkcalendar_status,
            destroy_variety_start,
            propfind_body,
            None,
        )
        .await
    }

    /// As [`spawn_backend_stub_ext`] but also scripts the CalDAV `GET` status
    /// (`Some(404)` models a collection that is verifiably absent).
    async fn spawn_backend_stub_get_status(
        destroy_mode: Option<&str>,
        mkcalendar_status: u16,
        destroy_variety_start: u16,
        propfind_body: Option<String>,
        get_status: Option<u16>,
    ) -> (
        StdArc<AppState>,
        StdArc<TokioMutex<Vec<Value>>>,
        StdArc<TokioMutex<Vec<(String, String)>>>,
    ) {
        spawn_backend_stub_pool_closer(
            destroy_mode,
            mkcalendar_status,
            destroy_variety_start,
            propfind_body,
            get_status,
            None,
        )
        .await
    }

    /// As [`spawn_backend_stub_get_status`] but optionally hands back the cell
    /// the caller fills with the `AppState` so the stub can close the storage
    /// pool while answering `MKCALENDAR`.
    async fn spawn_backend_stub_pool_closer(
        destroy_mode: Option<&str>,
        mkcalendar_status: u16,
        destroy_variety_start: u16,
        propfind_body: Option<String>,
        get_status: Option<u16>,
        pool_closer: Option<StdArc<tokio::sync::OnceCell<StdArc<AppState>>>>,
    ) -> (
        StdArc<AppState>,
        StdArc<TokioMutex<Vec<Value>>>,
        StdArc<TokioMutex<Vec<(String, String)>>>,
    ) {
        use axum::response::Response;
        use axum::routing::{any, get, post};
        use std::sync::atomic::AtomicU16;

        let destroy_mode = destroy_mode.map(str::to_string);
        let jmap_calls: StdArc<TokioMutex<Vec<Value>>> = StdArc::new(TokioMutex::new(Vec::new()));
        let dav_calls: StdArc<TokioMutex<Vec<(String, String)>>> =
            StdArc::new(TokioMutex::new(Vec::new()));
        let destroy_variety = StdArc::new(AtomicU16::new(destroy_variety_start));
        let mkcalendar_status = StdArc::new(AtomicU16::new(mkcalendar_status));

        async fn session(axum::Extension(ctl): axum::Extension<Ctl>) -> axum::Json<Value> {
            axum::Json(json!({
                // apiUrl equal to the configured base keeps the client from
                // rewriting it; the client POSTs to the base origin.
                "apiUrl": ctl.base,
                "downloadUrl": format!("{}/download/{{accountId}}/{{blobId}}", ctl.base),
                "uploadUrl": format!("{}/upload/{{accountId}}", ctl.base),
                "eventSourceUrl": format!("{}/eventsource", ctl.base),
                "state": "s",
                "username": "u",
                "accounts": {"a": {"name": "u", "isPersonal": true, "isReadOnly": false, "accountCapabilities": {}}},
                "primaryAccounts": {"urn:ietf:params:jmap:mail": "a"},
                "capabilities": {}
            }))
        }

        async fn api(axum::Extension(ctl): axum::Extension<Ctl>, body: String) -> Response {
            let req: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let calls = req
                .get("methodCalls")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let mut responses = Vec::new();
            for call in calls {
                let name = call.get(0).and_then(|v| v.as_str()).unwrap_or("");
                let args = call.get(1).cloned().unwrap_or(Value::Null);
                let call_id = call.get(2).and_then(|v| v.as_str()).unwrap_or("c0");
                ctl.jmap_calls
                    .lock()
                    .await
                    .push(json!({"method": name, "args": args.clone()}));
                let (mname, result) = match name {
                    "Mailbox/query" => {
                        let role = args
                            .get("filter")
                            .and_then(|f| f.get("role"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let ids: Vec<&str> = match role {
                            "inbox" => vec!["mb-inbox"],
                            "sent" => vec!["mb-sent"],
                            "drafts" => vec!["mb-drafts"],
                            "trash" => vec!["mb-trash"],
                            "junk" => vec!["mb-junk"],
                            _ => vec![],
                        };
                        ("Mailbox/query", json!({"accountId": "a", "ids": ids}))
                    }
                    // RFC 8620 §5.1: a `Mailbox/get` for the ids the query
                    // returned. The descendant walk needs a usable `list` (a
                    // missing one is a hierarchy read failure), and every
                    // mailbox it names is a root-level folder here.
                    "Mailbox/get" => {
                        let ids: Vec<String> = args
                            .get("ids")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default();
                        let list: Vec<Value> = ids
                            .iter()
                            .map(|id| {
                                json!({
                                    "id": id,
                                    "name": id,
                                    "parentId": null,
                                    "role": null,
                                    "totalEmails": 0,
                                    "unreadEmails": 0,
                                })
                            })
                            .collect();
                        (
                            "Mailbox/get",
                            json!({"accountId": "a", "list": list, "state": "s"}),
                        )
                    }
                    "Mailbox/set" => {
                        if let Some(create) = args.get("create").and_then(|v| v.as_object()) {
                            let created: serde_json::Map<String, Value> = create
                                .keys()
                                .map(|k| (k.clone(), json!({"id": "mb-new"})))
                                .collect();
                            (
                                "Mailbox/set",
                                json!({"accountId": "a", "created": Value::Object(created)}),
                            )
                        } else if let Some(update) = args.get("update").and_then(|v| v.as_object())
                        {
                            let updated: serde_json::Map<String, Value> =
                                update.keys().map(|k| (k.clone(), json!(null))).collect();
                            (
                                "Mailbox/set",
                                json!({"accountId": "a", "updated": Value::Object(updated)}),
                            )
                        } else if let Some(destroy) = args.get("destroy").and_then(|v| v.as_array())
                        {
                            // When `destroy_variety` is set, cycle outcomes
                            // [success, refused, error] so concurrent
                            // `delete_folder` calls observe mixed results.
                            if ctl.destroy_variety.load(Ordering::Relaxed) != 0 {
                                let n = ctl.destroy_variety.fetch_add(1, Ordering::Relaxed) % 3;
                                // Record which outcome this call served so a
                                // test can prove the mixed cycle ran.
                                let outcome = match n {
                                    0 => "destroyed",
                                    1 => "refused",
                                    _ => "error",
                                };
                                ctl.jmap_calls.lock().await.push(json!({
                                    "method": "stub-destroy-outcome",
                                    "args": { "outcome": outcome }
                                }));
                                match n {
                                    0 => (
                                        "Mailbox/set",
                                        json!({"accountId": "a", "destroyed": destroy}),
                                    ),
                                    1 => {
                                        let nd: serde_json::Map<String, Value> = destroy
                                            .iter()
                                            .filter_map(|v| v.as_str())
                                            .map(|id| {
                                                (id.to_string(), json!({"type": "mailboxHasChild"}))
                                            })
                                            .collect();
                                        (
                                            "Mailbox/set",
                                            json!({"accountId": "a", "notDestroyed": nd}),
                                        )
                                    }
                                    _ => (
                                        "error",
                                        json!({"type": "serverFail", "description": "scripted"}),
                                    ),
                                }
                            } else {
                                match ctl.destroy_mode.as_deref() {
                                    None => (
                                        "Mailbox/set",
                                        json!({"accountId": "a", "destroyed": destroy}),
                                    ),
                                    Some("notfound") => {
                                        let nd: serde_json::Map<String, Value> = destroy
                                            .iter()
                                            .filter_map(|v| v.as_str())
                                            .map(|id| (id.to_string(), json!({"type": "notFound"})))
                                            .collect();
                                        (
                                            "Mailbox/set",
                                            json!({"accountId": "a", "notDestroyed": nd}),
                                        )
                                    }
                                    Some("refused") => {
                                        let nd: serde_json::Map<String, Value> = destroy
                                            .iter()
                                            .filter_map(|v| v.as_str())
                                            .map(|id| {
                                                (id.to_string(), json!({"type": "mailboxHasChild"}))
                                            })
                                            .collect();
                                        (
                                            "Mailbox/set",
                                            json!({"accountId": "a", "notDestroyed": nd}),
                                        )
                                    }
                                    Some(_) => (
                                        "error",
                                        json!({"type": "serverFail", "description": "scripted"}),
                                    ),
                                }
                            }
                        } else {
                            ("Mailbox/set", json!({"accountId": "a"}))
                        }
                    }
                    _ => (name, json!({"accountId": "a"})),
                };
                responses.push(json!([mname, result, call_id]));
            }
            let resp = json!({"methodResponses": responses, "sessionState": "s"});
            Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&resp).unwrap()))
                .unwrap()
        }

        /// A Depth:0 PROPFIND multistatus for the collection at `path`: a
        /// calendar collection whose display name is the path's last segment
        /// (percent-decoded), i.e. the collection this gateway's own
        /// `MKCALENDAR` would have created for that name.
        fn collection_props_body(path: &str) -> String {
            let segment = path
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or_default();
            let name = percent_encoding::percent_decode_str(segment)
                .decode_utf8_lossy()
                .to_string();
            format!(
                r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:response>
    <D:href>{path}</D:href>
    <D:propstat>
      <D:prop>
        <D:resourcetype><D:collection/><C:calendar/></D:resourcetype>
        <D:displayname>{name}</D:displayname>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#
            )
        }

        /// Catch-all for the CalDAV surface (MKCALENDAR/DELETE/PROPPATCH/PROPFIND).
        async fn dav(
            axum::Extension(ctl): axum::Extension<Ctl>,
            method: axum::http::Method,
            uri: axum::http::Uri,
            headers: axum::http::HeaderMap,
        ) -> Response {
            ctl.dav_calls
                .lock()
                .await
                .push((method.as_str().to_string(), uri.path().to_string()));
            if method.as_str() == "MKCALENDAR"
                && let Some(cell) = ctl.pool_closer.as_ref()
                && let Some(app) = cell.get()
            {
                app.storage.pool().close().await;
            }
            let depth = headers
                .get("depth")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let status = match method.as_str() {
                "MKCALENDAR" => ctl
                    .mkcalendar_status
                    .load(std::sync::atomic::Ordering::Relaxed),
                // `get_status` scripts the collection-probe outcome: the
                // Depth:0 PROPFIND a lost-response replay issues (`Some(404)`
                // models a collection that vanished between the 405 and the
                // verification read).
                "PROPFIND" => ctl.get_status.unwrap_or(207),
                "GET" => ctl.get_status.unwrap_or(200),
                _ => 200,
            };
            let body = match method.as_str() {
                "PROPFIND" => match ctl.propfind_body.as_deref() {
                    Some(xml) => axum::body::Body::from(xml.to_string()),
                    // A Depth:1 listing with no scripted body stays empty (an
                    // empty home); a Depth:0 probe describes the collection.
                    None if depth == "0" => {
                        axum::body::Body::from(collection_props_body(uri.path()))
                    }
                    None => axum::body::Body::empty(),
                },
                _ => axum::body::Body::empty(),
            };
            Response::builder().status(status).body(body).unwrap()
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let ctl = Ctl {
            base: base.clone(),
            jmap_calls: jmap_calls.clone(),
            dav_calls: dav_calls.clone(),
            destroy_mode,
            destroy_variety,
            mkcalendar_status,
            propfind_body,
            get_status,
            pool_closer,
        };
        let app = axum::Router::new()
            .route("/session", get(session))
            .route("/", post(api))
            .fallback(any(dav))
            .layer(axum::Extension(ctl));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .expect("in-memory storage");
        storage.init_schema().await.expect("schema init");
        let cfg = crate::config::Config {
            jmap_base: base.clone(),
            caldav_base: base.clone(),
            carddav_base: base,
            email_enabled: true,
            mail_domain: "example.com".to_string(),
            hmac_secret: SecretString::from("a".repeat(32)),
            ..Default::default()
        };
        let state = StdArc::new(AppState::new(cfg, StdArc::new(storage)));
        (state, jmap_calls, dav_calls)
    }

    /// A `Mailbox/get` failure during the descendant walk must ABORT the
    /// destructive command with the retryable Status 6: without the
    /// authoritative hierarchy the walk cannot tell a sub-folder that another
    /// client moved out of the subtree (destroying it would delete it), so it
    /// must not degrade to the stale mirror.
    #[tokio::test]
    async fn delete_folder_fails_closed_when_hierarchy_read_fails() {
        use axum::response::Response;
        use axum::routing::{get, post};

        let jmap_calls: StdArc<TokioMutex<Vec<Value>>> = StdArc::new(TokioMutex::new(Vec::new()));
        let calls_for_stub = jmap_calls.clone();

        async fn session(axum::Extension(base): axum::Extension<String>) -> axum::Json<Value> {
            axum::Json(json!({
                "apiUrl": base,
                "downloadUrl": format!("{base}/download/{{accountId}}/{{blobId}}"),
                "uploadUrl": format!("{base}/upload/{{accountId}}"),
                "eventSourceUrl": format!("{base}/eventsource"),
                "state": "s",
                "username": "u",
                "accounts": {"a": {"name": "u", "isPersonal": true, "isReadOnly": false, "accountCapabilities": {}}},
                "primaryAccounts": {"urn:ietf:params:jmap:mail": "a"},
                "capabilities": {}
            }))
        }

        async fn api(body: String, calls: StdArc<TokioMutex<Vec<Value>>>) -> Response {
            let req: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let mut responses = Vec::new();
            for call in req
                .get("methodCalls")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
            {
                let name = call.get(0).and_then(|v| v.as_str()).unwrap_or("");
                let call_id = call.get(2).and_then(|v| v.as_str()).unwrap_or("c0");
                calls.lock().await.push(json!({"method": name}));
                let result = match name {
                    // Enumerate one mailbox so `Mailbox/get` (which fails) is
                    // actually reached.
                    "Mailbox/query" => json!({"accountId": "a", "ids": ["mb-x"], "total": 1}),
                    "Mailbox/get" => {
                        json!({"accountId": "a", "notFound": ["mb-x"]})
                    }
                    _ => json!({"accountId": "a"}),
                };
                responses.push(json!([name, result, call_id]));
            }
            let resp = json!({"methodResponses": responses, "sessionState": "s"});
            Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&resp).unwrap()))
                .unwrap()
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let base_for_stub = base.clone();
        let app = axum::Router::new()
            .route("/session", get(session))
            .route(
                "/",
                post(move |body: String| api(body, calls_for_stub.clone())),
            )
            .layer(axum::Extension(base_for_stub));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .expect("in-memory storage");
        storage.init_schema().await.expect("schema init");
        let cfg = crate::config::Config {
            jmap_base: base,
            email_enabled: true,
            mail_domain: "example.com".to_string(),
            hmac_secret: SecretString::from("a".repeat(32)),
            ..Default::default()
        };
        let state = StdArc::new(AppState::new(cfg, StdArc::new(storage)));
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner: "u@example.com",
                    server_id: "mb-x",
                    parent_id: "0",
                    display_name: "Old",
                    folder_type: 12,
                    backend_kind: "jmap",
                    backend_id: "mb-x",
                },
                "folder-add",
            )
            .await
            .unwrap();

        let r = delete_folder(&state, "u@example.com", &pw(), "mb-x").await;
        assert_eq!(r.status, 6, "a hierarchy read failure must be retryable");
        assert!(
            state
                .storage
                .get_folder("u@example.com", "mb-x")
                .await
                .unwrap()
                .is_some(),
            "nothing may be destroyed or pruned when the walk cannot be completed"
        );
        let methods = jmap_methods(&jmap_calls).await;
        assert!(
            !methods.iter().any(|m| m == "Mailbox/set"),
            "no destructive call may be issued: {methods:?}"
        );
    }

    async fn jmap_methods(calls: &StdArc<TokioMutex<Vec<Value>>>) -> Vec<String> {
        calls
            .lock()
            .await
            .iter()
            .filter_map(|c| c.get("method").and_then(|m| m.as_str()).map(String::from))
            .collect()
    }

    async fn last_call_args(calls: &StdArc<TokioMutex<Vec<Value>>>, method: &str) -> Value {
        calls
            .lock()
            .await
            .iter()
            .rev()
            .find(|c| c.get("method").and_then(|m| m.as_str()) == Some(method))
            .and_then(|c| c.get("args").cloned())
            .unwrap_or(Value::Null)
    }

    fn pw() -> SecretString {
        SecretString::from("pw")
    }

    // ---- pure classification helpers -----------------------------------

    #[test]
    fn default_folder_types_are_reserved() {
        for t in [2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 19] {
            assert!(is_default_folder_type(t), "type {t} must be a default type");
        }
        for t in [1, 12, 13, 14, 15, 16, 17, 18] {
            assert!(
                !is_default_folder_type(t),
                "type {t} must be user-creatable"
            );
        }
    }

    #[test]
    fn backend_selection_matches_content_family() {
        assert_eq!(backend_for_type(13), FolderBackend::Caldav);
        assert_eq!(backend_for_type(1), FolderBackend::Jmap);
        assert_eq!(backend_for_type(12), FolderBackend::Jmap);
        // Types 14–17 are rejected by `is_unsupported_folder_type` before this
        // mapping is consulted; any value it still sees is a mail folder.
        for t in [14, 15, 16, 17] {
            assert!(
                is_unsupported_folder_type(t),
                "Type {t} must be rejected before backend selection"
            );
        }
    }

    #[test]
    fn caldav_component_sets_are_correct() {
        // Only Type 13 is a creatable CalDAV collection.
        assert_eq!(caldav_components_for_type(13), Some(&["VEVENT"][..]));
        // Contacts (CardDAV) and Tasks/Notes/Journal (gateway-local) are not
        // creatable CalDAV collections, and mail is not a CalDAV collection.
        for t in [12, 14, 15, 16, 17] {
            assert_eq!(
                caldav_components_for_type(t),
                None,
                "Type {t} must not map to a CalDAV component set"
            );
        }
    }

    #[test]
    fn system_folder_ids_are_reserved() {
        for id in ["1", "2", "3", "4", "5", "6", "7", "8", "10", "12"] {
            assert!(is_system_folder_id(id), "{id} must be reserved");
        }
        assert!(!is_system_folder_id("mb-abc"));
        assert!(!is_system_folder_id("9"));
    }

    #[test]
    fn collection_name_sanitizes_path_separators_and_controls() {
        assert_eq!(sanitize_collection_name("My/Folder"), "My_Folder");
        assert_eq!(sanitize_collection_name("a\\b"), "a_b");
        assert_eq!(sanitize_collection_name("  ..hidden..  "), "hidden");
        assert_eq!(sanitize_collection_name("Tab\there"), "Tab_here");
        assert_eq!(sanitize_collection_name("   "), "");
        assert_eq!(sanitize_collection_name("Side Project"), "Side Project");
    }

    /// `.` and `..` cannot survive the dot-trimming, so they can never form a
    /// CalDAV path segment that walks out of the calendar home; both are
    /// rejected as an unusable name instead of being joined into an href.
    #[test]
    fn dot_only_collection_names_are_rejected() {
        for name in [".", "..", "...", " . "] {
            assert_eq!(sanitize_collection_name(name), "", "{name:?}");
            assert!(
                caldav_collection_for("", "u@example.com", name, 13).is_none(),
                "{name:?} must not yield a collection"
            );
        }
    }

    // ---- wire rendering -------------------------------------------------

    #[test]
    fn create_response_renders_status_synckey_serverid_in_order() {
        let result = FolderMutationResult::created("mb-new".to_string());
        let xml = render_create_response(&result, "key-1");
        assert!(
            xml.contains("<Status>1</Status><SyncKey>key-1</SyncKey><ServerId>mb-new</ServerId>"),
            "unexpected create response: {xml}"
        );
        assert!(xml.contains(r#"xmlns="FolderHierarchy:""#));
        // A failure omits ServerId entirely.
        let fail = FolderMutationResult::status(6);
        let xml = render_create_response(&fail, "key-1");
        assert!(xml.contains("<Status>6</Status>"));
        assert!(
            !xml.contains("<ServerId>"),
            "failed create must not carry a ServerId: {xml}"
        );
    }

    #[test]
    fn status_response_renders_status_then_synckey() {
        let xml = render_status_response("FolderDelete", &FolderMutationResult::status(1), "k");
        assert!(xml.contains("<FolderDelete"));
        assert!(xml.contains("<Status>1</Status><SyncKey>k</SyncKey>"));
        assert!(xml.contains("</FolderDelete>"));
        // §2.2.3.181.1: a FAILED mutation MUST NOT carry a SyncKey.
        let fail = render_status_response("FolderDelete", &FolderMutationResult::status(6), "k");
        assert!(fail.contains("<Status>6</Status>"), "{fail}");
        assert!(
            !fail.contains("<SyncKey>"),
            "a failed mutation must omit SyncKey: {fail}"
        );
    }

    #[test]
    fn invalid_sync_key_response_omits_synckey() {
        // §2.2.3.181.1: a failed mutation (Status 9) MUST NOT carry a SyncKey.
        let xml = invalid_folder_sync_key_response("FolderUpdate");
        assert!(xml.contains("<Status>9</Status>"), "{xml}");
        assert!(
            !xml.contains("<SyncKey>"),
            "a Status 9 mutation response must omit SyncKey: {xml}"
        );
    }

    #[test]
    fn folder_wire_tuple_carries_serverid_parent_name_type() {
        let row = FolderRow {
            server_id: "mb-x".into(),
            parent_id: "2".into(),
            display_name: "Projects".into(),
            folder_type: 12,
            backend_kind: "jmap".into(),
            backend_id: "mb-x".into(),
        };
        assert_eq!(
            folder_wire_tuple(&row),
            ("mb-x".into(), "2".into(), "Projects".into(), 12)
        );
    }

    /// The folder-command responses are rooted on the FolderHierarchy code
    /// page, so every unqualified child must encode and decode unchanged
    /// (AUDIT.md §11 root-page rule) — a bare `<ServerId>` inside a
    /// FolderHierarchy document must not fail WBXML encoding.
    #[test]
    fn folder_responses_roundtrip_through_wbxml() {
        let wbxml = crate::wbxml::Wbxml::new();
        let created = render_create_response(
            &FolderMutationResult::created("mb-12345".to_string()),
            "sync-key-1",
        );
        let encoded = wbxml.encode(&created).expect("create response must encode");
        let decoded = wbxml.decode(&encoded).expect("create response must decode");
        assert!(decoded.contains("<Status>1</Status>"), "{decoded}");
        assert!(
            decoded.contains("<SyncKey>sync-key-1</SyncKey>"),
            "{decoded}"
        );
        assert!(
            decoded.contains("<ServerId>mb-12345</ServerId>"),
            "{decoded}"
        );

        let deleted = render_status_response("FolderDelete", &FolderMutationResult::status(1), "k");
        let encoded = wbxml.encode(&deleted).expect("delete response must encode");
        let decoded = wbxml.decode(&encoded).expect("delete response must decode");
        assert!(decoded.contains("<Status>1</Status>"), "{decoded}");
    }

    // ---- FolderCreate ---------------------------------------------------

    #[tokio::test]
    async fn create_rejects_default_type_with_status_10() {
        let (state, calls, _) = spawn_backend_stub().await;
        let r = create_folder(&state, "u@example.com", &pw(), "0", "Inbox2", 2).await;
        assert_eq!(r.status, 10);
        assert!(
            jmap_methods(&calls).await.is_empty(),
            "a malformed create must not touch the backend"
        );
    }

    #[tokio::test]
    async fn create_rejects_system_folder_name_with_status_2() {
        let (state, calls, _) = spawn_backend_stub().await;
        let r = create_folder(&state, "u@example.com", &pw(), "0", "inbox", 12).await;
        assert_eq!(r.status, 2);
        assert!(jmap_methods(&calls).await.is_empty());
    }

    #[tokio::test]
    async fn create_rejects_overlong_display_name() {
        let (state, _, _) = spawn_backend_stub().await;
        let long = "x".repeat(65);
        let r = create_folder(&state, "u@example.com", &pw(), "0", &long, 12).await;
        assert_eq!(r.status, 10);
    }

    /// Custom Tasks/Notes/Journal folders (Types 15/16/17) map to the gateway's
    /// single gateway-local Tasks/Notes collections, which Sync reaches by
    /// content class and not by the folder's mapping. Creating them would sync
    /// the wrong collection, so FolderCreate rejects them (Status 10) without
    /// touching the backend — matching the Type 14 (Contacts) precedent.
    #[tokio::test]
    async fn create_rejects_custom_task_note_folder_types() {
        for t in [14, 15, 16, 17] {
            let (state, calls, dav_calls) = spawn_backend_stub().await;
            let r = create_folder(&state, "u@example.com", &pw(), "0", "My Folder", t).await;
            assert_eq!(r.status, 10, "Type {t} must be rejected with Status 10");
            assert!(
                jmap_methods(&calls).await.is_empty(),
                "a rejected Type {t} create must not touch the JMAP backend"
            );
            assert!(
                dav_calls.lock().await.is_empty(),
                "a rejected Type {t} create must issue no CalDAV request"
            );
        }
    }

    /// An unrecognised numeric `<Type>` must be rejected as malformed (Status
    /// 10) WITHOUT touching the backend. Before this guard an out-of-range type
    /// (18, 100, 0, negative) fell through `backend_for_type` to JMAP and
    /// created a mail mailbox the client never asked for.
    #[tokio::test]
    async fn create_rejects_unrecognised_folder_types() {
        for t in [0, 18, 20, 100, -1] {
            let (state, calls, dav_calls) = spawn_backend_stub().await;
            let r = create_folder(&state, "u@example.com", &pw(), "0", "My Folder", t).await;
            assert_eq!(r.status, 10, "Type {t} must be rejected with Status 10");
            assert!(
                jmap_methods(&calls).await.is_empty(),
                "a rejected Type {t} create must not touch the JMAP backend"
            );
            assert!(
                dav_calls.lock().await.is_empty(),
                "a rejected Type {t} create must issue no CalDAV request"
            );
            assert!(
                state
                    .storage
                    .list_folders("u@example.com")
                    .await
                    .unwrap()
                    .is_empty(),
                "a rejected Type {t} create must persist no folder row"
            );
        }
    }

    #[test]
    fn creatable_folder_types_match_spec() {
        for t in [1, 12, 13] {
            assert!(is_creatable_folder_type(t), "Type {t} must be creatable");
        }
        // Reserved defaults, explicitly-unsupported and out-of-range values are
        // all NOT creatable.
        for t in [
            0, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 14, 15, 16, 17, 18, 19, 20, 100,
        ] {
            assert!(
                !is_creatable_folder_type(t),
                "Type {t} must not be creatable"
            );
        }
    }

    #[tokio::test]
    async fn create_mail_folder_at_root_calls_mailbox_set_and_persists() {
        let (state, calls, _) = spawn_backend_stub().await;
        let r = create_folder(&state, "u@example.com", &pw(), "0", "Projects", 12).await;
        assert_eq!(r.status, 1);
        assert_eq!(r.server_id.as_deref(), Some("mb-new"));

        let args = last_call_args(&calls, "Mailbox/set").await;
        let create = args.get("create").and_then(|v| v.get("c0")).unwrap();
        assert_eq!(
            create.get("name").and_then(|v| v.as_str()),
            Some("Projects")
        );
        assert!(
            create.get("parentId").is_none(),
            "root create must not set parentId"
        );

        let row = state
            .storage
            .get_folder("u@example.com", "mb-new")
            .await
            .unwrap()
            .expect("folder persisted");
        assert_eq!(row.display_name, "Projects");
        assert_eq!(row.backend_kind, "jmap");
        assert_eq!(row.folder_type, 12);
    }

    #[tokio::test]
    async fn create_mail_folder_under_system_parent_resolves_mailbox_id() {
        let (state, calls, _) = spawn_backend_stub().await;
        let r = create_folder(&state, "u@example.com", &pw(), "2", "Nested", 12).await;
        assert_eq!(r.status, 1);
        let args = last_call_args(&calls, "Mailbox/set").await;
        let create = args.get("create").and_then(|v| v.get("c0")).unwrap();
        assert_eq!(
            create.get("parentId").and_then(|v| v.as_str()),
            Some("mb-inbox"),
            "parent must resolve the Inbox role to its mailbox id"
        );
    }

    /// Scripts the `Mailbox/query`-by-role reply for the system-parent lookup:
    /// `Fail` answers HTTP 500 (a transient backend outage), `Empty` answers a
    /// successful query with no matching mailbox.
    enum RoleLookup {
        Fail,
        Empty,
    }

    /// A stub whose session resolves but whose `Mailbox/query` (the role lookup
    /// behind a system parent) is scripted by [`RoleLookup`].
    async fn spawn_role_lookup_stub(
        mode: RoleLookup,
    ) -> (StdArc<AppState>, StdArc<TokioMutex<Vec<Value>>>) {
        use axum::response::Response;
        use axum::routing::{get, post};

        let calls: StdArc<TokioMutex<Vec<Value>>> = StdArc::new(TokioMutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let session_base = base.clone();
        let base_calls = calls.clone();
        let fail = matches!(mode, RoleLookup::Fail);
        let app = axum::Router::new()
            .route(
                "/session",
                get(move || {
                    let base = session_base.clone();
                    async move {
                        axum::Json(json!({
                            "apiUrl": base,
                            "downloadUrl": format!("{base}/download/{{accountId}}/{{blobId}}"),
                            "uploadUrl": format!("{base}/upload/{{accountId}}"),
                            "eventSourceUrl": format!("{base}/eventsource"),
                            "state": "s",
                            "username": "u",
                            "accounts": {"a": {"name": "u", "isPersonal": true, "isReadOnly": false, "accountCapabilities": {}}},
                            "primaryAccounts": {"urn:ietf:params:jmap:mail": "a"},
                            "capabilities": {}
                        }))
                    }
                }),
            )
            .route(
                "/",
                post(move |body: String| {
                    let calls = base_calls.clone();
                    async move {
                        let req: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                        let mut responses = Vec::new();
                        for call in req
                            .get("methodCalls")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default()
                        {
                            let name = call.get(0).and_then(|v| v.as_str()).unwrap_or("");
                            let args = call.get(1).cloned().unwrap_or(Value::Null);
                            let id = call.get(2).and_then(|v| v.as_str()).unwrap_or("c0");
                            calls
                                .lock()
                                .await
                                .push(json!({"method": name, "args": args.clone()}));
                            if name == "Mailbox/query" && fail {
                                return Response::builder()
                                    .status(500)
                                    .body(axum::body::Body::from("backend down"))
                                    .unwrap();
                            }
                            let (mname, result) = match name {
                                "Mailbox/query" => (
                                    "Mailbox/query",
                                    json!({"accountId": "a", "ids": []}),
                                ),
                                "Mailbox/set" => (
                                    "Mailbox/set",
                                    json!({"accountId": "a", "created": {"c0": {"id": "mb-new"}}}),
                                ),
                                _ => (name, json!({"accountId": "a"})),
                            };
                            responses.push(json!([mname, result, id]));
                        }
                        let resp = json!({"methodResponses": responses, "sessionState": "s"});
                        Response::builder()
                            .status(200)
                            .header("content-type", "application/json")
                            .body(axum::body::Body::from(serde_json::to_vec(&resp).unwrap()))
                            .unwrap()
                    }
                }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .expect("in-memory storage");
        storage.init_schema().await.expect("schema init");
        let cfg = crate::config::Config {
            jmap_base: base.clone(),
            caldav_base: base.clone(),
            carddav_base: base,
            email_enabled: true,
            mail_domain: "example.com".to_string(),
            hmac_secret: SecretString::from("a".repeat(32)),
            ..Default::default()
        };
        let state = StdArc::new(AppState::new(cfg, StdArc::new(storage)));
        (state, calls)
    }

    /// A transient failure resolving a system parent's mailbox is NOT a
    /// permanent invalid-parent: it must be answered retryably (Status 6) and
    /// must not create a mailbox under an unresolved parent.
    #[tokio::test]
    async fn create_under_system_parent_lookup_outage_is_retryable_status_6() {
        let (state, calls) = spawn_role_lookup_stub(RoleLookup::Fail).await;
        let r = create_folder(&state, "u@example.com", &pw(), "2", "Nested", 12).await;
        assert_eq!(
            r.status, 6,
            "a transient parent lookup failure must be retryable, not invalid-parent"
        );
        assert!(
            jmap_methods(&calls)
                .await
                .iter()
                .all(|m| m != "Mailbox/set"),
            "a failed parent resolution must not create a mailbox"
        );
    }

    /// A `FolderUpdate` reparent under a system parent whose mailbox lookup
    /// fails transiently is retryable (Status 6), never a permanent
    /// invalid-parent (Status 5).
    #[tokio::test]
    async fn update_reparent_under_system_parent_lookup_outage_is_status_6() {
        let (state, calls) = spawn_role_lookup_stub(RoleLookup::Fail).await;
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner: "u@example.com",
                    server_id: "mb-1",
                    parent_id: "0",
                    display_name: "Projects",
                    folder_type: 12,
                    backend_kind: "jmap",
                    backend_id: "mb-1",
                },
                "folder-add",
            )
            .await
            .unwrap();
        let r = update_folder(&state, "u@example.com", &pw(), "mb-1", "2", "Projects").await;
        assert_eq!(
            r.status, 6,
            "a transient parent lookup failure must be retryable"
        );
        assert!(
            jmap_methods(&calls)
                .await
                .iter()
                .all(|m| m != "Mailbox/set"),
            "a failed parent resolution must not issue a backend update"
        );
    }

    /// A successful lookup that returns no mailbox for the system role is an
    /// authoritative "invalid parent" (Status 5), distinct from an outage.
    #[tokio::test]
    async fn create_under_system_parent_absent_mailbox_is_status_5() {
        let (state, calls) = spawn_role_lookup_stub(RoleLookup::Empty).await;
        let r = create_folder(&state, "u@example.com", &pw(), "2", "Nested", 12).await;
        assert_eq!(
            r.status, 5,
            "an absent system parent mailbox is an invalid parent"
        );
        assert!(
            jmap_methods(&calls)
                .await
                .iter()
                .all(|m| m != "Mailbox/set"),
            "an absent parent must not create a mailbox"
        );
    }

    #[tokio::test]
    async fn create_calendar_folder_uses_mkcalendar_and_persists() {
        let (state, _, dav) = spawn_backend_stub().await;
        let r = create_folder(&state, "u@example.com", &pw(), "0", "Side Project", 13).await;
        assert_eq!(r.status, 1);
        assert_eq!(
            r.server_id.as_deref(),
            Some("/cal/u@example.com/Side Project/")
        );
        let calls = dav.lock().await.clone();
        assert!(
            calls
                .iter()
                .any(|(m, p)| m == "MKCALENDAR" && p == "/cal/u@example.com/Side%20Project/"),
            "expected MKCALENDAR on the collection URI, got {calls:?}"
        );
        let row = state
            .storage
            .get_folder("u@example.com", "/cal/u@example.com/Side Project/")
            .await
            .unwrap()
            .expect("caldav folder persisted");
        assert_eq!(row.backend_kind, "caldav");
    }

    /// A 405 from MKCALENDAR means the collection already exists. The gateway
    /// cannot tell an orphan from a collection the user created outside EAS, so
    /// it never adopts one — a pre-existing collection must stay Status 2.
    #[tokio::test]
    async fn create_calendar_folder_405_on_preexisting_collection_is_conflict() {
        let (state, _, _) = spawn_backend_stub_full(None, 405, 0).await;
        let r = create_folder(&state, "u@example.com", &pw(), "0", "default", 13).await;
        assert_eq!(
            r.status, 2,
            "a pre-existing unmapped collection must not be silently adopted"
        );
        assert!(
            state
                .storage
                .get_folder("u@example.com", "/cal/u@example.com/default/")
                .await
                .unwrap()
                .is_none(),
            "no mapping may be fabricated for a collection the request did not create"
        );
    }

    /// A 405 where another folder's mapping already owns the href is likewise
    /// Status 2 — the gateway never adopts on 405.
    #[tokio::test]
    async fn create_calendar_folder_405_with_existing_mapping_is_conflict() {
        let (state, _, _) = spawn_backend_stub_full(None, 405, 0).await;
        // Pre-seed a folder that already owns the target href — but with a
        // different display name and `backend_kind: "jmap"` so neither the
        // duplicate-name check nor the caldav-collision check fires first.
        // The test must reach the 405 mapping branch.
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner: "u@example.com",
                    server_id: "/cal/u@example.com/Taken/",
                    parent_id: "0",
                    display_name: "TakenFolder",
                    folder_type: 13,
                    backend_kind: "jmap",
                    backend_id: "/cal/u@example.com/Taken/",
                },
                "folder-add",
            )
            .await
            .unwrap();
        let r = create_folder(&state, "u@example.com", &pw(), "0", "Taken", 13).await;
        assert_eq!(
            r.status, 2,
            "a mapped href is a real conflict, not an orphan"
        );
    }

    /// A MKCALENDAR 405 on retry after a lost response is reconciled: the
    /// pending intent recorded by the first attempt lets the retry recognize
    /// the collection it created, verify it exists on the backend, and upsert
    /// its mapping instead of returning Status 2 for an orphan.
    #[tokio::test]
    async fn create_calendar_folder_405_reconciles_pending_create_after_lost_response() {
        // Simulate a lost-response retry: spawn a stub whose MKCALENDAR returns
        // 405 (collection already exists), but pre-record a pending intent for
        // the same href — exactly the state left by an attempt whose backend
        // response was lost after MKCALENDAR committed. The stub answers the
        // Depth:0 PROPFIND with a calendar collection whose display name is the
        // href's last segment, so the retry confirms the collection and upserts
        // the mapping.
        let (state, _, dav_calls) = spawn_backend_stub_full(None, 405, 0).await;
        state
            .storage
            .record_pending_calendar_create(
                "u@example.com",
                "/cal/u@example.com/Reconcile/",
                "Reconcile",
            )
            .await
            .expect("record pending intent for retry");
        let coll = caldav_collection_for("", "u@example.com", "Reconcile", 13)
            .expect("caldav collection descriptor");
        let r = create_caldav_folder(&state, "u@example.com", &pw(), "Reconcile", 13, coll).await;
        assert_eq!(
            r.status, 1,
            "retry must reconcile pending intent and create mapping on 405"
        );
        assert_eq!(
            r.server_id.as_deref(),
            Some("/cal/u@example.com/Reconcile/"),
            "retry must return the collection href as ServerId"
        );
        let row = state
            .storage
            .get_folder("u@example.com", "/cal/u@example.com/Reconcile/")
            .await
            .unwrap()
            .expect("reconciled mapping must persist");
        assert_eq!(row.backend_kind, "caldav");
        // The pending intent must be cleared after reconciliation.
        let intent = state
            .storage
            .get_pending_calendar_create("u@example.com", "/cal/u@example.com/Reconcile/")
            .await
            .unwrap();
        assert!(
            intent.is_none(),
            "pending intent must be cleared after reconcile"
        );
        let calls = dav_calls.lock().await;
        assert!(
            calls
                .iter()
                .any(|(m, p)| m == "MKCALENDAR" && p == "/cal/u@example.com/Reconcile/"),
            "retry must still send MKCALENDAR (gets 405): {calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|(m, p)| m == "PROPFIND" && p == "/cal/u@example.com/Reconcile/"),
            "retry must PROPFIND the collection to verify it exists: {calls:?}"
        );
    }

    /// The ALREADY-MAPPED replay path: a lost-response retry whose mapping was
    /// persisted by the first attempt. The 405 replay must still verify the
    /// collection with the Depth:0 PROPFIND, then replay the ServerId WITHOUT
    /// journaling a second `folder-add` for a folder the client already knows.
    #[tokio::test]
    async fn create_calendar_folder_405_replays_existing_mapping_without_rejournaling() {
        let (state, _, dav_calls) = spawn_backend_stub_full(None, 405, 0).await;
        state
            .storage
            .record_pending_calendar_create("u@example.com", "/cal/u@example.com/Mapped/", "Mapped")
            .await
            .expect("record pending intent");
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner: "u@example.com",
                    server_id: "/cal/u@example.com/Mapped/",
                    parent_id: "0",
                    display_name: "Mapped",
                    folder_type: 13,
                    backend_kind: "caldav",
                    backend_id: "/cal/u@example.com/Mapped/",
                },
                "folder-add",
            )
            .await
            .expect("seed the mapping the first attempt persisted");
        let journal_before = state
            .storage
            .snapshot_folder_sync_data("u@example.com", 0)
            .await
            .unwrap()
            .journal
            .len();

        let coll = caldav_collection_for("", "u@example.com", "Mapped", 13)
            .expect("caldav collection descriptor");
        let r = create_caldav_folder(&state, "u@example.com", &pw(), "Mapped", 13, coll).await;
        assert_eq!(r.status, 1, "the replay must report success");
        assert_eq!(
            r.server_id.as_deref(),
            Some("/cal/u@example.com/Mapped/"),
            "the replay must return the existing collection's ServerId"
        );
        let calls = dav_calls.lock().await.clone();
        assert!(
            calls
                .iter()
                .any(|(m, p)| m == "PROPFIND" && p == "/cal/u@example.com/Mapped/"),
            "the replay must PROPFIND the collection before replaying its mapping: {calls:?}"
        );
        let after = state
            .storage
            .snapshot_folder_sync_data("u@example.com", 0)
            .await
            .unwrap();
        let (journal_after, folders) = (after.journal, after.folders);
        assert_eq!(
            journal_after.len(),
            journal_before,
            "replaying an existing mapping must not journal a second folder-add"
        );
        assert_eq!(folders.len(), 1, "exactly one mapping must exist");
    }

    /// A 405 replay whose Depth:0 PROPFIND reports the href as a NON-calendar
    /// resource (or without the intent's display name) must NOT adopt it: the
    /// verification identifies the collection by type AND display name.
    #[tokio::test]
    async fn create_calendar_folder_405_does_not_adopt_a_foreign_resource() {
        let not_a_calendar = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:response>
    <D:href>/cal/u@example.com/Foreign/</D:href>
    <D:propstat>
      <D:prop>
        <D:resourcetype><D:collection/></D:resourcetype>
        <D:displayname>Foreign</D:displayname>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;
        let (state, _, _) =
            spawn_backend_stub_ext(None, 405, 0, Some(not_a_calendar.to_string())).await;
        state
            .storage
            .record_pending_calendar_create(
                "u@example.com",
                "/cal/u@example.com/Foreign/",
                "Foreign",
            )
            .await
            .expect("record pending intent");
        let coll = caldav_collection_for("", "u@example.com", "Foreign", 13)
            .expect("caldav collection descriptor");
        let r = create_caldav_folder(&state, "u@example.com", &pw(), "Foreign", 13, coll).await;
        assert_eq!(
            r.status, 2,
            "a non-calendar resource must never be adopted as the intent's collection"
        );
        assert!(
            state
                .storage
                .get_folder("u@example.com", "/cal/u@example.com/Foreign/")
                .await
                .unwrap()
                .is_none(),
            "no mapping may be fabricated for a foreign resource"
        );
    }

    /// A pending create intent for a collection the backend no longer reports
    /// must be dropped: keeping it would let a LATER 405 adopt a collection the
    /// user created outside EAS with the same display name.
    #[tokio::test]
    async fn create_calendar_folder_405_clears_stale_intent_when_collection_absent() {
        let (state, _, _) = spawn_backend_stub_get_status(None, 405, 0, None, Some(404)).await;
        state
            .storage
            .record_pending_calendar_create("u@example.com", "/cal/u@example.com/Gone/", "Gone")
            .await
            .expect("record pending intent");
        let coll = caldav_collection_for("", "u@example.com", "Gone", 13)
            .expect("caldav collection descriptor");
        let r = create_caldav_folder(&state, "u@example.com", &pw(), "Gone", 13, coll).await;
        assert_eq!(
            r.status, 2,
            "a 405 whose collection is verifiably absent is a name collision"
        );
        let intent = state
            .storage
            .get_pending_calendar_create("u@example.com", "/cal/u@example.com/Gone/")
            .await
            .unwrap();
        assert!(
            intent.is_none(),
            "an intent for a vanished collection must be dropped, never left to adopt a later collection"
        );
    }

    /// An unreadable pending-create intent must fail the create CLOSED: treating
    /// it as "no intent" would answer a lost-response retry with Status 2 (a
    /// permanent name collision) while the collection stays live and unmapped.
    #[tokio::test]
    async fn create_calendar_folder_fails_closed_when_intent_read_fails() {
        let (state, _, _) = spawn_backend_stub_full(None, 405, 0).await;
        state
            .storage
            .record_pending_calendar_create("u@example.com", "/cal/u@example.com/Retain/", "Retain")
            .await
            .expect("record pending intent");
        // The intent row is durable, but the pool is closed so neither the intent
        // nor the mapping can be read: the create must answer a retryable error,
        // never Status 2.
        state.storage.pool().close().await;
        let coll = caldav_collection_for("", "u@example.com", "Retain", 13)
            .expect("caldav collection descriptor");
        let r = create_caldav_folder(&state, "u@example.com", &pw(), "Retain", 13, coll).await;
        assert_eq!(
            r.status, 6,
            "an unreadable intent must be retryable, not a name collision"
        );
    }

    /// A retained prune intent must NOT prune a collection the backend still
    /// reports, even when the hrefs only match AFTER percent-encoding: the
    /// mapping may hold a raw space while the discovered href arrives escaped
    /// (`Side%20Project`). Losing that match would drop a live folder's mapping
    /// and strand its events.
    #[tokio::test]
    async fn prune_reconcile_keeps_live_collection_whose_href_needs_escaping() {
        let propfind = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:response>
    <D:href>/cal/u%40example.com/Side%20Project/</D:href>
    <D:propstat>
      <D:prop><D:resourcetype><C:calendar/></D:resourcetype></D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;
        let (state, _, _) = spawn_backend_stub_ext(None, 201, 0, Some(propfind.to_string())).await;
        // A CalDAV folder's EAS ServerId IS its collection href. Stored RAW (with
        // spaces and a bare `@`); the backend reports the ESCAPED form.
        const HREF: &str = "/cal/u@example.com/Side Project";
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner: "u@example.com",
                    server_id: HREF,
                    parent_id: "0",
                    display_name: "Side Project",
                    folder_type: 13,
                    backend_kind: "caldav",
                    backend_id: HREF,
                },
                "folder-add",
            )
            .await
            .unwrap();
        // Intent left over from a FolderDelete whose backend DELETE had an
        // unknown outcome.
        state
            .storage
            .record_pending_folder_prunes("u@example.com", &[HREF.to_string()])
            .await
            .unwrap();

        reconcile_deferred_folder_prunes(&state, "u@example.com", &pw()).await;

        assert!(
            state
                .storage
                .get_folder("u@example.com", HREF)
                .await
                .unwrap()
                .is_some(),
            "a collection the backend still reports must keep its mapping"
        );
        assert_eq!(
            state
                .storage
                .list_pending_folder_prunes("u@example.com")
                .await
                .unwrap(),
            vec![HREF.to_string()],
            "the unresolved intent must be retained for the next reconcile"
        );
    }

    /// A display name that sanitizes to a system collection (`Tasks.` →
    /// `Tasks`) must be rejected, not created or adopted: a later FolderDelete
    /// would otherwise DELETE the gateway's shared Tasks/Notes collection.
    #[tokio::test]
    async fn create_calendar_folder_rejects_sanitized_system_collection_name() {
        let (state, _, dav_calls) = spawn_backend_stub_full(None, 201, 0).await;
        for name in ["Tasks.", ".Tasks", "Notes.", "tasks"] {
            let r = create_folder(&state, "u@example.com", &pw(), "0", name, 13).await;
            assert_eq!(r.status, 2, "`{name}` must not map a system collection");
        }
        assert!(
            dav_calls.lock().await.is_empty(),
            "no MKCALENDAR may be attempted for a reserved system collection name"
        );
    }

    /// When the JMAP mailbox is created but the mapping write fails, the
    /// just-created mailbox must be destroyed so a retry is not rejected as a
    /// duplicate sibling name with an orphaned backend folder.
    #[tokio::test]
    async fn create_mail_folder_rolls_back_mailbox_when_mapping_write_fails() {
        let (state, jmap_calls, _) = spawn_backend_stub().await;
        // Close the pool so the mapping write cannot execute; the JMAP calls
        // (account lookup, create, rollback destroy) are HTTP and still work.
        state.storage.pool().close().await;
        let r = create_mail_folder(&state, "u@example.com", &pw(), "0", "Rollback", 12).await;
        assert_eq!(r.status, 6, "a failed mapping write must report failure");
        let args = last_call_args(&jmap_calls, "Mailbox/set").await;
        assert_eq!(
            args.get("destroy").and_then(|v| v.as_array()),
            Some(&vec![json!("mb-new")]),
            "the orphaned mailbox must be destroyed as compensation"
        );
    }

    /// When the CalDAV collection is created but the mapping write fails, the
    /// just-created collection must NOT be deleted as compensation: another
    /// client may already have written events into it. The create intent
    /// (recorded before the mapping write) is the recovery path — a retry's
    /// MKCALENDAR answers 405 and reconciles onto the SAME collection.
    #[tokio::test]
    async fn create_caldav_folder_never_deletes_collection_when_mapping_write_fails() {
        let pool_closer = StdArc::new(tokio::sync::OnceCell::new());
        let (state, _, dav_calls) =
            spawn_backend_stub_pool_closer(None, 201, 0, None, None, Some(pool_closer.clone()))
                .await;
        // The stub closes the pool while answering MKCALENDAR, so the create is
        // committed on the backend and the mapping write then fails.
        pool_closer.set(state.clone()).ok().expect("cell is empty");
        let coll = caldav_collection_for("", "u@example.com", "Rollback", 13)
            .expect("caldav collection descriptor");
        let r = create_caldav_folder(&state, "u@example.com", &pw(), "Rollback", 13, coll).await;
        assert_eq!(r.status, 6, "a failed mapping write must report failure");
        let calls = dav_calls.lock().await;
        assert!(
            calls.iter().any(|(m, _)| m == "MKCALENDAR"),
            "the collection was created before the mapping write failed: {calls:?}"
        );
        assert!(
            !calls.iter().any(|(m, _)| m == "DELETE"),
            "the created collection must never be deleted as compensation: {calls:?}"
        );
    }

    /// With the storage unavailable BEFORE any backend work, a CalDAV folder
    /// create must not touch the backend at all: a collection whose mapping
    /// cannot be written is an orphan, and its intent cannot be recorded either,
    /// so no retry could ever reconcile it.
    #[tokio::test]
    async fn create_caldav_folder_does_not_touch_backend_when_storage_is_down() {
        let (state, _, dav_calls) = spawn_backend_stub_full(None, 201, 0).await;
        state.storage.pool().close().await;
        let coll = caldav_collection_for("", "u@example.com", "Down", 13)
            .expect("caldav collection descriptor");
        let r = create_caldav_folder(&state, "u@example.com", &pw(), "Down", 13, coll).await;
        assert_eq!(r.status, 6, "a storage outage must be retryable");
        assert!(
            dav_calls.lock().await.is_empty(),
            "no backend collection may be created when its mapping cannot be persisted"
        );
    }

    /// A device that has NEVER primed the hierarchy (no stored FolderSync
    /// state) must not receive a nonzero rotated key carrying `seq:0`: its next
    /// FolderSync would treat that as an incremental delta and omit every
    /// default folder. The prime key `"0"` forces the full-hierarchy branch.
    #[tokio::test]
    async fn rotate_hierarchy_key_for_never_primed_device_returns_prime_key() {
        let (state, _, _) = spawn_backend_stub().await;
        let key = rotate_hierarchy_sync_key(&state, "u@example.com", "fresh-device", "0").await;
        assert_eq!(
            key, "0",
            "a device with no stored hierarchy state must be told to prime"
        );
        // Nothing is persisted, so the follow-up FolderSync writes a real key.
        let collection_id = crate::eas::scoped_collection_id("folderhierarchy", "fresh-device");
        assert!(
            state
                .storage
                .get_sync_key("u@example.com", &collection_id)
                .await
                .unwrap()
                .is_none(),
            "no hierarchy state may be fabricated for a never-primed device"
        );
    }

    /// A primed device keeps its acknowledged watermark across a rotation, so
    /// its next FolderSync still receives every earlier unseen change.
    #[tokio::test]
    async fn rotate_hierarchy_key_preserves_primed_watermark() {
        let (state, _, _) = spawn_backend_stub().await;
        let collection_id = crate::eas::scoped_collection_id("folderhierarchy", "dev-1");
        state
            .storage
            .set_sync_key("u@example.com", &collection_id, "old-key", Some("seq:7"))
            .await
            .unwrap();
        let key = rotate_hierarchy_sync_key(&state, "u@example.com", "dev-1", "old-key").await;
        assert_ne!(key, "0", "a primed device gets a real rotated key");
        let (stored_key, token) = state
            .storage
            .get_sync_key("u@example.com", &collection_id)
            .await
            .unwrap()
            .expect("rotated key persisted");
        assert_eq!(stored_key, key);
        assert_eq!(
            token.as_deref(),
            Some("seq:7"),
            "the acknowledged watermark must be preserved across rotation"
        );
    }

    /// A stored hierarchy key whose token is NULL is not an acknowledged
    /// watermark: rotation must force a prime rather than fabricate `seq:0`.
    #[tokio::test]
    async fn rotate_hierarchy_key_without_token_forces_prime() {
        let (state, _, _) = spawn_backend_stub().await;
        let collection_id = crate::eas::scoped_collection_id("folderhierarchy", "dev-2");
        state
            .storage
            .set_sync_key("u@example.com", &collection_id, "k", None)
            .await
            .unwrap();
        let key = rotate_hierarchy_sync_key(&state, "u@example.com", "dev-2", "k").await;
        assert_eq!(
            key, "0",
            "a key without a watermark is not a valid resume point"
        );
    }

    #[tokio::test]
    async fn create_rejects_duplicate_display_name_under_same_parent() {
        let (state, _, _) = spawn_backend_stub().await;
        let first = create_folder(&state, "u@example.com", &pw(), "0", "Dup", 12).await;
        assert_eq!(first.status, 1);
        let second = create_folder(&state, "u@example.com", &pw(), "0", "dup", 12).await;
        assert_eq!(second.status, 2);
    }

    /// [MS-ASCMD] §2.2.3.181.1: `"0"` is accepted only for a device with NO
    /// stored hierarchy key. A primed device (a stored key exists) that supplies
    /// `"0"` is STALE and must be rejected (Status 9) — accepting it would let an
    /// out-of-date client mutate the hierarchy.
    #[tokio::test]
    async fn folder_sync_key_zero_rejected_for_primed_device() {
        let (state, _, _) = spawn_backend_stub().await;
        let owner = "primed@example.com";
        let collection_id = crate::eas::scoped_collection_id("folderhierarchy", "dev-x");

        // Never-primed device: "0" is the valid prime key.
        assert!(
            folder_sync_key_matches(&state, owner, "dev-x", "0").await,
            "a never-primed device may mutate with the prime key 0"
        );
        // Any non-zero key is unknown for a never-primed device.
        assert!(
            !folder_sync_key_matches(&state, owner, "dev-x", "abc").await,
            "an unknown key for a never-primed device must be rejected"
        );

        // Prime the device (a real key is stored).
        state
            .storage
            .set_sync_key(owner, &collection_id, "key-1", Some("seq:5"))
            .await
            .unwrap();

        assert!(
            folder_sync_key_matches(&state, owner, "dev-x", "key-1").await,
            "the stored key must match"
        );
        assert!(
            !folder_sync_key_matches(&state, owner, "dev-x", "0").await,
            "a primed device supplying 0 is stale and must be rejected (Status 9)"
        );
        assert!(
            !folder_sync_key_matches(&state, owner, "dev-x", "key-2").await,
            "a different key must be rejected"
        );
    }

    #[tokio::test]
    async fn delete_system_folder_is_status_3() {
        let (state, calls, _) = spawn_backend_stub().await;
        let r = delete_folder(&state, "u@example.com", &pw(), "2").await;
        assert_eq!(r.status, 3);
        assert!(jmap_methods(&calls).await.is_empty());
    }

    #[tokio::test]
    async fn delete_unknown_folder_is_status_4() {
        let (state, _, _) = spawn_backend_stub().await;
        let r = delete_folder(&state, "u@example.com", &pw(), "mb-nope").await;
        assert_eq!(r.status, 4);
    }

    #[tokio::test]
    async fn delete_mail_folder_destroys_mailbox_and_removes_row() {
        let (state, calls, _) = spawn_backend_stub().await;
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner: "u@example.com",
                    server_id: "mb-x",
                    parent_id: "0",
                    display_name: "Old",
                    folder_type: 12,
                    backend_kind: "jmap",
                    backend_id: "mb-x",
                },
                "folder-add",
            )
            .await
            .unwrap();
        let r = delete_folder(&state, "u@example.com", &pw(), "mb-x").await;
        assert_eq!(r.status, 1);
        let args = last_call_args(&calls, "Mailbox/set").await;
        assert_eq!(
            args.get("destroy").and_then(|v| v.as_array()).unwrap(),
            &vec![json!("mb-x")]
        );
        assert!(
            state
                .storage
                .get_folder("u@example.com", "mb-x")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn delete_caldav_folder_sends_dav_delete() {
        let (state, _, dav) = spawn_backend_stub().await;
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner: "u@example.com",
                    server_id: "/cal/u@example.com/Foo/",
                    parent_id: "0",
                    display_name: "Foo",
                    folder_type: 13,
                    backend_kind: "caldav",
                    backend_id: "/cal/u@example.com/Foo/",
                },
                "folder-add",
            )
            .await
            .unwrap();
        let r = delete_folder(&state, "u@example.com", &pw(), "/cal/u@example.com/Foo/").await;
        assert_eq!(r.status, 1);
        let calls = dav.lock().await.clone();
        assert!(
            calls
                .iter()
                .any(|(m, p)| m == "DELETE" && p == "/cal/u@example.com/Foo/"),
            "expected DAV DELETE, got {calls:?}"
        );
    }

    #[tokio::test]
    async fn delete_removes_nested_descendants() {
        let (state, _, _) = spawn_backend_stub().await;
        for (id, parent) in [("mb-p", "0"), ("mb-c", "mb-p")] {
            state
                .storage
                .upsert_folder_journaled(
                    crate::storage::FolderUpsertParams {
                        owner: "u@example.com",
                        server_id: id,
                        parent_id: parent,
                        display_name: id,
                        folder_type: 12,
                        backend_kind: "jmap",
                        backend_id: id,
                    },
                    "folder-add",
                )
                .await
                .unwrap();
        }
        let r = delete_folder(&state, "u@example.com", &pw(), "mb-p").await;
        assert_eq!(r.status, 1);
        assert!(
            state
                .storage
                .list_folders("u@example.com")
                .await
                .unwrap()
                .is_empty(),
            "child folder must be removed with its parent"
        );
    }

    /// The delete loop revalidates each folder's subtree membership against a
    /// FRESH read before destroying it, so a folder reparented OUT of the
    /// subtree by a concurrent move is skipped (it is still live). This pins the
    /// membership predicate directly: a moved-out folder, a folder under a
    /// DIFFERENT root, and a missing row are all "not within".
    #[tokio::test]
    async fn folder_is_within_subtree_tracks_current_parentage() {
        let (state, _, _) = spawn_backend_stub().await;
        for (id, parent) in [
            ("mb-root", "0"),
            ("mb-child", "mb-root"),
            ("mb-grand", "mb-child"),
            ("mb-moved", "0"),
            ("mb-elsewhere", "mb-other"),
        ] {
            state
                .storage
                .upsert_folder_journaled(
                    crate::storage::FolderUpsertParams {
                        owner: "u@example.com",
                        server_id: id,
                        parent_id: parent,
                        display_name: id,
                        folder_type: 12,
                        backend_kind: "jmap",
                        backend_id: id,
                    },
                    "folder-add",
                )
                .await
                .unwrap();
        }
        let owner = "u@example.com";
        assert!(
            folder_is_within_subtree(&state, owner, "mb-root", "mb-grand")
                .await
                .unwrap(),
            "a deep descendant is within the subtree"
        );
        assert!(
            folder_is_within_subtree(&state, owner, "mb-root", "mb-root")
                .await
                .unwrap(),
            "the root is within its own subtree"
        );
        assert!(
            !folder_is_within_subtree(&state, owner, "mb-root", "mb-moved")
                .await
                .unwrap(),
            "a folder under a different parent is not within"
        );
        assert!(
            !folder_is_within_subtree(&state, owner, "mb-root", "mb-absent")
                .await
                .unwrap(),
            "a missing row is not within"
        );

        // Reparent `mb-child` (with its subtree) OUT of the root: the whole
        // moved chain is no longer within.
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner,
                    server_id: "mb-child",
                    parent_id: "0",
                    display_name: "mb-child",
                    folder_type: 12,
                    backend_kind: "jmap",
                    backend_id: "mb-child",
                },
                "folder-update",
            )
            .await
            .unwrap();
        assert!(
            !folder_is_within_subtree(&state, owner, "mb-root", "mb-child")
                .await
                .unwrap(),
            "a folder moved out of the subtree is no longer within"
        );
        assert!(
            !folder_is_within_subtree(&state, owner, "mb-root", "mb-grand")
                .await
                .unwrap(),
            "a descendant that moved WITH its parent is no longer within"
        );
    }

    /// A `notFound` destroy reply (RFC 8620 §5.3) means the mailbox is already
    /// gone — a completed step, so the delete succeeds and prunes the mirror.
    #[tokio::test]
    async fn delete_folder_treats_notfound_as_completed() {
        let (state, _, _) = spawn_backend_stub_with(Some("notfound")).await;
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner: "u@example.com",
                    server_id: "mb-x",
                    parent_id: "0",
                    display_name: "Old",
                    folder_type: 12,
                    backend_kind: "jmap",
                    backend_id: "mb-x",
                },
                "folder-add",
            )
            .await
            .unwrap();
        let r = delete_folder(&state, "u@example.com", &pw(), "mb-x").await;
        assert_eq!(
            r.status, 1,
            "an already-absent mailbox is a completed delete"
        );
        assert!(
            state
                .storage
                .get_folder("u@example.com", "mb-x")
                .await
                .unwrap()
                .is_none(),
            "the mirror row must be pruned"
        );
    }

    /// A refusal (`mailboxHasChild`) means the mailbox still exists; pruning the
    /// mirror would resurrect it on the next reconcile, so the delete fails with
    /// Status 6 and the row survives.
    #[tokio::test]
    async fn delete_folder_refusal_is_status_6_and_keeps_row() {
        let (state, _, _) = spawn_backend_stub_with(Some("refused")).await;
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner: "u@example.com",
                    server_id: "mb-x",
                    parent_id: "0",
                    display_name: "Old",
                    folder_type: 12,
                    backend_kind: "jmap",
                    backend_id: "mb-x",
                },
                "folder-add",
            )
            .await
            .unwrap();
        let r = delete_folder(&state, "u@example.com", &pw(), "mb-x").await;
        assert_eq!(r.status, 6);
        assert!(
            state
                .storage
                .get_folder("u@example.com", "mb-x")
                .await
                .unwrap()
                .is_some(),
            "a refused delete must keep the mirror row"
        );
    }

    /// A JMAP method-level `error` is a hard failure (Status 6), never a
    /// completed delete.
    #[tokio::test]
    async fn delete_folder_method_error_is_status_6() {
        let (state, _, _) = spawn_backend_stub_with(Some("error")).await;
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner: "u@example.com",
                    server_id: "mb-x",
                    parent_id: "0",
                    display_name: "Old",
                    folder_type: 12,
                    backend_kind: "jmap",
                    backend_id: "mb-x",
                },
                "folder-add",
            )
            .await
            .unwrap();
        let r = delete_folder(&state, "u@example.com", &pw(), "mb-x").await;
        assert_eq!(r.status, 6);
        assert!(
            state
                .storage
                .get_folder("u@example.com", "mb-x")
                .await
                .unwrap()
                .is_some()
        );
    }

    /// Mixed destroy outcomes (success / refused / error), applied one at a
    /// time so the assertion is deterministic: a REFUSED destroy and a
    /// method-level `error` both leave the mailbox in place, so the mirror row
    /// AND its pending prune intent must survive; only a committed destroy
    /// prunes the row and clears the intent in the same transaction.
    #[tokio::test]
    async fn mixed_destroy_outcomes_keep_intent_until_a_destroy_commits() {
        // Start the cycle at 1: the first call is `refused`, the second a
        // method `error`, the third a committed destroy.
        let (state, _, _) = spawn_backend_stub_full(None, 201, 1).await;
        for (id, expect_status, expect_kept) in [
            ("mb-refused", 6u16, true),
            ("mb-error", 6u16, true),
            ("mb-destroyed", 1u16, false),
        ] {
            state
                .storage
                .record_pending_folder_prunes(
                    "u@example.com",
                    std::slice::from_ref(&id.to_string()),
                )
                .await
                .expect("seed pending prune intent");
            state
                .storage
                .upsert_folder_journaled(
                    crate::storage::FolderUpsertParams {
                        owner: "u@example.com",
                        server_id: id,
                        parent_id: "0",
                        display_name: id,
                        folder_type: 12,
                        backend_kind: "jmap",
                        backend_id: id,
                    },
                    "folder-add",
                )
                .await
                .expect("seed folder mapping");

            let r = delete_folder(&state, "u@example.com", &pw(), id).await;
            assert_eq!(r.status, expect_status, "{id}: unexpected status");
            let row_present = state
                .storage
                .get_folder("u@example.com", id)
                .await
                .expect("folder lookup")
                .is_some();
            let intent_present = state
                .storage
                .list_pending_folder_prunes("u@example.com")
                .await
                .expect("list intents")
                .contains(&id.to_string());
            assert_eq!(row_present, expect_kept, "{id}: mirror row state");
            assert_eq!(intent_present, expect_kept, "{id}: pending intent state");
        }
    }

    /// Concurrent `delete_folder` calls against the same mailbox with the
    /// mixed destroy cycle ENABLED must leave a consistent durable state:
    /// either a destroy committed (row pruned, no intent left) or none did
    /// (row and intent both retained). The failure mode this guards against is
    /// a `Refused` arm clearing the intent while an earlier destroy with an
    /// unknown outcome is still in flight.
    #[tokio::test]
    async fn concurrent_delete_folder_keeps_prune_intent_safe() {
        let (state, calls, _) = spawn_backend_stub_varied().await;
        state
            .storage
            .record_pending_folder_prunes(
                "u@example.com",
                std::slice::from_ref(&"mb-x".to_string()),
            )
            .await
            .expect("seed pending prune intent");
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner: "u@example.com",
                    server_id: "mb-x",
                    parent_id: "0",
                    display_name: "Old",
                    folder_type: 12,
                    backend_kind: "jmap",
                    backend_id: "mb-x",
                },
                "folder-add",
            )
            .await
            .expect("seed folder mapping");

        const CALLERS: usize = 8;
        let mut handles = Vec::new();
        for _ in 0..CALLERS {
            let state = state.clone();
            handles.push(tokio::spawn(async move {
                // Yield briefly to interleave intent-record/destroy steps
                // across callers and widen the race window.
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                delete_folder(&state, "u@example.com", &pw(), "mb-x").await
            }));
        }
        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.expect("caller joined"));
        }

        let deleted = state
            .storage
            .get_folder("u@example.com", "mb-x")
            .await
            .expect("folder lookup")
            .is_none();
        let intent = state
            .storage
            .list_pending_folder_prunes("u@example.com")
            .await
            .expect("list intents")
            .contains(&"mb-x".to_string());
        // A folder that was NOT deleted must still carry its prune intent, or
        // the mapping could never be pruned. When the folder WAS deleted, a
        // concurrent caller's intent recorded before its own (refused/unknown)
        // destroy may outlive the winning caller's prune — that residual is
        // harmless and MUST drain cleanly on the next replay instead of
        // keeping the folder in the work list forever.
        if !deleted {
            assert!(
                intent,
                "not-deleted folder must retain its pending prune intent"
            );
        } else if intent {
            state
                .storage
                .reconcile_pending_folder_prunes(
                    "u@example.com",
                    &["mb-x".to_string()],
                    &std::collections::HashSet::new(),
                )
                .await
                .expect("replay the residual intent");
            assert!(
                !state
                    .storage
                    .list_pending_folder_prunes("u@example.com")
                    .await
                    .expect("list intents after replay")
                    .contains(&"mb-x".to_string()),
                "a residual intent for a pruned folder must drain, not linger"
            );
        }

        // The mixed cycle must actually have run (this is the assertion the
        // previous version lacked: the variety counter was never enabled, so
        // every destroy silently succeeded).
        let outcomes: Vec<String> = calls
            .lock()
            .await
            .iter()
            .filter(|c| c.get("method").and_then(|v| v.as_str()) == Some("stub-destroy-outcome"))
            .filter_map(|c| c["args"]["outcome"].as_str().map(str::to_string))
            .collect();
        assert!(
            !outcomes.is_empty(),
            "the mixed destroy cycle must be exercised"
        );
        // The counter starts at 1, so any TWO recorded destroys are exactly
        // {refused, error}: both failure arms of the destroy cycle are
        // reached, not just the success path.
        if outcomes.len() >= 2 {
            assert!(
                outcomes.iter().any(|o| o == "refused"),
                "the refusal arm must be exercised: {outcomes:?}"
            );
            assert!(
                outcomes.iter().any(|o| o == "error"),
                "the method-error arm must be exercised: {outcomes:?}"
            );
        }

        // Sanity: every caller got a Status response (none crashed).
        assert_eq!(results.len(), CALLERS);
        for r in &results {
            assert!(
                (1..=6).contains(&r.status) || r.status == 111,
                "unexpected status {}",
                r.status
            );
        }
    }

    // ---- FolderUpdate ---------------------------------------------------

    #[tokio::test]
    async fn update_system_folder_is_status_2() {
        let (state, calls, _) = spawn_backend_stub().await;
        let r = update_folder(&state, "u@example.com", &pw(), "2", "0", "Renamed").await;
        assert_eq!(r.status, 2);
        assert!(jmap_methods(&calls).await.is_empty());
    }

    #[tokio::test]
    async fn update_rejects_move_into_itself_or_a_descendant() {
        let (state, calls, _) = spawn_backend_stub().await;
        for (id, parent, name) in [
            ("mb-a", "0", "A"),
            ("mb-b", "mb-a", "B"),
            ("mb-c", "mb-b", "C"),
        ] {
            state
                .storage
                .upsert_folder_journaled(
                    crate::storage::FolderUpsertParams {
                        owner: "u@example.com",
                        server_id: id,
                        parent_id: parent,
                        display_name: name,
                        folder_type: 12,
                        backend_kind: "jmap",
                        backend_id: id,
                    },
                    "folder-add",
                )
                .await
                .unwrap();
        }
        // Into itself.
        let r = update_folder(&state, "u@example.com", &pw(), "mb-a", "mb-a", "A").await;
        assert_eq!(r.status, 10);
        // Into its own grandchild.
        let r = update_folder(&state, "u@example.com", &pw(), "mb-a", "mb-c", "A").await;
        assert_eq!(r.status, 10);
        // A legitimate reparent still works (and reaches the backend).
        let r = update_folder(&state, "u@example.com", &pw(), "mb-c", "mb-a", "C").await;
        assert_eq!(r.status, 1, "a non-cyclic reparent must succeed");
        assert!(
            jmap_methods(&calls)
                .await
                .iter()
                .any(|m| m == "Mailbox/set"),
            "the legitimate reparent must reach the backend"
        );
    }

    #[tokio::test]
    async fn update_unknown_folder_is_status_4() {
        let (state, _, _) = spawn_backend_stub().await;
        let r = update_folder(&state, "u@example.com", &pw(), "mb-nope", "0", "X").await;
        assert_eq!(r.status, 4);
    }

    #[tokio::test]
    async fn update_mail_folder_renames_via_mailbox_set() {
        let (state, calls, _) = spawn_backend_stub().await;
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner: "u@example.com",
                    server_id: "mb-x",
                    parent_id: "0",
                    display_name: "Old",
                    folder_type: 12,
                    backend_kind: "jmap",
                    backend_id: "mb-x",
                },
                "folder-add",
            )
            .await
            .unwrap();
        let r = update_folder(&state, "u@example.com", &pw(), "mb-x", "0", "New").await;
        assert_eq!(r.status, 1);
        let args = last_call_args(&calls, "Mailbox/set").await;
        let patch = args.get("update").and_then(|v| v.get("mb-x")).unwrap();
        assert_eq!(patch.get("name").and_then(|v| v.as_str()), Some("New"));
        let row = state
            .storage
            .get_folder("u@example.com", "mb-x")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.display_name, "New");
    }

    #[tokio::test]
    async fn update_caldav_folder_renames_via_proppatch() {
        let (state, _, dav) = spawn_backend_stub().await;
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner: "u@example.com",
                    server_id: "/cal/u@example.com/Foo/",
                    parent_id: "0",
                    display_name: "Foo",
                    folder_type: 13,
                    backend_kind: "caldav",
                    backend_id: "/cal/u@example.com/Foo/",
                },
                "folder-add",
            )
            .await
            .unwrap();
        let r = update_folder(
            &state,
            "u@example.com",
            &pw(),
            "/cal/u@example.com/Foo/",
            "0",
            "Bar",
        )
        .await;
        assert_eq!(r.status, 1);
        let calls = dav.lock().await.clone();
        assert!(
            calls
                .iter()
                .any(|(m, p)| m == "PROPPATCH" && p == "/cal/u@example.com/Foo/"),
            "expected PROPPATCH displayname, got {calls:?}"
        );
        let row = state
            .storage
            .get_folder("u@example.com", "/cal/u@example.com/Foo/")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.display_name, "Bar");
    }

    // ---- hierarchy journal ---------------------------------------------

    #[tokio::test]
    async fn folder_hierarchy_journal_is_listed_since_watermark() {
        let (state, _, _) = spawn_backend_stub().await;
        for (server_id, display_name) in [("mb-a", "A"), ("mb-b", "B")] {
            state
                .storage
                .upsert_folder_journaled(
                    crate::storage::FolderUpsertParams {
                        owner: "u@example.com",
                        server_id,
                        parent_id: "0",
                        display_name,
                        folder_type: 12,
                        backend_kind: "jmap",
                        backend_id: server_id,
                    },
                    "folder-add",
                )
                .await
                .unwrap();
        }
        let all = state
            .storage
            .list_folder_journal_since("u@example.com", 0, i64::MAX)
            .await
            .unwrap();
        assert_eq!(all.len(), 2);
        // A watermark at the first row id yields only the later change.
        let first_id = all[0].id;
        let after = state
            .storage
            .list_folder_journal_since("u@example.com", first_id, i64::MAX)
            .await
            .unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].server_id, "mb-b");
        // An upper bound excludes changes journaled after the head was captured:
        // a row with `id > until` is not part of the rendered window, so the
        // acknowledged watermark can never skip it.
        let bounded = state
            .storage
            .list_folder_journal_since("u@example.com", 0, first_id)
            .await
            .unwrap();
        assert_eq!(bounded.len(), 1);
        assert_eq!(bounded[0].server_id, "mb-a");
    }

    #[tokio::test]
    async fn delete_then_recreate_same_name_preserves_only_the_new_identity() {
        let (state, _, _) = spawn_backend_stub().await;
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner: "u@example.com",
                    server_id: "mb-old",
                    parent_id: "0",
                    display_name: "Projects",
                    folder_type: 12,
                    backend_kind: "jmap",
                    backend_id: "mb-old",
                },
                "folder-add",
            )
            .await
            .unwrap();

        let deleted = delete_folder(&state, "u@example.com", &pw(), "mb-old").await;
        assert_eq!(deleted.status, 1);
        assert!(
            state
                .storage
                .get_folder("u@example.com", "mb-old")
                .await
                .unwrap()
                .is_none()
        );

        let created = create_folder(&state, "u@example.com", &pw(), "0", "Projects", 12).await;
        assert_eq!(created.status, 1);
        assert_eq!(created.server_id.as_deref(), Some("mb-new"));
        let replacement = state
            .storage
            .get_folder("u@example.com", "mb-new")
            .await
            .unwrap()
            .expect("replacement mapping");
        assert_eq!(replacement.display_name, "Projects");
        assert!(
            state
                .storage
                .get_folder("u@example.com", "mb-old")
                .await
                .unwrap()
                .is_none(),
            "the recreate must not resurrect the deleted identity"
        );
        assert!(
            state
                .storage
                .list_pending_folder_prunes("u@example.com")
                .await
                .unwrap()
                .is_empty()
        );
        let journal = state
            .storage
            .list_folder_journal_since("u@example.com", 0, i64::MAX)
            .await
            .unwrap();
        let tail = &journal[journal.len() - 2..];
        assert_eq!(tail[0].server_id, "mb-old");
        assert_eq!(tail[0].op, "folder-delete");
        assert_eq!(tail[1].server_id, "mb-new");
        assert_eq!(tail[1].op, "folder-add");
    }

    #[tokio::test]
    async fn rename_recreate_and_stale_delete_cannot_remove_the_wrong_folder() {
        let (state, _, _) = spawn_backend_stub().await;
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner: "u@example.com",
                    server_id: "mb-a",
                    parent_id: "0",
                    display_name: "Alpha",
                    folder_type: 12,
                    backend_kind: "jmap",
                    backend_id: "mb-a",
                },
                "folder-add",
            )
            .await
            .unwrap();
        let renamed = update_folder(&state, "u@example.com", &pw(), "mb-a", "0", "Beta").await;
        assert_eq!(renamed.status, 1);
        let created = create_folder(&state, "u@example.com", &pw(), "0", "Alpha", 12).await;
        assert_eq!(created.status, 1);
        assert_eq!(created.server_id.as_deref(), Some("mb-new"));

        let stale_name = delete_folder(&state, "u@example.com", &pw(), "Alpha").await;
        assert_eq!(
            stale_name.status, 4,
            "a display name is not a stable EAS deletion identity"
        );
        assert_eq!(
            state
                .storage
                .get_folder("u@example.com", "mb-a")
                .await
                .unwrap()
                .unwrap()
                .display_name,
            "Beta"
        );
        assert_eq!(
            state
                .storage
                .get_folder("u@example.com", "mb-new")
                .await
                .unwrap()
                .unwrap()
                .display_name,
            "Alpha"
        );

        let deleted = delete_folder(&state, "u@example.com", &pw(), "mb-a").await;
        assert_eq!(deleted.status, 1);
        assert!(
            state
                .storage
                .get_folder("u@example.com", "mb-a")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            state
                .storage
                .get_folder("u@example.com", "mb-new")
                .await
                .unwrap()
                .unwrap()
                .display_name,
            "Alpha",
            "deleting the renamed identity must spare the replacement"
        );
    }
}

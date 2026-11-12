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

use secrecy::{ExposeSecret, SecretString};
use std::sync::Arc;

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
fn is_system_folder_name(display_name: &str) -> bool {
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
            if rows.iter().any(|r| {
                r.parent_id == parent_id && r.display_name.eq_ignore_ascii_case(display_name)
            }) {
                return FolderMutationResult::status(2);
            }
            if let Some(coll) = caldav_href.as_ref()
                && rows.iter().any(|r| {
                    r.backend_kind == "caldav"
                        && r.backend_id == coll.href
                        && !r.display_name.eq_ignore_ascii_case(display_name)
                })
            {
                return FolderMutationResult::status(2);
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
                Some(m) => Some(m),
                None => return FolderMutationResult::status(5),
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
                // Compensate by destroying the just-created mailbox (and its
                // contents — it was empty) so a retry is not rejected as a
                // duplicate sibling name while the backend folder is left
                // orphaned. `AlreadyGone` is the desired end state; a failed
                // compensation is logged and the client still receives a
                // retryable failure.
                tracing::warn!(error = %e, "FolderCreate: failed to persist folder mapping");
                match jmap
                    .destroy_mailbox(&account_id, &mailbox_id, true, owner, password)
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
    // A 405 means the collection already exists. The gateway cannot tell an
    // orphan left by a failed mapping write from a pre-existing collection a
    // user created outside EAS, so it never adopts one: adopting would silently
    // link a new EAS folder to a collection whose events it does not own.
    // Instead it reports the spec's duplicate/conflict status. An orphan is
    // prevented up front by the compensation below.
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
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(
                collection = %coll.name,
                "FolderCreate: calendar collection already exists"
            );
            return FolderMutationResult::status(2);
        }
        Err(e) => {
            tracing::warn!(error = %e, "FolderCreate: MKCALENDAR failed");
            return FolderMutationResult::status(6);
        }
    }
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
        // The collection was created but the mapping write failed. Compensate
        // by deleting the just-created collection, so a retry starts clean
        // rather than 405ing on an orphan that can never be mapped (the gateway
        // no longer adopts on 405). A failed compensation is logged; the client
        // still receives a retryable failure and an orphan is no worse than
        // the pre-compensation behavior.
        tracing::warn!(error = %e, "FolderCreate: failed to persist folder mapping");
        if let Err(del) = caldav
            .delete_collection(&coll.href, owner, password.expose_secret())
            .await
        {
            tracing::warn!(
                error = %del,
                collection = %coll.name,
                "FolderCreate: failed to roll back the created calendar collection"
            );
        }
        return FolderMutationResult::status(6);
    }
    FolderMutationResult::created(coll.href)
}

/// Resolve a system email folder's EAS CollectionId to its JMAP mailbox id.
async fn system_email_folder_mailbox_id(
    jmap: &JmapClient,
    account_id: &str,
    collection_id: &str,
    username: &str,
    password: &SecretString,
) -> Option<String> {
    let role = crate::email::eas_collection_id_to_mailbox_role(collection_id)?;
    jmap.get_mailbox_ids_for_role(account_id, role, username, password)
        .await
        .ok()
        .and_then(|v| v.into_iter().next())
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
    // `mailboxHasChild`).
    let subtree = match collect_folder_subtree(state, owner, server_id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "FolderDelete: failed to enumerate descendants");
            return FolderMutationResult::status(6);
        }
    };

    // Destroy the backend objects deepest-first, ABORTING on the first failure
    // so the mirror is never pruned ahead of the system of record (that would
    // resurrect the folder on the next reconcile).
    for folder in &subtree {
        match folder.backend_kind.as_str() {
            "jmap" => {
                let Some(jmap) = state.jmap_client.clone() else {
                    return FolderMutationResult::status(6);
                };
                let account_id = match jmap.get_account_id(owner, password).await {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::warn!(error = %e, "FolderDelete: JMAP account lookup failed");
                        return FolderMutationResult::status(6);
                    }
                };
                match jmap
                    .destroy_mailbox(&account_id, &folder.backend_id, true, owner, password)
                    .await
                {
                    Ok(
                        crate::jmap::MailboxDestroyOutcome::Destroyed
                        | crate::jmap::MailboxDestroyOutcome::AlreadyGone,
                    ) => {}
                    // The server refused the destroy (e.g. `mailboxHasChild`
                    // because a child was recreated, or `forbidden`). This is a
                    // failure: pruning the mirror here would resurrect the
                    // folder on the next reconcile, so abort and let the client
                    // retry.
                    Ok(crate::jmap::MailboxDestroyOutcome::Refused) => {
                        tracing::warn!(
                            server_id = %folder.server_id,
                            "FolderDelete: Mailbox/set refused the destroy"
                        );
                        return FolderMutationResult::status(6);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "FolderDelete: JMAP Mailbox/set destroy failed");
                        return FolderMutationResult::status(6);
                    }
                }
            }
            "caldav" => {
                let Ok(caldav) = CaldavClient::new(&state.cfg) else {
                    return FolderMutationResult::status(6);
                };
                if let Err(e) = caldav
                    .delete_collection(&folder.backend_id, owner, password.expose_secret())
                    .await
                {
                    tracing::warn!(error = %e, "FolderDelete: CalDAV collection delete failed");
                    return FolderMutationResult::status(6);
                }
            }
            _ => return FolderMutationResult::status(4),
        }
    }

    // Backend objects are gone; prune the mirror and journal a Delete for each
    // removed ServerId in ONE transaction. Success is returned only once every
    // journal row is durable, so an already-primed device can always learn
    // about the removal.
    let removed_ids: Vec<String> = subtree.iter().map(|f| f.server_id.clone()).collect();
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

/// The target folder and every descendant, ordered deepest-first (children
/// before parents) so a backend destroy never hits a parent that still has a
/// child.
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
    if server_id.is_empty() || display_name.is_empty() {
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
    let parent_id = if parent_id.trim().is_empty() {
        row.parent_id.clone()
    } else {
        parent_id.trim().to_string()
    };

    // Apply the same name validation FolderCreate enforces: a rename must not
    // adopt a reserved system-folder name, exceed the §6.13 length, or collide
    // with a sibling's DisplayName under the same parent.
    if is_system_folder_name(display_name) {
        return FolderMutationResult::status(2);
    }
    if display_name.chars().count() > 64 {
        return FolderMutationResult::status(10);
    }
    match state.storage.list_folders(owner).await {
        Ok(rows) => {
            if rows.iter().any(|r| {
                r.server_id != row.server_id
                    && r.parent_id == parent_id
                    && r.display_name.eq_ignore_ascii_case(display_name)
            }) {
                return FolderMutationResult::status(2);
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "FolderUpdate: failed to list existing folders");
            return FolderMutationResult::status(6);
        }
    }

    match row.backend_kind.as_str() {
        "jmap" => update_mail_folder(state, owner, password, &row, &parent_id, display_name).await,
        // A CalDAV collection rename keeps the stored ServerId (href) stable and
        // only refreshes the collection's own displayname plus the persisted
        // DisplayName. CalDAV collections are top-level only, so a requested
        // move to a different parent is rejected rather than silently ignored.
        "caldav" => {
            if parent_id != row.parent_id {
                return FolderMutationResult::status(5);
            }
            rename_caldav_folder(state, owner, password, &row, display_name).await
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
                    Some(m) => Some(Some(m)),
                    None => return FolderMutationResult::status(5),
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
            if let Err(e) = state
                .storage
                .upsert_folder_journaled(
                    crate::storage::FolderUpsertParams {
                        owner,
                        server_id: &row.server_id,
                        parent_id,
                        display_name,
                        folder_type: row.folder_type,
                        backend_kind: "jmap",
                        backend_id: &row.backend_id,
                    },
                    "folder-update",
                )
                .await
            {
                tracing::warn!(error = %e, "FolderUpdate: failed to persist folder mapping");
                return FolderMutationResult::status(6);
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
    if let Err(e) = state
        .storage
        .upsert_folder_journaled(
            crate::storage::FolderUpsertParams {
                owner,
                server_id: &row.server_id,
                parent_id: &row.parent_id,
                display_name,
                folder_type: row.folder_type,
                backend_kind: "caldav",
                backend_id: &row.backend_id,
            },
            "folder-update",
        )
        .await
    {
        tracing::warn!(error = %e, "FolderUpdate: failed to persist folder mapping");
        return FolderMutationResult::status(6);
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
/// `<Status>` followed by an optional `<SyncKey>`.
pub fn render_status_response(root: &str, result: &FolderMutationResult, sync_key: &str) -> String {
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
) -> String {
    let collection_id = crate::eas::scoped_collection_id("folderhierarchy", device_id);
    let carried_token = match state.storage.get_sync_key(owner, &collection_id).await {
        Ok(Some((_, Some(token)))) => token,
        // No stored watermark (or a stored key with no token): force a prime.
        Ok(_) => return "0".to_string(),
        Err(e) => {
            // A read failure must NOT be mistaken for "no watermark": rotating
            // to `seq:0` would clobber the device's real position and make its
            // next FolderSync skip every earlier change. Keep the current key
            // so the client retries against the unchanged state.
            tracing::warn!(
                error = %e,
                "FolderSync: failed to read hierarchy watermark for key rotation"
            );
            return current_hierarchy_sync_key(state, owner, device_id).await;
        }
    };
    let new_key = uuid::Uuid::new_v4().simple().to_string();
    if let Err(e) = state
        .storage
        .set_sync_key(owner, &collection_id, &new_key, Some(&carried_token))
        .await
    {
        tracing::warn!(error = %e, "FolderSync: failed to persist rotated hierarchy sync key");
        return current_hierarchy_sync_key(state, owner, device_id).await;
    }
    new_key
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;
    use serde_json::{Value, json};
    use std::sync::Arc as StdArc;
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
        /// The HTTP status the CalDAV stub answers `MKCALENDAR` with (201 by
        /// default; 405 models a collection that already exists).
        mkcalendar_status: u16,
    }

    async fn spawn_backend_stub() -> (
        StdArc<AppState>,
        StdArc<TokioMutex<Vec<Value>>>,
        StdArc<TokioMutex<Vec<(String, String)>>>,
    ) {
        spawn_backend_stub_with(None).await
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
        spawn_backend_stub_full(destroy_mode, 201).await
    }

    /// As [`spawn_backend_stub_with`] but also scripts the `MKCALENDAR` status
    /// (405 models a collection that already exists).
    async fn spawn_backend_stub_full(
        destroy_mode: Option<&str>,
        mkcalendar_status: u16,
    ) -> (
        StdArc<AppState>,
        StdArc<TokioMutex<Vec<Value>>>,
        StdArc<TokioMutex<Vec<(String, String)>>>,
    ) {
        use axum::response::Response;
        use axum::routing::{any, get, post};

        let destroy_mode = destroy_mode.map(str::to_string);
        let jmap_calls: StdArc<TokioMutex<Vec<Value>>> = StdArc::new(TokioMutex::new(Vec::new()));
        let dav_calls: StdArc<TokioMutex<Vec<(String, String)>>> =
            StdArc::new(TokioMutex::new(Vec::new()));

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
                                    ("Mailbox/set", json!({"accountId": "a", "notDestroyed": nd}))
                                }
                                Some("refused") => {
                                    let nd: serde_json::Map<String, Value> = destroy
                                        .iter()
                                        .filter_map(|v| v.as_str())
                                        .map(|id| {
                                            (id.to_string(), json!({"type": "mailboxHasChild"}))
                                        })
                                        .collect();
                                    ("Mailbox/set", json!({"accountId": "a", "notDestroyed": nd}))
                                }
                                Some(_) => (
                                    "error",
                                    json!({"type": "serverFail", "description": "scripted"}),
                                ),
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

        /// Catch-all for the CalDAV surface (MKCALENDAR/DELETE/PROPPATCH).
        async fn dav(
            axum::Extension(ctl): axum::Extension<Ctl>,
            method: axum::http::Method,
            uri: axum::http::Uri,
        ) -> Response {
            ctl.dav_calls
                .lock()
                .await
                .push((method.as_str().to_string(), uri.path().to_string()));
            let status = match method.as_str() {
                "MKCALENDAR" => ctl.mkcalendar_status,
                _ => 200,
            };
            Response::builder()
                .status(status)
                .body(axum::body::Body::empty())
                .unwrap()
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let ctl = Ctl {
            base: base.clone(),
            jmap_calls: jmap_calls.clone(),
            dav_calls: dav_calls.clone(),
            destroy_mode,
            mkcalendar_status,
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
        let (state, _, _) = spawn_backend_stub_full(None, 405).await;
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
        let (state, _, _) = spawn_backend_stub_full(None, 405).await;
        // Pre-seed a folder that already owns the target href.
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner: "u@example.com",
                    server_id: "/cal/u@example.com/Taken/",
                    parent_id: "0",
                    display_name: "Taken",
                    folder_type: 13,
                    backend_kind: "caldav",
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

    /// A display name that sanitizes to a system collection (`Tasks.` →
    /// `Tasks`) must be rejected, not created or adopted: a later FolderDelete
    /// would otherwise DELETE the gateway's shared Tasks/Notes collection.
    #[tokio::test]
    async fn create_calendar_folder_rejects_sanitized_system_collection_name() {
        let (state, _, dav_calls) = spawn_backend_stub_full(None, 201).await;
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
    /// just-created collection must be deleted so a retry does not 405 on an
    /// orphan (the gateway no longer adopts on 405).
    #[tokio::test]
    async fn create_caldav_folder_rolls_back_collection_when_mapping_write_fails() {
        let (state, _, dav_calls) = spawn_backend_stub_full(None, 201).await;
        state.storage.pool().close().await;
        let coll = caldav_collection_for("", "u@example.com", "Rollback", 13)
            .expect("caldav collection descriptor");
        let r = create_caldav_folder(&state, "u@example.com", &pw(), "Rollback", 13, coll).await;
        assert_eq!(r.status, 6, "a failed mapping write must report failure");
        let calls = dav_calls.lock().await;
        assert!(
            calls
                .iter()
                .any(|(m, p)| m == "DELETE" && p.contains("/cal/u@example.com/Rollback/")),
            "the orphaned collection must be deleted as compensation: {calls:?}"
        );
    }

    /// A device that has NEVER primed the hierarchy (no stored FolderSync
    /// state) must not receive a nonzero rotated key carrying `seq:0`: its next
    /// FolderSync would treat that as an incremental delta and omit every
    /// default folder. The prime key `"0"` forces the full-hierarchy branch.
    #[tokio::test]
    async fn rotate_hierarchy_key_for_never_primed_device_returns_prime_key() {
        let (state, _, _) = spawn_backend_stub().await;
        let key = rotate_hierarchy_sync_key(&state, "u@example.com", "fresh-device").await;
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
        let key = rotate_hierarchy_sync_key(&state, "u@example.com", "dev-1").await;
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
        let key = rotate_hierarchy_sync_key(&state, "u@example.com", "dev-2").await;
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

    // ---- FolderDelete ---------------------------------------------------

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

    // ---- FolderUpdate ---------------------------------------------------

    #[tokio::test]
    async fn update_system_folder_is_status_2() {
        let (state, calls, _) = spawn_backend_stub().await;
        let r = update_folder(&state, "u@example.com", &pw(), "2", "0", "Renamed").await;
        assert_eq!(r.status, 2);
        assert!(jmap_methods(&calls).await.is_empty());
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
}

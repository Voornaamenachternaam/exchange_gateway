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
/// ([MS-ASCMD] §2.2.3.186.2): 13 = calendar, 14 = contacts, 15 = tasks,
/// 16 = journal, 17 = notes. Contacts live on CardDAV, which the gateway
/// cannot create collections for, so they are unsupported here.
fn caldav_components_for_type(t: i64) -> Option<&'static [&'static str]> {
    match t {
        13 => Some(&["VEVENT"]),
        15 => Some(&["VTODO"]),
        16 | 17 => Some(&["VJOURNAL"]),
        _ => None,
    }
}

/// Whether an EAS user-created folder Type is unsupported by every backend the
/// gateway can write to. Type 14 (Contacts) lives on CardDAV, for which the
/// gateway cannot create collections, so `FolderCreate` must reject it rather
/// than silently backing a "Contacts" folder with a JMAP mail mailbox.
fn is_unsupported_folder_type(t: i64) -> bool {
    t == 14
}

/// Map an EAS user-created folder Type to the backend that hosts it.
fn backend_for_type(t: i64) -> FolderBackend {
    match t {
        13 | 15 | 16 | 17 => FolderBackend::Caldav,
        // 1 (generic), 12 (user mail) and anything unrecognised default to a
        // mail folder. Type 14 (Contacts) is rejected before this point.
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

/// Whether a DisplayName is already taken by a system folder (case-insensitive).
fn is_system_folder_name(display_name: &str) -> bool {
    [
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
    ]
    .iter()
    .any(|n| n.eq_ignore_ascii_case(display_name.trim()))
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
    // Contacts (14) live on CardDAV, which the gateway cannot create
    // collections for; reject rather than back a "Contacts" folder with a JMAP
    // mail mailbox.
    if is_unsupported_folder_type(folder_type) {
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
            // directly in Email/query).
            if let Err(e) = state
                .storage
                .upsert_folder(crate::storage::FolderUpsertParams {
                    owner,
                    server_id: &mailbox_id,
                    parent_id,
                    display_name,
                    folder_type,
                    backend_kind: "jmap",
                    backend_id: &mailbox_id,
                })
                .await
            {
                tracing::warn!(error = %e, "FolderCreate: failed to persist folder mapping");
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
    // A 405 means the collection already exists; adopting it as a "new" folder
    // would clobber whatever the existing collection is (or another folder's
    // mapping). Reject it as a conflict.
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
        .upsert_folder(crate::storage::FolderUpsertParams {
            owner,
            server_id: &coll.href,
            parent_id: "0",
            display_name,
            folder_type,
            backend_kind: "caldav",
            backend_id: &coll.href,
        })
        .await
    {
        tracing::warn!(error = %e, "FolderCreate: failed to persist folder mapping");
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
                    Ok(true) => {}
                    Ok(false) => {
                        tracing::warn!(
                            server_id = %folder.server_id,
                            "FolderDelete: Mailbox/set reported no destroy for the mailbox"
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

    // Backend objects are gone; prune the mirror and return the removed
    // ServerIds so the caller can journal a Delete for each one.
    let removed_ids: Vec<String> = subtree.iter().map(|f| f.server_id.clone()).collect();
    for id in &removed_ids {
        if let Err(e) = state.storage.delete_folder(owner, id).await {
            tracing::warn!(error = %e, "FolderDelete: failed to remove folder mapping");
            return FolderMutationResult::status(6);
        }
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
                .upsert_folder(crate::storage::FolderUpsertParams {
                    owner,
                    server_id: &row.server_id,
                    parent_id,
                    display_name,
                    folder_type: row.folder_type,
                    backend_kind: "jmap",
                    backend_id: &row.backend_id,
                })
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
        .upsert_folder(crate::storage::FolderUpsertParams {
            owner,
            server_id: &row.server_id,
            parent_id: &row.parent_id,
            display_name,
            folder_type: row.folder_type,
            backend_kind: "caldav",
            backend_id: &row.backend_id,
        })
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
/// folder-hierarchy SyncKey and advance its watermark to the latest journal
/// entry, so its next FolderSync (which carries that key) does not re-report the
/// change it just made. Other devices keep their own keys and watermarks and
/// still receive the change. Returns the new key; on a persistence failure the
/// previous key is returned so the client can still make progress.
pub async fn rotate_hierarchy_sync_key(
    state: &Arc<AppState>,
    owner: &str,
    device_id: &str,
) -> String {
    let collection_id = crate::eas::scoped_collection_id("folderhierarchy", device_id);
    let latest_seq = match state.storage.get_latest_change_seq().await {
        Ok(seq) => seq,
        Err(e) => {
            tracing::warn!(error = %e, "FolderSync: failed to read latest change seq for key rotation");
            return current_hierarchy_sync_key(state, owner, device_id).await;
        }
    };
    let new_key = uuid::Uuid::new_v4().simple().to_string();
    if let Err(e) = state
        .storage
        .set_sync_key(
            owner,
            &collection_id,
            &new_key,
            Some(&format!("seq:{}", latest_seq)),
        )
        .await
    {
        tracing::warn!(error = %e, "FolderSync: failed to persist rotated hierarchy sync key");
        return current_hierarchy_sync_key(state, owner, device_id).await;
    }
    new_key
}

/// Journal a folder-hierarchy change so an already-primed FolderSync reports
/// the change (rather than a stale `<Count>0</Count>`). `op` is one of
/// `folder-add`, `folder-update`, `folder-delete`.
pub async fn record_hierarchy_change(
    state: &Arc<AppState>,
    owner: &str,
    server_id: &str,
    op: &str,
) {
    if let Err(e) = state
        .storage
        .record_folder_hierarchy_change(owner, server_id, op)
        .await
    {
        tracing::warn!(error = %e, "Failed to record folder-hierarchy change");
    }
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
    }

    async fn spawn_backend_stub() -> (
        StdArc<AppState>,
        StdArc<TokioMutex<Vec<Value>>>,
        StdArc<TokioMutex<Vec<(String, String)>>>,
    ) {
        use axum::response::Response;
        use axum::routing::{any, get, post};

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
                            (
                                "Mailbox/set",
                                json!({"accountId": "a", "destroyed": destroy}),
                            )
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
                "MKCALENDAR" => 201,
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
        assert_eq!(backend_for_type(15), FolderBackend::Caldav);
        assert_eq!(backend_for_type(16), FolderBackend::Caldav);
        assert_eq!(backend_for_type(17), FolderBackend::Caldav);
        assert_eq!(backend_for_type(1), FolderBackend::Jmap);
        assert_eq!(backend_for_type(12), FolderBackend::Jmap);
        assert_eq!(backend_for_type(14), FolderBackend::Jmap);
    }

    #[test]
    fn caldav_component_sets_are_correct() {
        assert_eq!(caldav_components_for_type(13), Some(&["VEVENT"][..]));
        assert_eq!(caldav_components_for_type(15), Some(&["VTODO"][..]));
        assert_eq!(caldav_components_for_type(16), Some(&["VJOURNAL"][..]));
        assert_eq!(caldav_components_for_type(17), Some(&["VJOURNAL"][..]));
        // Contacts (CardDAV) and mail are not CalDAV collections.
        assert_eq!(caldav_components_for_type(14), None);
        assert_eq!(caldav_components_for_type(12), None);
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
            .upsert_folder(crate::storage::FolderUpsertParams {
                owner: "u@example.com",
                server_id: "mb-x",
                parent_id: "0",
                display_name: "Old",
                folder_type: 12,
                backend_kind: "jmap",
                backend_id: "mb-x",
            })
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
            .upsert_folder(crate::storage::FolderUpsertParams {
                owner: "u@example.com",
                server_id: "/cal/u@example.com/Foo/",
                parent_id: "0",
                display_name: "Foo",
                folder_type: 13,
                backend_kind: "caldav",
                backend_id: "/cal/u@example.com/Foo/",
            })
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
                .upsert_folder(crate::storage::FolderUpsertParams {
                    owner: "u@example.com",
                    server_id: id,
                    parent_id: parent,
                    display_name: id,
                    folder_type: 12,
                    backend_kind: "jmap",
                    backend_id: id,
                })
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
            .upsert_folder(crate::storage::FolderUpsertParams {
                owner: "u@example.com",
                server_id: "mb-x",
                parent_id: "0",
                display_name: "Old",
                folder_type: 12,
                backend_kind: "jmap",
                backend_id: "mb-x",
            })
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
            .upsert_folder(crate::storage::FolderUpsertParams {
                owner: "u@example.com",
                server_id: "/cal/u@example.com/Foo/",
                parent_id: "0",
                display_name: "Foo",
                folder_type: 13,
                backend_kind: "caldav",
                backend_id: "/cal/u@example.com/Foo/",
            })
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
    async fn record_hierarchy_change_is_listed_since_watermark() {
        let (state, _, _) = spawn_backend_stub().await;
        record_hierarchy_change(&state, "u@example.com", "mb-a", "folder-add").await;
        record_hierarchy_change(&state, "u@example.com", "mb-b", "folder-add").await;
        let all = state
            .storage
            .list_folder_journal_since("u@example.com", 0)
            .await
            .unwrap();
        assert_eq!(all.len(), 2);
        // A watermark at the first row id yields only the later change.
        let first_id = all[0].id;
        let after = state
            .storage
            .list_folder_journal_since("u@example.com", first_id)
            .await
            .unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].server_id, "mb-b");
    }
}

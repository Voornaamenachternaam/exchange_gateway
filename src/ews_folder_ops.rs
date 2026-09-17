//! EWS folder-management and user-configuration operations.
//!
//! Implements the folder-mutation and folder/config operations New Outlook
//! issues over EWS: `CreateFolder`, `UpdateFolder`, `MoveFolder`,
//! `CopyFolder`, `DeleteFolder`, `EmptyFolder`, `MarkAllItemsAsRead`,
//! `FindConversation`, `ExpandDL`, `GetUserRetentionPolicyTags`,
//! `GetSearchableMailboxes`, `GetSharingMetadata`, `GetSharingFolder`,
//! `RefreshSharingFolder`, `GetMessageTrackingReport`,
//! `SetUserConfiguration`, `UpdateUserConfiguration`,
//! `DeleteUserConfiguration`, `UploadItems` and `ExportItems`.
//!
//! Folder mutations are proxied to the Stalwart JMAP backend via
//! `Mailbox/set` (create/update/destroy) and `Email/set` (`copyFrom` for
//! copy, `destroy` for empty, `$seen` keyword for mark-read). This mirrors
//! the MAPI/HTTP folder ROP handlers in `src/mapi/handler.rs`, so both
//! protocols present and mutate the same folder tree.
//!
//! User-configuration objects (MS-OXWSUSRCFG) persist in the gateway's
//! local SQLite `user_config` table; `GetUserConfiguration` in `ews.rs`
//! consults the same store first, keeping the trio coherent.

use axum::{http::StatusCode, response::Response};
use secrecy::SecretString;
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Arc;

use crate::ews::{AuthContext, EwsAction, operation_error_response, owner_from_username, soap_ok};
use crate::ews_folders::{DistinguishedFolder, resolve_folder_id};
use crate::jmap::JmapClient;
use crate::models::AppState;
use crate::protocol_fixtures::{EWS_MSG_NS, EWS_TYPE_NS};
use crate::util::xml_escape;

// ---------------------------------------------------------------------------
// Small XML helpers (quick-xml event based, namespace-prefix agnostic)
// ---------------------------------------------------------------------------

/// First `attr` attribute value of the first element whose local name is `tag`.
pub(crate) fn first_attr(xml: &str, tag: &str, attr: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    doc.descendants()
        .find(|n| n.is_element() && n.tag_name().name() == tag)
        .and_then(|n| n.attribute(attr))
        .map(|s| s.to_string())
}

/// All `attr` attribute values of elements whose local name matches `tag`,
/// scoped to the first element named `container` when `container` is `Some`.
fn all_attrs(xml: &str, container: Option<&str>, tag: &str, attr: &str) -> Vec<String> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return Vec::new();
    };
    let iter: Box<dyn Iterator<Item = roxmltree::Node<'_, '_>> + '_> = match container {
        Some(c) => {
            let Some(cont) = doc
                .descendants()
                .find(|n| n.is_element() && n.tag_name().name() == c)
            else {
                return Vec::new();
            };
            Box::new(cont.descendants())
        }
        None => Box::new(doc.descendants()),
    };
    iter.filter(|n| n.is_element() && n.tag_name().name() == tag)
        .filter_map(|n| n.attribute(attr).map(|s| s.to_string()))
        .collect()
}

// ---------------------------------------------------------------------------
// Folder-reference resolution
// ---------------------------------------------------------------------------

/// Result of resolving an EWS folder reference to a JMAP mailbox.
enum MailboxTarget {
    /// JMAP mailbox id, usable directly in JMAP calls.
    Id(String),
    /// Folder has no JMAP mailbox behind it (MsgFolderRoot and non-mail
    /// folders). Folder-scoped mail operations cannot run against it.
    NotAMailFolder,
    /// The reference could not be resolved at all (no such folder).
    NotFound,
}

/// Map a `DistinguishedFolder` to its JMAP mailbox role, if it is a mail
/// folder. `MsgFolderRoot`/`Outbox` and non-mail folders have no mapping.
fn role_for_folder(f: DistinguishedFolder) -> Option<&'static str> {
    match f {
        DistinguishedFolder::Inbox => Some("inbox"),
        DistinguishedFolder::SentItems | DistinguishedFolder::Outbox => Some("sent"),
        DistinguishedFolder::DeletedItems => Some("trash"),
        DistinguishedFolder::Drafts => Some("drafts"),
        DistinguishedFolder::JunkEmail => Some("junk"),
        // MsgFolderRoot / Calendar / Contacts / Tasks / Notes / Journal
        _ => None,
    }
}

/// Resolve one folder reference. `explicit_id` is the `Id` attribute of a
/// `<t:FolderId>` element; `distinguished_id` the `Id` attribute of a
/// `<t:DistinguishedFolderId>` element. Exactly one is normally `Some`.
async fn resolve_mailbox_target(
    jmap: &JmapClient,
    account_id: &str,
    owner: &str,
    explicit_id: Option<&str>,
    distinguished_id: Option<&str>,
    username: &str,
    password: &SecretString,
) -> MailboxTarget {
    // 1. DistinguishedFolderId.
    if let Some(d) = distinguished_id {
        let parsed: Result<DistinguishedFolder, _> = d.parse();
        let Ok(df) = parsed else {
            return MailboxTarget::NotFound;
        };
        return match role_for_folder(df) {
            None => MailboxTarget::NotAMailFolder,
            Some(role) => match jmap
                .get_mailbox_ids_for_role(account_id, role, username, password)
                .await
            {
                Ok(ids) => match ids.into_iter().next() {
                    Some(id) => MailboxTarget::Id(id),
                    None => MailboxTarget::NotFound,
                },
                Err(e) => {
                    tracing::warn!(error = %e, role = %role, "Mailbox/query by role failed");
                    MailboxTarget::NotFound
                }
            },
        };
    }

    // 2. Explicit FolderId.
    let Some(id) = explicit_id else {
        return MailboxTarget::NotFound;
    };
    match resolve_folder_id(id, owner) {
        // Synthetic id emitted by `folder_id_for` for a distinguished folder.
        Some(df) => match role_for_folder(df) {
            None => MailboxTarget::NotAMailFolder,
            Some(role) => match jmap
                .get_mailbox_ids_for_role(account_id, role, username, password)
                .await
            {
                Ok(ids) => match ids.into_iter().next() {
                    Some(m) => MailboxTarget::Id(m),
                    None => MailboxTarget::NotFound,
                },
                Err(e) => {
                    tracing::warn!(error = %e, role = %role, "Mailbox/query by role failed");
                    MailboxTarget::NotFound
                }
            },
        },
        // Anything else is treated as a native JMAP mailbox id (this is how
        // user-created folders are referenced, symmetric with MAPI).
        None => MailboxTarget::Id(id.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Shared plumbing
// ---------------------------------------------------------------------------

/// Get the JMAP client + account id, or an error response for `action`.
async fn jmap_account(
    state: &Arc<AppState>,
    auth: &AuthContext,
    action: EwsAction,
) -> Result<(Arc<JmapClient>, String), Response> {
    let jmap = match state.jmap_client.clone() {
        Some(j) => j,
        None => {
            return Err(operation_error_response(
                &action,
                "ErrorInternalServerError",
                "JMAP backend not configured",
                StatusCode::INTERNAL_SERVER_ERROR,
            ));
        }
    };
    let account_id = match jmap.get_account_id(&auth.username, &auth.password).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "JMAP account lookup failed");
            return Err(operation_error_response(
                &action,
                "ErrorInternalServerError",
                "Could not resolve the mailbox account",
                StatusCode::INTERNAL_SERVER_ERROR,
            ));
        }
    };
    Ok((jmap, account_id))
}

fn internal_error(action: EwsAction, ctx: &str, e: &dyn std::fmt::Display) -> Response {
    tracing::error!(error = %e, context = %ctx, "EWS operation failed");
    operation_error_response(
        &action,
        "ErrorInternalServerError",
        ctx,
        StatusCode::INTERNAL_SERVER_ERROR,
    )
}

/// `<m:{action}Response><m:ResponseMessages><m:{action}ResponseMessage
/// ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode>{inner}`
fn success_response(action: &str, inner: &str) -> Response {
    soap_ok(format!(
        "<m:{a}Response xmlns:m=\"{m}\" xmlns:t=\"{t}\">\
         <m:ResponseMessages>\
         <m:{a}ResponseMessage ResponseClass=\"Success\">\
         <m:ResponseCode>NoError</m:ResponseCode>\
         {inner}\
         </m:{a}ResponseMessage>\
         </m:ResponseMessages>\
         </m:{a}Response>",
        a = action,
        m = EWS_MSG_NS,
        t = EWS_TYPE_NS,
        inner = inner,
    ))
}

fn folder_id_el(id: &str) -> String {
    format!(
        "<t:FolderId Id=\"{}\" ChangeKey=\"1\"/>",
        xml_escape(id)
    )
}

// ---------------------------------------------------------------------------
// CreateFolder (MS-OXWSFOLD §3.1.4.8)
// ---------------------------------------------------------------------------

pub(crate) async fn handle_create_folder(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let action = EwsAction::CreateFolder;
    let owner = owner_from_username(&auth.username).to_string();
    let (jmap, account_id) = match jmap_account(state, auth, action).await {
        Ok(t) => t,
        Err(r) => return r,
    };

    // Parent: <m:ParentFolderId><t:(DistinguishedFolderId|FolderId) Id=".."/>
    let parent_target = resolve_mailbox_target(
        &jmap,
        &account_id,
        &owner,
        first_attr(body, "FolderId", "Id").as_deref(),
        first_attr(body, "DistinguishedFolderId", "Id").as_deref(),
        &auth.username,
        &auth.password,
    )
    .await;
    let parent_id = match parent_target {
        MailboxTarget::Id(id) => Some(id),
        // Creating under MsgFolderRoot means a top-level mailbox.
        MailboxTarget::NotAMailFolder => None,
        MailboxTarget::NotFound => {
            return operation_error_response(
                &action,
                "ErrorFolderNotFound",
                "Parent folder was not found",
                StatusCode::OK,
            );
        }
    };

    // Display names of the folders to create (`<t:Folder><t:DisplayName>..`).
    // EWS Folder elements in a CreateFolder request carry DisplayName /
    // FolderClass; calendar/contact folders are rejected with a typed error
    // because they must live on the CalDAV/CardDAV side.
    let doc = match roxmltree::Document::parse(body) {
        Ok(d) => d,
        Err(e) => return internal_error(action, "Malformed CreateFolder XML", &anyhow::Error::from(e)),
    };
    let in_folders = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "Folders");
    let folder_specs: Vec<(String, String)> = match in_folders {
        None => Vec::new(),
        Some(folders_node) => folders_node
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "Folder")
            .map(|folder_el| {
                let name = folder_el
                    .descendants()
                    .find(|d| d.is_element() && d.tag_name().name() == "DisplayName")
                    .and_then(|d| d.text())
                    .unwrap_or("New Folder")
                    .to_string();
                let class = folder_el
                    .descendants()
                    .find(|d| d.is_element() && d.tag_name().name() == "FolderClass")
                    .and_then(|d| d.text())
                    .unwrap_or("IPF.Note")
                    .to_string();
                (name, class)
            })
            .collect(),
    };
    // Non-mail folder types must not create JMAP mailboxes.
    if doc
        .descendants()
        .any(|n| n.is_element() && matches!(n.tag_name().name(), "CalendarFolder" | "ContactsFolder" | "SearchFolder" | "TasksFolder"))
    {
        return operation_error_response(
            &action,
            "ErrorInvalidFolderTypeForOperation",
            "Calendar/Contacts/Search folders cannot be created through EWS folder operations",
            StatusCode::OK,
        );
    }
    if folder_specs.is_empty() {
        return operation_error_response(
            &action,
            "ErrorMissingFolderForOperation",
            "CreateFolder request contains no folders",
            StatusCode::OK,
        );
    }

    let name = folder_specs.first().map(|(n, _)| n.clone()).unwrap();
    if name.trim().is_empty() {
        return operation_error_response(
            &action,
            "ErrorInvalidProperty",
            "DisplayName cannot be empty",
            StatusCode::OK,
        );
    }

    match jmap
        .create_mailbox(
            &account_id,
            name.trim(),
            parent_id.as_deref(),
            &auth.username,
            &auth.password,
        )
        .await
    {
        Ok(id) => success_response(
            "CreateFolder",
            &format!("<m:Folders><t:Folder>{}</t:Folder></m:Folders>", folder_id_el(&id)),
        ),
        Err(e) => internal_error(action, "Mailbox/set create failed", &e),
    }
}

// ---------------------------------------------------------------------------
// UpdateFolder: rename via SetFolderField(folder:DisplayName)
// ---------------------------------------------------------------------------

pub(crate) async fn handle_update_folder(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let action = EwsAction::UpdateFolder;
    let owner = owner_from_username(&auth.username).to_string();
    let (jmap, account_id) = match jmap_account(state, auth, action).await {
        Ok(t) => t,
        Err(r) => return r,
    };

    let id = match first_attr(body, "FolderId", "Id") {
        Some(i) => i,
        None => {
            return operation_error_response(
                &action,
                "ErrorMissingFolderForOperation",
                "UpdateFolder request has no FolderId",
                StatusCode::OK,
            );
        }
    };
    let mailbox_id = match resolve_mailbox_target(
        &jmap,
        &account_id,
        &owner,
        Some(&id),
        None,
        &auth.username,
        &auth.password,
    )
    .await
    {
        MailboxTarget::Id(m) => m,
        MailboxTarget::NotAMailFolder => {
            return operation_error_response(
                &action,
                "ErrorFolderNotFound",
                "Distinguished folders cannot be renamed",
                StatusCode::OK,
            );
        }
        MailboxTarget::NotFound => {
            return operation_error_response(
                &action,
                "ErrorFolderNotFound",
                "Folder was not found",
                StatusCode::OK,
            );
        }
    };

    // New display name lives under
    // <t:Updates><t:SetFolderField><t:Folder><t:DisplayName>..
    let doc = match roxmltree::Document::parse(body) {
        Ok(d) => d,
        Err(e) => return internal_error(action, "Malformed UpdateFolder XML", &anyhow::Error::from(e)),
    };
    let new_name = doc
        .descendants()
        .find(|n| {
            n.is_element()
                && n.tag_name().name() == "DisplayName"
                && n.ancestors()
                    .any(|a| a.tag_name().name() == "SetFolderField")
        })
        .and_then(|n| n.text());

    match new_name {
        Some(name) if !name.trim().is_empty() => {}
        _ => {
            return operation_error_response(
                &action,
                "ErrorInvalidProperty",
                "UpdateFolder must contain a SetFolderField for DisplayName",
                StatusCode::OK,
            );
        }
    }
    let new_name = new_name.unwrap();

    match jmap
        .update_mailbox(
            &account_id,
            &mailbox_id,
            Some(new_name.trim()),
            None,
            &auth.username,
            &auth.password,
        )
        .await
    {
        Ok(_) => success_response(
            "UpdateFolder",
            &format!(
                "<m:Folders><t:Folder>{}</t:Folder></m:Folders>",
                folder_id_el(&mailbox_id)
            ),
        ),
        Err(e) => internal_error(action, "Mailbox/set update failed", &e),
    }
}

// ---------------------------------------------------------------------------
// MoveFolder: re-parent via Mailbox/set update parentId
// ---------------------------------------------------------------------------

pub(crate) async fn handle_move_folder(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let action = EwsAction::MoveFolder;
    let owner = owner_from_username(&auth.username).to_string();
    let (jmap, account_id) = match jmap_account(state, auth, action).await {
        Ok(t) => t,
        Err(r) => return r,
    };

    // <m:ToFolderId><t:(Distinguished)FolderId/></m:ToFolderId>
    let doc = match roxmltree::Document::parse(body) {
        Ok(d) => d,
        Err(e) => return internal_error(action, "Malformed MoveFolder XML", &anyhow::Error::from(e)),
    };
    let to_node = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "ToFolderId");
    let Some(to_node) = to_node else {
        return operation_error_response(
            &action,
            "ErrorMissingToFolderId",
            "MoveFolder request has no ToFolderId",
            StatusCode::OK,
        );
    };
    let to_explicit = to_node
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "FolderId")
        .and_then(|n| n.attribute("Id"))
        .map(|s| s.to_string());
    let to_distinguished = to_node
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "DistinguishedFolderId")
        .and_then(|n| n.attribute("Id"))
        .map(|s| s.to_string());

    let to_target = resolve_mailbox_target(
        &jmap,
        &account_id,
        &owner,
        to_explicit.as_deref(),
        to_distinguished.as_deref(),
        &auth.username,
        &auth.password,
    )
    .await;
    let new_parent = match to_target {
        MailboxTarget::Id(id) => Some(id),
        MailboxTarget::NotAMailFolder => None, // MsgFolderRoot -> top level
        MailboxTarget::NotFound => {
            return operation_error_response(
                &action,
                "ErrorFolderNotFound",
                "Destination folder was not found",
                StatusCode::OK,
            );
        }
    };

    for id in all_attrs(body, Some("FolderIds"), "FolderId", "Id") {
        let mailbox_id = match resolve_mailbox_target(
            &jmap,
            &account_id,
            &owner,
            Some(&id),
            None,
            &auth.username,
            &auth.password,
        )
        .await
        {
            MailboxTarget::Id(m) => m,
            MailboxTarget::NotAMailFolder | MailboxTarget::NotFound => {
                return operation_error_response(
                    &action,
                    "ErrorFolderNotFound",
                    "One of the folders to move was not found",
                    StatusCode::OK,
                );
            }
        };
        match jmap
            .update_mailbox(
                &account_id,
                &mailbox_id,
                None,
                Some(new_parent.as_deref()),
                &auth.username,
                &auth.password,
            )
            .await
        {
            Ok(_) => {}
            Err(e) => return internal_error(action, "Mailbox/set reparent failed", &e),
        }
    }

    success_response("MoveFolder", "<m:Folders/>")
}

// ---------------------------------------------------------------------------
// CopyFolder: create a destination mailbox and copy all messages into it
// (JMAP has no mailbox copy primitive; RFC 8621 §6.3 Email `copyFrom` is the
//  only server-side copy). Subfolders are copied recursively.
// ---------------------------------------------------------------------------

fn copy_folder_recursive<'a>(
    state: &'a Arc<AppState>,
    username: &'a str,
    password: &'a SecretString,
    jmap: &'a Arc<JmapClient>,
    account_id: &'a str,
    src_mailbox_id: &'a str,
    dest_parent: Option<&'a str>,
    depth: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, anyhow::Error>> + Send + 'a>>
{
    Box::pin(async move {
        const MAX_DEPTH: usize = 16;
        if depth > MAX_DEPTH {
            anyhow::bail!("folder copy exceeded depth {MAX_DEPTH}");
        }
        let _ = state;

        let all = jmap.query_mailboxes(username, password).await?;
        let src = all
            .mailboxes
            .iter()
            .find(|m| m.id.as_deref() == Some(src_mailbox_id))
            .ok_or_else(|| anyhow::anyhow!("source folder not found"))?;
        let src_name = src.name.clone().unwrap_or_else(|| "Folder".to_string());

        // Materialise the destination folder mirroring the source name.
        let new_id = jmap
            .create_mailbox(account_id, &src_name, dest_parent, username, password)
            .await?;

        // Copy all messages currently in the source folder.
        let ids: Vec<String> = jmap
            .list_email_ids_in_mailbox(account_id, src_mailbox_id, username, password)
            .await?
            .into_iter()
            .map(|(jid, _)| jid)
            .collect();
        if !ids.is_empty() {
            jmap.copy_emails(account_id, &ids, &new_id, username, password)
                .await?;
        }

        // Recurse: children of the source become children of the copy.
        for child in all
            .mailboxes
            .iter()
            .filter(|m| m.parent_id.as_deref() == Some(src_mailbox_id))
        {
            copy_folder_recursive(
                state,
                username,
                password,
                jmap,
                account_id,
                child.id.as_deref().unwrap_or(""),
                Some(&new_id),
                depth + 1,
            )
            .await?;
        }

        Ok(new_id)
    })
}

pub(crate) async fn handle_copy_folder(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let action = EwsAction::CopyFolder;
    let owner = owner_from_username(&auth.username).to_string();
    let (jmap, account_id) = match jmap_account(state, auth, action).await {
        Ok(t) => t,
        Err(r) => return r,
    };

    let doc = match roxmltree::Document::parse(body) {
        Ok(d) => d,
        Err(e) => return internal_error(action, "Malformed CopyFolder XML", &anyhow::Error::from(e)),
    };
    let to_node = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "ToFolderId");
    let Some(to_node) = to_node else {
        return operation_error_response(
            &action,
            "ErrorMissingToFolderId",
            "CopyFolder request has no ToFolderId",
            StatusCode::OK,
        );
    };
    let to_explicit = to_node
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "FolderId")
        .and_then(|n| n.attribute("Id"))
        .map(|s| s.to_string());
    let to_distinguished = to_node
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "DistinguishedFolderId")
        .and_then(|n| n.attribute("Id"))
        .map(|s| s.to_string());

    let dest_parent = match resolve_mailbox_target(
        &jmap,
        &account_id,
        &owner,
        to_explicit.as_deref(),
        to_distinguished.as_deref(),
        &auth.username,
        &auth.password,
    )
    .await
    {
        MailboxTarget::Id(id) => Some(id),
        MailboxTarget::NotAMailFolder => None,
        MailboxTarget::NotFound => {
            return operation_error_response(
                &action,
                "ErrorFolderNotFound",
                "Destination folder was not found",
                StatusCode::OK,
            );
        }
    };

    let mut created_ids: Vec<String> = Vec::new();
    for id in all_attrs(body, Some("FolderIds"), "FolderId", "Id") {
        let mailbox_id = match resolve_mailbox_target(
            &jmap,
            &account_id,
            &owner,
            Some(&id),
            None,
            &auth.username,
            &auth.password,
        )
        .await
        {
            MailboxTarget::Id(m) => m,
            MailboxTarget::NotAMailFolder | MailboxTarget::NotFound => {
                return operation_error_response(
                    &action,
                    "ErrorFolderNotFound",
                    "One of the folders to copy was not found",
                    StatusCode::OK,
                );
            }
        };
        match copy_folder_recursive(
            state,
            &auth.username,
            &auth.password,
            &jmap,
            &account_id,
            &mailbox_id,
            dest_parent.as_deref(),
            0,
        )
        .await
        {
            Ok(new_id) => created_ids.push(new_id),
            Err(e) => return internal_error(action, "Folder copy failed", &e),
        }
    }

    success_response(
        "CopyFolder",
        &format!(
            "<m:Folders>{}</m:Folders>",
            created_ids
                .iter()
                .map(|i| format!("<t:Folder>{}</t:Folder>", folder_id_el(i)))
                .collect::<String>()
        ),
    )
}

// ---------------------------------------------------------------------------
// DeleteFolder: destroy the mailbox (HardDelete) or move it under Trash
// (SoftDelete / MoveToDeletedItems), mirroring the MAPI ROP_DELETE_FOLDER.
// ---------------------------------------------------------------------------

pub(crate) async fn handle_delete_folder(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let action = EwsAction::DeleteFolder;
    let owner = owner_from_username(&auth.username).to_string();
    let (jmap, account_id) = match jmap_account(state, auth, action).await {
        Ok(t) => t,
        Err(r) => return r,
    };

    // DeleteType attribute on the <m:DeleteFolder> element itself:
    // HardDelete destroys; SoftDelete/MoveToDeletedItems move under Trash.
    let hard = first_attr(body, "DeleteFolder", "DeleteType")
        .map(|v| v == "HardDelete")
        .unwrap_or(false);

    // Target Trash for soft deletes.
    let trash_id: Option<String> = if hard {
        None
    } else {
        jmap.get_mailbox_ids_for_role(&account_id, "trash", &auth.username, &auth.password)
            .await
            .ok()
            .and_then(|ids| ids.into_iter().next())
    };

    let mut ids: Vec<String> = all_attrs(body, Some("FolderIds"), "FolderId", "Id");
    // Distinguished ids may also appear inside FolderIds.
    for d in all_attrs(body, Some("FolderIds"), "DistinguishedFolderId", "Id") {
        ids.push(format!("distinguished:{}", d));
    }
    if ids.is_empty() {
        return operation_error_response(
            &action,
            "ErrorMissingFolderForOperation",
            "DeleteFolder request contains no folders",
            StatusCode::OK,
        );
    }

    for id in ids {
        let (explicit, distinguished) = if let Some(d) = id.strip_prefix("distinguished:") {
            (None, Some(d.to_string()))
        } else {
            (Some(id), None)
        };
        let mailbox_id = match resolve_mailbox_target(
            &jmap,
            &account_id,
            &owner,
            explicit.as_deref(),
            distinguished.as_deref(),
            &auth.username,
            &auth.password,
        )
        .await
        {
            MailboxTarget::Id(m) => m,
            MailboxTarget::NotAMailFolder => {
                return operation_error_response(
                    &action,
                    "ErrorFolderNotFound",
                    "Distinguished/system folders cannot be deleted",
                    StatusCode::OK,
                );
            }
            MailboxTarget::NotFound => {
                return operation_error_response(
                    &action,
                    "ErrorFolderNotFound",
                    "Folder was not found",
                    StatusCode::OK,
                );
            }
        };

        if !hard {
            // Soft delete: reparent under Trash. If the server has no Trash
            // (or the move fails outright), fall back to a destroy, matching
            // the permissive JMAP semantics Stalwart applies.
            let moved = match &trash_id {
                Some(t) if *t != mailbox_id => jmap
                    .update_mailbox(
                        &account_id,
                        &mailbox_id,
                        None,
                        Some(Some(t.as_str())),
                        &auth.username,
                        &auth.password,
                    )
                    .await
                    .is_ok(),
                Some(t) => {
                    // Deleting Trash itself: contents are hard-deleted.
                    let _ = t;
                    false
                }
                None => false,
            };
            if moved {
                continue;
            }
        }

        // Hard delete (or soft-fallback): empty the mailbox, then destroy it.
        let ids_in = match jmap
            .list_email_ids_in_mailbox(&account_id, &mailbox_id, &auth.username, &auth.password)
            .await
        {
            Ok(v) => v.into_iter().map(|(jid, _)| jid).collect::<Vec<_>>(),
            Err(e) => return internal_error(action, "Email/query for hard delete failed", &e),
        };
        if !ids_in.is_empty() {
            if let Err(e) = jmap
                .destroy_emails(&account_id, &ids_in, &auth.username, &auth.password)
                .await
            {
                return internal_error(action, "Email/set destroy failed", &e);
            }
        }
        match jmap
            .destroy_mailbox(&account_id, &mailbox_id, &auth.username, &auth.password)
            .await
        {
            Ok(_) => {}
            Err(e) => return internal_error(action, "Mailbox/set destroy failed", &e),
        }
    }

    success_response("DeleteFolder", "")
}

// ---------------------------------------------------------------------------
// EmptyFolder: destroy every email in the resolved mailbox
// ---------------------------------------------------------------------------

pub(crate) async fn handle_empty_folder(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let action = EwsAction::EmptyFolder;
    let owner = owner_from_username(&auth.username).to_string();
    let (jmap, account_id) = match jmap_account(state, auth, action).await {
        Ok(t) => t,
        Err(r) => return r,
    };

    let hard = first_attr(body, "EmptyFolder", "DeleteType")
        .map(|v| v == "HardDelete")
        .unwrap_or(false);
    let include_subfolders = first_attr(body, "EmptyFolder", "SubFolders")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    let trash_id: Option<String> = if hard {
        None
    } else {
        jmap.get_mailbox_ids_for_role(&account_id, "trash", &auth.username, &auth.password)
            .await
            .ok()
            .and_then(|ids| ids.into_iter().next())
    };

    let mut folder_ids: Vec<String> = all_attrs(body, Some("FolderIds"), "FolderId", "Id");
    for d in all_attrs(body, Some("FolderIds"), "DistinguishedFolderId", "Id") {
        folder_ids.push(format!("distinguished:{}", d));
    }
    if folder_ids.is_empty() {
        return operation_error_response(
            &action,
            "ErrorMissingFolderForOperation",
            "EmptyFolder request contains no folders",
            StatusCode::OK,
        );
    }

    for id in folder_ids {
        let (explicit, distinguished) = if let Some(d) = id.strip_prefix("distinguished:") {
            (None, Some(d.to_string()))
        } else {
            (Some(id), None)
        };
        let mailbox_id = match resolve_mailbox_target(
            &jmap,
            &account_id,
            &owner,
            explicit.as_deref(),
            distinguished.as_deref(),
            &auth.username,
            &auth.password,
        )
        .await
        {
            MailboxTarget::Id(m) => m,
            MailboxTarget::NotAMailFolder | MailboxTarget::NotFound => {
                return operation_error_response(
                    &action,
                    "ErrorFolderNotFound",
                    "Folder was not found",
                    StatusCode::OK,
                );
            }
        };

        let this_result = jmap
            .list_email_ids_in_mailbox(&account_id, &mailbox_id, &auth.username, &auth.password)
            .await;
        let in_this: Vec<String> = match this_result {
            Ok(v) => v.into_iter().map(|(jid, _)| jid).collect(),
            Err(e) => return internal_error(action, "Email/query failed", &e),
        };

        // Sub-folder contents, if requested (EWS DeleteFolder semantics on
        // EmptyFolder recurse into subfolders when SubFolders="true").
        let mut all_ids = in_this;
        if include_subfolders {
            let all = match jmap.query_mailboxes(&auth.username, &auth.password).await {
                Ok(m) => m,
                Err(e) => return internal_error(action, "Mailbox/query failed", &e),
            };
            // BFS over children
            let mut queue: VecDeque<String> = all
                .mailboxes
                .iter()
                .filter(|m| m.parent_id.as_deref() == Some(mailbox_id.as_str()))
                .filter_map(|m| m.id.clone())
                .collect();
            let mut seen: std::collections::HashSet<String> = queue.iter().cloned().collect();
            while let Some(cur) = queue.pop_front() {
                let cur_ids: Vec<String> = jmap
                    .list_email_ids_in_mailbox(&account_id, &cur, &auth.username, &auth.password)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(jid, _)| jid)
                    .collect();
                all_ids.extend(cur_ids);
                for child in all
                    .mailboxes
                    .iter()
                    .filter(|m| m.parent_id.as_deref() == Some(cur.as_str()))
                {
                    if let Some(child_id) = &child.id {
                        if seen.insert(child_id.clone()) {
                            queue.push_back(child_id.clone());
                        }
                    }
                }
            }
        }

        if hard {
            if !all_ids.is_empty() {
                if let Err(e) = jmap
                    .destroy_emails(&account_id, &all_ids, &auth.username, &auth.password)
                    .await
                {
                    return internal_error(action, "Email/set destroy failed", &e);
                }
            }
        } else if let Some(trash) = trash_id.as_ref() {
            // Soft delete: move messages to Trash via Email/set mailboxIds patch.
            let mut update = serde_json::Map::new();
            for jid in &all_ids {
                update.insert(
                    jid.clone(),
                    serde_json::json!({ "mailboxIds": { trash: true } }),
                );
            }
            if !all_ids.is_empty() {
                let outcome = jmap
                    .update_email_checked(
                        &account_id,
                        &serde_json::Value::Object(update),
                        &auth.username,
                        &auth.password,
                    )
                    .await;
                if let Err(e) = outcome {
                    return internal_error(action, "Email/set move to Trash failed", &e);
                }
            }
        }
        // else: no Trash role exists; treat as success-no-op (nothing to move to)
    }

    success_response("EmptyFolder", "")
}

// ---------------------------------------------------------------------------
// MarkAllItemsAsRead: set $seen on every message in each resolved folder
// ---------------------------------------------------------------------------

pub(crate) async fn handle_mark_all_items_as_read(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let action = EwsAction::MarkAllItemsAsRead;
    let owner = owner_from_username(&auth.username).to_string();
    let (jmap, account_id) = match jmap_account(state, auth, action).await {
        Ok(t) => t,
        Err(r) => return r,
    };

    let mut folder_ids: Vec<String> = all_attrs(body, Some("FolderIds"), "FolderId", "Id");
    for d in all_attrs(body, Some("FolderIds"), "DistinguishedFolderId", "Id") {
        folder_ids.push(format!("distinguished:{}", d));
    }
    if folder_ids.is_empty() {
        return operation_error_response(
            &action,
            "ErrorMissingFolderForOperation",
            "MarkAllItemsAsRead request has no FolderIds",
            StatusCode::OK,
        );
    }

    for id in folder_ids {
        let (explicit, distinguished) = if let Some(d) = id.strip_prefix("distinguished:") {
            (None, Some(d.to_string()))
        } else {
            (Some(id), None)
        };
        let mailbox_id = match resolve_mailbox_target(
            &jmap,
            &account_id,
            &owner,
            explicit.as_deref(),
            distinguished.as_deref(),
            &auth.username,
            &auth.password,
        )
        .await
        {
            MailboxTarget::Id(m) => m,
            MailboxTarget::NotAMailFolder | MailboxTarget::NotFound => {
                return operation_error_response(
                    &action,
                    "ErrorFolderNotFound",
                    "Folder was not found",
                    StatusCode::OK,
                );
            }
        };

        let ids: Vec<String> = match jmap
            .list_email_ids_in_mailbox(&account_id, &mailbox_id, &auth.username, &auth.password)
            .await
        {
            Ok(v) => v.into_iter().map(|(jid, _)| jid).collect(),
            Err(e) => return internal_error(action, "Email/query failed", &e),
        };

        if ids.is_empty() {
            continue;
        }
        let mut update = serde_json::Map::new();
        for jid in &ids {
            update.insert(jid.clone(), json!({ "keywords/$seen": true }));
        }
        match jmap
            .update_email_checked(
                &account_id,
                &serde_json::Value::Object(update),
                &auth.username,
                &auth.password,
            )
            .await
        {
            Ok(_) => {}
            Err(e) => return internal_error(action, "Email/set mark-read failed", &e),
        }
    }

    success_response("MarkAllItemsAsRead", "")
}

// ---------------------------------------------------------------------------
// FindConversation (MS-OXWSCONV §3.1.4.3)
//
// Groups the folder's messages into conversations by JMAP `threadId` and
// renders one `<t:Conversation>` per thread, ordered by last delivery time.
// ---------------------------------------------------------------------------

pub(crate) async fn handle_find_conversation(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let action = EwsAction::FindConversation;
    let owner = owner_from_username(&auth.username).to_string();
    let (jmap, account_id) = match jmap_account(state, auth, action).await {
        Ok(t) => t,
        Err(r) => return r,
    };

    // Parent folder of the conversation listing.
    let parent_explicit = {
        match roxmltree::Document::parse(body) {
            Ok(d) => d
                .descendants()
                .find(|n| n.is_element() && n.tag_name().name() == "ParentFolderId")
                .and_then(|p| {
                    p.descendants()
                        .find(|n| n.is_element() && n.tag_name().name() == "FolderId")
                        .and_then(|n| n.attribute("Id").map(|s| s.to_string()))
                }),
            Err(e) => return internal_error(action, "Malformed FindConversation XML", &anyhow::Error::from(e)),
        }
    };
    let _ = parent_explicit;
    let explicit = first_attr(body, "FolderId", "Id");
    let distinguished = first_attr(body, "DistinguishedFolderId", "Id");

    let mailbox_id = match resolve_mailbox_target(
        &jmap,
        &account_id,
        &owner,
        explicit.as_deref(),
        distinguished.as_deref(),
        &auth.username,
        &auth.password,
    )
    .await
    {
        MailboxTarget::Id(m) => m,
        MailboxTarget::NotAMailFolder | MailboxTarget::NotFound => {
            return operation_error_response(
                &action,
                "ErrorFolderNotFound",
                "Parent folder was not found",
                StatusCode::OK,
            );
        }
    };

    // Paging controls (IndexedPageItemView / MaxEntriesReturned).
    let max_entries: u64 = first_attr(body, "IndexedPageItemView", "MaxEntriesReturned")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(50);
    let offset: u64 = first_attr(body, "IndexedPageItemView", "Offset")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    let list = match jmap
        .query_emails(crate::jmap::QueryEmailsParams {
            account_id: &account_id,
            filter: Some(json!({"inMailbox": mailbox_id})),
            sort: Some(vec![json!({"property": "receivedAt", "isAscending": false})]),
            position: 0,
            limit: offset.saturating_add(max_entries).max(50),
            username: &auth.username,
            password: &auth.password,
        })
        .await
    {
        Ok(l) => l,
        Err(e) => return internal_error(action, "Email/query failed", &e),
    };

    // Group by threadId, preserving first-appearance order (descending date).
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<&crate::jmap::JmapEmail>> =
        std::collections::HashMap::new();
    for e in &list.emails {
        let thread = e
            .thread_id
            .clone()
            .or_else(|| e.id.clone())
            .unwrap_or_default();
        if !order.contains(&thread) {
            order.push(thread.clone());
        }
        groups.entry(thread).or_default().push(e);
    }

    let total = order.len() as u64;
    let page: Vec<String> = order
        .iter()
        .skip(offset as usize)
        .take(max_entries as usize)
        .cloned()
        .collect();
    let includes_last = offset + page.len() as u64 >= total;

    let mut convs = String::new();
    for thread_id in page {
        let msgs = match groups.get(&thread_id) {
            Some(m) => m,
            None => continue,
        };
        let newest = msgs.first();
        let subject = newest.and_then(|m| m.subject.clone()).unwrap_or_default();
        let last_time = newest
            .and_then(|m| m.received_at.clone())
            .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());
        let preview = newest.and_then(|m| m.preview.clone()).unwrap_or_default();

        let unread = msgs
            .iter()
            .filter(|m| m.keywords.as_ref().map(|k| !k.contains_key("$seen")).unwrap_or(true))
            .count();
        let has_att = msgs.iter().any(|m| m.has_attachment == Some(true));
        let senders: Vec<String> = {
            let mut v: Vec<String> = msgs
                .iter()
                .filter_map(|m| m.from.as_ref())
                .flat_map(|from| from.iter().filter_map(|a| a.email.clone()))
                .collect();
            v.sort();
            v.dedup();
            v
        };

        let conv_id = format!("conv-{}", thread_id);
        convs.push_str(&format!(
            "<t:Conversation>\
             <t:ConversationId Id=\"{id}\"/>\
             <t:ConversationTopic>{topic}</t:ConversationTopic>\
             <t:UniqueRecipients>{recips}</t:UniqueRecipients>\
             <t:GlobalUniqueRecipients>{recips}</t:GlobalUniqueRecipients>\
             <t:UniqueSenders/>\
             <t:GlobalUniqueSenders/>\
             <t:LastDeliveryTime>{last}</t:LastDeliveryTime>\
             <t:Categories/>\
             <t:HasAttachments>{has_att}</t:HasAttachments>\
             <t:MessageCount>{count}</t:MessageCount>\
             <t:GlobalMessageCount>{count}</t:GlobalMessageCount>\
             <t:UnreadCount>{unread}</t:UnreadCount>\
             <t:GlobalUnreadCount>{unread}</t:GlobalUnreadCount>\
             <t:Size>0</t:Size>\
             <t:ItemIds/>\
             <t:Preview>{preview}</t:Preview>\
             </t:Conversation>",
            id = xml_escape(&conv_id),
            topic = xml_escape(&subject),
            recips = senders
                .iter()
                .map(|s| format!("<t:String>{}</t:String>", xml_escape(s)))
                .collect::<String>(),
            last = xml_escape(&last_time),
            has_att = has_att,
            count = msgs.len(),
            unread = unread,
            preview = xml_escape(&preview),
        ));
    }

    success_response(
        "FindConversation",
        &format!(
            "<m:Conversations TotalConversationsInView=\"{total}\" IndexedPagingOffset=\"{off}\" IncludesLastItemInRange=\"{incl}\">{convs}</m:Conversations>",
            total = total,
            off = offset + 1,
            incl = includes_last,
            convs = convs,
        ),
    )
}

// ---------------------------------------------------------------------------
// ExpandDL (MS-OXWSDLIST §3.1.4.2)
//
// Stalwart has no distribution lists. Return an Expansion that carries the
// requested mailbox on its own: the shape New Outlook expects for "this
// address is not actually a DL", without producing a UI error.
// ---------------------------------------------------------------------------

pub(crate) async fn handle_expand_dl(
    _state: &Arc<AppState>,
    _auth: &AuthContext,
    body: &str,
) -> Response {
    let email_match = roxmltree::Document::parse(body)
        .ok()
        .and_then(|d| {
            d.descendants()
                .find(|n| n.is_element() && n.tag_name().name() == "EmailAddress")
                .and_then(|n| n.text().map(|s| s.to_string()))
        });

    let inner = match email_match {
        Some(email) => format!(
            "<m:DLExpansion TotalItemsInView=\"1\" IncludesLastItemInRange=\"true\">\
             <t:Mailbox><t:EmailAddress>{}</t:EmailAddress></t:Mailbox>\
             </m:DLExpansion>",
            xml_escape(&email)
        ),
        None => {
            "<m:DLExpansion TotalItemsInView=\"0\" IncludesLastItemInRange=\"true\"/>"
                .to_string()
        }
    };
    success_response("ExpandDL", &inner)
}

// ---------------------------------------------------------------------------
// GetUserRetentionPolicyTags / GetSearchableMailboxes / sharing / tracking
//
// Stalwart has no retention-policy, approved-cross-mailbox-search, sharing
// invitation, or transport message-tracking subsystem. Respond with typed
// errors or with the honest single-mailbox view instead of fabricating data.
// ---------------------------------------------------------------------------

pub(crate) async fn handle_get_user_retention_policy_tags(_auth: &AuthContext) -> Response {
    success_response("GetUserRetentionPolicyTags", "<m:RetentionPolicyTags/>")
}

pub(crate) async fn handle_get_searchable_mailboxes(
    _state: &Arc<AppState>,
    auth: &AuthContext,
    _body: &str,
) -> Response {
    // Single-mailbox gateway: the searchable set is exactly the caller's own
    // mailbox. New Outlook uses this for its search-scope selector.
    let email = xml_escape(&auth.username);
    let inner = format!(
        "<m:SearchableMailboxes>\
         <t:SearchableMailbox>\
         <t:Guid>00000000-0000-0000-0000-000000000000</t:Guid>\
         <t:PrimarySmtpAddress>{email}</t:PrimarySmtpAddress>\
         <t:IsExternalMailbox>false</t:IsExternalMailbox>\
         <t:DisplayName>{email}</t:DisplayName>\
         <t:IsMembershipGroup>false</t:IsMembershipGroup>\
         <t:ReferenceId>ewsgw</t:ReferenceId>\
         </t:SearchableMailbox>\
         </m:SearchableMailboxes>",
        email = email,
    );
    success_response("GetSearchableMailboxes", &inner)
}

pub(crate) async fn handle_get_sharing_metadata(_auth: &AuthContext) -> Response {
    operation_error_response(
        &EwsAction::GetSharingMetadata,
        "ErrorInvalidSharingMessage",
        "No sharing metadata is available: this mailbox has no sharing invitations",
        StatusCode::OK,
    )
}

pub(crate) async fn handle_get_sharing_folder(_auth: &AuthContext) -> Response {
    operation_error_response(
        &EwsAction::GetSharingFolder,
        "ErrorNoSharingFolderFound",
        "No shared folder is currently provisioned on this mailbox",
        StatusCode::OK,
    )
}

pub(crate) async fn handle_refresh_sharing_folder(_auth: &AuthContext) -> Response {
    operation_error_response(
        &EwsAction::RefreshSharingFolder,
        "ErrorNoSharingFolderFound",
        "No shared folder is currently provisioned on this mailbox",
        StatusCode::OK,
    )
}

pub(crate) async fn handle_get_message_tracking_report(_auth: &AuthContext) -> Response {
    operation_error_response(
        &EwsAction::GetMessageTrackingReport,
        "ErrorNotSupported",
        "Message tracking is not exposed by the gateway (JMAP-only backend)",
        StatusCode::OK,
    )
}

pub(crate) async fn handle_upload_items(_auth: &AuthContext) -> Response {
    // UploadItems is a streaming operation New Outlook does not use
    // interactively; it is only exercised by mailbox-migration tooling.
    operation_error_response(
        &EwsAction::UploadItems,
        "ErrorNotSupported",
        "UploadItems streaming is not supported; use CreateItem for new messages",
        StatusCode::OK,
    )
}

pub(crate) async fn handle_export_items(_auth: &AuthContext) -> Response {
    operation_error_response(
        &EwsAction::ExportItems,
        "ErrorNotSupported",
        "ExportItems streaming is not supported; use GetItem for message retrieval",
        StatusCode::OK,
    )
}

// ---------------------------------------------------------------------------
// SetUserConfiguration / DeleteUserConfiguration (MS-OXWSUSRCFG)
// ---------------------------------------------------------------------------

/// Folder key used as the storage primary-key discriminator. When the request
/// carries an `<m:DistinguishedFolderId Id=".."/>` we store it as the
/// (lowercased) distinguished name; otherwise we store the raw FolderId.
fn config_folder_key(body: &str) -> String {
    if let Some(d) = first_attr(body, "DistinguishedFolderId", "Id") {
        d.to_ascii_lowercase()
    } else if let Some(f) = first_attr(body, "FolderId", "Id") {
        f
    } else {
        "msgfolderroot".to_string()
    }
}

/// Capture the raw inner XML of the first child of `uc_node` named `tag`.
/// `body` is the original request text so ranges map back into the source.
fn raw_inner_xml<'a>(uc_node: roxmltree::Node<'_, 'a>, body: &'a str, tag: &str) -> Option<String> {
    let node = uc_node
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == tag)?;
    let r = node.range();
    let full = body.get(r.start..r.end)?;
    // Strip the outer element: everything up to and including '>' from the
    // start tag, and the matching '</...>' at the end.
    let after_open = full.find('>')?;
    let before_close = full.rfind("</")?;
    if after_open >= before_close {
        Some(String::new())
    } else {
        Some(full[after_open + 1..before_close].to_string())
    }
}

pub(crate) async fn handle_set_user_configuration(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let is_update = body.contains("<m:UpdateUserConfiguration")
        || body.contains("<UpdateUserConfiguration");
    let action = if is_update {
        EwsAction::UpdateUserConfiguration
    } else {
        EwsAction::SetUserConfiguration
    };
    let owner = owner_from_username(&auth.username).to_string();
    let folder_key = config_folder_key(body);

    let doc = match roxmltree::Document::parse(body) {
        Ok(d) => d,
        Err(e) => {
            return operation_error_response(
                &action,
                "ErrorInvalidRequest",
                &format!("Malformed SetUserConfiguration XML: {e}"),
                StatusCode::BAD_REQUEST,
            );
        }
    };
    let uc_node = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "UserConfiguration");
    let Some(uc_node) = uc_node else {
        return operation_error_response(
            &action,
            "ErrorInvalidRequest",
            "SetUserConfiguration request has no <m:UserConfiguration> element",
            StatusCode::BAD_REQUEST,
        );
    };
    let name_node = uc_node
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "UserConfigurationName");
    let Some(name_node) = name_node else {
        return operation_error_response(
            &action,
            "ErrorInvalidRequest",
            "SetUserConfiguration request has no UserConfigurationName",
            StatusCode::BAD_REQUEST,
        );
    };
    let Some(name) = name_node.attribute("Name").map(|s| s.to_string()) else {
        return operation_error_response(
            &action,
            "ErrorInvalidRequest",
            "UserConfigurationName has no Name attribute",
            StatusCode::BAD_REQUEST,
        );
    };

    let dictionary = raw_inner_xml(uc_node, body, "Dictionary");
    let xml_data = raw_inner_xml(uc_node, body, "XmlData");
    let binary_data = raw_inner_xml(uc_node, body, "BinaryData");

    match state
        .storage
        .put_user_config(
            &owner,
            &folder_key,
            &name,
            dictionary.as_deref(),
            xml_data.as_deref(),
            binary_data.as_deref(),
        )
        .await
    {
        Ok(_) => success_response(
            if is_update {
                "UpdateUserConfiguration"
            } else {
                "SetUserConfiguration"
            },
            "",
        ),
        Err(e) => internal_error(action, "user_config persistence failed", &e),
    }
}

pub(crate) async fn handle_delete_user_configuration(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let action = EwsAction::DeleteUserConfiguration;
    let owner = owner_from_username(&auth.username).to_string();
    let folder_key = config_folder_key(body);

    let Some(name) = first_attr(body, "UserConfigurationName", "Name") else {
        return operation_error_response(
            &action,
            "ErrorInvalidRequest",
            "DeleteUserConfiguration request has no UserConfigurationName",
            StatusCode::BAD_REQUEST,
        );
    };

    match state
        .storage
        .delete_user_config(&owner, &folder_key, &name)
        .await
    {
        Ok(_) => success_response("DeleteUserConfiguration", ""),
        Err(e) => internal_error(action, "user_config delete failed", &e),
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_attr_extracts_id() {
        let xml = r#"<m:GetFolder xmlns:m="m" xmlns:t="t"><m:FolderIds><t:FolderId Id="abc123" ChangeKey="1"/></m:FolderIds></m:GetFolder>"#;
        assert_eq!(first_attr(xml, "FolderId", "Id"), Some("abc123".into()));
        assert_eq!(first_attr(xml, "FolderId", "ChangeKey"), Some("1".into()));
        assert_eq!(first_attr(xml, "Nope", "Id"), None);
    }

    #[test]
    fn all_attrs_scoped_to_container() {
        let xml = r#"<m:DeleteFolder xmlns:m="m" xmlns:t="t"><m:FolderIds><t:FolderId Id="a"/><t:FolderId Id="b"/></m:FolderIds><t:FolderId Id="outside"/></m:DeleteFolder>"#;
        let ids = all_attrs(xml, Some("FolderIds"), "FolderId", "Id");
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn raw_inner_xml_extracts_fragment() {
        let body = r#"<m:SetUserConfiguration xmlns:m="m" xmlns:t="t"><m:UserConfiguration><t:UserConfigurationName Name="X"/><t:XmlData><a b="1">hi</a></t:XmlData></m:UserConfiguration></m:SetUserConfiguration>"#;
        let doc = roxmltree::Document::parse(body).unwrap();
        let uc = doc
            .descendants()
            .find(|n| n.tag_name().name() == "UserConfiguration")
            .unwrap();
        let inner = raw_inner_xml(uc, body, "XmlData").unwrap();
        assert_eq!(inner, r#"<a b="1">hi</a>"#);
    }

    #[test]
    fn config_folder_key_defaults_to_root() {
        assert_eq!(config_folder_key("<m:SetUserConfiguration/>"), "msgfolderroot");
        let with_dist = r#"<m:SetUserConfiguration xmlns:m="m" xmlns:t="t"><m:UserConfiguration><t:UserConfigurationName Name="X"><t:DistinguishedFolderId Id="Calendar"/></t:UserConfigurationName></m:UserConfiguration></m:SetUserConfiguration>"#;
        assert_eq!(config_folder_key(with_dist), "calendar");
    }

    #[test]
    fn folder_id_element_is_wellformed() {
        let el = folder_id_el("id<>");
        assert!(el.contains("Id=\"id&lt;&gt;\""));
        assert!(el.contains("ChangeKey=\"1\""));
    }
}

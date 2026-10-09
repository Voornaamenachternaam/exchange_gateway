// src/tasks.rs
//
// EAS Tasks and Notes, backed by Stalwart Mailserver.
//
// Stalwart has no native Tasks or Notes object type (no JMAP Task in v0.16),
// but its CalDAV calendar store accepts the full iCalendar component set —
// including VTODO (RFC 5545 §3.6.2) and VJOURNAL (§3.6.3) — so the gateway
// maps EAS Tasks to VTODOs and EAS Notes to VJOURNALs in two per-user CalDAV
// collections (`/{cal}/{user}/Tasks/`, `/{cal}/{user}/Notes/`), created via
// MKCALENDAR on first use. Stalwart is the system of record:
//
//   * every EAS Add/Change/Delete is written through to CalDAV FIRST and the
//     mirror row (`task_map`/`note_map`) is updated only after the backend
//     accepted it (backend failure → MS-ASCMD status 6, nothing recorded);
//   * every Tasks/Notes Sync first reconciles the mirror against a
//     calendar-query REPORT of the collection (new/changed/deleted VTODOs and
//     VJOURNALs from ANY client — Stalwart webmail included — flow to EAS),
//     so the folder can never diverge from the backend;
//   * the change journal is written for every backend-observed mutation, so
//     the EAS Ping/direct-push path (handle_ping) and downstream change
//     tracking observe them exactly like calendar mutations.
//
// Gateway-mediated round-trips are lossless: EAS properties with no exact
// iCalendar equivalent keep their original wire strings in `X-SGW-*`
// properties, and unmapped iCalendar properties survive EAS Change commands
// verbatim (only the EAS-mapped property lines are rewritten).
//
// When CalDAV is not configured (`GATEWAY_CALDAV_BASE` empty) the module
// degrades to the pre-Stalwart gateway-local store so an explicitly local
// deployment keeps working.
use crate::models::AppState;
use crate::storage::{NoteFields, NoteRow, TaskFields, TaskRow};
use crate::util::resolve_xml_reference;
use anyhow::Result;
use quick_xml::Reader;
use quick_xml::events::Event;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// EAS collection type discriminators (see MS-ASCMD §2.2.3.186.3 / FolderSync Type).
pub const TASKS_COLLECTION_ID: &str = "7";
pub const NOTES_COLLECTION_ID: &str = "10";

fn xml_escape(s: &str) -> String {
    crate::util::escape_xml_text(s).to_string()
}

/// Render a single task's `<ApplicationData>` content using the MS-ASTASK schema.
pub fn render_eas_task(row: &TaskRow) -> String {
    let mut xml = String::new();

    if let Some(subject) = row.subject.as_deref()
        && !subject.is_empty()
    {
        xml.push_str(&format!(
            "<Tasks:Subject>{}</Tasks:Subject>",
            xml_escape(subject)
        ));
    }

    if let Some(importance) = row.importance {
        xml.push_str(&format!(
            "<Tasks:Importance>{}</Tasks:Importance>",
            importance
        ));
    }

    if let Some(sensitivity) = row.sensitivity {
        xml.push_str(&format!(
            "<Tasks:Sensitivity>{}</Tasks:Sensitivity>",
            sensitivity
        ));
    }

    if let Some(start_date) = row.start_date.as_deref() {
        xml.push_str(&format!(
            "<Tasks:StartDate>{}</Tasks:StartDate>",
            xml_escape(start_date)
        ));
    }

    if let Some(due_date) = row.due_date.as_deref() {
        xml.push_str(&format!(
            "<Tasks:DueDate>{}</Tasks:DueDate>",
            xml_escape(due_date)
        ));
    }

    if let Some(utc_start_date) = row.utc_start_date.as_deref() {
        xml.push_str(&format!(
            "<Tasks:UtcStartDate>{}</Tasks:UtcStartDate>",
            xml_escape(utc_start_date)
        ));
    }

    if let Some(utc_due_date) = row.utc_due_date.as_deref() {
        xml.push_str(&format!(
            "<Tasks:UtcDueDate>{}</Tasks:UtcDueDate>",
            xml_escape(utc_due_date)
        ));
    }

    // MS-ASTASK: 0 = incomplete, 1 = complete.
    xml.push_str(&format!(
        "<Tasks:Complete>{}</Tasks:Complete>",
        row.complete
    ));

    if let Some(date_completed) = row.date_completed.as_deref()
        && !date_completed.is_empty()
    {
        xml.push_str(&format!(
            "<Tasks:DateCompleted>{}</Tasks:DateCompleted>",
            xml_escape(date_completed)
        ));
    }

    xml.push_str(&format!(
        "<Tasks:ReminderSet>{}</Tasks:ReminderSet>",
        row.reminder_set
    ));

    if let Some(reminder_time) = row.reminder_time.as_deref()
        && !reminder_time.is_empty()
    {
        xml.push_str(&format!(
            "<Tasks:ReminderTime>{}</Tasks:ReminderTime>",
            xml_escape(reminder_time)
        ));
    }

    if let Some(categories) = row.categories.as_deref()
        && !categories.is_empty()
    {
        xml.push_str("<Tasks:Categories>");
        for category in categories.split(';').filter(|c| !c.is_empty()) {
            xml.push_str(&format!(
                "<Tasks:Category>{}</Tasks:Category>",
                xml_escape(category)
            ));
        }
        xml.push_str("</Tasks:Categories>");
    }

    if let Some(body) = row.body.as_deref()
        && !body.is_empty()
    {
        xml.push_str(&format!("<Tasks:Body>{}</Tasks:Body>", xml_escape(body)));
    }

    xml
}

/// Render a complete `<Add>` element for a task.
pub fn render_eas_task_add(server_id: &str, row: &TaskRow) -> String {
    format!(
        r#"<Add><ServerId>{}</ServerId><ApplicationData>{}</ApplicationData></Add>"#,
        xml_escape(server_id),
        render_eas_task(row)
    )
}

/// Render a single note's `<ApplicationData>` content using the MS-ASNOTE schema.
pub fn render_eas_note(row: &NoteRow) -> String {
    let mut xml = String::new();

    if let Some(subject) = row.subject.as_deref()
        && !subject.is_empty()
    {
        xml.push_str(&format!(
            "<Notes:Subject>{}</Notes:Subject>",
            xml_escape(subject)
        ));
    }

    let message_class = row
        .message_class
        .as_deref()
        .filter(|m| !m.is_empty())
        .unwrap_or("IPM.StickyNote");
    xml.push_str(&format!(
        "<Notes:MessageClass>{}</Notes:MessageClass>",
        xml_escape(message_class)
    ));

    if let Some(last_modified) = row.last_modified_date.as_deref()
        && !last_modified.is_empty()
    {
        xml.push_str(&format!(
            "<Notes:LastModifiedDate>{}</Notes:LastModifiedDate>",
            xml_escape(last_modified)
        ));
    }

    if let Some(categories) = row.categories.as_deref()
        && !categories.is_empty()
    {
        xml.push_str("<Notes:Categories>");
        for category in categories.split(';').filter(|c| !c.is_empty()) {
            xml.push_str(&format!(
                "<Notes:Category>{}</Notes:Category>",
                xml_escape(category)
            ));
        }
        xml.push_str("</Notes:Categories>");
    }

    if let Some(body) = row.body.as_deref()
        && !body.is_empty()
    {
        xml.push_str(&format!(
            "<AirSyncBase:Body><AirSyncBase:Type>1</AirSyncBase:Type><AirSyncBase:Data>{}</AirSyncBase:Data></AirSyncBase:Body>",
            xml_escape(body)
        ));
    }

    xml
}

/// Render a complete `<Add>` element for a note.
pub fn render_eas_note_add(server_id: &str, row: &NoteRow) -> String {
    format!(
        r#"<Add><ServerId>{}</ServerId><ApplicationData>{}</ApplicationData></Add>"#,
        xml_escape(server_id),
        render_eas_note(row)
    )
}

/// Operation kinds for task/note client mutations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationKind {
    Add,
    Change,
    Delete,
}

#[derive(Debug, Clone)]
pub struct MutationResult {
    pub server_id: String,
    pub status: &'static str,
    pub kind: MutationKind,
    pub client_id: Option<String>,
}

/// A parsed task mutation (Add/Change/Delete) from a Sync `<Collection>`.
#[derive(Debug, Clone)]
pub struct TaskMutation {
    pub kind: MutationKind,
    pub client_id: Option<String>,
    pub server_id: String,
    pub fields: TaskFieldsOwned,
}

/// A parsed note mutation (Add/Change/Delete) from a Sync `<Collection>`.
#[derive(Debug, Clone)]
pub struct NoteMutation {
    pub kind: MutationKind,
    pub client_id: Option<String>,
    pub server_id: String,
    pub fields: NoteFieldsOwned,
}

/// Owned field set for a task, parsed from EAS XML.
#[derive(Debug, Clone, Default)]
pub struct TaskFieldsOwned {
    pub subject: Option<String>,
    pub importance: Option<i64>,
    pub sensitivity: Option<i64>,
    pub start_date: Option<String>,
    pub due_date: Option<String>,
    pub utc_start_date: Option<String>,
    pub utc_due_date: Option<String>,
    pub complete: Option<i64>,
    pub date_completed: Option<String>,
    pub reminder_set: Option<i64>,
    pub reminder_time: Option<String>,
    pub categories: Option<String>,
    pub body: Option<String>,
}

/// Owned field set for a note, parsed from EAS XML.
#[derive(Debug, Clone, Default)]
pub struct NoteFieldsOwned {
    pub subject: Option<String>,
    pub message_class: Option<String>,
    pub body: Option<String>,
    pub categories: Option<String>,
}

/// A namespace-insensitive parse of an EAS Sync collection into raw mutations.
///
/// Each `<Add>/<Change>/<Delete>` becomes a `RawMutation` carrying the single
/// `<ServerId>` / `<ClientId>` (matched by local name, avoiding the op scope) and
/// leaf field values captured while inside `<ApplicationData>`.
#[derive(Debug, Default)]
struct RawMutation {
    kind: Option<MutationKind>,
    server_id: Option<String>,
    client_id: Option<String>,
    fields: Vec<(String, String)>,
}

/// Parse raw mutations from the collection XML body.
///
/// The error from the underlying XML reader is propagated so the caller can
/// reject a malformed collection with a protocol error instead of silently
/// applying a partial command set.
fn parse_raw_mutations(xml: &str) -> Result<Vec<RawMutation>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();

    // Stack of open element local names; used to disambiguate leaf text (ServerId,
    // ClientId) from ApplicationData field text by inspecting the enclosing scope.
    let mut element_stack: Vec<Vec<u8>> = Vec::new();
    let mut op_kind: Option<MutationKind> = None;
    let mut in_app_data = false;
    let mut mutations: Vec<RawMutation> = Vec::new();
    let mut current = RawMutation::default();

    // A tiny struct-free way to remember pending leaf name for the next text event.
    let mut pending_leaf: Option<Vec<u8>> = None;
    // Accumulated text of the current leaf element (resolves entity references).
    let mut leaf_text = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name().local_name().as_ref().as_bytes().to_vec();
                match name.as_slice() {
                    b"Add" => {
                        op_kind = Some(MutationKind::Add);
                        current = RawMutation {
                            kind: Some(MutationKind::Add),
                            ..RawMutation::default()
                        };
                    }
                    b"Change" => {
                        op_kind = Some(MutationKind::Change);
                        current = RawMutation {
                            kind: Some(MutationKind::Change),
                            ..RawMutation::default()
                        };
                    }
                    b"Delete" => {
                        op_kind = Some(MutationKind::Delete);
                        current = RawMutation {
                            kind: Some(MutationKind::Delete),
                            ..RawMutation::default()
                        };
                    }
                    b"ApplicationData" => {
                        in_app_data = true;
                    }
                    _ => {}
                }
                pending_leaf = Some(name.clone());
                element_stack.push(name);
                leaf_text.clear();
            }
            Ok(Event::End(e)) => {
                let name = e.name().local_name().as_ref().as_bytes().to_vec();
                match name.as_slice() {
                    b"Add" | b"Change" | b"Delete" => {
                        if op_kind.is_some() {
                            mutations.push(std::mem::take(&mut current));
                            op_kind = None;
                            in_app_data = false;
                        }
                    }
                    b"ApplicationData" => {
                        in_app_data = false;
                    }
                    _ => {}
                }
                // Pop the matching element off the stack.
                if let Some(pos) = element_stack.iter().rposition(|n| n == &name) {
                    element_stack.truncate(pos);
                }
                // Flush the leaf's accumulated text (entity references already
                // resolved) into the current mutation field.
                if !leaf_text.trim().is_empty()
                    && let Some(leaf) = pending_leaf.as_ref()
                {
                    match leaf.as_slice() {
                        b"ServerId" if !in_app_data => {
                            current.server_id = Some(std::mem::take(&mut leaf_text));
                        }
                        b"ClientId" if !in_app_data => {
                            current.client_id = Some(std::mem::take(&mut leaf_text));
                        }
                        b"Category" if in_app_data => {
                            // Accumulate categories joined by ';'.
                            merge_category(&mut current, &std::mem::take(&mut leaf_text));
                        }
                        _ if in_app_data => {
                            // Leaf field text (Subject, Body, Data, Complete,
                            // Importance, MessageClass, ...). "Body" (Tasks:Body)
                            // and "Data" (AirSyncBase:Body/Data) are both mapped
                            // to the body field in apply_*_field below.
                            let local = String::from_utf8_lossy(leaf).into_owned();
                            current.fields.push((local, std::mem::take(&mut leaf_text)));
                        }
                        _ => {}
                    }
                }
                pending_leaf = None;
                leaf_text.clear();
            }
            Ok(Event::Text(t)) => {
                leaf_text.push_str(t.as_ref());
            }
            Ok(Event::GeneralRef(r)) => {
                leaf_text.push_str(&resolve_xml_reference(r.as_ref()));
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow::anyhow!("Failed to parse Sync XML: {}", e)),
            _ => {}
        }
        buf.clear();
    }

    Ok(mutations)
}

fn merge_category(current: &mut RawMutation, text: &str) {
    let key = "Categories";
    match current.fields.iter_mut().find(|(k, _)| k == key) {
        Some((_, existing)) => {
            if !existing.is_empty() {
                existing.push(';');
            }
            existing.push_str(text);
        }
        None => current.fields.push((key.to_string(), text.to_string())),
    }
}

/// Parse an EAS Sync collection into task mutations.
pub fn parse_task_mutations(xml: &str) -> Result<Vec<TaskMutation>> {
    let parsed = parse_raw_mutations(xml)?
        .into_iter()
        .filter_map(|raw| {
            let kind = raw.kind?;
            let server_id = raw.server_id.unwrap_or_default();
            let mut fields = TaskFieldsOwned::default();
            for (key, value) in raw.fields {
                apply_task_field(&mut fields, &key, &value);
            }
            Some(TaskMutation {
                kind,
                client_id: raw.client_id,
                server_id,
                fields,
            })
        })
        .collect();
    Ok(parsed)
}

/// Parse an EAS Sync collection into note mutations.
pub fn parse_note_mutations(xml: &str) -> Result<Vec<NoteMutation>> {
    let parsed = parse_raw_mutations(xml)?
        .into_iter()
        .filter_map(|raw| {
            let kind = raw.kind?;
            let server_id = raw.server_id.unwrap_or_default();
            let mut fields = NoteFieldsOwned::default();
            for (key, value) in raw.fields {
                apply_note_field(&mut fields, &key, &value);
            }
            Some(NoteMutation {
                kind,
                client_id: raw.client_id,
                server_id,
                fields,
            })
        })
        .collect();
    Ok(parsed)
}

fn apply_task_field(fields: &mut TaskFieldsOwned, key: &str, value: &str) {
    match key {
        "Subject" => fields.subject = Some(value.to_string()),
        "Importance" => fields.importance = value.parse::<i64>().ok(),
        "Sensitivity" => fields.sensitivity = value.parse::<i64>().ok(),
        "StartDate" => fields.start_date = Some(value.to_string()),
        "DueDate" => fields.due_date = Some(value.to_string()),
        "UtcStartDate" => fields.utc_start_date = Some(value.to_string()),
        "UtcDueDate" => fields.utc_due_date = Some(value.to_string()),
        "Complete" => fields.complete = value.parse::<i64>().ok(),
        "DateCompleted" => fields.date_completed = Some(value.to_string()),
        "ReminderSet" => fields.reminder_set = value.parse::<i64>().ok(),
        // ReminderTime is an MS-ASTASK dateTime (ISO 8601), preserved as text.
        "ReminderTime" => fields.reminder_time = Some(value.to_string()),
        "Categories" => fields.categories = Some(value.to_string()),
        // Tasks:Body (a direct text element) and AirSyncBase:Body/Data both carry
        // the task body text; capture either.
        "Body" | "Data" => fields.body = Some(value.to_string()),
        _ => {}
    }
}

fn apply_note_field(fields: &mut NoteFieldsOwned, key: &str, value: &str) {
    match key {
        "Subject" => fields.subject = Some(value.to_string()),
        "MessageClass" => fields.message_class = Some(value.to_string()),
        "Categories" => fields.categories = Some(value.to_string()),
        // AirSyncBase:Body/Data carries the note body text.
        "Body" | "Data" => fields.body = Some(value.to_string()),
        _ => {}
    }
}

/// Apply parsed task mutations.
///
/// With a CalDAV backend configured, every operation is written through to
/// Stalwart FIRST ([MS-ASCMD] §2.2.3.9 semantics: the item is not reported to
/// the client as processed — status 1 — unless the backend accepted it); a
/// backend failure yields status 6 and leaves the mirror untouched so a retry
/// is safe. Without CalDAV, mutations land in the gateway-local store.
pub async fn apply_task_mutations(
    state: &AppState,
    username: &str,
    password: &str,
    mutations: &[TaskMutation],
) -> Result<Vec<MutationResult>> {
    let backend_configured = !state.cfg.caldav_base.is_empty();
    let backend = if backend_configured {
        match TaskBackendStore::for_user(state, username, password, "Tasks", &["VTODO"]).await {
            Ok(b) => Some(b),
            Err(e) => {
                tracing::warn!(error = %e, "Tasks CalDAV backend unavailable");
                None
            }
        }
    } else {
        None
    };
    let mut results = Vec::with_capacity(mutations.len());
    for m in mutations {
        results.push(
            apply_task_mutation(
                state,
                username,
                password,
                backend.as_ref(),
                backend_configured,
                m,
            )
            .await,
        );
    }
    Ok(results)
}

/// Apply parsed note mutations (backend routing as for tasks).
pub async fn apply_note_mutations(
    state: &AppState,
    username: &str,
    password: &str,
    mutations: &[NoteMutation],
) -> Result<Vec<MutationResult>> {
    let backend_configured = !state.cfg.caldav_base.is_empty();
    let backend = if backend_configured {
        match TaskBackendStore::for_user(state, username, password, "Notes", &["VJOURNAL"]).await {
            Ok(b) => Some(b),
            Err(e) => {
                tracing::warn!(error = %e, "Notes CalDAV backend unavailable");
                None
            }
        }
    } else {
        None
    };
    let mut results = Vec::with_capacity(mutations.len());
    for m in mutations {
        results.push(
            apply_note_mutation(
                state,
                username,
                password,
                backend.as_ref(),
                backend_configured,
                m,
            )
            .await,
        );
    }
    Ok(results)
}

/// Handle one parsed task mutation.
///
/// Backend routing ([MS-ASCMD] §2.2.3.9.4): a task that lives in Stalwart
/// (mirrored, i.e. `caldav_href` set) is written through to CalDAV first —
/// backend failure yields status 6 and the mirror keeps its previous state.
/// A backend-configured gateway that cannot reach the backend fails closed for
/// Add/Change/Delete of mirrored rows (status 6) rather than diverging.
/// Unmirrored rows (pre-backend records) take the local path; the next
/// reconcile pushes them into Stalwart. With no CalDAV backend configured the
/// gateway-local store is authoritative, as before.
async fn apply_task_mutation(
    state: &AppState,
    username: &str,
    password: &str,
    backend: Option<&TaskBackendStore>,
    backend_configured: bool,
    m: &TaskMutation,
) -> MutationResult {
    let make = |status: &'static str| MutationResult {
        server_id: m.server_id.clone(),
        status,
        kind: m.kind,
        client_id: m.client_id.clone(),
    };
    match m.kind {
        MutationKind::Delete => {
            if m.server_id.is_empty() {
                return MutationResult {
                    server_id: String::new(),
                    status: "6",
                    kind: MutationKind::Delete,
                    client_id: m.client_id.clone(),
                };
            }
            let existing = match state.storage.get_task(username, &m.server_id).await {
                Ok(row) => row,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to load task for delete");
                    return make("6");
                }
            };
            // Mirrored row → delete the backing VTODO first.
            if let (Some(b), Some(row)) = (backend, existing.as_ref())
                && row.caldav_href.is_some()
            {
                match b
                    .caldav
                    .delete_calendar_resource_if_absent(
                        row.caldav_href.as_deref().unwrap_or_default(),
                        username,
                        password,
                    )
                    .await
                {
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "Backend task delete failed");
                        return make("6");
                    }
                }
            }
            // A mirrored row with the backend configured but unreachable
            // fails closed: deleting the mirror alone would let the next
            // reconcile re-import the still-live VTODO as a "new" item.
            if backend.is_none()
                && backend_configured
                && existing.as_ref().is_some_and(|r| r.caldav_href.is_some())
            {
                return make("6");
            }
            match state.storage.delete_task(username, &m.server_id).await {
                Ok(0) => make("8"),
                Ok(_) => make("1"),
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to delete task");
                    make("6")
                }
            }
        }
        MutationKind::Add => {
            let server_id = if m.server_id.is_empty() {
                format!("task-{}", Uuid::new_v4().simple())
            } else {
                m.server_id.clone()
            };
            let result_for = |status: &'static str| MutationResult {
                server_id: server_id.clone(),
                status,
                kind: MutationKind::Add,
                client_id: m.client_id.clone(),
            };
            if let Some(b) = backend {
                // Write-through: create the VTODO in Stalwart, mirror only on
                // success. The EAS ServerId becomes the iCalendar UID, so any
                // CalDAV client sees a stable identity.
                let uid = server_id.clone();
                let mirror = TaskMirror::from_mutation(m, None);
                let ics = build_vtodo(&uid, &mirror);
                let filename = resource_filename(&server_id);
                match b
                    .caldav
                    .put_event(
                        &b.collection,
                        Some(&filename),
                        &ics,
                        username,
                        password,
                        None,
                    )
                    .await
                {
                    Ok((href, etag)) => {
                        let fields = TaskFields {
                            caldav_href: Some(href.as_str()),
                            etag: Some(etag.as_str()),
                            uid: Some(uid.as_str()),
                            ..mirror.fields()
                        };
                        match state
                            .storage
                            .upsert_task(username, &server_id, &fields)
                            .await
                        {
                            Ok(()) => result_for("1"),
                            Err(e) => {
                                tracing::warn!(error = %e, "Task mirror upsert failed");
                                result_for("6")
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Backend task add failed");
                        result_for("6")
                    }
                }
            } else if backend_configured {
                // Backend configured but unreachable: fail closed. Recording
                // the add locally would diverge (and a client retry would
                // duplicate it once the backend returns).
                result_for("6")
            } else {
                let mirror = TaskMirror::from_mutation(m, None);
                let fields = mirror.fields();
                match state
                    .storage
                    .upsert_task(username, &server_id, &fields)
                    .await
                {
                    Ok(()) => result_for("1"),
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to upsert task");
                        result_for("6")
                    }
                }
            }
        }
        MutationKind::Change => {
            if m.server_id.is_empty() {
                return MutationResult {
                    server_id: String::new(),
                    status: "6",
                    kind: MutationKind::Change,
                    client_id: m.client_id.clone(),
                };
            }
            let existing = match state.storage.get_task(username, &m.server_id).await {
                Ok(Some(row)) => row,
                Ok(None) => return make("8"),
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to load task for change");
                    return make("6");
                }
            };
            // Merge: only overwrite the properties the client actually sent.
            let mirror = TaskMirror::from_mutation(m, Some(&existing));
            match (backend, existing.caldav_href.as_deref()) {
                (Some(b), Some(href)) => {
                    // Fetch the authoritative VTODO, rewrite only the mapped
                    // property lines, and PUT back with an If-Match so a
                    // concurrent remote write is not silently clobbered.
                    match b
                        .caldav
                        .get_calendar_resource(href, username, password)
                        .await
                    {
                        Ok(None) => make("8"),
                        Ok(Some((ics, _))) => {
                            let patched = patch_component_ics(
                                &ics,
                                "VTODO",
                                VTODO_MAPPED,
                                &mirror.vtodo_lines(Some(&ics)),
                                existing.uid.as_deref(),
                            )
                            .unwrap_or_else(|e| {
                                tracing::warn!(error = %e, "VTODO patch failed; rewriting whole object");
                                build_vtodo(mirror.uid.as_deref().unwrap_or(&m.server_id), &mirror)
                            });
                            match b
                                .caldav
                                .put_event(
                                    &b.collection,
                                    Some(href),
                                    &patched,
                                    username,
                                    password,
                                    existing.etag.as_deref(),
                                )
                                .await
                            {
                                Ok((new_href, new_etag)) => {
                                    let fields = TaskFields {
                                        caldav_href: Some(new_href.as_str()),
                                        etag: Some(new_etag.as_str()),
                                        uid: mirror.uid.as_deref(),
                                        ..mirror.fields()
                                    };
                                    match state
                                        .storage
                                        .upsert_task(username, &m.server_id, &fields)
                                        .await
                                    {
                                        Ok(()) => make("1"),
                                        Err(e) => {
                                            tracing::warn!(
                                                error = %e,
                                                "Task mirror upsert failed"
                                            );
                                            make("6")
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "Backend task change failed");
                                    make("6")
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "Backend task fetch failed");
                            make("6")
                        }
                    }
                }
                // Unmirrored (legacy) row or backend not configured: the local
                // store remains authoritative for this row; a reconcile pushes
                // it to Stalwart when one becomes reachable.
                _ if !backend_configured || existing.caldav_href.is_none() => {
                    let fields = TaskFields {
                        caldav_href: existing.caldav_href.as_deref(),
                        etag: existing.etag.as_deref(),
                        uid: existing.uid.as_deref(),
                        ..mirror.fields()
                    };
                    match state
                        .storage
                        .upsert_task(username, &m.server_id, &fields)
                        .await
                    {
                        Ok(()) => make("1"),
                        Err(e) => {
                            tracing::warn!(error = %e, "Failed to update task");
                            make("6")
                        }
                    }
                }
                // Mirrored row but backend unreachable: fail closed.
                _ => make("6"),
            }
        }
    }
}

/// Handle one parsed note mutation (backend routing as for tasks).
async fn apply_note_mutation(
    state: &AppState,
    username: &str,
    password: &str,
    backend: Option<&TaskBackendStore>,
    backend_configured: bool,
    m: &NoteMutation,
) -> MutationResult {
    let make = |status: &'static str| MutationResult {
        server_id: m.server_id.clone(),
        status,
        kind: m.kind,
        client_id: m.client_id.clone(),
    };
    match m.kind {
        MutationKind::Delete => {
            if m.server_id.is_empty() {
                return MutationResult {
                    server_id: String::new(),
                    status: "6",
                    kind: MutationKind::Delete,
                    client_id: m.client_id.clone(),
                };
            }
            let existing = match state.storage.get_note(username, &m.server_id).await {
                Ok(row) => row,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to load note for delete");
                    return make("6");
                }
            };
            if let (Some(b), Some(row)) = (backend, existing.as_ref())
                && row.caldav_href.is_some()
            {
                match b
                    .caldav
                    .delete_calendar_resource_if_absent(
                        row.caldav_href.as_deref().unwrap_or_default(),
                        username,
                        password,
                    )
                    .await
                {
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "Backend note delete failed");
                        return make("6");
                    }
                }
            }
            // A mirrored row with the backend configured but unreachable
            // fails closed: deleting the mirror alone would let the next
            // reconcile re-import the still-live VJOURNAL as a "new" item.
            if backend.is_none()
                && backend_configured
                && existing.as_ref().is_some_and(|r| r.caldav_href.is_some())
            {
                return make("6");
            }
            match state.storage.delete_note(username, &m.server_id).await {
                Ok(0) => make("8"),
                Ok(_) => make("1"),
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to delete note");
                    make("6")
                }
            }
        }
        MutationKind::Add => {
            let server_id = if m.server_id.is_empty() {
                format!("note-{}", Uuid::new_v4().simple())
            } else {
                m.server_id.clone()
            };
            let result_for = |status: &'static str| MutationResult {
                server_id: server_id.clone(),
                status,
                kind: MutationKind::Add,
                client_id: m.client_id.clone(),
            };
            if let Some(b) = backend {
                let uid = server_id.clone();
                let last_modified = now_utc_string();
                let mut mirror = NoteMirror::from_mutation(m, None, &last_modified);
                mirror.uid = Some(uid.clone());
                let ics = build_vjournal(&uid, &mirror);
                let filename = resource_filename(&server_id);
                match b
                    .caldav
                    .put_event(
                        &b.collection,
                        Some(&filename),
                        &ics,
                        username,
                        password,
                        None,
                    )
                    .await
                {
                    Ok((href, etag)) => {
                        let fields = NoteFields {
                            caldav_href: Some(href.as_str()),
                            etag: Some(etag.as_str()),
                            uid: Some(uid.as_str()),
                            ..mirror.fields()
                        };
                        match state
                            .storage
                            .upsert_note(username, &server_id, &fields)
                            .await
                        {
                            Ok(()) => result_for("1"),
                            Err(e) => {
                                tracing::warn!(error = %e, "Note mirror upsert failed");
                                result_for("6")
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Backend note add failed");
                        result_for("6")
                    }
                }
            } else if backend_configured {
                result_for("6")
            } else {
                let last_modified = now_utc_string();
                let mirror = NoteMirror::from_mutation(m, None, &last_modified);
                let fields = mirror.fields();
                match state
                    .storage
                    .upsert_note(username, &server_id, &fields)
                    .await
                {
                    Ok(()) => result_for("1"),
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to upsert note");
                        result_for("6")
                    }
                }
            }
        }
        MutationKind::Change => {
            if m.server_id.is_empty() {
                return MutationResult {
                    server_id: String::new(),
                    status: "6",
                    kind: MutationKind::Change,
                    client_id: m.client_id.clone(),
                };
            }
            let existing = match state.storage.get_note(username, &m.server_id).await {
                Ok(Some(row)) => row,
                Ok(None) => return make("8"),
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to load note for change");
                    return make("6");
                }
            };
            let last_modified = now_utc_string();
            let mirror = NoteMirror::from_mutation(m, Some(&existing), &last_modified);
            match (backend, existing.caldav_href.as_deref()) {
                (Some(b), Some(href)) => {
                    match b
                        .caldav
                        .get_calendar_resource(href, username, password)
                        .await
                    {
                        Ok(None) => make("8"),
                        Ok(Some((ics, _))) => {
                            let patched = patch_component_ics(
                                &ics,
                                "VJOURNAL",
                                VJOURNAL_MAPPED,
                                &mirror.vjournal_lines(),
                                existing.uid.as_deref(),
                            )
                            .unwrap_or_else(|e| {
                                tracing::warn!(error = %e, "VJOURNAL patch failed; rewriting whole object");
                                build_vjournal(mirror.uid.as_deref().unwrap_or(&m.server_id), &mirror)
                            });
                            match b
                                .caldav
                                .put_event(
                                    &b.collection,
                                    Some(href),
                                    &patched,
                                    username,
                                    password,
                                    existing.etag.as_deref(),
                                )
                                .await
                            {
                                Ok((new_href, new_etag)) => {
                                    let fields = NoteFields {
                                        caldav_href: Some(new_href.as_str()),
                                        etag: Some(new_etag.as_str()),
                                        uid: mirror.uid.as_deref(),
                                        ..mirror.fields()
                                    };
                                    match state
                                        .storage
                                        .upsert_note(username, &m.server_id, &fields)
                                        .await
                                    {
                                        Ok(()) => make("1"),
                                        Err(e) => {
                                            tracing::warn!(
                                                error = %e,
                                                "Note mirror upsert failed"
                                            );
                                            make("6")
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "Backend note change failed");
                                    make("6")
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "Backend note fetch failed");
                            make("6")
                        }
                    }
                }
                _ if !backend_configured || existing.caldav_href.is_none() => {
                    let fields = NoteFields {
                        caldav_href: existing.caldav_href.as_deref(),
                        etag: existing.etag.as_deref(),
                        uid: existing.uid.as_deref(),
                        ..mirror.fields()
                    };
                    match state
                        .storage
                        .upsert_note(username, &m.server_id, &fields)
                        .await
                    {
                        Ok(()) => make("1"),
                        Err(e) => {
                            tracing::warn!(error = %e, "Failed to update note");
                            make("6")
                        }
                    }
                }
                _ => make("6"),
            }
        }
    }
}

// ===================== Stalwart CalDAV backing (§15) =====================
//
// Stalwart has no native Tasks/Notes object, but its CalDAV store holds the
// full iCalendar component set — the gateway maps EAS Tasks to VTODOs
// (RFC 5545 §3.6.2) and EAS Notes to VJOURNALs (§3.6.3) in per-user
// collections (`/{cal}/{user}/Tasks/`, `/{cal}/{user}/Notes/`). Stalwart is
// the system of record: writes go through to CalDAV first, and every Sync
// reconciles the mirror against a calendar-query REPORT of the collection
// so items created or edited by ANY CalDAV client flow back to EAS.

/// An ensured CalDAV collection plus the client that reaches it.
pub struct TaskBackendStore {
    pub caldav: crate::caldav::CaldavClient,
    /// Collection href the store is bound to (with trailing slash).
    pub collection: String,
}

impl TaskBackendStore {
    /// Build the CalDAV client and make sure the user's collection exists
    /// (MKCALENDAR on first use, per RFC 4791 §5.3.1).
    pub async fn for_user(
        state: &AppState,
        username: &str,
        password: &str,
        name: &str,
        components: &[&str],
    ) -> Result<Self> {
        let caldav = crate::caldav::CaldavClient::new(&state.cfg)?;
        caldav
            .ensure_calendar_collection(username, password, name, components, name)
            .await?;
        let collection = if name == "Notes" {
            caldav.notes_collection_href(username)
        } else {
            caldav.tasks_collection_href(username)
        };
        Ok(Self { caldav, collection })
    }
}

/// Owned MS-ASTASK projection of one task — the common currency between the
/// EAS mutation path, the iCalendar bridge, and the mirror row.
#[derive(Debug, Default, Clone)]
struct TaskMirror {
    subject: Option<String>,
    importance: Option<i64>,
    sensitivity: Option<i64>,
    start_date: Option<String>,
    due_date: Option<String>,
    utc_start_date: Option<String>,
    utc_due_date: Option<String>,
    complete: i64,
    date_completed: Option<String>,
    reminder_set: i64,
    reminder_time: Option<String>,
    categories: Option<String>,
    body: Option<String>,
    uid: Option<String>,
}

impl TaskMirror {
    /// Merge a client mutation into the existing row; only properties the
    /// client actually sent overwrite the stored values ([MS-ASCMD]
    /// §2.2.3.9: a Change "can include one or more of the elements" and
    /// omitted elements keep their previous value).
    fn from_mutation(m: &TaskMutation, existing: Option<&TaskRow>) -> Self {
        let f = &m.fields;
        match existing {
            None => Self {
                subject: f.subject.clone(),
                importance: f.importance,
                sensitivity: f.sensitivity,
                start_date: f.start_date.clone(),
                due_date: f.due_date.clone(),
                utc_start_date: f.utc_start_date.clone(),
                utc_due_date: f.utc_due_date.clone(),
                complete: f.complete.unwrap_or(0),
                date_completed: f.date_completed.clone(),
                reminder_set: f.reminder_set.unwrap_or(0),
                reminder_time: f.reminder_time.clone(),
                categories: f.categories.clone(),
                body: f.body.clone(),
                uid: None,
            },
            Some(row) => Self {
                subject: f.subject.clone().or_else(|| row.subject.clone()),
                importance: f.importance.or(row.importance),
                sensitivity: f.sensitivity.or(row.sensitivity),
                start_date: f.start_date.clone().or_else(|| row.start_date.clone()),
                due_date: f.due_date.clone().or_else(|| row.due_date.clone()),
                utc_start_date: f
                    .utc_start_date
                    .clone()
                    .or_else(|| row.utc_start_date.clone()),
                utc_due_date: f.utc_due_date.clone().or_else(|| row.utc_due_date.clone()),
                complete: f.complete.unwrap_or(row.complete),
                date_completed: f
                    .date_completed
                    .clone()
                    .or_else(|| row.date_completed.clone()),
                reminder_set: f.reminder_set.unwrap_or(row.reminder_set),
                reminder_time: f
                    .reminder_time
                    .clone()
                    .or_else(|| row.reminder_time.clone()),
                categories: f.categories.clone().or_else(|| row.categories.clone()),
                body: f.body.clone().or_else(|| row.body.clone()),
                uid: row.uid.clone(),
            },
        }
    }

    fn from_row(row: &TaskRow) -> Self {
        Self {
            subject: row.subject.clone(),
            importance: row.importance,
            sensitivity: row.sensitivity,
            start_date: row.start_date.clone(),
            due_date: row.due_date.clone(),
            utc_start_date: row.utc_start_date.clone(),
            utc_due_date: row.utc_due_date.clone(),
            complete: row.complete,
            date_completed: row.date_completed.clone(),
            reminder_set: row.reminder_set,
            reminder_time: row.reminder_time.clone(),
            categories: row.categories.clone(),
            body: row.body.clone(),
            uid: row.uid.clone(),
        }
    }

    fn fields(&self) -> TaskFields<'_> {
        TaskFields {
            subject: self.subject.as_deref(),
            importance: self.importance,
            sensitivity: self.sensitivity,
            start_date: self.start_date.as_deref(),
            due_date: self.due_date.as_deref(),
            utc_start_date: self.utc_start_date.as_deref(),
            utc_due_date: self.utc_due_date.as_deref(),
            complete: self.complete,
            date_completed: self.date_completed.as_deref(),
            reminder_set: self.reminder_set,
            reminder_time: self.reminder_time.as_deref(),
            categories: self.categories.as_deref(),
            body: self.body.as_deref(),
            caldav_href: None,
            etag: None,
            uid: self.uid.as_deref(),
        }
    }

    /// iCalendar property lines for the VTODO, with `old_ics` (when patching
    /// an existing object) consulted for properties whose EAS projection is
    /// contextual: a task being un-completed keeps a foreign STATUS such as
    /// IN-PROCESS, but a stored COMPLETED status is reset to NEEDS-ACTION.
    fn vtodo_lines(&self, old_ics: Option<&str>) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(s) = &self.subject {
            lines.push(format!("SUMMARY:{}", escape_ical_text(s)));
        }
        if let Some(b) = &self.body {
            lines.push(format!("DESCRIPTION:{}", escape_ical_text(b)));
        }
        if let Some(imp) = self.importance {
            lines.push(format!("PRIORITY:{}", importance_to_priority(imp)));
        }
        if let Some(s) = self.sensitivity {
            lines.push(format!("CLASS:{}", sensitivity_to_class(s)));
        }
        if self.complete == 1 {
            lines.push("STATUS:COMPLETED".to_string());
            let completed = self
                .date_completed
                .as_deref()
                .and_then(eas_datetime_to_ical_utc)
                .unwrap_or_else(now_ical_utc);
            lines.push(format!("COMPLETED:{completed}"));
        } else if let Some(old) = old_ics
            .and_then(|ics| parse_component_block_ics("VTODO", ics))
            .as_deref()
            .and_then(status_of_component)
            && old == "COMPLETED"
        {
            lines.push("STATUS:NEEDS-ACTION".to_string());
        }
        // The wire-exact EAS date strings travel in X-SGW-* properties and the
        // absolute instant in DTSTART/DUE, so a gateway round-trip preserves
        // both the local wall clock and the UTC form the client sent.
        if let Some(sd) = &self.start_date {
            lines.push(format!("X-SGW-STARTDATE:{}", escape_ical_text(sd)));
        }
        if let Some(ud) = &self.utc_start_date {
            lines.push(format!("X-SGW-UTCSTARTDATE:{}", escape_ical_text(ud)));
        }
        if let Some(dd) = &self.due_date {
            lines.push(format!("X-SGW-DUEDATE:{}", escape_ical_text(dd)));
        }
        if let Some(ud) = &self.utc_due_date {
            lines.push(format!("X-SGW-UTCDUEDATE:{}", escape_ical_text(ud)));
        }
        if let Some(dt) = self
            .utc_start_date
            .as_deref()
            .and_then(eas_datetime_to_ical_utc)
            .or_else(|| {
                self.start_date
                    .as_deref()
                    .and_then(eas_datetime_to_ical_utc)
            })
        {
            lines.push(format!("DTSTART:{dt}"));
        }
        if let Some(dt) = self
            .utc_due_date
            .as_deref()
            .and_then(eas_datetime_to_ical_utc)
            .or_else(|| self.due_date.as_deref().and_then(eas_datetime_to_ical_utc))
        {
            lines.push(format!("DUE:{dt}"));
        }
        if let Some(cats) = &self.categories {
            let joined = cats
                .split(';')
                .filter(|c| !c.is_empty())
                .map(escape_ical_text)
                .collect::<Vec<_>>()
                .join(",");
            if !joined.is_empty() {
                lines.push(format!("CATEGORIES:{joined}"));
            }
        }
        if self.reminder_set == 1
            && let Some(trigger) = self
                .reminder_time
                .as_deref()
                .and_then(eas_datetime_to_ical_utc)
        {
            lines.push("BEGIN:VALARM".to_string());
            lines.push("ACTION:DISPLAY".to_string());
            lines.push(format!(
                "DESCRIPTION:{}",
                escape_ical_text(self.subject.as_deref().unwrap_or("Reminder"))
            ));
            lines.push(format!("TRIGGER;VALUE=DATE-TIME:{trigger}"));
            lines.push("END:VALARM".to_string());
        }
        lines
    }
}

/// Owned MS-ASNOTE projection of one note.
#[derive(Debug, Default, Clone)]
struct NoteMirror {
    subject: Option<String>,
    message_class: Option<String>,
    body: Option<String>,
    categories: Option<String>,
    last_modified_date: Option<String>,
    uid: Option<String>,
}

impl NoteMirror {
    fn from_mutation(m: &NoteMutation, existing: Option<&NoteRow>, last_modified: &str) -> Self {
        let f = &m.fields;
        match existing {
            None => Self {
                subject: f.subject.clone(),
                message_class: f.message_class.clone(),
                body: f.body.clone(),
                categories: f.categories.clone(),
                last_modified_date: Some(last_modified.to_string()),
                uid: None,
            },
            Some(row) => Self {
                subject: f.subject.clone().or_else(|| row.subject.clone()),
                message_class: f
                    .message_class
                    .clone()
                    .or_else(|| row.message_class.clone()),
                body: f.body.clone().or_else(|| row.body.clone()),
                categories: f.categories.clone().or_else(|| row.categories.clone()),
                last_modified_date: Some(last_modified.to_string()),
                uid: row.uid.clone(),
            },
        }
    }

    fn from_row(row: &NoteRow) -> Self {
        Self {
            subject: row.subject.clone(),
            message_class: row.message_class.clone(),
            body: row.body.clone(),
            categories: row.categories.clone(),
            last_modified_date: row.last_modified_date.clone(),
            uid: row.uid.clone(),
        }
    }

    fn fields(&self) -> NoteFields<'_> {
        NoteFields {
            subject: self.subject.as_deref(),
            message_class: self.message_class.as_deref(),
            body: self.body.as_deref(),
            categories: self.categories.as_deref(),
            last_modified_date: self.last_modified_date.as_deref(),
            caldav_href: None,
            etag: None,
            uid: self.uid.as_deref(),
        }
    }

    fn vjournal_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(s) = &self.subject {
            lines.push(format!("SUMMARY:{}", escape_ical_text(s)));
        }
        if let Some(b) = &self.body {
            lines.push(format!("DESCRIPTION:{}", escape_ical_text(b)));
        }
        if let Some(cats) = &self.categories {
            let joined = cats
                .split(';')
                .filter(|c| !c.is_empty())
                .map(escape_ical_text)
                .collect::<Vec<_>>()
                .join(",");
            if !joined.is_empty() {
                lines.push(format!("CATEGORIES:{joined}"));
            }
        }
        // IPM.StickyNote is the Outlook default; other message classes ride
        // along verbatim so a round-trip keeps what the client declared.
        lines.push(format!(
            "X-SGW-MESSAGECLASS:{}",
            escape_ical_text(self.message_class.as_deref().unwrap_or("IPM.StickyNote"))
        ));
        if let Some(lm) = &self.last_modified_date {
            lines.push(format!("X-SGW-LASTMODIFIED:{}", escape_ical_text(lm)));
        }
        lines
    }
}

// ---------------------------- iCalendar helpers ---------------------------

/// The property names of a VTODO that the EAS projection owns; everything
/// else in a backend object survives a gateway Change untouched.
const VTODO_MAPPED: &[&str] = &[
    "SUMMARY",
    "DESCRIPTION",
    "PRIORITY",
    "CLASS",
    "STATUS",
    "COMPLETED",
    "DTSTART",
    "DUE",
    "CATEGORIES",
    "X-SGW-STARTDATE",
    "X-SGW-UTCSTARTDATE",
    "X-SGW-DUEDATE",
    "X-SGW-UTCDUEDATE",
    "VALARM",
];

/// The property names of a VJOURNAL that the EAS projection owns.
const VJOURNAL_MAPPED: &[&str] = &[
    "SUMMARY",
    "DESCRIPTION",
    "CATEGORIES",
    "X-SGW-MESSAGECLASS",
    "X-SGW-LASTMODIFIED",
];

/// Escape TEXT property values per RFC 5545 §3.3.11.
fn escape_ical_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            _ => out.push(c),
        }
    }
    out
}

/// Current instant as an iCalendar UTC DATE-TIME (YYYYMMDDTHHMMSSZ).
fn now_ical_utc() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

/// EAS dateTime ([MS-ASDTYPE] §2.3) → iCalendar UTC DATE-TIME.
/// A value without a zone designator is interpreted as UTC.
fn eas_datetime_to_ical_utc(value: &str) -> Option<String> {
    let v = value.trim();
    let stripped = v.strip_suffix('Z').unwrap_or(v);
    let no_frac = match stripped.find('.') {
        Some(pos) => &stripped[..pos],
        None => stripped,
    };
    for fmt in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y%m%dT%H%M%S"] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(no_frac, fmt) {
            return Some(dt.format("%Y%m%dT%H%M%SZ").to_string());
        }
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(no_frac, "%Y-%m-%d") {
        return Some(d.format("%Y%m%dT000000Z").to_string());
    }
    None
}

/// iCalendar DATE-TIME → EAS UTC dateTime string.
fn ical_utc_to_eas(value: &str) -> Option<String> {
    let v = value.trim().trim_end_matches('Z');
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S") {
        return Some(format!("{}Z", dt.format("%Y-%m-%dT%H:%M:%S.000")));
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%S") {
        return Some(format!("{}Z", dt.format("%Y-%m-%dT%H:%M:%S.000")));
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(v, "%Y%m%d") {
        return Some(format!("{}Z", d.format("%Y-%m-%dT00:00:00.000")));
    }
    None
}

/// iCalendar DATE-TIME → EAS local (floating) dateTime string.
fn ical_floating_to_eas(value: &str) -> Option<String> {
    let v = value.trim().trim_end_matches('Z');
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S") {
        return Some(dt.format("%Y-%m-%dT%H:%M:%S.000").to_string());
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(v, "%Y%m%d") {
        return Some(d.format("%Y-%m-%dT00:00:00.000").to_string());
    }
    None
}

/// MS-ASTASK Importance (0=Low, 1=Normal, 2=High) → RFC 5545 PRIORITY
/// ([MS-OXCIMAL §2.1.3.2 / MS-OXCICAL]; RFC 5545: 1-4 high, 5 normal, 6-9 low).
fn importance_to_priority(importance: i64) -> u8 {
    match importance {
        2 => 1,
        1 => 5,
        _ => 9,
    }
}

fn priority_to_importance(priority: &str) -> Option<i64> {
    let p: u8 = priority.trim().parse().ok()?;
    match p {
        1..=4 => Some(2),
        5 => Some(1),
        6..=9 => Some(0),
        _ => None,
    }
}

/// MS-ASTASK Sensitivity (0=Normal, 1=Personal, 2=Private, 3=Confidential) →
/// RFC 5545 CLASS. "Personal" has no RFC equivalent and travels as the
/// experimental X-PERSONAL.
fn sensitivity_to_class(sensitivity: i64) -> &'static str {
    match sensitivity {
        1 => "X-PERSONAL",
        2 => "PRIVATE",
        3 => "CONFIDENTIAL",
        _ => "PUBLIC",
    }
}

fn class_to_sensitivity(class: &str) -> Option<i64> {
    match class.trim() {
        "PUBLIC" => Some(0),
        "X-PERSONAL" => Some(1),
        "PRIVATE" => Some(2),
        "CONFIDENTIAL" => Some(3),
        _ => None,
    }
}

/// Split an iCalendar CATEGORIES value into EAS ';'-joined form.
fn categories_ical_to_eas(value: &str) -> String {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for c in value.chars() {
        if escaped {
            current.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == ',' {
            out.push(crate::ical_parser::unescape_ical_text(&std::mem::take(
                &mut current,
            )));
        } else {
            current.push(c);
        }
    }
    out.push(crate::ical_parser::unescape_ical_text(&current));
    out.into_iter()
        .filter(|c| !c.is_empty())
        .collect::<Vec<_>>()
        .join(";")
}

/// Unfold an iCalendar document into logical lines (CRLF stripped).
fn unfolded_lines(ics: &str) -> Vec<String> {
    crate::ical_parser::unfold_ical_content(ics)
        .lines()
        .map(|l| l.trim_end_matches('\r').to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// The property NAME of a content line (before ';' or ':'), upper-cased.
fn property_name_of(line: &str) -> String {
    let end = line.find([';', ':']).unwrap_or(line.len());
    line[..end].to_uppercase()
}

/// The raw (still escaped) value of a content line.
fn property_value_of(line: &str) -> &str {
    line.find(':').map(|p| &line[p + 1..]).unwrap_or("")
}

/// Extract the first component block of one type as unfolded lines,
/// excluding the BEGIN/END markers.
fn parse_component_block_ics(component: &str, ics: &str) -> Option<Vec<String>> {
    let lines = unfolded_lines(ics);
    let begin = format!("BEGIN:{component}");
    let end = format!("END:{component}");
    let start = lines.iter().position(|l| l == &begin)?;
    let stop = lines[start + 1..].iter().position(|l| l == &end)? + start + 1;
    Some(lines[start + 1..stop].to_vec())
}

fn component_property<'a>(block: &'a [String], name: &str) -> Option<&'a str> {
    block
        .iter()
        .find(|l| property_name_of(l) == name)
        .map(|l| property_value_of(l))
}

fn status_of_component(block: &[String]) -> Option<String> {
    component_property(block, "STATUS")
        .map(|v| crate::ical_parser::unescape_ical_text(v).to_uppercase())
}

fn uid_of_ics(component: &str, ics: &str) -> Option<String> {
    parse_component_block_ics(component, ics)
        .and_then(|b| component_property(&b, "UID").map(|v| v.to_string()))
        .map(|v| crate::ical_parser::unescape_ical_text(&v))
}

/// Fold content lines at 75 octets per RFC 5545 §3.1, joining with CRLF.
/// Folding never splits a UTF-8 sequence (the boundary is char-based and the
/// budget is octet-based, the RFC §3.1 normative requirement).
fn fold_ical_document(lines: &[String]) -> String {
    let mut out = String::new();
    for line in lines {
        let mut current_line = String::new();
        let mut octets = 0usize;
        let mut first = true;
        for ch in line.chars() {
            let ch_octets = ch.len_utf8();
            // First segment may reach 75 octets; continuations spend one
            // octet on the leading space.
            let budget = if first { 75 } else { 74 };
            if octets + ch_octets > budget {
                out.push_str(current_line.trim_end());
                out.push_str("\r\n ");
                current_line.clear();
                octets = 0;
                first = false;
            }
            current_line.push(ch);
            octets += ch_octets;
        }
        out.push_str(current_line.trim_end());
        out.push_str("\r\n");
    }
    out
}

/// Build a complete VCALENDAR/VTODO document from a mirror.
fn build_vtodo(uid: &str, mirror: &TaskMirror) -> String {
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//exchange-gateway//Tasks 1.0//EN".to_string(),
        "BEGIN:VTODO".to_string(),
        format!("UID:{uid}"),
        format!("DTSTAMP:{}", now_ical_utc()),
    ];
    lines.extend(mirror.vtodo_lines(None));
    lines.push("END:VTODO".to_string());
    lines.push("END:VCALENDAR".to_string());
    fold_ical_document(&lines)
}

/// Build a complete VCALENDAR/VJOURNAL document from a mirror.
fn build_vjournal(uid: &str, mirror: &NoteMirror) -> String {
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//exchange-gateway//Notes 1.0//EN".to_string(),
        "BEGIN:VJOURNAL".to_string(),
        format!("UID:{uid}"),
        format!("DTSTAMP:{}", now_ical_utc()),
    ];
    let vj = mirror.vjournal_lines();
    lines.extend(vj);
    // DTSTART keeps the note's "entry date" sensible for CalDAV clients.
    if let Some(stamp) = mirror
        .last_modified_date
        .as_deref()
        .and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ").ok())
        .map(|dt| dt.format("%Y%m%dT%H%M%SZ").to_string())
    {
        lines.push(format!("DTSTART:{stamp}"));
    }
    lines.push("END:VJOURNAL".to_string());
    lines.push("END:VCALENDAR".to_string());
    fold_ical_document(&lines)
}

/// Rewrite ONLY the mapped property lines of one component block in an
/// existing iCalendar document; every other line survives verbatim, so a
/// gateway Change can never destroy backend properties EAS does not model
/// (recurrence rules, attendees, URLs, …). `new_lines` are unfolded content
/// lines (possibly a full BEGIN:VALARM…END:VALARM block).
fn patch_component_ics(
    ics: &str,
    component: &str,
    mapped: &[&str],
    new_lines: &[String],
    uid: Option<&str>,
) -> Result<String> {
    let lines = unfolded_lines(ics);
    let begin = format!("BEGIN:{component}");
    let end = format!("END:{component}");
    let start = lines
        .iter()
        .position(|l| l == &begin)
        .ok_or_else(|| anyhow::anyhow!("no {component} block in backend object"))?;
    let stop = lines[start + 1..]
        .iter()
        .position(|l| l == &end)
        .ok_or_else(|| anyhow::anyhow!("unterminated {component} block"))?
        + start
        + 1;

    // Lines inside the component that belong to the EAS projection. A line
    // inside a VALARM sub-block is owned when VALARM is mapped, because the
    // alarm is rewritten as a unit.
    let mut in_alarm = false;
    let mut removed: HashSet<usize> = HashSet::new();
    for (i, line) in lines.iter().enumerate().take(stop).skip(start + 1) {
        let name = property_name_of(line);
        if name == "BEGIN" && property_value_of(line) == "VALARM" {
            in_alarm = true;
        }
        if (in_alarm && mapped.contains(&"VALARM")) || mapped.contains(&name.as_str()) {
            removed.insert(i);
        }
        if name == "END" && property_value_of(line) == "VALARM" {
            in_alarm = false;
        }
    }

    let insert_at = (start + 1..stop)
        .find(|i| removed.contains(i))
        .unwrap_or(stop);

    let mut out: Vec<String> = Vec::with_capacity(lines.len() + new_lines.len());
    out.extend(lines[..start + 1].iter().cloned());
    let mut i = start + 1;
    while i < stop {
        if i == insert_at {
            out.extend(new_lines.iter().cloned());
        }
        if !removed.contains(&i) {
            out.push(lines[i].clone());
        }
        i += 1;
    }
    if insert_at == stop {
        out.extend(new_lines.iter().cloned());
    }
    out.push(lines[stop].clone());
    out.extend(lines[stop + 1..].iter().cloned());

    // Bump DTSTAMP (RFC 5545 §3.8.7.2) and guarantee UID presence.
    let stamp_line = format!("DTSTAMP:{}", now_ical_utc());
    if let Some(i) = out[start + 1..]
        .iter()
        .position(|l| property_name_of(l) == "DTSTAMP")
    {
        out[start + 1 + i] = stamp_line;
    } else {
        out.insert(start + 1, stamp_line);
    }
    if !out[start + 1..]
        .iter()
        .any(|l| property_name_of(l) == "UID")
    {
        let uid = uid.unwrap_or_default();
        if !uid.is_empty() {
            out.insert(start + 1, format!("UID:{}", escape_ical_text(uid)));
        }
    }
    Ok(fold_ical_document(&out))
}

/// Parse a backend VTODO into the EAS projection.
fn task_mirror_from_vtodo(ics: &str) -> Option<TaskMirror> {
    let block = parse_component_block_ics("VTODO", ics)?;
    let text = |name: &str| -> Option<String> {
        component_property(&block, name).map(crate::ical_parser::unescape_ical_text)
    };
    let status = status_of_component(&block);
    let completed = component_property(&block, "COMPLETED");
    let complete = if status.as_deref() == Some("COMPLETED") || completed.is_some() {
        1
    } else {
        0
    };
    let dtstamp = component_property(&block, "DTSTAMP");
    let date_completed = completed.and_then(ical_utc_to_eas).or(match complete {
        1 => dtstamp.and_then(ical_utc_to_eas),
        _ => None,
    });
    let dtstart = component_property(&block, "DTSTART");
    let due = component_property(&block, "DUE");
    // X-SGW-* values win: they carry the exact EAS wire strings of a prior
    // gateway round-trip. Foreign objects fall back to the iCalendar values.
    let start_date = text("X-SGW-STARTDATE").or_else(|| dtstart.and_then(ical_floating_to_eas));
    let utc_start_date = text("X-SGW-UTCSTARTDATE").or_else(|| dtstart.and_then(ical_utc_to_eas));
    let due_date = text("X-SGW-DUEDATE").or_else(|| due.and_then(ical_floating_to_eas));
    let utc_due_date = text("X-SGW-UTCDUEDATE").or_else(|| due.and_then(ical_utc_to_eas));

    // A VALARM with an absolute TRIGGER;VALUE=DATE-TIME is the EAS reminder.
    let mut reminder_set = 0;
    let mut reminder_time = None;
    let mut i = 0;
    while i < block.len() {
        if block[i] == "BEGIN:VALARM" {
            let mut j = i + 1;
            let mut alarm_lines = Vec::new();
            while j < block.len() && block[j] != "END:VALARM" {
                alarm_lines.push(block[j].clone());
                j += 1;
            }
            let absolute = alarm_lines
                .iter()
                .any(|l| property_name_of(l) == "TRIGGER" && l.contains("VALUE=DATE-TIME"));
            if absolute
                && let Some(trigger) = component_property(&alarm_lines, "TRIGGER")
                && let Some(t) = ical_utc_to_eas(trigger)
            {
                reminder_set = 1;
                reminder_time = Some(t);
            }
            i = j;
        }
        i += 1;
    }

    Some(TaskMirror {
        subject: text("SUMMARY"),
        importance: component_property(&block, "PRIORITY").and_then(priority_to_importance),
        sensitivity: component_property(&block, "CLASS").and_then(class_to_sensitivity),
        start_date,
        due_date,
        utc_start_date,
        utc_due_date,
        complete,
        date_completed,
        reminder_set,
        reminder_time,
        categories: component_property(&block, "CATEGORIES").map(categories_ical_to_eas),
        body: text("DESCRIPTION"),
        uid: text("UID"),
    })
}

/// Parse a backend VJOURNAL into the EAS projection.
fn note_mirror_from_vjournal(ics: &str) -> Option<NoteMirror> {
    let block = parse_component_block_ics("VJOURNAL", ics)?;
    let text = |name: &str| -> Option<String> {
        component_property(&block, name).map(crate::ical_parser::unescape_ical_text)
    };
    Some(NoteMirror {
        subject: text("SUMMARY"),
        message_class: text("X-SGW-MESSAGECLASS").or(Some("IPM.StickyNote".to_string())),
        body: text("DESCRIPTION"),
        categories: component_property(&block, "CATEGORIES").map(categories_ical_to_eas),
        last_modified_date: text("X-SGW-LASTMODIFIED")
            .or_else(|| component_property(&block, "LAST-MODIFIED").and_then(ical_utc_to_eas))
            .or_else(|| component_property(&block, "DTSTAMP").and_then(ical_utc_to_eas)),
        uid: text("UID"),
    })
}

/// Resource filename for a gateway-pushed object: the EAS ServerId when it is
/// URL-safe, else a fresh uuid (the ServerId stays the EAS identity).
fn resource_filename(server_id: &str) -> String {
    let safe = !server_id.is_empty()
        && server_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if safe {
        format!("{server_id}.ics")
    } else {
        format!("{}.ics", Uuid::new_v4().simple())
    }
}

// ------------------------------ Reconciliation ----------------------------

/// Pure inventory comparison for the Ping change probe: `mirror` is the set
/// of (href, etag) pairs the mirror rows record, `remote` the set the backend
/// just reported. A href present on only one side is a hard difference (a
/// backend addition or deletion). For hrefs on both sides, differing etags
/// mean a backend edit; a MISSING etag on either side is *no signal*, not a
/// difference — a server that omits `getetag` for a resource must not make
/// the probe report a permanent change (which would reconcile-loop every
/// tick); that resource converges through the Sync-time reconcile instead.
fn inventory_differs(
    mirror: &[(String, Option<String>)],
    remote: &[(String, Option<String>)],
) -> bool {
    let mirror_hrefs: HashSet<&str> = mirror.iter().map(|(h, _)| h.as_str()).collect();
    let remote_hrefs: HashSet<&str> = remote.iter().map(|(h, _)| h.as_str()).collect();
    if mirror_hrefs != remote_hrefs {
        return true;
    }
    let mirror_etags: HashMap<&str, &str> = mirror
        .iter()
        .filter_map(|(h, e)| e.as_deref().map(|e| (h.as_str(), e)))
        .collect();
    remote.iter().any(|(h, e)| match e.as_deref() {
        Some(e) => mirror_etags
            .get(h.as_str())
            .is_some_and(|known| *known != e),
        None => false,
    })
}

/// Cheap backend change detection for a Ping waiting on Tasks/Notes folders
/// between Syncs: one etag-only REPORT per collection ([MS-ASCMD] Ping has no
/// backend push channel; CalDAV has neither — polling is the only door) and a
/// set comparison against the mirror rows. When the inventories differ, the
/// caller runs the same reconcile the Sync path uses, which journals the
/// imports so the Ping's journal-based detection reports the folder.
///
/// No MKCALENDAR here: a user who never synced Tasks has no collection, the
/// REPORT 404s, and the caller disables probing for the rest of that Ping.
pub async fn backend_inventory_changed(
    state: &AppState,
    username: &str,
    password: &str,
    tasks: bool,
    notes: bool,
) -> Result<bool> {
    let caldav = crate::caldav::CaldavClient::new(&state.cfg)?;
    let mut changed = false;
    if tasks {
        let remote = caldav
            .query_calendar_etags(
                &caldav.tasks_collection_href(username),
                "VTODO",
                username,
                password,
            )
            .await?;
        let mirror: Vec<(String, Option<String>)> = state
            .storage
            .get_all_tasks_for_owner(username)
            .await?
            .iter()
            .filter_map(|row| {
                row.caldav_href
                    .as_ref()
                    .map(|href| (href.clone(), row.etag.clone()))
            })
            .collect();
        let remote: Vec<(String, Option<String>)> = remote
            .iter()
            .map(|i| (i.href.clone(), i.etag.clone()))
            .collect();
        changed |= inventory_differs(&mirror, &remote);
    }
    if notes && !changed {
        let remote = caldav
            .query_calendar_etags(
                &caldav.notes_collection_href(username),
                "VJOURNAL",
                username,
                password,
            )
            .await?;
        let mirror: Vec<(String, Option<String>)> = state
            .storage
            .get_all_notes_for_owner(username)
            .await?
            .iter()
            .filter_map(|row| {
                row.caldav_href
                    .as_ref()
                    .map(|href| (href.clone(), row.etag.clone()))
            })
            .collect();
        let remote: Vec<(String, Option<String>)> = remote
            .iter()
            .map(|i| (i.href.clone(), i.etag.clone()))
            .collect();
        changed |= inventory_differs(&mirror, &remote);
    }
    Ok(changed)
}

/// Pull every VTODO of the user's Tasks collection and converge the mirror
/// onto it: remote new/changed objects are imported (journal upserts), remote
/// deletions remove mirror rows (journal tombstones), and rows that were
/// never pushed (pre-backend records, or rows left behind by a partially
/// failed Add) are uploaded so the backend ends up owning every row.
pub async fn reconcile_tasks(state: &AppState, username: &str, password: &str) -> Result<()> {
    let store = TaskBackendStore::for_user(state, username, password, "Tasks", &["VTODO"]).await?;
    let body = store
        .caldav
        .query_calendar_components(&store.collection, "VTODO", username, password)
        .await?;
    let items = crate::caldav::parse_calendar_query_items(&body, &store.collection);

    let rows = state.storage.get_all_tasks_for_owner(username).await?;
    let mut by_href: HashMap<String, TaskRow> = HashMap::new();
    let mut by_uid: HashMap<String, TaskRow> = HashMap::new();
    let mut used_server_ids: HashSet<String> = HashSet::new();
    for row in &rows {
        used_server_ids.insert(row.server_id.clone());
        if let Some(href) = row.caldav_href.clone() {
            by_href.insert(href, row.clone());
        }
        if let Some(uid) = row.uid.clone() {
            by_uid.entry(uid).or_insert_with(|| row.clone());
        }
    }
    let mut seen_rows: HashSet<String> = HashSet::new();
    let mut seen_hrefs: HashSet<String> = HashSet::new();

    for item in items {
        seen_hrefs.insert(item.href.clone());
        let ics = match item.ics {
            Some(ics) => ics,
            None => match store
                .caldav
                .get_calendar_resource(&item.href, username, password)
                .await
            {
                Ok(Some((ics, _))) => ics,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(href = %item.href, error = %e, "Skipping unfetchable task");
                    continue;
                }
            },
        };
        let uid = uid_of_ics("VTODO", &ics).unwrap_or_else(|| item.href.clone());
        let row = by_href
            .get(&item.href)
            .cloned()
            .or_else(|| by_uid.get(&uid).cloned());
        match row {
            Some(row) if row.caldav_href.as_deref() == Some(item.href.as_str()) => {
                seen_rows.insert(row.server_id.clone());
                if row.etag.as_deref() == item.etag.as_deref()
                    && row.uid.as_deref() == Some(uid.as_str())
                {
                    continue;
                }
                let Some(mirror) = task_mirror_from_vtodo(&ics) else {
                    tracing::warn!(href = %item.href, "Unparsable VTODO skipped");
                    continue;
                };
                let mut mirror = mirror;
                mirror.uid = Some(uid.clone());
                let fields = TaskFields {
                    caldav_href: Some(item.href.as_str()),
                    etag: item.etag.as_deref(),
                    ..mirror.fields()
                };
                if let Err(e) = state
                    .storage
                    .upsert_task(username, &row.server_id, &fields)
                    .await
                {
                    tracing::warn!(error = %e, "Task reconcile upsert failed");
                }
            }
            Some(row) => {
                // Same UID under a new href (moved/recreated remotely): keep
                // the EAS ServerId, adopt the new backend identity.
                seen_rows.insert(row.server_id.clone());
                let Some(mirror) = task_mirror_from_vtodo(&ics) else {
                    tracing::warn!(href = %item.href, "Unparsable VTODO skipped");
                    continue;
                };
                let mut mirror = mirror;
                mirror.uid = Some(uid.clone());
                let fields = TaskFields {
                    caldav_href: Some(item.href.as_str()),
                    etag: item.etag.as_deref(),
                    ..mirror.fields()
                };
                if let Err(e) = state
                    .storage
                    .upsert_task(username, &row.server_id, &fields)
                    .await
                {
                    tracing::warn!(error = %e, "Task reconcile move failed");
                }
            }
            None => {
                // Brand-new backend object (created by another CalDAV client).
                let Some(mirror) = task_mirror_from_vtodo(&ics) else {
                    tracing::warn!(href = %item.href, "Unparsable VTODO skipped");
                    continue;
                };
                let mut mirror = mirror;
                mirror.uid = Some(uid.clone());
                let server_id = if !uid.is_empty() && used_server_ids.insert(uid.clone()) {
                    uid.clone()
                } else {
                    format!("task-{}", Uuid::new_v4().simple())
                };
                let fields = TaskFields {
                    caldav_href: Some(item.href.as_str()),
                    etag: item.etag.as_deref(),
                    ..mirror.fields()
                };
                if let Err(e) = state
                    .storage
                    .upsert_task(username, &server_id, &fields)
                    .await
                {
                    tracing::warn!(error = %e, "Task reconcile import failed");
                }
            }
        }
    }

    // Mirror rows with a backend href that the REPORT no longer lists were
    // deleted remotely → tombstone them.
    for row in by_href.values() {
        if !seen_rows.contains(&row.server_id)
            && !seen_hrefs.contains(row.caldav_href.as_deref().unwrap_or_default())
            && let Err(e) = state.storage.delete_task(username, &row.server_id).await
        {
            tracing::warn!(error = %e, "Task reconcile delete failed");
        }
    }

    // Rows that were never pushed into the backend get uploaded now (their
    // fields stay authoritative; only the backend identity is recorded).
    for row in rows.iter().filter(|r| r.caldav_href.is_none()) {
        let uid = row.uid.clone().unwrap_or_else(|| row.server_id.clone());
        let mut mirror = TaskMirror::from_row(row);
        mirror.uid = Some(uid.clone());
        let ics = build_vtodo(&uid, &mirror);
        let filename = resource_filename(&row.server_id);
        match store
            .caldav
            .put_event(
                &store.collection,
                Some(&filename),
                &ics,
                username,
                password,
                None,
            )
            .await
        {
            Ok((href, etag)) => {
                if let Err(e) = state
                    .storage
                    .set_task_backend_ref(
                        username,
                        &row.server_id,
                        &href,
                        Some(etag.as_str()),
                        Some(uid.as_str()),
                    )
                    .await
                {
                    tracing::warn!(error = %e, "Task push bookkeeping failed");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, server_id = %row.server_id, "Task push failed; retry on next sync");
            }
        }
    }
    Ok(())
}

/// The VJOURNAL twin of [`reconcile_tasks`].
pub async fn reconcile_notes(state: &AppState, username: &str, password: &str) -> Result<()> {
    let store =
        TaskBackendStore::for_user(state, username, password, "Notes", &["VJOURNAL"]).await?;
    let body = store
        .caldav
        .query_calendar_components(&store.collection, "VJOURNAL", username, password)
        .await?;
    let items = crate::caldav::parse_calendar_query_items(&body, &store.collection);

    let rows = state.storage.get_all_notes_for_owner(username).await?;
    let mut by_href: HashMap<String, NoteRow> = HashMap::new();
    let mut by_uid: HashMap<String, NoteRow> = HashMap::new();
    let mut used_server_ids: HashSet<String> = HashSet::new();
    for row in &rows {
        used_server_ids.insert(row.server_id.clone());
        if let Some(href) = row.caldav_href.clone() {
            by_href.insert(href, row.clone());
        }
        if let Some(uid) = row.uid.clone() {
            by_uid.entry(uid).or_insert_with(|| row.clone());
        }
    }
    let mut seen_rows: HashSet<String> = HashSet::new();
    let mut seen_hrefs: HashSet<String> = HashSet::new();

    for item in items {
        seen_hrefs.insert(item.href.clone());
        let ics = match item.ics {
            Some(ics) => ics,
            None => match store
                .caldav
                .get_calendar_resource(&item.href, username, password)
                .await
            {
                Ok(Some((ics, _))) => ics,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(href = %item.href, error = %e, "Skipping unfetchable note");
                    continue;
                }
            },
        };
        let uid = uid_of_ics("VJOURNAL", &ics).unwrap_or_else(|| item.href.clone());
        let row = by_href
            .get(&item.href)
            .cloned()
            .or_else(|| by_uid.get(&uid).cloned());
        match row {
            Some(row) => {
                seen_rows.insert(row.server_id.clone());
                if row.etag.as_deref() == item.etag.as_deref()
                    && row.caldav_href.as_deref() == Some(item.href.as_str())
                {
                    continue;
                }
                let Some(mirror) = note_mirror_from_vjournal(&ics) else {
                    tracing::warn!(href = %item.href, "Unparsable VJOURNAL skipped");
                    continue;
                };
                let mut mirror = mirror;
                mirror.uid = Some(uid.clone());
                let fields = NoteFields {
                    caldav_href: Some(item.href.as_str()),
                    etag: item.etag.as_deref(),
                    ..mirror.fields()
                };
                if let Err(e) = state
                    .storage
                    .upsert_note(username, &row.server_id, &fields)
                    .await
                {
                    tracing::warn!(error = %e, "Note reconcile upsert failed");
                }
            }
            None => {
                let Some(mirror) = note_mirror_from_vjournal(&ics) else {
                    tracing::warn!(href = %item.href, "Unparsable VJOURNAL skipped");
                    continue;
                };
                let mut mirror = mirror;
                mirror.uid = Some(uid.clone());
                let server_id = if !uid.is_empty() && used_server_ids.insert(uid.clone()) {
                    uid.clone()
                } else {
                    format!("note-{}", Uuid::new_v4().simple())
                };
                let fields = NoteFields {
                    caldav_href: Some(item.href.as_str()),
                    etag: item.etag.as_deref(),
                    ..mirror.fields()
                };
                if let Err(e) = state
                    .storage
                    .upsert_note(username, &server_id, &fields)
                    .await
                {
                    tracing::warn!(error = %e, "Note reconcile import failed");
                }
            }
        }
    }

    for row in by_href.values() {
        if !seen_rows.contains(&row.server_id)
            && !seen_hrefs.contains(row.caldav_href.as_deref().unwrap_or_default())
            && let Err(e) = state.storage.delete_note(username, &row.server_id).await
        {
            tracing::warn!(error = %e, "Note reconcile delete failed");
        }
    }

    for row in rows.iter().filter(|r| r.caldav_href.is_none()) {
        let uid = row.uid.clone().unwrap_or_else(|| row.server_id.clone());
        let mut mirror = NoteMirror::from_row(row);
        mirror.uid = Some(uid.clone());
        let ics = build_vjournal(&uid, &mirror);
        let filename = resource_filename(&row.server_id);
        match store
            .caldav
            .put_event(
                &store.collection,
                Some(&filename),
                &ics,
                username,
                password,
                None,
            )
            .await
        {
            Ok((href, etag)) => {
                if let Err(e) = state
                    .storage
                    .set_note_backend_ref(
                        username,
                        &row.server_id,
                        &href,
                        Some(etag.as_str()),
                        Some(uid.as_str()),
                    )
                    .await
                {
                    tracing::warn!(error = %e, "Note push bookkeeping failed");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, server_id = %row.server_id, "Note push failed; retry on next sync");
            }
        }
    }
    Ok(())
}

/// `last_modified_date` in ISO 8601 UTC (MS-ASNOTE LastModifiedDate is informational).
fn now_utc_string() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Render `<Add>/<Change>/<Delete>` mutation responses for a Sync reply.
///
/// The echoes MUST be wrapped in a `<Responses>` container (MS-ASCMD
/// §2.2.3.154: the Responses element "contains responses to operations that
/// are processed by the server" and is the only legal parent of response
/// Add/Change/Delete elements); a bare Add directly under Collection is not
/// schema-valid. The container is omitted entirely when nothing was
/// processed, per §2.2.3.154 ("present only if the server has processed
/// operation from the client").
pub fn render_mutation_responses(results: &[MutationResult]) -> String {
    if results.is_empty() {
        return String::new();
    }
    let mut xml = String::with_capacity(256 + results.len() * 128);
    xml.push_str("<Responses>");
    for res in results {
        match res.kind {
            MutationKind::Add => {
                // Per MS-ASCMD §2.2.3.7.2, an Add response returns the client
                // supplied ClientId alongside the assigned ServerId so the client
                // can correlate its local item with the server item.
                xml.push_str(&format!(
                    r#"<Add><ClientId>{}</ClientId><ServerId>{}</ServerId><Status>{}</Status></Add>"#,
                    xml_escape(res.client_id.as_deref().unwrap_or_default()),
                    xml_escape(&res.server_id),
                    res.status
                ));
            }
            MutationKind::Change => {
                xml.push_str(&format!(
                    r#"<Change><ServerId>{}</ServerId><Status>{}</Status></Change>"#,
                    xml_escape(&res.server_id),
                    res.status
                ));
            }
            MutationKind::Delete => {
                xml.push_str(&format!(
                    r#"<Delete><ServerId>{}</ServerId><Status>{}</Status></Delete>"#,
                    xml_escape(&res.server_id),
                    res.status
                ));
            }
        }
    }
    xml.push_str("</Responses>");
    xml
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn parse_task_mutation_add_with_body() {
        let xml = r#"<Collection>
            <Commands>
              <Add>
                <ClientId>client-1</ClientId>
                <ApplicationData>
                  <Tasks:Subject>Buy milk</Tasks:Subject>
                  <Tasks:Importance>2</Tasks:Importance>
                  <Tasks:Complete>0</Tasks:Complete>
                  <Tasks:Body>Remember the 2%</Tasks:Body>
                  <Tasks:Categories><Tasks:Category>Errands</Tasks:Category><Tasks:Category>Home</Tasks:Category></Tasks:Categories>
                </ApplicationData>
              </Add>
            </Commands>
          </Collection>"#;
        let muts = parse_task_mutations(xml).unwrap();
        assert_eq!(muts.len(), 1);
        let m = &muts[0];
        assert_eq!(m.kind, MutationKind::Add);
        assert_eq!(m.client_id.as_deref(), Some("client-1"));
        assert_eq!(m.fields.subject.as_deref(), Some("Buy milk"));
        assert_eq!(m.fields.importance, Some(2));
        assert_eq!(m.fields.complete, Some(0));
        assert_eq!(m.fields.body.as_deref(), Some("Remember the 2%"));
        assert_eq!(m.fields.categories.as_deref(), Some("Errands;Home"));
    }

    #[test]
    fn parse_task_mutation_change_and_delete() {
        let xml = r#"<Collection>
            <Commands>
              <Change>
                <ServerId>task-abc</ServerId>
                <ApplicationData>
                  <Tasks:Subject>Updated</Tasks:Subject>
                  <Tasks:Complete>1</Tasks:Complete>
                </ApplicationData>
              </Change>
              <Delete>
                <ServerId>task-xyz</ServerId>
              </Delete>
            </Commands>
          </Collection>"#;
        let muts = parse_task_mutations(xml).unwrap();
        assert_eq!(muts.len(), 2);
        assert_eq!(muts[0].kind, MutationKind::Change);
        assert_eq!(muts[0].server_id, "task-abc");
        assert_eq!(muts[0].fields.subject.as_deref(), Some("Updated"));
        assert_eq!(muts[0].fields.complete, Some(1));
        assert_eq!(muts[1].kind, MutationKind::Delete);
        assert_eq!(muts[1].server_id, "task-xyz");
    }

    #[test]
    fn parse_note_mutation_with_airsyncbody() {
        let xml = r#"<Collection>
            <Commands>
              <Add>
                <ClientId>c-1</ClientId>
                <ApplicationData>
                  <Notes:Subject>My note</Notes:Subject>
                  <Notes:MessageClass>IPM.StickyNote</Notes:MessageClass>
                  <AirSyncBase:Body>
                    <AirSyncBase:Type>1</AirSyncBase:Type>
                    <AirSyncBase:Data>Hello note body</AirSyncBase:Data>
                  </AirSyncBase:Body>
                  <Notes:Categories><Notes:Category>Ideas</Notes:Category></Notes:Categories>
                </ApplicationData>
              </Add>
            </Commands>
          </Collection>"#;
        let muts = parse_note_mutations(xml).unwrap();
        assert_eq!(muts.len(), 1);
        let m = &muts[0];
        assert_eq!(m.kind, MutationKind::Add);
        assert_eq!(m.fields.subject.as_deref(), Some("My note"));
        assert_eq!(m.fields.message_class.as_deref(), Some("IPM.StickyNote"));
        assert_eq!(m.fields.body.as_deref(), Some("Hello note body"));
        assert_eq!(m.fields.categories.as_deref(), Some("Ideas"));
    }

    #[test]
    fn render_task_roundtrip_fields() {
        let row = TaskRow {
            id: 1,
            owner: "u".to_string(),
            server_id: "task-1".to_string(),
            subject: Some("Test".to_string()),
            importance: Some(2),
            sensitivity: None,
            start_date: Some("2026-09-03".to_string()),
            due_date: Some("2026-09-04".to_string()),
            utc_start_date: None,
            utc_due_date: None,
            complete: 0,
            date_completed: None,
            reminder_set: 1,
            reminder_time: Some("2026-09-03T09:00:00Z".to_string()),
            categories: Some("A;B".to_string()),
            body: Some("body text".to_string()),
            caldav_href: None,
            etag: None,
            uid: None,
            updated_at: None,
        };
        let rendered = render_eas_task(&row);
        assert!(rendered.contains("<Tasks:Subject>Test</Tasks:Subject>"));
        assert!(rendered.contains("<Tasks:Importance>2</Tasks:Importance>"));
        assert!(rendered.contains("<Tasks:Complete>0</Tasks:Complete>"));
        assert!(rendered.contains("<Tasks:ReminderSet>1</Tasks:ReminderSet>"));
        assert!(rendered.contains("<Tasks:ReminderTime>2026-09-03T09:00:00Z</Tasks:ReminderTime>"));
        assert!(rendered.contains("<Tasks:Body>body text</Tasks:Body>"));
        assert!(rendered.contains("<Tasks:Category>A</Tasks:Category>"));
        assert!(rendered.contains("<Tasks:Category>B</Tasks:Category>"));
    }

    #[test]
    fn render_note_defaults_message_class() {
        let row = NoteRow {
            id: 1,
            owner: "u".to_string(),
            server_id: "note-1".to_string(),
            subject: Some("S".to_string()),
            message_class: None,
            body: Some("B".to_string()),
            categories: None,
            last_modified_date: None,
            caldav_href: None,
            etag: None,
            uid: None,
            updated_at: None,
        };
        let rendered = render_eas_note(&row);
        assert!(rendered.contains("<Notes:MessageClass>IPM.StickyNote</Notes:MessageClass>"));
        assert!(rendered.contains("<Notes:Subject>S</Notes:Subject>"));
        assert!(rendered.contains("<AirSyncBase:Data>B</AirSyncBase:Data>"));
    }

    // ===================== §15 CalDAV bridge tests =====================

    fn sample_task_mirror() -> TaskMirror {
        TaskMirror {
            subject: Some("Ship the bridge; check lists, and alarms".to_string()),
            importance: Some(2),
            sensitivity: Some(2),
            start_date: Some("2026-09-03T09:00:00.000".to_string()),
            due_date: Some("2026-09-04T17:00:00.000".to_string()),
            utc_start_date: Some("2026-09-03T07:00:00.000Z".to_string()),
            utc_due_date: Some("2026-09-04T15:00:00.000Z".to_string()),
            complete: 0,
            date_completed: None,
            reminder_set: 1,
            reminder_time: Some("2026-09-03T08:45:00.000Z".to_string()),
            categories: Some("Errands;Home, Sweet".to_string()),
            body: Some("Line one\nLine two, with comma".to_string()),
            uid: Some("task-roundtrip".to_string()),
        }
    }

    #[test]
    fn vtodo_build_parse_roundtrip_preserves_projection() {
        let mirror = sample_task_mirror();
        let ics = build_vtodo("task-roundtrip", &mirror);
        assert!(ics.contains("BEGIN:VTODO"));
        // Folded output must re-unfold into the same logical lines.
        let parsed = task_mirror_from_vtodo(&ics).expect("round-trip parse");
        assert_eq!(
            parsed.subject.as_deref(),
            Some("Ship the bridge; check lists, and alarms")
        );
        assert_eq!(parsed.importance, Some(2));
        assert_eq!(parsed.sensitivity, Some(2));
        assert_eq!(
            parsed.start_date.as_deref(),
            Some("2026-09-03T09:00:00.000")
        );
        assert_eq!(parsed.due_date.as_deref(), Some("2026-09-04T17:00:00.000"));
        assert_eq!(
            parsed.utc_start_date.as_deref(),
            Some("2026-09-03T07:00:00.000Z")
        );
        assert_eq!(
            parsed.utc_due_date.as_deref(),
            Some("2026-09-04T15:00:00.000Z")
        );
        assert_eq!(parsed.complete, 0);
        assert!(parsed.date_completed.is_none());
        assert_eq!(parsed.reminder_set, 1);
        assert_eq!(
            parsed.reminder_time.as_deref(),
            Some("2026-09-03T08:45:00.000Z")
        );
        assert_eq!(parsed.categories.as_deref(), Some("Errands;Home, Sweet"));
        assert_eq!(
            parsed.body.as_deref(),
            Some("Line one\nLine two, with comma")
        );
        assert_eq!(parsed.uid.as_deref(), Some("task-roundtrip"));
    }

    #[test]
    fn vtodo_completed_roundtrip_and_reset() {
        let mut mirror = sample_task_mirror();
        mirror.complete = 1;
        mirror.date_completed = Some("2026-09-05T10:00:00.000Z".to_string());
        mirror.reminder_set = 0;
        mirror.reminder_time = None;
        let ics = build_vtodo("done-1", &mirror);
        assert!(ics.contains("STATUS:COMPLETED"));
        let parsed = task_mirror_from_vtodo(&ics).expect("parse");
        assert_eq!(parsed.complete, 1);
        assert_eq!(
            parsed.date_completed.as_deref(),
            Some("2026-09-05T10:00:00.000Z")
        );

        // Un-completing a previously completed task resets STATUS.
        let mut mirror2 = mirror.clone();
        mirror2.complete = 0;
        mirror2.date_completed = None;
        let patched = patch_component_ics(
            &ics,
            "VTODO",
            VTODO_MAPPED,
            &mirror2.vtodo_lines(Some(&ics)),
            Some("done-1"),
        )
        .expect("patch");
        assert!(patched.contains("STATUS:NEEDS-ACTION"));
        assert!(!patched.contains("STATUS:COMPLETED"));
        let reparsed = task_mirror_from_vtodo(&patched).expect("re-parse");
        assert_eq!(reparsed.complete, 0);
        assert!(reparsed.date_completed.is_none());
    }

    #[test]
    fn patch_vtodo_preserves_foreign_properties() {
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//foreign//EN\r\nBEGIN:VTODO\r\nUID:keep-uid\r\nDTSTAMP:20250101T000000Z\r\nSUMMARY:Old subject\r\nDUE;TZID=Europe/Berlin:20260904T170000\r\nRRULE:FREQ=WEEKLY;BYDAY=MO\r\nATTENDEE;CN=Alice:mailto:alice@example.com\r\nURL:https://example.com/info\r\nX-COMPANY-CODE:ABC-123\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;VALUE=DATE-TIME:20260903T080000Z\r\nEND:VALARM\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
        let mirror = TaskMirror {
            subject: Some("New subject".to_string()),
            importance: Some(1),
            uid: Some("keep-uid".to_string()),
            ..Default::default()
        };
        let patched = patch_component_ics(
            ics,
            "VTODO",
            VTODO_MAPPED,
            &mirror.vtodo_lines(Some(ics)),
            Some("keep-uid"),
        )
        .expect("patch");
        // Foreign properties survive verbatim.
        assert!(patched.contains("RRULE:FREQ=WEEKLY;BYDAY=MO"));
        assert!(patched.contains("ATTENDEE;CN=Alice:mailto:alice@example.com"));
        assert!(patched.contains("URL:https://example.com/info"));
        assert!(patched.contains("X-COMPANY-CODE:ABC-123"));
        // Mapped properties are replaced, not duplicated.
        assert!(patched.contains("SUMMARY:New subject"));
        assert!(!patched.contains("Old subject"));
        assert!(patched.matches("SUMMARY:").count() == 1);
        // The whole VALARM block is rewritten as a unit (no reminder in the
        // new mirror → the block is gone, ACTION:DISPLAY gone with it).
        assert!(!patched.contains("BEGIN:VALARM"));
        // The gateway-owned DUE is replaced by the (empty) mirror's — no
        // stale DUE survives when EAS has none.
        assert!(!patched.contains("DUE;TZID="));
        // UID preserved, DTSTAMP refreshed past the original.
        assert!(patched.contains("UID:keep-uid"));
        assert!(!patched.contains("DTSTAMP:20250101T000000Z"));
        // And the patched document still parses.
        assert!(task_mirror_from_vtodo(&patched).is_some());
    }

    #[test]
    fn vjournal_build_parse_roundtrip() {
        let mirror = NoteMirror {
            subject: Some("Idea; keep it".to_string()),
            message_class: Some("IPM.StickyNote".to_string()),
            body: Some("Body, with; punctuation\nand newline".to_string()),
            categories: Some("Ideas;Later".to_string()),
            last_modified_date: Some("2026-09-03T07:00:00Z".to_string()),
            uid: Some("note-roundtrip".to_string()),
        };
        let ics = build_vjournal("note-roundtrip", &mirror);
        assert!(ics.contains("BEGIN:VJOURNAL"));
        let parsed = note_mirror_from_vjournal(&ics).expect("parse");
        assert_eq!(parsed.subject.as_deref(), Some("Idea; keep it"));
        assert_eq!(parsed.message_class.as_deref(), Some("IPM.StickyNote"));
        assert_eq!(
            parsed.body.as_deref(),
            Some("Body, with; punctuation\nand newline")
        );
        assert_eq!(parsed.categories.as_deref(), Some("Ideas;Later"));
        assert_eq!(parsed.uid.as_deref(), Some("note-roundtrip"));

        let patched = patch_component_ics(
            &ics,
            "VJOURNAL",
            VJOURNAL_MAPPED,
            &mirror.vjournal_lines(),
            Some("note-roundtrip"),
        )
        .expect("patch");
        assert!(patched.contains("BEGIN:VJOURNAL"));
        let reparsed = note_mirror_from_vjournal(&patched).expect("re-parse");
        assert_eq!(reparsed.subject.as_deref(), Some("Idea; keep it"));
    }

    #[test]
    fn importance_priority_mapping_matrix() {
        for (imp, pri) in [(0, "9"), (1, "5"), (2, "1")] {
            assert_eq!(importance_to_priority(imp).to_string(), pri);
            assert_eq!(priority_to_importance(pri), Some(imp));
        }
        assert_eq!(priority_to_importance("0"), None);
        assert_eq!(priority_to_importance("10"), None);
    }

    #[test]
    fn sensitivity_class_mapping_matrix() {
        for (sens, class) in [
            (0, "PUBLIC"),
            (1, "X-PERSONAL"),
            (2, "PRIVATE"),
            (3, "CONFIDENTIAL"),
        ] {
            assert_eq!(sensitivity_to_class(sens), class);
            assert_eq!(class_to_sensitivity(class), Some(sens));
        }
        assert_eq!(class_to_sensitivity("X-UNKNOWN"), None);
    }

    #[test]
    fn eas_ical_datetime_conversions() {
        // UTC wire value → iCalendar → back, format preserved.
        assert_eq!(
            eas_datetime_to_ical_utc("2026-09-03T07:00:00.000Z").as_deref(),
            Some("20260903T070000Z")
        );
        assert_eq!(
            ical_utc_to_eas("20260903T070000Z").as_deref(),
            Some("2026-09-03T07:00:00.000Z")
        );
        // Zone-less EAS value is UTC per [MS-ASDTYPE].
        assert_eq!(
            eas_datetime_to_ical_utc("2026-09-03T07:00:00.000").as_deref(),
            Some("20260903T070000Z")
        );
        // Date-only → midnight UTC.
        assert_eq!(
            eas_datetime_to_ical_utc("2026-09-03").as_deref(),
            Some("20260903T000000Z")
        );
        // Floating parse of a foreign DTSTART keeps wall clock.
        assert_eq!(
            ical_floating_to_eas("20260903T070000").as_deref(),
            Some("2026-09-03T07:00:00.000")
        );
        assert!(eas_datetime_to_ical_utc("not a date").is_none());
    }

    #[test]
    fn ical_text_escaping_roundtrips() {
        let raw = "semi;colon, comma\\ backslash\nnewline";
        let escaped = escape_ical_text(raw);
        assert!(escaped.contains("\\;"));
        assert!(escaped.contains("\\,"));
        assert!(escaped.contains("\\\\"));
        assert!(escaped.contains("\\n"));
        assert!(!escaped.contains('\n'));
        let unescaped = crate::ical_parser::unescape_ical_text(&escaped);
        assert_eq!(unescaped, raw);
    }

    #[test]
    fn folded_document_unfolds_to_logical_lines() {
        let long = format!("DESCRIPTION:{}", "x".repeat(200));
        let doc = fold_ical_document(std::slice::from_ref(&long));
        for line in doc.lines() {
            assert!(line.len() <= 76, "folded line too long: {}", line.len());
        }
        assert_eq!(unfolded_lines(&doc), vec![long]);
    }

    async fn backend_down_state() -> Arc<crate::models::AppState> {
        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .expect("in-memory storage");
        storage.init_schema().await.expect("schema init");
        let cfg = crate::config::Config {
            caldav_base: "http://127.0.0.1:1".to_string(),
            hmac_secret: secrecy::SecretString::from("a".repeat(32)),
            ..Default::default()
        };
        Arc::new(crate::models::AppState::new(cfg, Arc::new(storage)))
    }

    async fn backend_off_state() -> Arc<crate::models::AppState> {
        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .expect("in-memory storage");
        storage.init_schema().await.expect("schema init");
        let cfg = crate::config::Config {
            hmac_secret: secrecy::SecretString::from("a".repeat(32)),
            ..Default::default()
        };
        Arc::new(crate::models::AppState::new(cfg, Arc::new(storage)))
    }

    fn add_task_mutation() -> TaskMutation {
        let xml = r#"<Collection><Commands><Add><ClientId>c1</ClientId><ApplicationData><Tasks:Subject>Write-through me</Tasks:Subject><Tasks:Importance>1</Tasks:Importance></ApplicationData></Add></Commands></Collection>"#;
        parse_task_mutations(xml)
            .expect("parse")
            .into_iter()
            .next()
            .unwrap()
    }

    fn change_task_mutation(server_id: &str) -> TaskMutation {
        let xml = format!(
            r#"<Collection><Commands><Change><ServerId>{server_id}</ServerId><ApplicationData><Tasks:Subject>Changed while down</Tasks:Subject></ApplicationData></Change></Commands></Collection>"#
        );
        parse_task_mutations(&xml)
            .expect("parse")
            .into_iter()
            .next()
            .unwrap()
    }

    fn delete_task_mutation(server_id: &str) -> TaskMutation {
        let xml = format!(
            r#"<Collection><Commands><Delete><ServerId>{server_id}</ServerId></Delete></Commands></Collection>"#
        );
        parse_task_mutations(&xml)
            .expect("parse")
            .into_iter()
            .next()
            .unwrap()
    }

    async fn seed_mirrored_task(state: &crate::models::AppState, server_id: &str) {
        state
            .storage
            .upsert_task(
                "u@example.com",
                server_id,
                &crate::storage::TaskFields {
                    subject: Some("Original"),
                    importance: Some(1),
                    caldav_href: Some("/cal/u@example.com/Tasks/task-x.ics"),
                    etag: Some("etag-1"),
                    uid: Some("task-x"),
                    ..Default::default()
                },
            )
            .await
            .expect("seed");
    }

    #[tokio::test]
    async fn task_add_fails_closed_when_backend_unreachable() {
        let state = backend_down_state().await;
        let results = apply_task_mutations(&state, "u@example.com", "pw", &[add_task_mutation()])
            .await
            .expect("apply");
        assert_eq!(
            results[0].status, "6",
            "unreachable backend must fail closed"
        );
        let row = state
            .storage
            .get_task("u@example.com", &results[0].server_id)
            .await
            .expect("read");
        assert!(
            row.is_none(),
            "no local mirror may exist for a failed write-through"
        );
    }

    #[tokio::test]
    async fn task_change_fails_closed_when_backend_unreachable() {
        let state = backend_down_state().await;
        seed_mirrored_task(&state, "task-x").await;
        let results = apply_task_mutations(
            &state,
            "u@example.com",
            "pw",
            &[change_task_mutation("task-x")],
        )
        .await
        .expect("apply");
        assert_eq!(results[0].status, "6");
        let row = state
            .storage
            .get_task("u@example.com", "task-x")
            .await
            .expect("read")
            .expect("row kept");
        assert_eq!(row.subject.as_deref(), Some("Original"), "mirror untouched");
    }

    #[tokio::test]
    async fn task_delete_fails_closed_when_backend_unreachable() {
        let state = backend_down_state().await;
        seed_mirrored_task(&state, "task-x").await;
        let results = apply_task_mutations(
            &state,
            "u@example.com",
            "pw",
            &[delete_task_mutation("task-x")],
        )
        .await
        .expect("apply");
        assert_eq!(results[0].status, "6");
        let row = state
            .storage
            .get_task("u@example.com", "task-x")
            .await
            .expect("read");
        assert!(
            row.is_some(),
            "mirrored row must survive a failed backend delete"
        );
    }

    #[tokio::test]
    async fn task_delete_of_unmirrored_row_works_without_backend() {
        // A legacy (never-pushed) row has no backend identity, so a Delete
        // stays local even when the backend is unreachable.
        let state = backend_down_state().await;
        state
            .storage
            .upsert_task(
                "u@example.com",
                "task-local",
                &crate::storage::TaskFields {
                    subject: Some("Legacy"),
                    ..Default::default()
                },
            )
            .await
            .expect("seed");
        let results = apply_task_mutations(
            &state,
            "u@example.com",
            "pw",
            &[delete_task_mutation("task-local")],
        )
        .await
        .expect("apply");
        assert_eq!(results[0].status, "1");
        let row = state
            .storage
            .get_task("u@example.com", "task-local")
            .await
            .expect("read");
        assert!(row.is_none());
    }

    #[tokio::test]
    async fn task_mutation_local_when_backend_not_configured() {
        let state = backend_off_state().await;
        let results = apply_task_mutations(&state, "u@example.com", "pw", &[add_task_mutation()])
            .await
            .expect("apply");
        assert_eq!(results[0].status, "1");
        let row = state
            .storage
            .get_task("u@example.com", &results[0].server_id)
            .await
            .expect("read")
            .expect("local add stored");
        assert_eq!(row.subject.as_deref(), Some("Write-through me"));
        assert!(row.caldav_href.is_none());
    }

    fn add_note_mutation() -> NoteMutation {
        let xml = r#"<Collection><Commands><Add><ClientId>c1</ClientId><ApplicationData><Notes:Subject>Note one</Notes:Subject><AirSyncBase:Body><AirSyncBase:Type>1</AirSyncBase:Type><AirSyncBase:Data>NB</AirSyncBase:Data></AirSyncBase:Body></ApplicationData></Add></Commands></Collection>"#;
        parse_note_mutations(xml)
            .expect("parse")
            .into_iter()
            .next()
            .unwrap()
    }

    fn delete_note_mutation(server_id: &str) -> NoteMutation {
        let xml = format!(
            r#"<Collection><Commands><Delete><ServerId>{server_id}</ServerId></Delete></Commands></Collection>"#
        );
        parse_note_mutations(&xml)
            .expect("parse")
            .into_iter()
            .next()
            .unwrap()
    }

    async fn seed_mirrored_note(state: &crate::models::AppState, server_id: &str) {
        state
            .storage
            .upsert_note(
                "u@example.com",
                server_id,
                &crate::storage::NoteFields {
                    subject: Some("Original"),
                    caldav_href: Some("/cal/u@example.com/Notes/note-x.ics"),
                    etag: Some("etag-1"),
                    uid: Some("note-x"),
                    ..Default::default()
                },
            )
            .await
            .expect("seed");
    }

    #[tokio::test]
    async fn note_add_fails_closed_when_backend_unreachable() {
        let state = backend_down_state().await;
        let results = apply_note_mutations(&state, "u@example.com", "pw", &[add_note_mutation()])
            .await
            .expect("apply");
        assert_eq!(results[0].status, "6");
        let row = state
            .storage
            .get_note("u@example.com", &results[0].server_id)
            .await
            .expect("read");
        assert!(row.is_none());
    }

    #[tokio::test]
    async fn note_delete_fails_closed_when_backend_unreachable() {
        let state = backend_down_state().await;
        seed_mirrored_note(&state, "note-x").await;
        let results = apply_note_mutations(
            &state,
            "u@example.com",
            "pw",
            &[delete_note_mutation("note-x")],
        )
        .await
        .expect("apply");
        assert_eq!(results[0].status, "6");
        let row = state
            .storage
            .get_note("u@example.com", "note-x")
            .await
            .expect("read");
        assert!(row.is_some());
    }

    #[tokio::test]
    async fn note_mutation_local_when_backend_not_configured() {
        let state = backend_off_state().await;
        let results = apply_note_mutations(&state, "u@example.com", "pw", &[add_note_mutation()])
            .await
            .expect("apply");
        assert_eq!(results[0].status, "1");
        let row = state
            .storage
            .get_note("u@example.com", &results[0].server_id)
            .await
            .expect("read")
            .expect("local note stored");
        assert_eq!(row.subject.as_deref(), Some("Note one"));
        assert!(row.caldav_href.is_none());
    }

    #[test]
    fn inventory_diff_detects_every_divergence() {
        let m = |h: &str, e: &str| (h.to_string(), Some(e.to_string()));
        let mirror = vec![m("/cal/u/Tasks/a.ics", "e1"), m("/cal/u/Tasks/b.ics", "e2")];
        // Identical inventory: unchanged.
        assert!(!inventory_differs(
            &mirror,
            &[m("/cal/u/Tasks/a.ics", "e1"), m("/cal/u/Tasks/b.ics", "e2")]
        ));
        // Remote etag changed.
        assert!(inventory_differs(
            &mirror,
            &[m("/cal/u/Tasks/a.ics", "e1"), m("/cal/u/Tasks/b.ics", "eX")]
        ));
        // Remote object added.
        assert!(inventory_differs(
            &mirror,
            &[
                m("/cal/u/Tasks/a.ics", "e1"),
                m("/cal/u/Tasks/b.ics", "e2"),
                m("/cal/u/Tasks/c.ics", "e3")
            ]
        ));
        // Remote object deleted.
        assert!(inventory_differs(&mirror, &[m("/cal/u/Tasks/a.ics", "e1")]));
        // Both empty: unchanged.
        assert!(!inventory_differs(&[], &[]));
        // Mirror empty, remote has one: changed.
        assert!(inventory_differs(&[], &[m("/cal/u/Tasks/a.ics", "e1")]));
        // A resource the server reported WITHOUT an etag is no signal: the
        // probe must not report a permanent change (and reconcile-loop every
        // tick) for a resource that simply omitted getetag.
        assert!(!inventory_differs(
            &mirror,
            &[
                m("/cal/u/Tasks/a.ics", "e1"),
                ("/cal/u/Tasks/b.ics".to_string(), None)
            ]
        ));
        // Mirror row never fetched an etag vs remote etag: still no signal.
        assert!(!inventory_differs(
            &[
                m("/cal/u/Tasks/a.ics", "e1"),
                ("/cal/u/Tasks/b.ics".to_string(), None)
            ],
            &[m("/cal/u/Tasks/a.ics", "e1"), m("/cal/u/Tasks/b.ics", "e2")]
        ));
    }

    #[tokio::test]
    async fn backend_inventory_probe_fails_closed_when_unreachable() {
        let state = backend_down_state().await;
        let err =
            crate::tasks::backend_inventory_changed(&state, "u@example.com", "pw", true, false)
                .await
                .expect_err("unreachable backend must be an Err so the Ping probe disables");
        assert!(err.to_string().contains("error"));
    }

    #[tokio::test]
    async fn backend_inventory_probe_not_configured() {
        // caldav_base empty: CaldavClient::new fails -> Err -> caller disables.
        let state = backend_off_state().await;
        assert!(
            crate::tasks::backend_inventory_changed(&state, "u@example.com", "pw", true, true)
                .await
                .is_err()
        );
    }

    #[test]
    fn resource_filename_is_url_safe() {
        assert_eq!(resource_filename("task-abc_1.2"), "task-abc_1.2.ics");
        assert_ne!(resource_filename("task/../../etc"), "task/../../etc.ics");
        assert!(resource_filename("task/../../etc").ends_with(".ics"));
        assert!(resource_filename("").ends_with(".ics"));
    }
}

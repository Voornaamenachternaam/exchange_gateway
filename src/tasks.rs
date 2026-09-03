// src/tasks.rs
//
// Gateway-local EAS Tasks and Notes store.
//
// Stalwart Mailserver has no native Tasks or Notes backend, so to deliver full
// "Exchange account" parity for Outlook Android / New Outlook the gateway keeps
// these two namespaces in its own SQLite database (task_map / note_map, see
// sqlite_schema.sql) and surfaces them through the EAS Tasks: / Notes: classes.
//
// Every mutation is persisted atomically and mirrored into change_journal so the
// EAS Ping/direct-push path (handle_ping) and downstream change tracking observe
// them, exactly like calendar mutations.
use crate::models::AppState;
use crate::storage::{NoteFields, NoteRow, TaskFields, TaskRow};
use anyhow::Result;
use quick_xml::Reader;
use quick_xml::events::Event;
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

/// Full sync of the gateway-local Tasks store for an owner.
///
/// Since Tasks are stored locally (no upstream diff), a Sync always renders the
/// current full set as `<Add>` elements. Returns the XML fragment (without the
/// enclosing `<Collection>`).
pub async fn sync_tasks(state: &AppState, username: &str) -> Result<String> {
    let rows = state.storage.get_all_tasks_for_owner(username).await?;
    let mut xml = String::new();
    for row in &rows {
        xml.push_str(&render_eas_task_add(&row.server_id, row));
    }
    Ok(xml)
}

/// Full sync of the gateway-local Notes store for an owner.
pub async fn sync_notes(state: &AppState, username: &str) -> Result<String> {
    let rows = state.storage.get_all_notes_for_owner(username).await?;
    let mut xml = String::new();
    for row in &rows {
        xml.push_str(&render_eas_note_add(&row.server_id, row));
    }
    Ok(xml)
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

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name().local_name().as_ref().to_vec();
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
            }
            Ok(Event::End(e)) => {
                let name = e.name().local_name().as_ref().to_vec();
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
                pending_leaf = None;
            }
            Ok(Event::Text(t)) => {
                let text = t.decode().unwrap_or_default().into_owned();
                if text.trim().is_empty() {
                    buf.clear();
                    continue;
                }
                if let Some(leaf) = pending_leaf.clone() {
                    match leaf.as_slice() {
                        b"ServerId" if !in_app_data => current.server_id = Some(text),
                        b"ClientId" if !in_app_data => current.client_id = Some(text),
                        b"Category" if in_app_data => {
                            // Accumulate categories joined by ';'.
                            merge_category(&mut current, &text);
                        }
                        _ if in_app_data => {
                            // Leaf field text (Subject, Body, Data, Complete,
                            // Importance, MessageClass, ...). "Body" (Tasks:Body)
                            // and "Data" (AirSyncBase:Body/Data) are both mapped
                            // to the body field in apply_*_field below.
                            let local = String::from_utf8_lossy(&leaf).into_owned();
                            current.fields.push((local, text));
                        }
                        _ => {}
                    }
                }
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

/// Apply parsed task mutations against the gateway-local task store.
pub async fn apply_task_mutations(
    state: &AppState,
    username: &str,
    mutations: &[TaskMutation],
) -> Result<Vec<MutationResult>> {
    let mut results = Vec::new();

    for m in mutations {
        match m.kind {
            MutationKind::Delete => {
                // Delete by explicit server_id if present.
                if m.server_id.is_empty() {
                    results.push(MutationResult {
                        server_id: String::new(),
                        status: "6",
                        kind: MutationKind::Delete,
                        client_id: m.client_id.clone(),
                    });
                    continue;
                }
                match state.storage.delete_task(username, &m.server_id).await {
                    Ok(0) => {
                        // ServerId no longer exists on the server (MS-ASCMD status 8).
                        results.push(MutationResult {
                            server_id: m.server_id.clone(),
                            status: "8",
                            kind: MutationKind::Delete,
                            client_id: m.client_id.clone(),
                        });
                    }
                    Ok(_) => results.push(MutationResult {
                        server_id: m.server_id.clone(),
                        status: "1",
                        kind: MutationKind::Delete,
                        client_id: m.client_id.clone(),
                    }),
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to delete task");
                        results.push(MutationResult {
                            server_id: m.server_id.clone(),
                            status: "6",
                            kind: MutationKind::Delete,
                            client_id: m.client_id.clone(),
                        });
                    }
                }
            }
            MutationKind::Add => {
                // For Add the client may supply a client-provided server_id; use it
                // if present, otherwise generate a stable gateway id.
                let server_id = if m.server_id.is_empty() {
                    format!("task-{}", Uuid::new_v4().simple())
                } else {
                    m.server_id.clone()
                };
                let fields = TaskFields {
                    subject: m.fields.subject.as_deref(),
                    importance: m.fields.importance,
                    sensitivity: m.fields.sensitivity,
                    start_date: m.fields.start_date.as_deref(),
                    due_date: m.fields.due_date.as_deref(),
                    utc_start_date: m.fields.utc_start_date.as_deref(),
                    utc_due_date: m.fields.utc_due_date.as_deref(),
                    complete: m.fields.complete.unwrap_or(0),
                    date_completed: m.fields.date_completed.as_deref(),
                    reminder_set: m.fields.reminder_set.unwrap_or(0),
                    reminder_time: m.fields.reminder_time.as_deref(),
                    categories: m.fields.categories.as_deref(),
                    body: m.fields.body.as_deref(),
                };
                match state
                    .storage
                    .upsert_task(username, &server_id, &fields)
                    .await
                {
                    Ok(()) => results.push(MutationResult {
                        server_id,
                        status: "1",
                        kind: MutationKind::Add,
                        client_id: m.client_id.clone(),
                    }),
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to upsert task");
                        results.push(MutationResult {
                            server_id,
                            status: "6",
                            kind: MutationKind::Add,
                            client_id: m.client_id.clone(),
                        });
                    }
                }
            }
            MutationKind::Change => {
                if m.server_id.is_empty() {
                    results.push(MutationResult {
                        server_id: String::new(),
                        status: "6",
                        kind: MutationKind::Change,
                        client_id: m.client_id.clone(),
                    });
                    continue;
                }
                // A Change must reference an existing record and only overwrite the
                // properties the client actually supplied (MS-ASCMD permits partial
                // Change commands; unsupplied properties must be preserved).
                let existing = match state.storage.get_task(username, &m.server_id).await {
                    Ok(Some(row)) => row,
                    Ok(None) => {
                        results.push(MutationResult {
                            server_id: m.server_id.clone(),
                            status: "8",
                            kind: MutationKind::Change,
                            client_id: m.client_id.clone(),
                        });
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to load task for change");
                        results.push(MutationResult {
                            server_id: m.server_id.clone(),
                            status: "6",
                            kind: MutationKind::Change,
                            client_id: m.client_id.clone(),
                        });
                        continue;
                    }
                };
                let fields = TaskFields {
                    subject: m.fields.subject.as_deref().or(existing.subject.as_deref()),
                    importance: m.fields.importance.or(existing.importance),
                    sensitivity: m.fields.sensitivity.or(existing.sensitivity),
                    start_date: m
                        .fields
                        .start_date
                        .as_deref()
                        .or(existing.start_date.as_deref()),
                    due_date: m
                        .fields
                        .due_date
                        .as_deref()
                        .or(existing.due_date.as_deref()),
                    utc_start_date: m
                        .fields
                        .utc_start_date
                        .as_deref()
                        .or(existing.utc_start_date.as_deref()),
                    utc_due_date: m
                        .fields
                        .utc_due_date
                        .as_deref()
                        .or(existing.utc_due_date.as_deref()),
                    complete: m.fields.complete.unwrap_or(existing.complete),
                    date_completed: m
                        .fields
                        .date_completed
                        .as_deref()
                        .or(existing.date_completed.as_deref()),
                    reminder_set: m.fields.reminder_set.unwrap_or(existing.reminder_set),
                    reminder_time: m
                        .fields
                        .reminder_time
                        .as_deref()
                        .or(existing.reminder_time.as_deref()),
                    categories: m
                        .fields
                        .categories
                        .as_deref()
                        .or(existing.categories.as_deref()),
                    body: m.fields.body.as_deref().or(existing.body.as_deref()),
                };
                match state
                    .storage
                    .upsert_task(username, &m.server_id, &fields)
                    .await
                {
                    Ok(()) => results.push(MutationResult {
                        server_id: m.server_id.clone(),
                        status: "1",
                        kind: MutationKind::Change,
                        client_id: m.client_id.clone(),
                    }),
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to update task");
                        results.push(MutationResult {
                            server_id: m.server_id.clone(),
                            status: "6",
                            kind: MutationKind::Change,
                            client_id: m.client_id.clone(),
                        });
                    }
                }
            }
        }
    }

    Ok(results)
}

/// Apply parsed note mutations against the gateway-local note store.
pub async fn apply_note_mutations(
    state: &AppState,
    username: &str,
    mutations: &[NoteMutation],
) -> Result<Vec<MutationResult>> {
    let mut results = Vec::new();

    for m in mutations {
        match m.kind {
            MutationKind::Delete => {
                if m.server_id.is_empty() {
                    results.push(MutationResult {
                        server_id: String::new(),
                        status: "6",
                        kind: MutationKind::Delete,
                        client_id: m.client_id.clone(),
                    });
                    continue;
                }
                match state.storage.delete_note(username, &m.server_id).await {
                    Ok(0) => {
                        // ServerId no longer exists on the server (MS-ASCMD status 8).
                        results.push(MutationResult {
                            server_id: m.server_id.clone(),
                            status: "8",
                            kind: MutationKind::Delete,
                            client_id: m.client_id.clone(),
                        });
                    }
                    Ok(_) => results.push(MutationResult {
                        server_id: m.server_id.clone(),
                        status: "1",
                        kind: MutationKind::Delete,
                        client_id: m.client_id.clone(),
                    }),
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to delete note");
                        results.push(MutationResult {
                            server_id: m.server_id.clone(),
                            status: "6",
                            kind: MutationKind::Delete,
                            client_id: m.client_id.clone(),
                        });
                    }
                }
            }
            MutationKind::Add => {
                let server_id = if m.server_id.is_empty() {
                    format!("note-{}", Uuid::new_v4().simple())
                } else {
                    m.server_id.clone()
                };
                let last_modified = now_utc_string();
                let fields = NoteFields {
                    subject: m.fields.subject.as_deref(),
                    message_class: m.fields.message_class.as_deref(),
                    body: m.fields.body.as_deref(),
                    categories: m.fields.categories.as_deref(),
                    last_modified_date: Some(last_modified.as_str()),
                };
                match state
                    .storage
                    .upsert_note(username, &server_id, &fields)
                    .await
                {
                    Ok(()) => results.push(MutationResult {
                        server_id,
                        status: "1",
                        kind: MutationKind::Add,
                        client_id: m.client_id.clone(),
                    }),
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to upsert note");
                        results.push(MutationResult {
                            server_id,
                            status: "6",
                            kind: MutationKind::Add,
                            client_id: m.client_id.clone(),
                        });
                    }
                }
            }
            MutationKind::Change => {
                if m.server_id.is_empty() {
                    results.push(MutationResult {
                        server_id: String::new(),
                        status: "6",
                        kind: MutationKind::Change,
                        client_id: m.client_id.clone(),
                    });
                    continue;
                }
                // A Change must reference an existing record and only overwrite the
                // properties the client actually supplied.
                let existing = match state.storage.get_note(username, &m.server_id).await {
                    Ok(Some(row)) => row,
                    Ok(None) => {
                        results.push(MutationResult {
                            server_id: m.server_id.clone(),
                            status: "8",
                            kind: MutationKind::Change,
                            client_id: m.client_id.clone(),
                        });
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to load note for change");
                        results.push(MutationResult {
                            server_id: m.server_id.clone(),
                            status: "6",
                            kind: MutationKind::Change,
                            client_id: m.client_id.clone(),
                        });
                        continue;
                    }
                };
                let last_modified = now_utc_string();
                let fields = NoteFields {
                    subject: m.fields.subject.as_deref().or(existing.subject.as_deref()),
                    message_class: m
                        .fields
                        .message_class
                        .as_deref()
                        .or(existing.message_class.as_deref()),
                    body: m.fields.body.as_deref().or(existing.body.as_deref()),
                    categories: m
                        .fields
                        .categories
                        .as_deref()
                        .or(existing.categories.as_deref()),
                    last_modified_date: Some(last_modified.as_str()),
                };
                match state
                    .storage
                    .upsert_note(username, &m.server_id, &fields)
                    .await
                {
                    Ok(()) => results.push(MutationResult {
                        server_id: m.server_id.clone(),
                        status: "1",
                        kind: MutationKind::Change,
                        client_id: m.client_id.clone(),
                    }),
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to update note");
                        results.push(MutationResult {
                            server_id: m.server_id.clone(),
                            status: "6",
                            kind: MutationKind::Change,
                            client_id: m.client_id.clone(),
                        });
                    }
                }
            }
        }
    }

    Ok(results)
}

/// `last_modified_date` in ISO 8601 UTC (MS-ASNOTE LastModifiedDate is informational).
fn now_utc_string() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Render `<Add>/<Change>/<Delete>` mutation responses for a Sync reply.
pub fn render_mutation_responses(results: &[MutationResult]) -> String {
    let mut xml = String::new();
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
    xml
}

#[cfg(test)]
mod tests {
    use super::*;

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
            updated_at: None,
        };
        let rendered = render_eas_note(&row);
        assert!(rendered.contains("<Notes:MessageClass>IPM.StickyNote</Notes:MessageClass>"));
        assert!(rendered.contains("<Notes:Subject>S</Notes:Subject>"));
        assert!(rendered.contains("<AirSyncBase:Data>B</AirSyncBase:Data>"));
    }
}

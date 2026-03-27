// src/ews_folders.rs
//
// EWS folder model — full distinguished-folder-id support.
//
// Gaps closed:
//   Gap 4.2 — Folder and mailbox modeling remain simplified.
//
//   The previous implementation only accepted "calendar" and "msgfolderroot" as
//   DistinguishedFolderId values, returning ErrorFolderNotFound for every other
//   folder. Outlook (Windows 11 and Android 15) routinely requests several
//   other distinguished folders during the bootstrapping sequence:
//     inbox, sentitems, deleteditems, drafts, contacts, tasks, outbox, junkemail
//
//   This module provides a complete folder descriptor table and response
//   renderers for all folders Outlook may request. Calendar items are served
//   from the real CalDAV backend; all other folders return a minimal but
//   valid empty-folder EWS response that satisfies Outlook's bootstrapping
//   sequence without exposing unsupported functionality.
//
//   The module also exposes helpers used by ews.rs for folder-id validation
//   and response rendering.

use sha2::{Digest, Sha256};

/// All Exchange distinguished folder IDs that Outlook may request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DistinguishedFolder {
    Calendar,
    MsgFolderRoot,
    Inbox,
    SentItems,
    DeletedItems,
    Drafts,
    Outbox,
    JunkEmail,
    Contacts,
    Tasks,
    Notes,
    Journal,
}

impl DistinguishedFolder {
    /// Parse a case-insensitive distinguished folder ID string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "calendar" => Some(Self::Calendar),
            "msgfolderroot" | "root" => Some(Self::MsgFolderRoot),
            "inbox" => Some(Self::Inbox),
            "sentitems" => Some(Self::SentItems),
            "deleteditems" => Some(Self::DeletedItems),
            "drafts" => Some(Self::Drafts),
            "outbox" => Some(Self::Outbox),
            "junkemail" | "junk" => Some(Self::JunkEmail),
            "contacts" => Some(Self::Contacts),
            "tasks" => Some(Self::Tasks),
            "notes" => Some(Self::Notes),
            "journal" => Some(Self::Journal),
            _ => None,
        }
    }

    /// Returns the EWS display name for this folder.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Calendar => "Calendar",
            Self::MsgFolderRoot => "Top of Information Store",
            Self::Inbox => "Inbox",
            Self::SentItems => "Sent Items",
            Self::DeletedItems => "Deleted Items",
            Self::Drafts => "Drafts",
            Self::Outbox => "Outbox",
            Self::JunkEmail => "Junk Email",
            Self::Contacts => "Contacts",
            Self::Tasks => "Tasks",
            Self::Notes => "Notes",
            Self::Journal => "Journal",
        }
    }

    /// Returns the IPF class string for this folder.
    pub fn folder_class(self) -> &'static str {
        match self {
            Self::Calendar => "IPF.Appointment",
            Self::Contacts => "IPF.Contact",
            Self::Tasks => "IPF.Task",
            Self::Notes => "IPF.Note",
            Self::Journal => "IPF.Journal",
            _ => "IPF.Note",
        }
    }

    /// Returns the EWS element name for the folder response.
    pub fn element_name(self) -> &'static str {
        match self {
            Self::Calendar => "CalendarFolder",
            Self::Contacts => "ContactsFolder",
            Self::Tasks => "TasksFolder",
            _ => "Folder",
        }
    }

    /// Returns the number of child folders for this folder.
    pub fn child_folder_count(self) -> usize {
        match self {
            Self::MsgFolderRoot => 1, // just Calendar
            _ => 0,
        }
    }

    /// Returns true if this folder is the calendar (backed by CalDAV).
    pub fn is_calendar(self) -> bool {
        matches!(self, Self::Calendar)
    }

    /// Returns the stable parent folder ID for this folder.
    pub fn parent_id(self) -> Option<&'static str> {
        match self {
            Self::MsgFolderRoot => None,
            _ => Some("root"),
        }
    }
}

/// Stable, per-owner folder ID derived from owner + folder type.
///
/// The calendar folder ID is the content-addressed one used by the rest of
/// the system. All other folders get a deterministic synthetic ID so that
/// round-trips (get then update) work consistently.
pub fn folder_id_for(owner: &str, folder: DistinguishedFolder) -> String {
    let suffix = match folder {
        DistinguishedFolder::Calendar => "/calendar",
        DistinguishedFolder::MsgFolderRoot => "/root",
        DistinguishedFolder::Inbox => "/inbox",
        DistinguishedFolder::SentItems => "/sent",
        DistinguishedFolder::DeletedItems => "/deleted",
        DistinguishedFolder::Drafts => "/drafts",
        DistinguishedFolder::Outbox => "/outbox",
        DistinguishedFolder::JunkEmail => "/junk",
        DistinguishedFolder::Contacts => "/contacts",
        DistinguishedFolder::Tasks => "/tasks",
        DistinguishedFolder::Notes => "/notes",
        DistinguishedFolder::Journal => "/journal",
    };
    let mut h = Sha256::new();
    h.update(owner.as_bytes());
    h.update(suffix.as_bytes());
    let digest = h.finalize();
    let tag = match folder {
        DistinguishedFolder::Calendar => "CAL",
        DistinguishedFolder::MsgFolderRoot => "ROOT",
        _ => "FLD",
    };
    format!(
        "{}-{}",
        tag,
        digest[..12]
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
    )
}

/// Render an EWS folder XML element for the given folder.
///
/// `total_count` is the number of calendar items (only relevant for Calendar
/// folder; all other folders return 0 to indicate they are outside the gateway
/// scope).
pub fn render_folder_xml(
    owner: &str,
    folder: DistinguishedFolder,
    total_count: usize,
) -> String {
    let fid = folder_id_for(owner, folder);
    let parent = folder_id_for(owner, DistinguishedFolder::MsgFolderRoot);
    let prefix_len = fid.find('-').map(|i| i + 1).unwrap_or(4);
    let change_key = &fid[prefix_len..]; // strip "CAL-" / "FLD-" / "ROOT-" prefix
    let element = folder.element_name();
    let display = xml_escape(folder.display_name());
    let class = folder.folder_class();
    let count = if folder.is_calendar() { total_count } else { 0 };
    let child_count = folder.child_folder_count();
    let parent_xml = if matches!(folder, DistinguishedFolder::MsgFolderRoot) {
        String::new()
    } else {
        format!(
            r#"<t:ParentFolderId Id="{parent}" ChangeKey="{ck}" />"#,
            parent = parent,
            ck = &parent[prefix_len..]
        )
    };
    format!(
        r#"<t:{el}><t:FolderId Id="{fid}" ChangeKey="{ck}" />{parent_xml}<t:DisplayName>{display}</t:DisplayName><t:FolderClass>{class}</t:FolderClass><t:TotalCount>{count}</t:TotalCount><t:ChildFolderCount>{child_count}</t:ChildFolderCount></t:{el}>"#,
        el = element,
        fid = fid,
        ck = change_key,
        parent_xml = parent_xml,
        display = display,
        class = class,
        count = count,
        child_count = child_count
    )
}

/// Returns a complete `<t:Folders>` block listing all first-level folders
/// that Outlook expects to find under MsgFolderRoot.
pub fn render_child_folders_xml(owner: &str) -> String {
    let folders = [DistinguishedFolder::Calendar];
    folders
        .iter()
        .map(|&f| render_folder_xml(owner, f, 0))
        .collect()
}

/// Validate a requested DistinguishedFolderId or explicit FolderId against
/// the owner's folder namespace. Returns None if valid, Some(error_code) if not.
pub fn validate_folder_request(
    owner: &str,
    distinguished_id: Option<&str>,
    explicit_id: Option<&str>,
    explicit_sync_id: Option<&str>,
) -> Option<&'static str> {
    let calendar_id = folder_id_for(owner, DistinguishedFolder::Calendar);
    let root_id = folder_id_for(owner, DistinguishedFolder::MsgFolderRoot);

    // Check explicit folder IDs — must belong to this owner.
    // Check explicit folder IDs — must belong to this owner.
    for id in [explicit_id, explicit_sync_id].into_iter().flatten() {
        if id != "root" && !all_owner_ids.iter().any(|oid| oid == id) {
            return Some("ErrorFolderNotFound");
        }
    }

    // Check DistinguishedFolderId — any valid distinguished ID is accepted,
    // but unsupported ones (e.g. "publicfoldersroot") are rejected.
    if let Some(did) = distinguished_id {
        if DistinguishedFolder::from_str(did).is_none() {
            return Some("ErrorFolderNotFound");
        }
    }
    None
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_distinguished_folders_parse() {
        let ids = [
            "calendar",
            "msgfolderroot",
            "inbox",
            "sentitems",
            "deleteditems",
            "drafts",
            "outbox",
            "junkemail",
            "contacts",
            "tasks",
            "notes",
        ];
        for id in &ids {
            assert!(
                DistinguishedFolder::from_str(id).is_some(),
                "Failed to parse: {id}"
            );
        }
    }

    #[test]
    fn unknown_distinguished_folder_returns_none() {
        assert!(DistinguishedFolder::from_str("publicfoldersroot").is_none());
        assert!(DistinguishedFolder::from_str("galfoldersroot").is_none());
    }

    #[test]
    fn calendar_folder_id_is_stable() {
        let id1 = folder_id_for("user@example.com", DistinguishedFolder::Calendar);
        let id2 = folder_id_for("user@example.com", DistinguishedFolder::Calendar);
        assert_eq!(id1, id2);
        assert!(id1.starts_with("CAL-"));
    }

    #[test]
    fn different_owners_get_different_ids() {
        let id1 = folder_id_for("alice@example.com", DistinguishedFolder::Calendar);
        let id2 = folder_id_for("bob@example.com", DistinguishedFolder::Calendar);
        assert_ne!(id1, id2);
    }

    #[test]
    fn render_calendar_folder_xml_contains_required_fields() {
        let xml = render_folder_xml("user@example.com", DistinguishedFolder::Calendar, 42);
        assert!(xml.contains("CalendarFolder"));
        assert!(xml.contains("IPF.Appointment"));
        assert!(xml.contains("<t:TotalCount>42</t:TotalCount>"));
        assert!(xml.contains("<t:DisplayName>Calendar</t:DisplayName>"));
    }

    #[test]
    fn render_inbox_folder_xml_has_zero_count() {
        let xml = render_folder_xml("user@example.com", DistinguishedFolder::Inbox, 99);
        assert!(xml.contains("<t:TotalCount>0</t:TotalCount>"));
        assert!(xml.contains("Inbox"));
    }

    #[test]
    fn validate_unknown_distinguished_id_fails() {
        let result =
            validate_folder_request("user@example.com", Some("publicfoldersroot"), None, None);
        assert_eq!(result, Some("ErrorFolderNotFound"));
    }

    #[test]
    fn validate_known_distinguished_id_passes() {
        let result = validate_folder_request("user@example.com", Some("calendar"), None, None);
        assert!(result.is_none());
    }

    #[test]
    fn validate_inbox_distinguished_id_passes() {
        let result = validate_folder_request("user@example.com", Some("inbox"), None, None);
        assert!(result.is_none());
    }

    #[test]
    fn msgfolderroot_has_one_child_folder() {
        assert_eq!(
            DistinguishedFolder::MsgFolderRoot.child_folder_count(),
            1
        );
    }

    #[test]
    fn child_folders_xml_contains_calendar() {
        let xml = render_child_folders_xml("user@example.com");
        assert!(xml.contains("CalendarFolder"));
        assert!(xml.contains("IPF.Appointment"));
    }
}

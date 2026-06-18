// src/ews_folders.rs
use crate::util::xml_escape;
use const_hex;
use sha2::{Digest, Sha256};

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

use std::str::FromStr;

impl DistinguishedFolder {
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

    pub fn folder_class(self) -> &'static str {
        match self {
            Self::Calendar => "IPF.Appointment",
            Self::Contacts => "IPF.Contact",
            Self::Tasks => "IPF.Task",
            Self::Notes => "IPF.Note",
            Self::Journal => "IPF.Journal",
            Self::MsgFolderRoot => "IPF",
            _ => "IPF.Note",
        }
    }

    pub fn element_name(self) -> &'static str {
        match self {
            Self::Calendar => "CalendarFolder",
            Self::Contacts => "ContactsFolder",
            Self::Tasks => "TasksFolder",
            _ => "Folder",
        }
    }

    /// All folders that are direct children of MsgFolderRoot.
    /// Note: Contacts, Tasks, Notes, Journal are not exposed as syncable folders
    /// by the gateway and are excluded here to prevent clients from attempting
    /// to sync them via EAS/EWS.
    pub fn root_children() -> &'static [DistinguishedFolder] {
        &[
            DistinguishedFolder::Calendar,
            DistinguishedFolder::Inbox,
            DistinguishedFolder::SentItems,
            DistinguishedFolder::DeletedItems,
            DistinguishedFolder::Drafts,
            DistinguishedFolder::Outbox,
            DistinguishedFolder::JunkEmail,
        ]
    }

    pub fn child_folder_count(self) -> usize {
        match self {
            Self::MsgFolderRoot => Self::root_children().len(),
            _ => 0,
        }
    }

    pub fn is_calendar(self) -> bool {
        matches!(self, Self::Calendar)
    }

    /// Returns true if this folder contains email messages.
    pub fn is_email(self) -> bool {
        matches!(
            self,
            Self::Inbox
                | Self::SentItems
                | Self::DeletedItems
                | Self::Drafts
                | Self::Outbox
                | Self::JunkEmail
                | Self::MsgFolderRoot
        )
    }

    pub fn parent_id(self) -> Option<&'static str> {
        match self {
            Self::MsgFolderRoot => None,
            _ => Some("root"),
        }
    }
}

impl FromStr for DistinguishedFolder {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "calendar" => Ok(Self::Calendar),
            "msgfolderroot" | "root" => Ok(Self::MsgFolderRoot),
            "inbox" => Ok(Self::Inbox),
            "sentitems" => Ok(Self::SentItems),
            "deleteditems" => Ok(Self::DeletedItems),
            "drafts" => Ok(Self::Drafts),
            "outbox" => Ok(Self::Outbox),
            "junkemail" | "junk" => Ok(Self::JunkEmail),
            "contacts" => Ok(Self::Contacts),
            "tasks" => Ok(Self::Tasks),
            "notes" => Ok(Self::Notes),
            "journal" => Ok(Self::Journal),
            _ => Err(()),
        }
    }
}

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
    format!("{}-{}", tag, const_hex::encode(&digest[..12]))
}

/// Resolve an explicit FolderId (as returned by folder_id_for) back to a DistinguishedFolder.
/// Returns None if the ID doesn't match any known folder.
pub fn resolve_folder_id(id: &str, owner: &str) -> Option<DistinguishedFolder> {
    let all_folders: &[DistinguishedFolder] = &[
        DistinguishedFolder::MsgFolderRoot,
        DistinguishedFolder::Calendar,
        DistinguishedFolder::Inbox,
        DistinguishedFolder::SentItems,
        DistinguishedFolder::DeletedItems,
        DistinguishedFolder::Drafts,
        DistinguishedFolder::Outbox,
        DistinguishedFolder::JunkEmail,
        DistinguishedFolder::Contacts,
        DistinguishedFolder::Tasks,
        DistinguishedFolder::Notes,
        DistinguishedFolder::Journal,
    ];
    all_folders
        .iter()
        .find(|&&f| folder_id_for(owner, f) == id)
        .copied()
}

pub fn render_folder_xml(owner: &str, folder: DistinguishedFolder, total_count: usize) -> String {
    let fid = folder_id_for(owner, folder);
    let parent = folder_id_for(owner, DistinguishedFolder::MsgFolderRoot);
    let prefix_len = fid.find('-').map(|i| i + 1).unwrap_or(4);
    let change_key = &fid[prefix_len..];
    let element = folder.element_name();
    let display = xml_escape(folder.display_name());
    let class = folder.folder_class();
    let count = if folder.is_calendar() { total_count } else { 0 };
    let child_count = folder.child_folder_count();
    let parent_xml = if matches!(folder, DistinguishedFolder::MsgFolderRoot) {
        String::new()
    } else {
        let parent_prefix_len = parent.find('-').map(|i| i + 1).unwrap_or(5);
        format!(
            r#"<t:ParentFolderId Id="{parent}" ChangeKey="{ck}" />"#,
            parent = parent,
            ck = &parent[parent_prefix_len..]
        )
    };
    format!(
        r#"<t:{el}><t:FolderId Id="{fid}" ChangeKey="{ck}" />{parent_xml}<t:DisplayName>{display}</t:DisplayName><t:FolderClass>{class}</t:FolderClass><t:TotalCount>{count}</t:TotalCount><t:ChildFolderCount>{child_count}</t:ChildFolderCount><t:UnreadCount>0</t:UnreadCount><t:EffectiveRights><t:CreateAssociated>false</t:CreateAssociated><t:CreateContents>true</t:CreateContents><t:CreateHierarchy>false</t:CreateHierarchy><t:Delete>true</t:Delete><t:Modify>true</t:Modify><t:Read>true</t:Read></t:EffectiveRights></t:{el}>"#,
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

pub fn render_child_folders_xml(owner: &str) -> String {
    DistinguishedFolder::root_children()
        .iter()
        .map(|&f| render_folder_xml(owner, f, 0))
        .collect()
}

/// Render the full folder hierarchy as `<t:Create>` elements for SyncFolderHierarchy.
/// Returns MsgFolderRoot first, then all direct children.
pub fn render_folder_hierarchy_creates(owner: &str, calendar_item_count: usize) -> String {
    let mut creates = String::new();
    // MsgFolderRoot must come first — clients need its FolderId for GetUserConfiguration
    let root_xml = render_folder_xml(owner, DistinguishedFolder::MsgFolderRoot, 0);
    creates.push_str(&format!("<t:Create>{}</t:Create>", root_xml));
    // Calendar folder with actual item count
    let cal_xml = render_folder_xml(owner, DistinguishedFolder::Calendar, calendar_item_count);
    creates.push_str(&format!("<t:Create>{}</t:Create>", cal_xml));
    // All other children
    for &f in DistinguishedFolder::root_children() {
        if f != DistinguishedFolder::Calendar {
            let xml = render_folder_xml(owner, f, 0);
            creates.push_str(&format!("<t:Create>{}</t:Create>", xml));
        }
    }
    creates
}

/// Render the direct children of MsgFolderRoot. Used by FindFolder when querying msgfolderroot.
pub fn render_root_and_children(owner: &str, calendar_item_count: usize) -> (usize, String) {
    let children = DistinguishedFolder::root_children();
    let total = children.len();
    let mut xml = render_folder_xml(owner, DistinguishedFolder::Calendar, calendar_item_count);
    // All other children
    for &f in children {
        if f != DistinguishedFolder::Calendar {
            xml.push_str(&render_folder_xml(owner, f, 0));
        }
    }
    (total, xml)
}

pub fn validate_folder_request(
    owner: &str,
    distinguished_id: Option<&str>,
    explicit_id: Option<&str>,
    explicit_sync_id: Option<&str>,
) -> Option<&'static str> {
    let all_owner_ids: Vec<String> = [
        DistinguishedFolder::Calendar,
        DistinguishedFolder::MsgFolderRoot,
        DistinguishedFolder::Inbox,
        DistinguishedFolder::SentItems,
        DistinguishedFolder::DeletedItems,
        DistinguishedFolder::Drafts,
        DistinguishedFolder::Outbox,
        DistinguishedFolder::JunkEmail,
        DistinguishedFolder::Contacts,
        DistinguishedFolder::Tasks,
        DistinguishedFolder::Notes,
        DistinguishedFolder::Journal,
    ]
    .iter()
    .map(|&f| folder_id_for(owner, f))
    .collect();

    for id in [explicit_id, explicit_sync_id].into_iter().flatten() {
        if id != "root" && !all_owner_ids.iter().any(|oid| oid == id) {
            return Some("ErrorFolderNotFound");
        }
    }

    if distinguished_id.is_some_and(|did| DistinguishedFolder::from_str(did).is_err()) {
        return Some("ErrorFolderNotFound");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_folder_id_roundtrip() {
        let owner = "contact@example.com";
        for &folder in DistinguishedFolder::root_children() {
            let id = folder_id_for(owner, folder);
            let resolved = resolve_folder_id(&id, owner);
            assert_eq!(
                resolved,
                Some(folder),
                "resolve_folder_id({}) should return {:?}",
                id,
                folder
            );
        }
        // MsgFolderRoot
        let root_id = folder_id_for(owner, DistinguishedFolder::MsgFolderRoot);
        assert_eq!(
            resolve_folder_id(&root_id, owner),
            Some(DistinguishedFolder::MsgFolderRoot)
        );
    }

    #[test]
    fn test_resolve_folder_id_unknown_returns_none() {
        assert_eq!(resolve_folder_id("unknown-id", "test@example.com"), None);
    }

    #[test]
    fn test_render_folder_hierarchy_creates_includes_root() {
        let owner = "contact@example.com";
        let xml = render_folder_hierarchy_creates(owner, 0);
        // Must include MsgFolderRoot
        assert!(
            xml.contains("Top of Information Store"),
            "Folder hierarchy must include MsgFolderRoot"
        );
        // Must include Calendar
        assert!(
            xml.contains("<t:CalendarFolder>"),
            "Folder hierarchy must include Calendar"
        );
        // Must include Inbox
        assert!(
            xml.contains(">Inbox<"),
            "Folder hierarchy must include Inbox"
        );
        // Must include Sent Items
        assert!(
            xml.contains(">Sent Items<"),
            "Folder hierarchy must include Sent Items"
        );
        // All must be wrapped in <t:Create>
        assert!(
            xml.contains("<t:Create>"),
            "Folders must be wrapped in <t:Create>"
        );
    }

    #[test]
    fn test_render_root_and_children_count() {
        let owner = "contact@example.com";
        let (total, xml) = render_root_and_children(owner, 5);
        // Expect only the direct children of MsgFolderRoot, not MsgFolderRoot itself.
        assert_eq!(total, DistinguishedFolder::root_children().len());
        assert!(
            !xml.contains("Top of Information Store"),
            "Must not include MsgFolderRoot as its own child"
        );
        assert!(xml.contains(">Calendar<"), "Must include Calendar");
    }

    #[test]
    fn test_parent_folder_id_structure() {
        let owner = "contact@example.com";
        let xml = render_folder_xml(owner, DistinguishedFolder::Inbox, 0);
        // Ensure ParentFolderId element exists and has both Id and ChangeKey attributes directly on it.
        assert!(
            xml.contains("<t:ParentFolderId"),
            "Missing ParentFolderId element"
        );
        // Find the ParentFolderId element and verify its attributes.
        if let Some(start) = xml.find("<t:ParentFolderId") {
            let rest = &xml[start..];
            // Find the closing of the element (/> or >)
            let end_idx = rest
                .find("/>")
                .map(|i| i + 2)
                .or_else(|| rest.find('>').map(|i| i + 1))
                .unwrap_or(rest.len());
            let parent_el = &rest[..end_idx];
            assert!(
                parent_el.contains("Id=\"") && parent_el.contains("ChangeKey=\""),
                "ParentFolderId must have both Id and ChangeKey attributes"
            );
            assert!(
                !parent_el.contains("<t:FolderId"),
                "ParentFolderId must NOT contain nested FolderId element (non-standard)"
            );
        } else {
            panic!("ParentFolderId element not found");
        }
        // Check that ParentFolderId appears after FolderId and before DisplayName
        let folder_id_pos = xml.find("<t:FolderId Id=").unwrap();
        let parent_id_pos = xml.find("<t:ParentFolderId").unwrap();
        let display_name_pos = xml.find("<t:DisplayName>").unwrap();
        assert!(
            folder_id_pos < parent_id_pos,
            "FolderId must come before ParentFolderId"
        );
        assert!(
            parent_id_pos < display_name_pos,
            "ParentFolderId must come before DisplayName"
        );
    }
}

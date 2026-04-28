// src/ews_folders.rs
use crate::util::xml_escape;
use const_hex;
use heck::ToSnakeCase;
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

    pub fn child_folder_count(self) -> usize {
        match self {
            Self::MsgFolderRoot => 1,
            _ => 0,
        }
    }

    pub fn is_calendar(self) -> bool {
        matches!(self, Self::Calendar)
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
        // Use heck for robust snake_case normalization before matching
        let normalized = s.to_snake_case();
        match normalized.as_str() {
            "calendar" => Ok(Self::Calendar),
            "msg_folder_root" | "root" => Ok(Self::MsgFolderRoot),
            "inbox" => Ok(Self::Inbox),
            "sent_items" => Ok(Self::SentItems),
            "deleted_items" => Ok(Self::DeletedItems),
            "drafts" => Ok(Self::Drafts),
            "outbox" => Ok(Self::Outbox),
            "junk_email" | "junk" => Ok(Self::JunkEmail),
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
    let folders = [DistinguishedFolder::Calendar];
    folders
        .iter()
        .map(|&f| render_folder_xml(owner, f, 0))
        .collect()
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

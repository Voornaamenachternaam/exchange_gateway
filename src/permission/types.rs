// src/permission/types.rs
use bitflags::bitflags;
use serde::{Deserialize, Serialize};
use std::fmt;

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct PermissionRights: u32 {
        const READ_ANY          = 0x00000001;
        const CREATE            = 0x00000002;
        const EDIT_OWNED        = 0x00000008;
        const DELETE_OWNED      = 0x00000010;
        const EDIT_ANY          = 0x00000020;
        const DELETE_ANY        = 0x00000040;
        const CREATE_SUBFOLDER  = 0x00000080;
        const FOLDER_OWNER      = 0x00000100;
        const FOLDER_CONTACT    = 0x00000200;
        const FOLDER_VISIBLE    = 0x00000400;
        const FREEBUSY_SIMPLE   = 0x00000800;
        const FREEBUSY_DETAILED = 0x00001000;
    }
}

// Serde: serialize as u32, deserialize from u32 — preserves wire format compatibility
impl Serialize for PermissionRights {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(self.bits())
    }
}

impl<'de> Deserialize<'de> for PermissionRights {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bits = u32::deserialize(deserializer)?;
        Ok(Self::from_bits_retain(bits))
    }
}

impl PermissionRights {
    // Named single-flag constructors
    pub fn read_any() -> Self {
        Self::READ_ANY
    }
    pub fn create() -> Self {
        Self::CREATE
    }
    pub fn edit_owned() -> Self {
        Self::EDIT_OWNED
    }
    pub fn delete_owned() -> Self {
        Self::DELETE_OWNED
    }
    pub fn edit_any() -> Self {
        Self::EDIT_ANY | Self::EDIT_OWNED
    }
    pub fn delete_any() -> Self {
        Self::DELETE_ANY | Self::DELETE_OWNED
    }
    pub fn folder_owner() -> Self {
        Self::FOLDER_OWNER | Self::FOLDER_VISIBLE
    }
    pub fn folder_contact() -> Self {
        Self::FOLDER_CONTACT
    }
    pub fn folder_visible() -> Self {
        Self::FOLDER_VISIBLE
    }
    pub fn freebusy_simple() -> Self {
        Self::FREEBUSY_SIMPLE
    }
    pub fn freebusy_detailed() -> Self {
        Self::FREEBUSY_DETAILED | Self::FREEBUSY_SIMPLE
    }

    // Named composite constructors (permission levels)
    pub fn none() -> Self {
        Self::empty()
    }
    pub fn reviewer() -> Self {
        Self::READ_ANY | Self::FOLDER_VISIBLE
    }
    pub fn contributor() -> Self {
        Self::CREATE | Self::FOLDER_VISIBLE
    }
    pub fn author() -> Self {
        Self::READ_ANY | Self::CREATE | Self::EDIT_OWNED | Self::DELETE_OWNED | Self::FOLDER_VISIBLE
    }
    pub fn non_editing_author() -> Self {
        Self::READ_ANY | Self::CREATE | Self::DELETE_OWNED | Self::FOLDER_VISIBLE
    }
    pub fn editor() -> Self {
        Self::READ_ANY | Self::CREATE | Self::edit_any() | Self::delete_any() | Self::FOLDER_VISIBLE
    }
    pub fn publishing_author() -> Self {
        Self::READ_ANY
            | Self::CREATE
            | Self::EDIT_OWNED
            | Self::DELETE_OWNED
            | Self::CREATE_SUBFOLDER
            | Self::FOLDER_VISIBLE
    }
    pub fn publishing_editor() -> Self {
        Self::READ_ANY
            | Self::CREATE
            | Self::edit_any()
            | Self::delete_any()
            | Self::CREATE_SUBFOLDER
            | Self::FOLDER_VISIBLE
    }
    pub fn owner() -> Self {
        Self::READ_ANY
            | Self::CREATE
            | Self::edit_any()
            | Self::delete_any()
            | Self::CREATE_SUBFOLDER
            | Self::FOLDER_OWNER
            | Self::FOLDER_CONTACT
            | Self::FOLDER_VISIBLE
    }
    pub fn freebusy() -> Self {
        Self::FREEBUSY_SIMPLE | Self::FREEBUSY_DETAILED | Self::FOLDER_VISIBLE
    }

    // Convenience predicates (preserving the existing API names)
    pub fn can_read_any(&self) -> bool {
        self.contains(Self::READ_ANY)
    }
    pub fn can_create(&self) -> bool {
        self.contains(Self::CREATE)
    }
    pub fn can_edit_owned(&self) -> bool {
        self.contains(Self::EDIT_OWNED)
    }
    pub fn can_delete_owned(&self) -> bool {
        self.contains(Self::DELETE_OWNED)
    }
    pub fn can_edit_any(&self) -> bool {
        self.contains(Self::EDIT_ANY)
    }
    pub fn can_delete_any(&self) -> bool {
        self.contains(Self::DELETE_ANY)
    }
    pub fn can_create_subfolder(&self) -> bool {
        self.contains(Self::CREATE_SUBFOLDER)
    }
    pub fn is_folder_owner(&self) -> bool {
        self.contains(Self::FOLDER_OWNER)
    }
    pub fn is_folder_contact(&self) -> bool {
        self.contains(Self::FOLDER_CONTACT)
    }
    pub fn is_folder_visible(&self) -> bool {
        self.contains(Self::FOLDER_VISIBLE)
    }
    pub fn can_freebusy_simple(&self) -> bool {
        self.contains(Self::FREEBUSY_SIMPLE)
    }
    pub fn can_freebusy_detailed(&self) -> bool {
        self.contains(Self::FREEBUSY_DETAILED)
    }
}

impl fmt::Display for PermissionRights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return write!(f, "None");
        }
        let mut parts = Vec::new();
        if self.can_read_any() {
            parts.push("ReadAny");
        }
        if self.can_create() {
            parts.push("Create");
        }
        if self.can_edit_owned() {
            parts.push("EditOwned");
        }
        if self.can_delete_owned() {
            parts.push("DeleteOwned");
        }
        if self.can_edit_any() {
            parts.push("EditAny");
        }
        if self.can_delete_any() {
            parts.push("DeleteAny");
        }
        if self.can_create_subfolder() {
            parts.push("CreateSubfolder");
        }
        if self.is_folder_owner() {
            parts.push("FolderOwner");
        }
        if self.is_folder_contact() {
            parts.push("FolderContact");
        }
        if self.is_folder_visible() {
            parts.push("FolderVisible");
        }
        if self.can_freebusy_simple() {
            parts.push("FreeBusySimple");
        }
        if self.can_freebusy_detailed() {
            parts.push("FreeBusyDetailed");
        }
        write!(f, "{}", parts.join("|"))
    }
}

impl From<u32> for PermissionRights {
    fn from(value: u32) -> Self {
        Self::from_bits_retain(value)
    }
}

impl From<PermissionRights> for u32 {
    fn from(value: PermissionRights) -> Self {
        value.bits()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PermissionLevel {
    #[default]
    None = 0,
    FreeBusy = 1,
    Reviewer = 2,
    Contributor = 3,
    NonEditingAuthor = 4,
    Author = 5,
    PublishingAuthor = 6,
    Editor = 7,
    PublishingEditor = 8,
    Owner = 9,
}

impl PermissionLevel {
    pub fn to_rights(&self) -> PermissionRights {
        match self {
            Self::None => PermissionRights::none(),
            Self::FreeBusy => PermissionRights::freebusy(),
            Self::Reviewer => PermissionRights::reviewer(),
            Self::Contributor => PermissionRights::contributor(),
            Self::NonEditingAuthor => PermissionRights::non_editing_author(),
            Self::Author => PermissionRights::author(),
            Self::PublishingAuthor => PermissionRights::publishing_author(),
            Self::Editor => PermissionRights::editor(),
            Self::PublishingEditor => PermissionRights::publishing_editor(),
            Self::Owner => PermissionRights::owner(),
        }
    }

    pub fn from_rights(rights: &PermissionRights) -> Self {
        if rights.is_folder_owner() {
            return Self::Owner;
        }
        if rights.contains(PermissionRights::publishing_editor()) {
            return Self::PublishingEditor;
        }
        if rights.contains(PermissionRights::editor()) {
            return Self::Editor;
        }
        if rights.contains(PermissionRights::publishing_author()) {
            return Self::PublishingAuthor;
        }
        if rights.contains(PermissionRights::author()) {
            return Self::Author;
        }
        if rights.contains(PermissionRights::non_editing_author()) {
            return Self::NonEditingAuthor;
        }
        if rights.contains(PermissionRights::contributor()) {
            return Self::Contributor;
        }
        if rights.contains(PermissionRights::reviewer()) {
            return Self::Reviewer;
        }
        if rights.can_freebusy_simple() {
            return Self::FreeBusy;
        }
        Self::None
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "freebusy" | "freebusyonly" => Some(Self::FreeBusy),
            "reviewer" => Some(Self::Reviewer),
            "contributor" => Some(Self::Contributor),
            "noneditingauthor" => Some(Self::NonEditingAuthor),
            "author" => Some(Self::Author),
            "publishingauthor" => Some(Self::PublishingAuthor),
            "editor" => Some(Self::Editor),
            "publishingeditor" => Some(Self::PublishingEditor),
            "owner" => Some(Self::Owner),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "None",
            // EWS uses FreeBusyTimeOnly for this permission level
            Self::FreeBusy => "FreeBusyTimeOnly",
            Self::Reviewer => "Reviewer",
            Self::Contributor => "Contributor",
            Self::NonEditingAuthor => "NonEditingAuthor",
            Self::Author => "Author",
            Self::PublishingAuthor => "PublishingAuthor",
            Self::Editor => "Editor",
            Self::PublishingEditor => "PublishingEditor",
            Self::Owner => "Owner",
        }
    }
}

impl fmt::Display for PermissionLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl From<u8> for PermissionLevel {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::None,
            1 => Self::FreeBusy,
            2 => Self::Reviewer,
            3 => Self::Contributor,
            4 => Self::NonEditingAuthor,
            5 => Self::Author,
            6 => Self::PublishingAuthor,
            7 => Self::Editor,
            8 => Self::PublishingEditor,
            9 => Self::Owner,
            _ => Self::None,
        }
    }
}

impl From<PermissionLevel> for u8 {
    fn from(value: PermissionLevel) -> Self {
        value as u8
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CalendarPermission {
    pub id: String,
    pub folder_id: String,
    pub owner: String,
    pub user_email: String,
    pub user_name: Option<String>,
    pub rights: u32,
    pub is_default: bool,
    pub is_anonymous: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl CalendarPermission {
    pub fn new(
        folder_id: String,
        owner: String,
        user_email: String,
        rights: PermissionRights,
    ) -> Self {
        let now = chrono::Utc::now();
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            id,
            folder_id,
            owner,
            user_email,
            user_name: None,
            rights: rights.bits(),
            is_default: false,
            is_anonymous: false,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn default_permission(folder_id: String, owner: String, rights: PermissionRights) -> Self {
        let now = chrono::Utc::now();
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            id,
            folder_id,
            owner,
            user_email: "default".to_string(),
            user_name: Some("Default".to_string()),
            rights: rights.bits(),
            is_default: true,
            is_anonymous: false,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn anonymous_permission(
        folder_id: String,
        owner: String,
        rights: PermissionRights,
    ) -> Self {
        let now = chrono::Utc::now();
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            id,
            folder_id,
            owner,
            user_email: "anonymous".to_string(),
            user_name: Some("Anonymous".to_string()),
            rights: rights.bits(),
            is_default: false,
            is_anonymous: true,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn rights(&self) -> PermissionRights {
        PermissionRights::from_bits_retain(self.rights)
    }

    pub fn set_rights(&mut self, rights: PermissionRights) {
        self.rights = rights.bits();
        self.updated_at = chrono::Utc::now();
    }

    pub fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::from_rights(&self.rights())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DelegateInfo {
    pub id: String,
    pub delegator: String,
    pub delegate_email: String,
    pub delegate_name: Option<String>,
    pub calendar_permission: u8,
    pub inbox_permission: u8,
    pub tasks_permission: u8,
    pub contacts_permission: u8,
    pub notes_permission: u8,
    pub journal_permission: u8,
    pub receive_copies: bool,
    pub receive_infos: bool,
    pub view_private: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl DelegateInfo {
    pub fn new(delegator: String, delegate_email: String, delegate_name: Option<String>) -> Self {
        let now = chrono::Utc::now();
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            id,
            delegator,
            delegate_email,
            delegate_name,
            calendar_permission: PermissionLevel::Reviewer as u8,
            inbox_permission: PermissionLevel::None as u8,
            tasks_permission: PermissionLevel::None as u8,
            contacts_permission: PermissionLevel::None as u8,
            notes_permission: PermissionLevel::None as u8,
            journal_permission: PermissionLevel::None as u8,
            receive_copies: false,
            receive_infos: false,
            view_private: false,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn set_calendar_permission(&mut self, level: PermissionLevel) {
        self.calendar_permission = level as u8;
        self.updated_at = chrono::Utc::now();
    }

    pub fn calendar_permission_level(&self) -> PermissionLevel {
        PermissionLevel::from(self.calendar_permission)
    }

    pub fn to_calendar_rights(&self) -> PermissionRights {
        self.calendar_permission_level().to_rights()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DelegatePermission {
    #[default]
    None = 0,
    Author = 1,
    Editor = 2,
}

impl DelegatePermission {
    pub fn to_rights(&self) -> PermissionRights {
        match self {
            Self::None => PermissionRights::none(),
            Self::Author => PermissionRights::author(),
            Self::Editor => PermissionRights::editor(),
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "author" => Some(Self::Author),
            "editor" => Some(Self::Editor),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Author => "Author",
            Self::Editor => "Editor",
        }
    }
}

impl fmt::Display for DelegatePermission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PermissionAuditEntry {
    pub id: String,
    pub folder_id: String,
    pub owner: String,
    pub actor_email: String,
    pub target_email: String,
    pub operation: String,
    pub old_rights: Option<u32>,
    pub new_rights: Option<u32>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl PermissionAuditEntry {
    pub fn new(
        folder_id: String,
        owner: String,
        actor_email: String,
        target_email: String,
        operation: String,
        old_rights: Option<u32>,
        new_rights: Option<u32>,
    ) -> Self {
        let now = chrono::Utc::now();
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            id,
            folder_id,
            owner,
            actor_email,
            target_email,
            operation,
            old_rights,
            new_rights,
            created_at: now,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_rights_flags() {
        let rights = PermissionRights::owner();
        assert!(rights.can_read_any());
        assert!(rights.can_create());
        assert!(rights.can_edit_any());
        assert!(rights.can_delete_any());
        assert!(rights.is_folder_owner());
    }

    #[test]
    fn test_permission_level_conversion() {
        let level = PermissionLevel::Editor;
        let rights = level.to_rights();
        assert!(rights.can_edit_any());
        assert!(rights.can_delete_any());

        let converted = PermissionLevel::from_rights(&rights);
        assert_eq!(level, converted);
    }

    #[test]
    fn test_permission_rights_contains() {
        let editor = PermissionRights::editor();
        let author = PermissionRights::author();
        assert!(editor.contains(author));
        assert!(!author.contains(editor));
    }

    #[test]
    fn test_delegate_info() {
        let delegate = DelegateInfo::new(
            "owner@example.com".to_string(),
            "delegate@example.com".to_string(),
            Some("Delegate Name".to_string()),
        );
        assert_eq!(
            delegate.calendar_permission_level(),
            PermissionLevel::Reviewer
        );
    }
}

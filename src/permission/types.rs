use bitflags::bitflags;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[serde(transparent)]
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

impl Default for PermissionRights {
    fn default() -> Self {
        Self::empty()
    }
}

impl PermissionRights {
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
        Self::READ_ANY | Self::CREATE | Self::EDIT_ANY | Self::EDIT_OWNED | Self::DELETE_ANY | Self::DELETE_OWNED | Self::FOLDER_VISIBLE
    }
    pub fn publishing_author() -> Self {
        Self::READ_ANY | Self::CREATE | Self::EDIT_OWNED | Self::DELETE_OWNED | Self::CREATE_SUBFOLDER | Self::FOLDER_VISIBLE
    }
    pub fn publishing_editor() -> Self {
        Self::READ_ANY | Self::CREATE | Self::EDIT_ANY | Self::EDIT_OWNED | Self::DELETE_ANY | Self::DELETE_OWNED | Self::CREATE_SUBFOLDER | Self::FOLDER_VISIBLE
    }
    pub fn owner() -> Self {
        Self::READ_ANY | Self::CREATE | Self::EDIT_ANY | Self::EDIT_OWNED | Self::DELETE_ANY | Self::DELETE_OWNED | Self::CREATE_SUBFOLDER | Self::FOLDER_OWNER | Self::FOLDER_CONTACT | Self::FOLDER_VISIBLE
    }
    pub fn freebusy() -> Self {
        Self::FREEBUSY_SIMPLE | Self::FREEBUSY_DETAILED | Self::FOLDER_VISIBLE
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
#[repr(u8)]
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
    pub fn to_rights(self) -> PermissionRights {
        match self {
            Self::None => PermissionRights::empty(),
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

    pub fn from_rights(rights: PermissionRights) -> Self {
        if rights.contains(PermissionRights::FOLDER_OWNER) { return Self::Owner; }
        if rights.contains(PermissionRights::publishing_editor()) { return Self::PublishingEditor; }
        if rights.contains(PermissionRights::editor()) { return Self::Editor; }
        if rights.contains(PermissionRights::publishing_author()) { return Self::PublishingAuthor; }
        if rights.contains(PermissionRights::author()) { return Self::Author; }
        if rights.contains(PermissionRights::non_editing_author()) { return Self::NonEditingAuthor; }
        if rights.contains(PermissionRights::contributor()) { return Self::Contributor; }
        if rights.contains(PermissionRights::reviewer()) { return Self::Reviewer; }
        if rights.contains(PermissionRights::FREEBUSY_SIMPLE) { return Self::FreeBusy; }
        Self::None
    }

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::None => "None",
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
        f.write_str(self.as_str())
    }
}

impl FromStr for PermissionLevel {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "freebusy" | "freebusyonly" => Ok(Self::FreeBusy),
            "reviewer" => Ok(Self::Reviewer),
            "contributor" => Ok(Self::Contributor),
            "noneditingauthor" => Ok(Self::NonEditingAuthor),
            "author" => Ok(Self::Author),
            "publishingauthor" => Ok(Self::PublishingAuthor),
            "editor" => Ok(Self::Editor),
            "publishingeditor" => Ok(Self::PublishingEditor),
            "owner" => Ok(Self::Owner),
            _ => Err(()),
        }
    }
}

impl From<u8> for PermissionLevel {
    fn from(value: u8) -> Self {
        match value {
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
    fn initialize(
        folder_id: String,
        owner: String,
        user_email: String,
        user_name: Option<String>,
        rights: PermissionRights,
        is_default: bool,
        is_anonymous: bool,
    ) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            folder_id,
            owner,
            user_email,
            user_name,
            rights: rights.bits(),
            is_default,
            is_anonymous,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn new(folder_id: String, owner: String, user_email: String, rights: PermissionRights) -> Self {
        Self::initialize(folder_id, owner, user_email, None, rights, false, false)
    }

    pub fn default_permission(folder_id: String, owner: String, rights: PermissionRights) -> Self {
        Self::initialize(folder_id, owner, "default".to_string(), Some("Default".to_string()), rights, true, false)
    }

    pub fn anonymous_permission(folder_id: String, owner: String, rights: PermissionRights) -> Self {
        Self::initialize(folder_id, owner, "anonymous".to_string(), Some("Anonymous".to_string()), rights, false, true)
    }

    pub fn rights(&self) -> PermissionRights {
        PermissionRights::from_bits_retain(self.rights)
    }

    pub fn set_rights(&mut self, rights: PermissionRights) {
        self.rights = rights.bits();
        self.updated_at = chrono::Utc::now();
    }

    pub fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::from_rights(self.rights())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DelegateInfo {
    pub id: String,
    pub delegator: String,
    pub delegate_email: String,
    pub delegate_name: Option<String>,
    pub calendar_permission: u8,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl DelegateInfo {
    pub fn new(delegator: String, delegate_email: String, delegate_name: Option<String>) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            delegator,
            delegate_email,
            delegate_name,
            calendar_permission: PermissionLevel::Reviewer as u8,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_rights_flags() {
        let rights = PermissionRights::owner();
        assert!(rights.contains(PermissionRights::READ_ANY));
        assert!(rights.contains(PermissionRights::CREATE));
        assert!(rights.contains(PermissionRights::EDIT_ANY));
        assert!(rights.contains(PermissionRights::DELETE_ANY));
        assert!(rights.contains(PermissionRights::FOLDER_OWNER));
    }

    #[test]
    fn test_permission_level_conversion() {
        let level = PermissionLevel::Editor;
        let rights = level.to_rights();
        assert!(rights.contains(PermissionRights::EDIT_ANY));
        assert!(rights.contains(PermissionRights::DELETE_ANY));

        let converted = PermissionLevel::from_rights(rights);
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
        assert_eq!(delegate.calendar_permission_level(), PermissionLevel::Reviewer);
    }
}

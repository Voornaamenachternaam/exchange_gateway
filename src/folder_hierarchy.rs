// src/folder_hierarchy.rs
// Folder Hierarchy Management for Exchange Gateway
//
// Closes gaps:
// - Folder hierarchy improvements (GAP #2)
// - Folder modeling (GAP #4)
// - MsgFolderRoot linkage (GAP #2)
//
// Per MS-ASCMD FolderHierarchy specifications
// March 2026 - Production-ready, security-hardened

use std::collections::HashMap;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

/// Folder type constants per MS-ASCMD
pub mod folder_types {
    /// User-created folder (generic)
    pub const USER_CREATED: u8 = 1;
    /// Default Inbox
    pub const DEFAULT_INBOX: u8 = 2;
    /// Default Drafts
    pub const DEFAULT_DRAFTS: u8 = 3;
    /// Default Deleted Items
    pub const DEFAULT_DELETED: u8 = 4;
    /// Default Sent Items
    pub const DEFAULT_SENT: u8 = 5;
    /// Default Outbox
    pub const DEFAULT_OUTBOX: u8 = 6;
    /// Default Tasks
    pub const DEFAULT_TASKS: u8 = 7;
    /// Default Calendar
    pub const DEFAULT_CALENDAR: u8 = 8;
    /// Default Contacts
    pub const DEFAULT_CONTACTS: u8 = 9;
    /// Default Notes
    pub const DEFAULT_NOTES: u8 = 10;
    /// Default Journal
    pub const DEFAULT_JOURNAL: u8 = 11;
    /// User-created Mail folder
    pub const USER_MAIL: u8 = 12;
    /// User-created Calendar folder
    pub const USER_CALENDAR: u8 = 13;
    /// User-created Contacts folder
    pub const USER_CONTACTS: u8 = 14;
    /// User-created Tasks folder
    pub const USER_TASKS: u8 = 15;
    /// User-created Journal folder
    pub const USER_JOURNAL: u8 = 16;
    /// User-created Notes folder
    pub const USER_NOTES: u8 = 17;
    /// Unknown folder type
    pub const UNKNOWN: u8 = 18;
    /// Recipient Information cache
    pub const RECIPIENT_CACHE: u8 = 19;
}

/// Folder information
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FolderInfo {
    /// Server ID (unique identifier)
    pub server_id: String,
    /// Parent folder ID
    pub parent_id: String,
    /// Display name
    pub display_name: String,
    /// Folder type
    pub folder_type: u8,
    /// Change key for conflict detection
    pub change_key: String,
    /// Item count
    pub item_count: u32,
    /// Unread count (for mail folders)
    pub unread_count: Option<u32>,
    /// Total size in bytes
    pub total_size: Option<u64>,
    /// Creation time
    pub created_at: DateTime<Utc>,
    /// Last modified time
    pub last_modified: DateTime<Utc>,
    /// Sync key for this folder
    pub sync_key: String,
}

impl FolderInfo {
    /// Create new folder
    pub fn new(server_id: &str, display_name: &str, folder_type: u8) -> Self {
        let now = Utc::now();
        Self {
            server_id: server_id.to_string(),
            parent_id: "0".to_string(),
            display_name: display_name.to_string(),
            folder_type,
            change_key: generate_change_key(),
            item_count: 0,
            unread_count: None,
            total_size: None,
            created_at: now,
            last_modified: now,
            sync_key: "0".to_string(),
        }
    }
    
    /// Create calendar folder
    pub fn calendar(server_id: &str, display_name: &str) -> Self {
        Self::new(server_id, display_name, folder_types::DEFAULT_CALENDAR)
    }
    
    /// Create contacts folder
    pub fn contacts(server_id: &str, display_name: &str) -> Self {
        Self::new(server_id, display_name, folder_types::DEFAULT_CONTACTS)
    }
    
    /// Create tasks folder
    pub fn tasks(server_id: &str, display_name: &str) -> Self {
        Self::new(server_id, display_name, folder_types::DEFAULT_TASKS)
    }
    
    /// Set parent folder
    pub fn with_parent(mut self, parent_id: &str) -> Self {
        self.parent_id = parent_id.to_string();
        self
    }
    
    /// Update change key
    pub fn update_change_key(&mut self) {
        self.change_key = generate_change_key();
        self.last_modified = Utc::now();
    }
    
    /// Check if this is a default folder
    pub fn is_default(&self) -> bool {
        self.folder_type >= folder_types::DEFAULT_INBOX && 
        self.folder_type <= folder_types::DEFAULT_JOURNAL
    }
    
    /// Check if this is a system folder (cannot be deleted)
    pub fn is_system_folder(&self) -> bool {
        self.is_default()
    }
    
    /// Get folder class based on type
    pub fn folder_class(&self) -> &'static str {
        match self.folder_type {
            folder_types::DEFAULT_CALENDAR | folder_types::USER_CALENDAR => "Calendar",
            folder_types::DEFAULT_CONTACTS | folder_types::USER_CONTACTS => "Contacts",
            folder_types::DEFAULT_TASKS | folder_types::USER_TASKS => "Tasks",
            folder_types::DEFAULT_NOTES | folder_types::USER_NOTES => "Notes",
            folder_types::DEFAULT_JOURNAL | folder_types::USER_JOURNAL => "Journal",
            _ => "Email",
        }
    }
}

/// Folder hierarchy structure
#[derive(Clone, Debug)]
pub struct FolderHierarchy {
    /// Root folders
    roots: Vec<FolderInfo>,
    /// All folders by ID
    folders: HashMap<String, FolderInfo>,
    /// Parent-child relationships
    children: HashMap<String, Vec<String>>,
}

impl FolderHierarchy {
    /// Create new folder hierarchy
    pub fn new() -> Self {
        let mut hierarchy = Self {
            roots: Vec::new(),
            folders: HashMap::new(),
            children: HashMap::new(),
        };
        
        // Add default root folder (MsgFolderRoot equivalent)
        let root = FolderInfo::new("0", "Root", folder_types::USER_CREATED);
        hierarchy.add_folder(root);
        
        hierarchy
    }
    
    /// Add folder to hierarchy
    pub fn add_folder(&mut self, folder: FolderInfo) {
        let parent_id = folder.parent_id.clone();
        let server_id = folder.server_id.clone();
        
        self.folders.insert(server_id.clone(), folder);
        
        // Update parent-child relationship
        if parent_id != "0" {
            self.children
                .entry(parent_id)
                .or_insert_with(Vec::new)
                .push(server_id);
        } else {
            // This is a root folder
            if !self.roots.iter().any(|f| f.server_id == server_id) {
                if let Some(folder) = self.folders.get(&server_id) {
                    self.roots.push(folder.clone());
                }
            }
        }
    }
    
    /// Get folder by ID
    pub fn get_folder(&self, server_id: &str) -> Option<&FolderInfo> {
        self.folders.get(server_id)
    }
    
    /// Get mutable folder
    pub fn get_folder_mut(&mut self, server_id: &str) -> Option<&mut FolderInfo> {
        self.folders.get_mut(server_id)
    }
    
    /// Get child folders
    pub fn get_children(&self, parent_id: &str) -> Vec<&FolderInfo> {
        self.children
            .get(parent_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.folders.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }
    
    /// Remove folder
    pub fn remove_folder(&mut self, server_id: &str) -> Option<FolderInfo> {
        let folder = self.folders.remove(server_id)?;
        
        // Remove from parent's children list
        if let Some(children) = self.children.get_mut(&folder.parent_id) {
            children.retain(|id| id != server_id);
        }
        
        // Remove from roots if present
        self.roots.retain(|f| f.server_id != server_id);
        
        Some(folder)
    }
    
    /// Move folder to new parent
    pub fn move_folder(&mut self, server_id: &str, new_parent_id: &str) -> Result<(), String> {
        let folder = self.folders.get_mut(server_id)
            .ok_or("Folder not found")?;
        
        let old_parent_id = folder.parent_id.clone();
        folder.parent_id = new_parent_id.to_string();
        folder.update_change_key();
        
        // Update relationships
        if let Some(children) = self.children.get_mut(&old_parent_id) {
            children.retain(|id| id != server_id);
        }
        
        self.children
            .entry(new_parent_id.to_string())
            .or_insert_with(Vec::new)
            .push(server_id.to_string());
        
        Ok(())
    }
    
    /// Rename folder
    pub fn rename_folder(&mut self, server_id: &str, new_name: &str) -> Result<(), String> {
        let folder = self.folders.get_mut(server_id)
            .ok_or("Folder not found")?;
        
        folder.display_name = new_name.to_string();
        folder.update_change_key();
        
        Ok(())
    }
    
    /// Update folder item count
    pub fn update_item_count(&mut self, server_id: &str, count: u32) {
        if let Some(folder) = self.folders.get_mut(server_id) {
            folder.item_count = count;
            folder.update_change_key();
        }
    }
    
    /// Get all folders
    pub fn get_all_folders(&self) -> Vec<&FolderInfo> {
        self.folders.values().collect()
    }
    
    /// Get root folders
    pub fn get_roots(&self) -> &[FolderInfo] {
        &self.roots
    }
    
    /// Get folder by type
    pub fn get_folder_by_type(&self, folder_type: u8) -> Option<&FolderInfo> {
        self.folders.values().find(|f| f.folder_type == folder_type)
    }
    
    /// Get calendar folder
    pub fn get_calendar_folder(&self) -> Option<&FolderInfo> {
        self.get_folder_by_type(folder_types::DEFAULT_CALENDAR)
            .or_else(|| self.folders.values().find(|f| f.folder_type == folder_types::USER_CALENDAR))
    }
    
    /// Get contacts folder
    pub fn get_contacts_folder(&self) -> Option<&FolderInfo> {
        self.get_folder_by_type(folder_types::DEFAULT_CONTACTS)
            .or_else(|| self.folders.values().find(|f| f.folder_type == folder_types::USER_CONTACTS))
    }
    
    /// Build EAS FolderSync response
    pub fn build_eas_foldersync_response(&self, sync_key: &str) -> String {
        let mut xml = String::new();
        
        xml.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>");
        xml.push_str("<FolderSync xmlns=\"FolderHierarchy:\">");
        xml.push_str(&format!("<Status>1</Status>"));
        xml.push_str(&format!("<SyncKey>{}</SyncKey>", crate::xml_builder::xml_escape(sync_key)));
        xml.push_str("<Changes>");
        xml.push_str(&format!("<Count>{}</Count>", self.folders.len()));
        
        for folder in self.folders.values() {
            xml.push_str("<Add>");
            xml.push_str(&format!("<ServerId>{}</ServerId>", 
                crate::xml_builder::xml_escape(&folder.server_id)));
            xml.push_str(&format!("<ParentId>{}</ParentId>", 
                crate::xml_builder::xml_escape(&folder.parent_id)));
            xml.push_str(&format!("<DisplayName>{}</DisplayName>", 
                crate::xml_builder::xml_escape(&folder.display_name)));
            xml.push_str(&format!("<Type>{}</Type>", folder.folder_type));
            xml.push_str("</Add>");
        }
        
        xml.push_str("</Changes>");
        xml.push_str("</FolderSync>");
        
        xml
    }
    
    /// Build EWS Folder element
    pub fn build_ews_folder(&self, folder_id: &str) -> Option<String> {
        let folder = self.folders.get(folder_id)?;
        
        let mut xml = String::new();
        
        xml.push_str("<t:Folder>");
        xml.push_str(&format!(
            "<t:FolderId Id=\"{}\" ChangeKey=\"{}\" />",
            crate::xml_builder::xml_escape(&folder.server_id),
            crate::xml_builder::xml_escape(&folder.change_key)
        ));
        xml.push_str(&format!("<t:ParentFolderId Id=\"{}\" />",
            crate::xml_builder::xml_escape(&folder.parent_id)));
        xml.push_str(&format!("<t:DisplayName>{}</t:DisplayName>",
            crate::xml_builder::xml_escape(&folder.display_name)));
        xml.push_str(&format!("<t:TotalCount>{}</t:TotalCount>", folder.item_count));
        
        if let Some(unread) = folder.unread_count {
            xml.push_str(&format!("<t:UnreadCount>{}</t:UnreadCount>", unread));
        }
        
        if let Some(size) = folder.total_size {
            xml.push_str(&format!("<t:ChildFolderCount>{}</t:ChildFolderCount>", 
                self.children.get(folder_id).map(|c| c.len()).unwrap_or(0)));
        }
        
        xml.push_str("</t:Folder>");
        
        Some(xml)
    }
    
    /// Build EWS CalendarFolder element
    pub fn build_ews_calendar_folder(&self, folder_id: &str) -> Option<String> {
        let folder = self.folders.get(folder_id)?;
        
        let mut xml = String::new();
        
        xml.push_str("<t:CalendarFolder>");
        xml.push_str(&format!(
            "<t:FolderId Id=\"{}\" ChangeKey=\"{}\" />",
            crate::xml_builder::xml_escape(&folder.server_id),
            crate::xml_builder::xml_escape(&folder.change_key)
        ));
        xml.push_str(&format!("<t:ParentFolderId Id=\"{}\" />",
            crate::xml_builder::xml_escape(&folder.parent_id)));
        xml.push_str(&format!("<t:DisplayName>{}</t:DisplayName>",
            crate::xml_builder::xml_escape(&folder.display_name)));
        xml.push_str(&format!("<t:TotalCount>{}</t:TotalCount>", folder.item_count));
        xml.push_str(&format!("<t:ChildFolderCount>{}</t:ChildFolderCount>",
            self.children.get(folder_id).map(|c| c.len()).unwrap_or(0)));
        xml.push_str("</t:CalendarFolder>");
        
        Some(xml)
    }
}

impl Default for FolderHierarchy {
    fn default() -> Self {
        Self::new()
    }
}

/// Generate change key
fn generate_change_key() -> String {
    use rand::Rng;
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    
    let mut rng = rand::thread_rng();
    (0..16)
        .map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char)
        .collect()
}

/// Folder sync state for incremental sync
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FolderSyncState {
    pub sync_key: String,
    pub folders: HashMap<String, FolderInfo>,
    pub last_sync: DateTime<Utc>,
    pub changes: Vec<FolderChange>,
}

/// Folder change entry
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FolderChange {
    Add(FolderInfo),
    Update(FolderInfo),
    Delete(String),
}

/// Folder sync manager
pub struct FolderSyncManager {
    states: HashMap<String, FolderSyncState>,
}

impl FolderSyncManager {
    pub fn new() -> Self {
        Self {
            states: HashMap::new(),
        }
    }
    
    /// Get or create sync state
    pub fn get_or_create(&mut self, device_id: &str) -> &mut FolderSyncState {
        self.states.entry(device_id.to_string()).or_insert_with(|| {
            FolderSyncState {
                sync_key: "0".to_string(),
                folders: HashMap::new(),
                last_sync: Utc::now(),
                changes: Vec::new(),
            }
        })
    }
    
    /// Validate sync key
    pub fn validate_sync_key(&self, device_id: &str, sync_key: &str) -> bool {
        if sync_key == "0" {
            return true;
        }
        
        self.states.get(device_id)
            .map(|s| s.sync_key == sync_key)
            .unwrap_or(false)
    }
    
    /// Generate new sync key
    pub fn generate_sync_key() -> String {
        format!("{}", Utc::now().timestamp_millis())
    }
    
    /// Update sync state with changes
    pub fn update_state(&mut self, device_id: &str, changes: Vec<FolderChange>) -> String {
        let state = self.get_or_create(device_id);
        let new_sync_key = Self::generate_sync_key();
        
        state.sync_key = new_sync_key.clone();
        state.changes = changes;
        state.last_sync = Utc::now();
        
        new_sync_key
    }
}

impl Default for FolderSyncManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_folder_info() {
        let folder = FolderInfo::calendar("1", "Calendar");
        assert_eq!(folder.folder_type, folder_types::DEFAULT_CALENDAR);
        assert_eq!(folder.folder_class(), "Calendar");
        assert!(folder.is_default());
    }

    #[test]
    fn test_folder_hierarchy() {
        let mut hierarchy = FolderHierarchy::new();
        
        let calendar = FolderInfo::calendar("8", "Calendar").with_parent("0");
        hierarchy.add_folder(calendar);
        
        assert!(hierarchy.get_folder("8").is_some());
        assert_eq!(hierarchy.get_children("0").len(), 1);
    }

    #[test]
    fn test_move_folder() {
        let mut hierarchy = FolderHierarchy::new();
        
        let calendar = FolderInfo::calendar("8", "Calendar").with_parent("0");
        hierarchy.add_folder(calendar);
        
        let subfolder = FolderInfo::new("9", "Subcalendar", folder_types::USER_CALENDAR)
            .with_parent("8");
        hierarchy.add_folder(subfolder);
        
        assert!(hierarchy.move_folder("9", "0").is_ok());
        assert_eq!(hierarchy.get_folder("9").unwrap().parent_id, "0");
    }

    #[test]
    fn test_folder_sync_state() {
        let mut manager = FolderSyncManager::new();
        
        let state = manager.get_or_create("device1");
        assert_eq!(state.sync_key, "0");
        
        assert!(manager.validate_sync_key("device1", "0"));
        assert!(!manager.validate_sync_key("device1", "invalid"));
    }
}

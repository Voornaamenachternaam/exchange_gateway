// src/permission/delegate.rs
use crate::permission::types::{DelegateInfo, DelegatePermission, PermissionLevel, PermissionRights, PermissionAuditEntry};
use crate::permission::storage::PermissionStorage;
use crate::storage::Storage;
use anyhow::Result;

pub struct DelegateManager<'a> {
    storage: PermissionStorage<'a>,
}

impl<'a> DelegateManager<'a> {
    pub fn new(storage: &'a Storage) -> Self {
        Self {
            storage: PermissionStorage::new(storage),
        }
    }

    pub async fn get_delegate(&self, delegator: &str, delegate_email: &str) -> Result<Option<DelegateInfo>> {
        self.storage.get_delegate(delegator, delegate_email).await
    }

    pub async fn get_delegates(&self, delegator: &str) -> Result<Vec<DelegateInfo>> {
        self.storage.get_delegates(delegator).await
    }

    pub async fn add_delegate(
        &self,
        delegator: &str,
        delegate_email: &str,
        delegate_name: Option<&str>,
        calendar_permission: PermissionLevel,
        actor_email: &str,
    ) -> Result<DelegateInfo> {
        if delegator == delegate_email {
            anyhow::bail!("Cannot add self as delegate");
        }

        let existing = self.storage.get_delegate(delegator, delegate_email).await?;
        if existing.is_some() {
            anyhow::bail!("Delegate already exists");
        }

        let mut delegate = DelegateInfo::new(
            delegator.to_string(),
            delegate_email.to_string(),
            delegate_name.map(String::from),
        );
        delegate.set_calendar_permission(calendar_permission);

        self.storage.upsert_delegate(&delegate).await?;

        let audit = PermissionAuditEntry::new(
            "calendar".to_string(),
            delegator.to_string(),
            actor_email.to_string(),
            delegate_email.to_string(),
            "add_delegate".to_string(),
            None,
            Some(calendar_permission.to_rights().bits()),
        );
        self.storage.add_audit_entry(&audit).await?;

        Ok(delegate)
    }

    pub async fn update_delegate(
        &self,
        delegator: &str,
        delegate_email: &str,
        calendar_permission: Option<PermissionLevel>,
        receive_copies: Option<bool>,
        receive_infos: Option<bool>,
        view_private: Option<bool>,
        actor_email: &str,
    ) -> Result<DelegateInfo> {
        let mut delegate = self.storage.get_delegate(delegator, delegate_email).await?
            .ok_or_else(|| anyhow::anyhow!("Delegate not found"))?;

        let old_rights = delegate.to_calendar_rights().bits();

        if let Some(level) = calendar_permission {
            delegate.set_calendar_permission(level);
        }
        if let Some(copies) = receive_copies {
            delegate.receive_copies = copies;
        }
        if let Some(infos) = receive_infos {
            delegate.receive_infos = infos;
        }
        if let Some(private) = view_private {
            delegate.view_private = private;
        }
        delegate.updated_at = chrono::Utc::now();

        self.storage.upsert_delegate(&delegate).await?;

        let audit = PermissionAuditEntry::new(
            "calendar".to_string(),
            delegator.to_string(),
            actor_email.to_string(),
            delegate_email.to_string(),
            "update_delegate".to_string(),
            Some(old_rights),
            Some(delegate.to_calendar_rights().bits()),
        );
        self.storage.add_audit_entry(&audit).await?;

        Ok(delegate)
    }

    pub async fn remove_delegate(&self, delegator: &str, delegate_email: &str, actor_email: &str) -> Result<()> {
        let delegate = self.storage.get_delegate(delegator, delegate_email).await?
            .ok_or_else(|| anyhow::anyhow!("Delegate not found"))?;

        let old_rights = delegate.to_calendar_rights().bits();

        self.storage.delete_delegate(delegator, delegate_email).await?;

        let audit = PermissionAuditEntry::new(
            "calendar".to_string(),
            delegator.to_string(),
            actor_email.to_string(),
            delegate_email.to_string(),
            "remove_delegate".to_string(),
            Some(old_rights),
            None,
        );
        self.storage.add_audit_entry(&audit).await?;

        Ok(())
    }

    pub async fn is_delegate(&self, delegator: &str, delegate_email: &str) -> Result<bool> {
        Ok(self.storage.get_delegate(delegator, delegate_email).await?.is_some())
    }

    pub async fn get_delegates_for_freebusy(&self, delegator: &str) -> Result<Vec<DelegateInfo>> {
        let delegates = self.storage.get_delegates(delegator).await?;
        Ok(delegates.into_iter()
            .filter(|d| d.calendar_permission_level() != PermissionLevel::None)
            .collect())
    }

    pub async fn get_delegates_with_copy_permission(&self, delegator: &str) -> Result<Vec<DelegateInfo>> {
        let delegates = self.storage.get_delegates(delegator).await?;
        Ok(delegates.into_iter()
            .filter(|d| d.receive_copies)
            .collect())
    }

    pub async fn get_delegates_with_info_permission(&self, delegator: &str) -> Result<Vec<DelegateInfo>> {
        let delegates = self.storage.get_delegates(delegator).await?;
        Ok(delegates.into_iter()
            .filter(|d| d.receive_infos)
            .collect())
    }

    pub fn render_delegate_xml(&self, delegate: &DelegateInfo) -> String {
        let calendar_perm = Self::permission_level_to_delegate_permission_xml(delegate.calendar_permission_level());
        let inbox_perm = Self::permission_level_to_delegate_permission_xml(PermissionLevel::from(delegate.inbox_permission));
        let contacts_perm = Self::permission_level_to_delegate_permission_xml(PermissionLevel::from(delegate.contacts_permission));
        let tasks_perm = Self::permission_level_to_delegate_permission_xml(PermissionLevel::from(delegate.tasks_permission));
        let notes_perm = Self::permission_level_to_delegate_permission_xml(PermissionLevel::from(delegate.notes_permission));
        let journal_perm = Self::permission_level_to_delegate_permission_xml(PermissionLevel::from(delegate.journal_permission));

        format!(
            r#"<t:DelegateUser>
    <t:UserId>
        <t:PrimarySmtpAddress>{}</t:PrimarySmtpAddress>
        {}
    </t:UserId>
    <t:DelegatePermissions>
        <t:CalendarFolderPermissionLevel>{}</t:CalendarFolderPermissionLevel>
        <t:InboxFolderPermissionLevel>{}</t:InboxFolderPermissionLevel>
        <t:ContactsFolderPermissionLevel>{}</t:ContactsFolderPermissionLevel>
        <t:TasksFolderPermissionLevel>{}</t:TasksFolderPermissionLevel>
        <t:NotesFolderPermissionLevel>{}</t:NotesFolderPermissionLevel>
        <t:JournalFolderPermissionLevel>{}</t:JournalFolderPermissionLevel>
    </t:DelegatePermissions>
    <t:ReceiveCopiesOfMeetingMessages>{}</t:ReceiveCopiesOfMeetingMessages>
    <t:ViewPrivateItems>{}</t:ViewPrivateItems>
</t:DelegateUser>"#,
            crate::util::xml_escape(&delegate.delegate_email),
            delegate.delegate_name.as_ref().map(|n| format!("<t:DisplayName>{}</t:DisplayName>", crate::util::xml_escape(n))).unwrap_or_default(),
            calendar_perm,
            inbox_perm,
            contacts_perm,
            tasks_perm,
            notes_perm,
            journal_perm,
            if delegate.receive_copies { "true" } else { "false" },
            if delegate.view_private { "true" } else { "false" },
        )
    }

    fn permission_level_to_delegate_permission_xml(level: PermissionLevel) -> &'static str {
        // Per [MS-OXODLGT], EWS DelegatePermissions only supports:
        // None, Reviewer, Author, Editor
        // Map other levels to the closest supported value
        match level {
            PermissionLevel::None => "None",
            // FreeBusy is not a valid DelegatePermissionLevel, map to None
            // (delegate would need folder-level permission for free/busy)
            PermissionLevel::FreeBusy => "None",
            PermissionLevel::Reviewer => "Reviewer",
            // Contributor/NonEditingAuthor are not valid delegate permission levels
            // Map to Author as the closest equivalent
            PermissionLevel::Contributor => "Author",
            PermissionLevel::NonEditingAuthor => "Author",
            PermissionLevel::Author => "Author",
            PermissionLevel::PublishingAuthor => "Author",
            PermissionLevel::Editor => "Editor",
            PermissionLevel::PublishingEditor => "Editor",
            // Owner is not a valid DelegatePermissionLevel
            // Map to Editor as the highest available permission
            PermissionLevel::Owner => "Editor",
        }
    }
}
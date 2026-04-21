// src/permission/enforcement.rs
use crate::permission::storage::PermissionStorage;
use crate::permission::types::{CalendarPermission, PermissionLevel, PermissionRights};
use crate::storage::Storage;
use crate::util::normalize_email;
use anyhow::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionCheck {
    ReadItem,
    CreateItem,
    EditOwned,
    EditAny,
    DeleteOwned,
    DeleteAny,
    FolderOwner,
    FolderVisible,
    FreeBusySimple,
    FreeBusyDetailed,
}

#[derive(Clone, Debug)]
pub struct PermissionContext {
    pub actor_email: String,
    pub folder_owner: String,
    pub folder_id: String,
    pub item_owner: Option<String>,
    pub is_delegate: bool,
    pub delegator: Option<String>,
}

impl PermissionContext {
    pub fn new(actor_email: String, folder_owner: String, folder_id: String) -> Self {
        Self {
            actor_email,
            folder_owner,
            folder_id,
            item_owner: None,
            is_delegate: false,
            delegator: None,
        }
    }

    pub fn with_item_owner(mut self, item_owner: String) -> Self {
        self.item_owner = Some(item_owner);
        self
    }

    pub fn as_delegate(mut self, delegator: String) -> Self {
        self.is_delegate = true;
        self.delegator = Some(delegator);
        self
    }

    pub fn is_owner(&self) -> bool {
        normalize_email(&self.actor_email) == normalize_email(&self.folder_owner)
    }

    pub fn owns_item(&self) -> bool {
        match &self.item_owner {
            Some(owner) => normalize_email(&self.actor_email) == normalize_email(owner),
            None => false, // SECURITY: Deny by default when ownership is unknown
        }
    }
}

pub struct PermissionEnforcement<'a> {
    storage: PermissionStorage<'a>,
}

impl<'a> PermissionEnforcement<'a> {
    pub fn new(storage: &'a Storage) -> Self {
        Self {
            storage: PermissionStorage::new(storage),
        }
    }

    pub async fn get_effective_rights(&self, ctx: &PermissionContext) -> Result<PermissionRights> {
        // Owner has full rights
        if ctx.is_owner() {
            return Ok(PermissionRights::owner());
        }

        // Determine the folder owner (support delegate context)
        let owner = if ctx.is_delegate {
            ctx.delegator.as_deref().unwrap_or(&ctx.folder_owner)
        } else {
            &ctx.folder_owner
        };

        // Check calendar_permission table for explicit folder permissions
        if let Some(perm) = self
            .storage
            .get_permission(owner, &ctx.folder_id, &ctx.actor_email)
            .await?
        {
            return Ok(perm.rights());
        }

        // Check calendar_delegate table for delegate permissions
        // This is essential because DelegateManager::add_delegate creates records
        // in the delegate table, not the permission table
        if let Some(delegate) = self.storage.get_delegate(owner, &ctx.actor_email).await? {
            let delegate_rights = delegate.to_calendar_rights();
            // Check if delegate has any permissions (bits != 0)
            if delegate_rights.bits() != 0 {
                return Ok(delegate_rights);
            }
        }

        // Check default permission for the folder
        if let Some(default_perm) = self
            .storage
            .get_default_permission(owner, &ctx.folder_id)
            .await?
        {
            return Ok(default_perm.rights());
        }

        // Check anonymous permission
        if let Some(anon_perm) = self
            .storage
            .get_anonymous_permission(owner, &ctx.folder_id)
            .await?
        {
            return Ok(anon_perm.rights());
        }

        Ok(PermissionRights::none())
    }

    pub async fn check_permission(
        &self,
        ctx: &PermissionContext,
        check: PermissionCheck,
    ) -> Result<bool> {
        let rights = self.get_effective_rights(ctx).await?;
        Ok(self.check_rights(&rights, check, ctx))
    }

    fn check_rights(
        &self,
        rights: &PermissionRights,
        check: PermissionCheck,
        ctx: &PermissionContext,
    ) -> bool {
        match check {
            PermissionCheck::ReadItem => rights.can_read_any() || ctx.owns_item(),
            PermissionCheck::CreateItem => rights.can_create(),
            PermissionCheck::EditOwned => rights.can_edit_owned() && ctx.owns_item(),
            PermissionCheck::EditAny => rights.can_edit_any(),
            PermissionCheck::DeleteOwned => rights.can_delete_owned() && ctx.owns_item(),
            PermissionCheck::DeleteAny => rights.can_delete_any(),
            PermissionCheck::FolderOwner => rights.is_folder_owner(),
            PermissionCheck::FolderVisible => rights.is_folder_visible(),
            PermissionCheck::FreeBusySimple => rights.can_freebusy_simple(),
            PermissionCheck::FreeBusyDetailed => rights.can_freebusy_detailed(),
        }
    }

    pub async fn can_read_item(&self, ctx: &PermissionContext) -> Result<bool> {
        self.check_permission(ctx, PermissionCheck::ReadItem).await
    }

    pub async fn can_create_item(&self, ctx: &PermissionContext) -> Result<bool> {
        self.check_permission(ctx, PermissionCheck::CreateItem)
            .await
    }

    pub async fn can_edit_item(&self, ctx: &PermissionContext) -> Result<bool> {
        if ctx.owns_item() {
            self.check_permission(ctx, PermissionCheck::EditOwned).await
        } else {
            self.check_permission(ctx, PermissionCheck::EditAny).await
        }
    }

    pub async fn can_delete_item(&self, ctx: &PermissionContext) -> Result<bool> {
        if ctx.owns_item() {
            self.check_permission(ctx, PermissionCheck::DeleteOwned)
                .await
        } else {
            self.check_permission(ctx, PermissionCheck::DeleteAny).await
        }
    }

    pub async fn can_modify_permissions(&self, ctx: &PermissionContext) -> Result<bool> {
        self.check_permission(ctx, PermissionCheck::FolderOwner)
            .await
    }

    pub async fn get_permissions_for_display(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Vec<CalendarPermission>> {
        self.storage
            .get_permissions_for_folder(owner, folder_id)
            .await
    }

    pub async fn set_permission(
        &self,
        owner: &str,
        folder_id: &str,
        user_email: &str,
        user_name: Option<&str>,
        level: PermissionLevel,
        actor_email: &str,
    ) -> Result<CalendarPermission> {
        let ctx = PermissionContext::new(
            actor_email.to_string(),
            owner.to_string(),
            folder_id.to_string(),
        );
        if !self
            .check_permission(&ctx, PermissionCheck::FolderOwner)
            .await?
        {
            anyhow::bail!("Permission denied: only folder owner can modify permissions");
        }

        let old_perm = self
            .storage
            .get_permission(owner, folder_id, user_email)
            .await?;
        let old_rights = old_perm.as_ref().map(|p| p.rights().bits());

        let mut perm = CalendarPermission::new(
            folder_id.to_string(),
            owner.to_string(),
            user_email.to_string(),
            level.to_rights(),
        );
        perm.user_name = user_name.map(String::from);

        self.storage.upsert_permission(&perm).await?;

        let audit = crate::permission::types::PermissionAuditEntry::new(
            folder_id.to_string(),
            owner.to_string(),
            actor_email.to_string(),
            user_email.to_string(),
            "set".to_string(),
            old_rights,
            Some(perm.rights().bits()),
        );
        self.storage.add_audit_entry(&audit).await?;

        Ok(perm)
    }

    pub async fn remove_permission(
        &self,
        owner: &str,
        folder_id: &str,
        user_email: &str,
        actor_email: &str,
    ) -> Result<()> {
        let ctx = PermissionContext::new(
            actor_email.to_string(),
            owner.to_string(),
            folder_id.to_string(),
        );
        if !self
            .check_permission(&ctx, PermissionCheck::FolderOwner)
            .await?
        {
            anyhow::bail!("Permission denied: only folder owner can modify permissions");
        }

        let old_perm = self
            .storage
            .get_permission(owner, folder_id, user_email)
            .await?;

        self.storage
            .delete_permission(owner, folder_id, user_email)
            .await?;

        if let Some(perm) = old_perm {
            let audit = crate::permission::types::PermissionAuditEntry::new(
                folder_id.to_string(),
                owner.to_string(),
                actor_email.to_string(),
                user_email.to_string(),
                "remove".to_string(),
                Some(perm.rights().bits()),
                None,
            );
            self.storage.add_audit_entry(&audit).await?;
        }

        Ok(())
    }

    pub async fn set_default_permission(
        &self,
        owner: &str,
        folder_id: &str,
        level: PermissionLevel,
        actor_email: &str,
    ) -> Result<CalendarPermission> {
        let ctx = PermissionContext::new(
            actor_email.to_string(),
            owner.to_string(),
            folder_id.to_string(),
        );
        if !self
            .check_permission(&ctx, PermissionCheck::FolderOwner)
            .await?
        {
            anyhow::bail!("Permission denied: only folder owner can modify permissions");
        }

        let old_perm = self
            .storage
            .get_default_permission(owner, folder_id)
            .await?;
        let old_rights = old_perm.as_ref().map(|p| p.rights().bits());

        let perm = CalendarPermission::default_permission(
            folder_id.to_string(),
            owner.to_string(),
            level.to_rights(),
        );

        self.storage.upsert_permission(&perm).await?;

        let audit = crate::permission::types::PermissionAuditEntry::new(
            folder_id.to_string(),
            owner.to_string(),
            actor_email.to_string(),
            "default".to_string(),
            "set_default".to_string(),
            old_rights,
            Some(perm.rights().bits()),
        );
        self.storage.add_audit_entry(&audit).await?;

        Ok(perm)
    }

    pub async fn set_anonymous_permission(
        &self,
        owner: &str,
        folder_id: &str,
        level: PermissionLevel,
        actor_email: &str,
    ) -> Result<CalendarPermission> {
        let ctx = PermissionContext::new(
            actor_email.to_string(),
            owner.to_string(),
            folder_id.to_string(),
        );
        if !self
            .check_permission(&ctx, PermissionCheck::FolderOwner)
            .await?
        {
            anyhow::bail!("Permission denied: only folder owner can modify permissions");
        }

        let old_perm = self
            .storage
            .get_anonymous_permission(owner, folder_id)
            .await?;
        let old_rights = old_perm.as_ref().map(|p| p.rights().bits());

        let perm = CalendarPermission::anonymous_permission(
            folder_id.to_string(),
            owner.to_string(),
            level.to_rights(),
        );

        self.storage.upsert_permission(&perm).await?;

        let audit = crate::permission::types::PermissionAuditEntry::new(
            folder_id.to_string(),
            owner.to_string(),
            actor_email.to_string(),
            "anonymous".to_string(),
            "set_anonymous".to_string(),
            old_rights,
            Some(perm.rights().bits()),
        );
        self.storage.add_audit_entry(&audit).await?;

        Ok(perm)
    }

    pub fn render_permission_xml(&self, perm: &CalendarPermission) -> String {
        let level = perm.permission_level();
        let rights = perm.rights();
        let can_read = rights.can_read_any();
        let can_create = rights.can_create();
        let is_owner = rights.is_folder_owner();
        let is_visible = rights.is_folder_visible();

        format!(
            r#"<t:Permission>
    <t:UserId>
        <t:PrimarySmtpAddress>{}</t:PrimarySmtpAddress>
        {}
    </t:UserId>
    <t:CanCreateItems>{}</t:CanCreateItems>
    <t:CanCreateSubFolders>{}</t:CanCreateSubFolders>
    <t:IsFolderOwner>{}</t:IsFolderOwner>
    <t:IsFolderVisible>{}</t:IsFolderVisible>
    <t:IsFolderContact>{}</t:IsFolderContact>
    <t:EditItems>{}</t:EditItems>
    <t:DeleteItems>{}</t:DeleteItems>
    <t:ReadItems>{}</t:ReadItems>
    <t:PermissionLevel>{}</t:PermissionLevel>
</t:Permission>"#,
            crate::util::xml_escape(&perm.user_email),
            perm.user_name
                .as_ref()
                .map(|n| format!(
                    "<t:DisplayName>{}</t:DisplayName>",
                    crate::util::xml_escape(n)
                ))
                .unwrap_or_default(),
            if can_create { "true" } else { "false" },
            if rights.can_create_subfolder() {
                "true"
            } else {
                "false"
            },
            if is_owner { "true" } else { "false" },
            if is_visible { "true" } else { "false" },
            if rights.is_folder_contact() {
                "true"
            } else {
                "false"
            },
            if rights.can_edit_any() {
                "All"
            } else if rights.can_edit_owned() {
                "Owned"
            } else {
                "None"
            },
            if rights.can_delete_any() {
                "All"
            } else if rights.can_delete_owned() {
                "Owned"
            } else {
                "None"
            },
            if can_read { "FullDetails" } else { "None" },
            level.as_str()
        )
    }
}

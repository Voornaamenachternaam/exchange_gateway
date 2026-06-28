// src/contacts.rs
use crate::carddav::{CarddavClient, Contact as CarddavContact};
use crate::storage::{Storage, ContactRow};
use crate::vcard::{parse_vcard_from_data, Vcard};
use anyhow::{Result, anyhow};
use tracing::warn;
use uuid::Uuid;

/// Convert a CardDAV contact into an EAS Contacts XML element.
pub fn render_eas_contact(server_id: &str, carddav_contact: &CarddavContact) -> String {
    // Parse vCard to extract fields; on failure, use minimal info.
    let vcard = parse_vcard_from_data(&carddav_contact.vcard).unwrap_or_else(|_| {
        warn!(href = %carddav_contact.href, "Failed to parse vCard, using blank");
        Vcard::default()
    });

    // Extract common fields
    let display_name = vcard.name.first().map(|n| n.value.as_str()).unwrap_or("");
    let email = vcard.email.first().map(|e| e.email.as_str()).unwrap_or("");
    let phone = vcard.telephone.first().map(|t| t.number.as_str()).unwrap_or("");
    let org = vcard.org.first().map(|o| o.value.as_str()).unwrap_or("");
    let title = vcard.title.as_ref().map(|t| t.value.as_str()).unwrap_or("");

    // Build XML with proper escaping
    let mut xml = String::new();
    xml.push_str(&format!(r#"<Contact>"#));
    xml.push_str(&format!(r#"<ServerId>{}</ServerId>"#, xml_escape(server_id)));
    xml.push_str(&format!(r#"<DisplayName>{}</DisplayName>"#, xml_escape(display_name)));
    if !email.is_empty() {
        xml.push_str(&format!(r#"<EmailAddresses><EmailAddress><Address>{}</Address></EmailAddress></EmailAddresses>"#, xml_escape(email)));
    }
    if !phone.is_empty() {
        xml.push_str(&format!(r#"<PhoneNumbers><PhoneNumber><Number>{}</Number></PhoneNumber></PhoneNumbers>"#, xml_escape(phone)));
    }
    if !org.is_empty() {
        xml.push_str(&format!(r#"<Company>{}</Company>"#, xml_escape(org)));
    }
    if !title.is_empty() {
        xml.push_str(&format!(r#"<JobTitle>{}</JobTitle>"#, xml_escape(title)));
    }
    xml.push_str(r#"</Contact>"#);
    xml
}

/// Sync contacts for a user using CardDAV as the backend.
/// Returns (xml_fragment, new_sync_key). The xml_fragment contains <Add>/<Change>/<Delete> elements.
pub async fn sync_contacts(
    state: &crate::models::AppState,
    username: &str,
    password: &str,
    client_sync_key: Option<&str>,
    device_id: &str,
) -> Result<String> {
    // Determine the scoped collection ID for storage lookup
    let state_collection_id = format!("8::{}", device_id);

    // 1. Get the last server sync key from storage (or empty string if first sync)
    let server_sync_key = state
        .storage
        .get_sync_key(username, &state_collection_id, "Contacts")
        .await?
        .unwrap_or_default();

    // If client_sync_key is "0" or empty, treat as initial full sync.
    // Otherwise, we need to fetch changes from CardDAV using its sync token.
    // Stalwart CardDAV does not expose sync tokens, so we perform a full fetch and diff against DB.

    // 2. Fetch all current contacts from CardDAV
    let Some(carddav) = state.carddav_client.as_ref() else {
        return Err(anyhow!("CardDAV client not configured"));
    };
    let (carddav_contacts, _new_sync_token) = carddav.list_contacts(username, password, None).await?;

    // 3. Build lookup of DB contacts by server_id and href
    let db_contacts_by_server_id: std::collections::HashMap<_, _> = state
        .storage
        .get_all_contacts_for_owner(username)
        .await?
        .into_iter()
        .map(|row| (row.server_id.clone(), row))
        .collect();

    let db_contacts_by_href: std::collections::HashMap<_, _> = db_contacts_by_server_id
        .values()
        .map(|row| (row.carddav_href.clone(), row))
        .collect();

    // 4. Compute changes: Added, Updated, Deleted
    let mut adds = Vec::new();
    let mut changes = Vec::new();
    let mut deletes = Vec::new();

    for c in &carddav_contacts {
        // Check if this contact exists in DB by href
        if let Some(db_row) = db_contacts_by_href.get(&c.href) {
            // Exists: compare etag to see if changed
            if db_row.etag.as_deref() != Some(c.etag.as_str()) {
                // Changed: use existing server_id
                changes.push((db_row.server_id.clone(), c.clone()));
            }
        } else {
            // New contact: generate a server_id and store mapping
            let new_server_id = format!("contact-{}", Uuid::new_v4().to_string().replace("-", ""));
            adds.push((new_server_id.clone(), c.clone()));
            // Insert into DB for future syncs
            state.storage.insert_contact(
                username,
                &c.href,
                &new_server_id,
                Some(&c.etag),
                Some(&c.vcard),
            ).await?;
        }
    }

    // Deletions: any DB contact whose href is not in current CardDAV list
    for (server_id, db_row) in db_contacts_by_server_id {
        if !carddav_contacts.iter().any(|c| c.href == db_row.carddav_href) {
            deletes.push(server_id);
            state.storage.delete_contact(username, &server_id).await?;
        }
    }

    // 5. Generate EAS sync XML: <Add> for adds, <Change> for changes, <Delete> for deletes.
    let mut response = String::new();
    for (server_id, carddav_contact) in adds {
        response.push_str(&format!(r#"<Add>"#));
        response.push_str(&render_eas_contact(&server_id, &carddav_contact));
        response.push_str(r#"</Add>"#);
    }
    for (server_id, carddav_contact) in changes {
        response.push_str(&format!(r#"<Change>"#));
        response.push_str(&render_eas_contact(&server_id, &carddav_contact));
        response.push_str(r#"</Change>"#);
    }
    for server_id in deletes {
        response.push_str(&format!(r#"<Delete><ServerId>{}</ServerId></Delete>"#, xml_escape(&server_id)));
    }

    // 6. Generate new sync key (random UUID)
    let new_sync_key = uuid::Uuid::new_v4().simple().to_string();
    state.storage.set_contacts_sync_state(username, &state_collection_id, &new_sync_key).await?;

    // Final response: <Collection> wrapper will be added by caller.
    Ok(response)
}

/// Helper: xml_escape used above
fn xml_escape(s: &str) -> String {
    crate::util::escape_xml_text(s)
}

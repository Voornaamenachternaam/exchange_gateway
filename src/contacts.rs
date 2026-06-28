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
    let vcard = match parse_vcard_from_data(&carddav_contact.vcard) {
        Ok(v) => v,
        Err(_) => {
            warn!(href = %carddav_contact.href, "Failed to parse vCard, using blank");
            Vcard::default()
        }
    };

    // Determine display name: use full_name if present, else first N component.
    let display_name = if !vcard.full_name.is_empty() {
        vcard.full_name.clone()
    } else if let Some(first) = vcard.name.first() {
        first.value.clone()
    } else {
        String::new()
    };

    let email = vcard.email.first().map(|e| e.email.as_str()).unwrap_or("");
    let phone = vcard.telephone.first().map(|t| t.number.as_str()).unwrap_or("");
    // ORG components are joined by ';' to match vCard representation.
    let org = vcard.org.first().map(|o| o.value.join(";")).unwrap_or_default();
    let title = vcard.title.as_ref().map(|t| t.value.as_str()).unwrap_or("");

    // Build XML with proper escaping
    let mut xml = String::new();
    xml.push_str("<Contact>");
    xml.push_str(&format!("<ServerId>{}</ServerId>", xml_escape(server_id)));
    if !display_name.is_empty() {
        xml.push_str(&format!("<DisplayName>{}</DisplayName>", xml_escape(&display_name)));
    }
    if !email.is_empty() {
        xml.push_str(&format!("<EmailAddresses><EmailAddress><Address>{}</Address></EmailAddress></EmailAddresses>", xml_escape(email)));
    }
    if !phone.is_empty() {
        xml.push_str(&format!("<PhoneNumbers><PhoneNumber><Number>{}</Number></PhoneNumber></PhoneNumbers>", xml_escape(phone)));
    }
    if !org.is_empty() {
        xml.push_str(&format!("<Company>{}</Company>", xml_escape(&org)));
    }
    if !title.is_empty() {
        xml.push_str(&format!("<JobTitle>{}</JobTitle>", xml_escape(title)));
    }
    xml.push_str("</Contact>");
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

/// Parse EAS Contacts sync mutations (Add/Change/Delete) from the <Collection> body.
pub fn parse_contacts_mutations(xml: &str) -> Result<Vec<ContactsMutation>> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    // Stack stores element names as byte slices for cheap comparison
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut current_kind: Option<ContactsOpKind> = None;
    let mut current_server_id = String::new();
    let mut current_vcard = String::new();
    let mut mutations = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name_bytes = e.name().as_bytes().to_vec();
                let name = String::from_utf8_lossy(&name_bytes).to_string();
                if name == "Add" || name == "Change" || name == "Delete" {
                    current_kind = match name.as_str() {
                        "Add" => Some(ContactsOpKind::Add),
                        "Change" => Some(ContactsOpKind::Change),
                        "Delete" => Some(ContactsOpKind::Delete),
                        _ => None,
                    };
                    current_server_id.clear();
                    current_vcard.clear();
                }
                stack.push(name_bytes);
            }
            Ok(Event::End(_)) => {
                if let Some(name_bytes) = stack.pop() {
                    let name = String::from_utf8_lossy(&name_bytes).to_string();
                    match name.as_str() {
                        "Add" | "Change" | "Delete" => {
                            if let Some(kind) = current_kind.take() {
                                match kind {
                                    ContactsOpKind::Add => {
                                        if !current_server_id.is_empty() && !current_vcard.is_empty() {
                                            mutations.push(ContactsMutation::Add {
                                                client_id: None,
                                                server_id: current_server_id.clone(),
                                                vcard: current_vcard.clone(),
                                            });
                                        }
                                    }
                                    ContactsOpKind::Change => {
                                        if !current_server_id.is_empty() && !current_vcard.is_empty() {
                                            mutations.push(ContactsMutation::Change {
                                                server_id: current_server_id.clone(),
                                                vcard: current_vcard.clone(),
                                            });
                                        }
                                    }
                                    ContactsOpKind::Delete => {
                                        if !current_server_id.is_empty() {
                                            mutations.push(ContactsMutation::Delete {
                                                server_id: current_server_id.clone(),
                                            });
                                        }
                                    }
                                }
                            }
                        }
                        "ServerId" => {}
                        "vCard" => {}
                        _ => {}
                    }
                }
            }
            Ok(Event::Text(e)) => {
                match e.unescape() {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes).to_string();
                        let in_vcard = stack.iter().any(|n| n == b"vCard");
                        let in_server_id = stack.iter().any(|n| n == b"ServerId");
                        if in_server_id {
                            current_server_id.push_str(&text);
                        } else if in_vcard && (current_kind == Some(ContactsOpKind::Add) || current_kind == Some(ContactsOpKind::Change)) {
                            current_vcard.push_str(&text);
                        }
                    }
                    Err(_) => {}
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(mutations)
}

#[derive(Debug, Clone, PartialEq)]
pub enum ContactsOpKind {
    Add,
    Change,
    Delete,
}

#[derive(Debug, Clone)]
pub enum ContactsMutation {
    Add {
        client_id: Option<String>,
        server_id: String,
        vcard: String,
    },
    Change {
        server_id: String,
        vcard: String,
    },
    Delete {
        server_id: String,
    },
}

/// Apply client mutations from a Sync request to the CardDAV backend.
/// Returns a list of results indicating success/failure per mutation.
pub async fn apply_contacts_mutations(
    state: &crate::models::AppState,
    username: &str,
    password: &str,
    mutations_xml: &str,
) -> Result<Vec<ContactsMutationResult>> {
    let mutations = parse_contacts_mutations(mutations_xml)?;
    let mut results = Vec::new();

    let Some(carddav) = state.carddav_client.as_ref() else {
        // All mutations fail if CardDAV not configured
        for mut m in mutations {
            results.push(ContactsMutationResult {
                server_id: match &mut m {
                    ContactsMutation::Add { server_id, .. } => server_id.clone(),
                    ContactsMutation::Change { server_id, .. } => server_id.clone(),
                    ContactsMutation::Delete { server_id } => server_id.clone(),
                },
                status: "6",
            });
        }
        return Ok(results);
    };

    // Process mutations in order
    for mutation in mutations {
        match mutation {
            ContactsMutation::Add { server_id: _, vcard, .. } => {
                // POST to addressbook
                let addressbook = carddav.addressbook_home(username);
                let response = match carddav
                    .client
                    .post(&addressbook)
                    .basic_auth(username, Some(password))
                    .header("Content-Type", "text/vcard; charset=utf-8")
                    .body(&vcard)
                    .send()
                    .await
                {
                    Ok(resp) => resp,
                    Err(_) => {
                        results.push(ContactsMutationResult {
                            server_id: "".to_string(),
                            status: "6",
                        });
                        continue;
                    }
                };

                if response.status().is_success() || response.status() == StatusCode::CREATED {
                    let location = response
                        .headers()
                        .get("Location")
                        .and_then(|h| h.to_str().ok())
                        .unwrap_or("");
                    let href = location
                        .strip_prefix(&addressbook)
                        .unwrap_or(location)
                        .to_string();
                    let etag = response
                        .headers()
                        .get("ETag")
                        .and_then(|h| h.to_str().ok())
                        .map(|s| s.trim_matches('"').to_string());

                    // We need to store the contact with server_id mapping. For Add, the server_id came from client or generated.
                    // For simplicity, generate a new server_id here.
                    let new_server_id = format!("contact-{}", Uuid::new_v4().simple());
                    if let Ok(Some(existing)) = state.storage.get_contact_by_href(username, &href).await {
                        // Already exists? That's an error; but we'll consider it success anyway to avoid client loops.
                        results.push(ContactsMutationResult {
                            server_id: existing.server_id,
                            status: "1",
                        });
                        continue;
                    }

                    if let Err(e) = state
                        .storage
                        .insert_contact(username, &href, &new_server_id, etag.as_deref(), &vcard)
                        .await
                    {
                        tracing::warn!(error = %e, "Failed to store contact in DB after Add");
                    }

                    results.push(ContactsMutationResult {
                        server_id: new_server_id,
                        status: "1",
                    });
                } else {
                    results.push(ContactsMutationResult {
                        server_id: "".to_string(),
                        status: "6",
                    });
                }
            }
            ContactsMutation::Change { server_id: sid, vcard } => {
                // Fetch existing contact to get href and etag
                let db_contact = match state.storage.get_contact(username, &sid).await {
                    Ok(Some(c)) => c,
                    _ => {
                        results.push(ContactsMutationResult {
                            server_id: sid,
                            status: "6",
                        });
                        continue;
                    }
                };

                let url = format!("{}{}", carddav.addressbook_home(username), db_contact.carddav_href);
                let mut request = carddav
                    .client
                    .put(&url)
                    .basic_auth(username, Some(password))
                    .header("Content-Type", "text/vcard; charset=utf-8")
                    .body(&vcard);

                if let Some(ref etag) = db_contact.etag {
                    request = request.header("If-Match", format!("\"{}\"", etag));
                }

                let response = match request.send().await {
                    Ok(resp) => resp,
                    Err(_) => {
                        results.push(ContactsMutationResult {
                            server_id: sid,
                            status: "6",
                        });
                        continue;
                    }
                };

                if response.status().is_success() {
                    let new_etag = response
                        .headers()
                        .get("ETag")
                        .and_then(|h| h.to_str().ok())
                        .map(|s| s.trim_matches('"').to_string());

                    if let Err(e) = state
                        .storage
                        .update_contact(username, &sid, new_etag.as_deref(), &vcard)
                        .await
                    {
                        tracing::warn!(error = %e, "Failed to update contact in DB");
                    }

                    results.push(ContactsMutationResult {
                        server_id: sid,
                        status: "1",
                    });
                } else if response.status() == StatusCode::PRECONDITION_FAILED {
                    results.push(ContactsMutationResult {
                        server_id: sid,
                        status: "5", // EAS status for ChangeKey conflict?
                    });
                } else {
                    results.push(ContactsMutationResult {
                        server_id: sid,
                        status: "6",
                    });
                }
            }
            ContactsMutation::Delete { server_id: sid } => {
                let db_contact = match state.storage.get_contact(username, &sid).await {
                    Ok(Some(c)) => c,
                    _ => {
                        results.push(ContactsMutationResult {
                            server_id: sid,
                            status: "6",
                        });
                        continue;
                    }
                };

                let url = format!("{}{}", carddav.addressbook_home(username), db_contact.carddav_href);
                let mut request = carddav
                    .client
                    .delete(&url)
                    .basic_auth(username, Some(password));

                if let Some(ref etag) = db_contact.etag {
                    request = request.header("If-Match", format!("\"{}\"", etag));
                }

                let response = match request.send().await {
                    Ok(resp) => resp,
                    Err(_) => {
                        results.push(ContactsMutationResult {
                            server_id: sid,
                            status: "6",
                        });
                        continue;
                    }
                };

                if response.status().is_success() {
                    if let Err(e) = state.storage.delete_contact(username, &sid).await {
                        tracing::warn!(error = %e, "Failed to delete contact from DB");
                    }
                    results.push(ContactsMutationResult {
                        server_id: sid,
                        status: "1",
                    });
                } else if response.status() == StatusCode::PRECONDITION_FAILED {
                    results.push(ContactsMutationResult {
                        server_id: sid,
                        status: "5",
                    });
                } else {
                    results.push(ContactsMutationResult {
                        server_id: sid,
                        status: "6",
                    });
                }
            }
        }
    }

    Ok(results)
}

/// Render client mutation responses for EAS Sync: maps mutation results to <Status> values.
pub fn render_contacts_mutation_responses(results: &[ContactsMutationResult]) -> String {
    let mut xml = String::new();
    for res in results {
        xml.push_str(&format!(
            r#"<Status>{}</Status>"#,
            xml_escape(res.status)
        ));
        if !res.server_id.is_empty() {
            xml.push_str(&format!(
                r#"<ServerId>{}</ServerId>"#,
                xml_escape(&res.server_id)
            ));
        }
    }
    xml
}

#[derive(Debug, Clone)]
pub struct ContactsMutationResult {
    pub server_id: String,
    pub status: &'static str,
}

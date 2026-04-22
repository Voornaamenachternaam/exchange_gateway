// src/room.rs
use crate::protocol_fixtures::{EWS_MSG_NS, EWS_TYPE_NS};
use crate::storage::Storage;
use crate::util::xml_escape;
use anyhow::Result;
use quick_xml::events::Event;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoomListRecord {
    pub id: String,
    pub email: String,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoomRecord {
    pub id: String,
    pub room_list_email: Option<String>,
    pub email: String,
    pub name: String,
    pub capacity: i32,
    pub is_available: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug)]
pub struct RoomList {
    pub id: String,
    pub email: String,
    pub name: String,
}

impl RoomList {
    pub fn from_record(rec: &RoomListRecord) -> Self {
        Self {
            id: rec.id.clone(),
            email: rec.email.clone(),
            name: rec.name.clone(),
        }
    }

    pub fn to_record(&self) -> RoomListRecord {
        RoomListRecord {
            id: self.id.clone(),
            email: self.email.clone(),
            name: self.name.clone(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Room {
    pub id: String,
    pub room_list_email: Option<String>,
    pub email: String,
    pub name: String,
    pub capacity: i32,
    pub is_available: bool,
}

impl Room {
    pub fn from_record(rec: &RoomRecord) -> Self {
        Self {
            id: rec.id.clone(),
            room_list_email: rec.room_list_email.clone(),
            email: rec.email.clone(),
            name: rec.name.clone(),
            capacity: rec.capacity,
            is_available: rec.is_available,
        }
    }

    pub fn to_record(&self) -> RoomRecord {
        RoomRecord {
            id: self.id.clone(),
            room_list_email: self.room_list_email.clone(),
            email: self.email.clone(),
            name: self.name.clone(),
            capacity: self.capacity,
            is_available: self.is_available,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }
}

pub struct RoomManager {
    storage: Arc<Storage>,
}

impl RoomManager {
    pub fn new(storage: Arc<Storage>) -> Self {
        Self { storage }
    }

    pub async fn get_room_lists(&self) -> Result<Vec<RoomList>> {
        let recs = self.storage.get_room_lists().await?;
        Ok(recs.iter().map(RoomList::from_record).collect())
    }

    pub async fn get_rooms_for_list(&self, room_list_email: &str) -> Result<Vec<Room>> {
        let recs = self.storage.get_rooms_for_list(room_list_email).await?;
        Ok(recs.iter().map(Room::from_record).collect())
    }

    pub async fn get_all_rooms(&self) -> Result<Vec<Room>> {
        let recs = self.storage.get_all_rooms().await?;
        Ok(recs.iter().map(Room::from_record).collect())
    }

    pub async fn add_room_list(&self, email: &str, name: &str) -> Result<RoomList> {
        let id = uuid::Uuid::new_v4().to_string();
        let room_list = RoomList {
            id,
            email: email.to_string(),
            name: name.to_string(),
        };
        self.storage
            .upsert_room_list(&room_list.to_record())
            .await?;
        Ok(room_list)
    }

    pub async fn add_room(
        &self,
        email: &str,
        name: &str,
        room_list_email: Option<&str>,
        capacity: i32,
    ) -> Result<Room> {
        let id = uuid::Uuid::new_v4().to_string();
        let room = Room {
            id,
            room_list_email: room_list_email.map(String::from),
            email: email.to_string(),
            name: name.to_string(),
            capacity,
            is_available: true,
        };
        self.storage.upsert_room(&room.to_record()).await?;
        Ok(room)
    }

    pub async fn remove_room_list(&self, email: &str) -> Result<()> {
        self.storage.delete_room_list(email).await
    }

    pub async fn remove_room(&self, email: &str) -> Result<()> {
        self.storage.delete_room(email).await
    }
}

pub fn parse_get_rooms_request(xml: &str) -> Option<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut in_room_list = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let local = e.name().local_name();
                match local.as_ref() {
                    b"RoomList" => {
                        in_room_list = true;
                    }
                    b"EmailAddress" if in_room_list => {
                        if let Ok(text) = reader.read_text(e.to_end().name()) {
                            return Some(text.into_owned());
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref() == b"RoomList" => {
                in_room_list = false;
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    None
}

pub fn render_room_list_xml(room_list: &RoomList) -> String {
    format!(
        r#"<t:Address>
            <t:EmailAddress>{}</t:EmailAddress>
            <t:Name>{}</t:Name>
            <t:RoutingType>SMTP</t:RoutingType>
            <t:MailboxType>PublicDL</t:MailboxType>
        </t:Address>"#,
        xml_escape(&room_list.email),
        xml_escape(&room_list.name),
    )
}

pub fn render_room_xml(room: &Room) -> String {
    format!(
        r#"<t:Room>
            <t:Id>
                <t:EmailAddress>{}</t:EmailAddress>
                <t:Name>{}</t:Name>
                <t:RoutingType>SMTP</t:RoutingType>
                <t:MailboxType>Room</t:MailboxType>
            </t:Id>
        </t:Room>"#,
        xml_escape(&room.email),
        xml_escape(&room.name),
    )
}

pub fn render_get_room_lists_response(room_lists: &[RoomList]) -> String {
    let addresses_xml = room_lists
        .iter()
        .map(render_room_list_xml)
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"<m:GetRoomListsResponse xmlns:m="{}" xmlns:t="{}">
            <m:ResponseMessages>
                <m:GetRoomListsResponseMessage ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                    <m:RoomLists>
                        {}
                    </m:RoomLists>
                </m:GetRoomListsResponseMessage>
            </m:ResponseMessages>
        </m:GetRoomListsResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS, addresses_xml,
    )
}

pub fn render_get_rooms_response(rooms: &[Room]) -> String {
    let rooms_xml = rooms
        .iter()
        .map(render_room_xml)
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"<m:GetRoomsResponse xmlns:m="{}" xmlns:t="{}">
            <m:ResponseMessages>
                <m:GetRoomsResponseMessage ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                    <m:Rooms>
                        {}
                    </m:Rooms>
                </m:GetRoomsResponseMessage>
            </m:ResponseMessages>
        </m:GetRoomsResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS, rooms_xml,
    )
}

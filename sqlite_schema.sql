-- sqlite_schema.sql
-- Idempotent schema for Exchange Gateway SQLite database


CREATE TABLE IF NOT EXISTS sync_state (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL,
    collection_id TEXT NOT NULL,
    sync_key TEXT NOT NULL,
    token TEXT,
    protocol_version TEXT DEFAULT '16.1',
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, collection_id)
);

CREATE TABLE IF NOT EXISTS item_map (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL,
    caldav_href TEXT,
    resource_href TEXT NOT NULL,
    server_id TEXT NOT NULL,
    uid TEXT,
    etag TEXT,
    instance_id TEXT,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, server_id)
);

CREATE TABLE IF NOT EXISTS deleted_item_tombstone (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL,
    server_id TEXT NOT NULL,
    instance_id TEXT,
    deleted_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, server_id)
);

CREATE TABLE IF NOT EXISTS change_journal (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL,
    server_id TEXT NOT NULL,
    op TEXT NOT NULL,
    resource_href TEXT,
    instance_id TEXT,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS client_sync_command (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL,
    collection_id TEXT NOT NULL,
    client_id TEXT NOT NULL,
    server_id TEXT,
    instance_id TEXT,
    status TEXT NOT NULL,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, collection_id, client_id)
);

CREATE TABLE IF NOT EXISTS provision_state (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL,
    device_id TEXT NOT NULL,
    policy_key TEXT NOT NULL,
    policy_status TEXT NOT NULL,
    policy_type TEXT DEFAULT 'MS-EAS-Provisioning-WBXML',
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, device_id)
);

CREATE TABLE IF NOT EXISTS ews_sync_state (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_email TEXT NOT NULL,
    folder_id TEXT NOT NULL,
    sync_state TEXT NOT NULL,
    jmap_state TEXT,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(user_email, folder_id)
);

CREATE TABLE IF NOT EXISTS device_info (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_email TEXT NOT NULL,
    device_id TEXT NOT NULL,
    friendly_name TEXT,
    model TEXT,
    os TEXT,
    os_version TEXT,
    phone_number TEXT,
    imei TEXT,
    user_agent TEXT,
    protocol_version TEXT DEFAULT '16.1',
    last_seen DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(user_email, device_id)
);

CREATE TABLE IF NOT EXISTS api_idempotency (
    idempotency_key TEXT PRIMARY KEY,
    route_name TEXT NOT NULL,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY,
    description TEXT NOT NULL,
    applied_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS meeting_response (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL,
    request_id TEXT NOT NULL,
    calendar_id TEXT NOT NULL,
    user_response INTEGER NOT NULL,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, request_id)
);

CREATE TABLE IF NOT EXISTS calendar_exceptions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL,
    parent_server_id TEXT NOT NULL,
    exception_start TEXT NOT NULL,
    server_id TEXT,
    is_deleted INTEGER DEFAULT 0,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, parent_server_id, exception_start)
);

INSERT OR IGNORE INTO schema_version (version, description) VALUES (1, 'initial gateway typed schema');

CREATE INDEX IF NOT EXISTS idx_api_idempotency_created ON api_idempotency(created_at);
CREATE INDEX IF NOT EXISTS idx_item_map_owner_time ON item_map(owner, updated_at);
CREATE INDEX IF NOT EXISTS idx_item_map_resource ON item_map(owner, resource_href);
CREATE INDEX IF NOT EXISTS idx_item_map_uid ON item_map(owner, uid);
CREATE INDEX IF NOT EXISTS idx_item_map_instance ON item_map(owner, instance_id);
CREATE INDEX IF NOT EXISTS idx_deleted_owner_time ON deleted_item_tombstone(owner, deleted_at);
CREATE INDEX IF NOT EXISTS idx_change_journal_owner ON change_journal(owner, id);
CREATE INDEX IF NOT EXISTS idx_change_journal_op ON change_journal(owner, op, id);
CREATE INDEX IF NOT EXISTS idx_change_journal_server ON change_journal(owner, server_id);
CREATE INDEX IF NOT EXISTS idx_ews_sync_lookup ON ews_sync_state(user_email, folder_id);
CREATE INDEX IF NOT EXISTS idx_provision_lookup ON provision_state(owner, device_id);
CREATE INDEX IF NOT EXISTS idx_device_info_owner ON device_info(user_email);
CREATE INDEX IF NOT EXISTS idx_meeting_response_owner ON meeting_response(owner);
CREATE INDEX IF NOT EXISTS idx_calendar_exceptions_parent ON calendar_exceptions(owner, parent_server_id);

INSERT OR IGNORE INTO schema_version (version, description)
VALUES (2, 'v2: change_journal.resource_href inline; additional indexes');

INSERT OR IGNORE INTO schema_version (version, description)
VALUES (3, 'v3: device_info expanded with model, os, phone_number, imei, user_agent columns');

INSERT OR IGNORE INTO schema_version (version, description)
VALUES (4, 'v4: Added protocol_version, instance_id, meeting_response, calendar_exceptions for protocol 16.1 compatibility');


CREATE TABLE IF NOT EXISTS meeting_state (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    uid TEXT NOT NULL,
    owner TEXT NOT NULL,
    sequence INTEGER DEFAULT 0,
    state TEXT NOT NULL DEFAULT 'Draft',
    state_flags INTEGER DEFAULT 0,
    is_organizer INTEGER DEFAULT 0,
    organizer_email TEXT,
    organizer_name TEXT,
    subject TEXT,
    location TEXT,
    start_time DATETIME NOT NULL,
    end_time DATETIME NOT NULL,
    timezone TEXT,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    last_sequence_time DATETIME,
    UNIQUE(owner, uid)
);

CREATE TABLE IF NOT EXISTS meeting_attendee (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    meeting_uid TEXT NOT NULL,
    owner TEXT NOT NULL,
    email TEXT NOT NULL,
    name TEXT,
    status INTEGER DEFAULT 0,
    role INTEGER DEFAULT 1,
    response_time DATETIME,
    proposed_start DATETIME,
    proposed_end DATETIME,
    sequence INTEGER DEFAULT 0,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, meeting_uid, email)
);

CREATE TABLE IF NOT EXISTS meeting_scheduling_queue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    meeting_uid TEXT NOT NULL,
    owner TEXT NOT NULL,
    operation TEXT NOT NULL,
    sequence INTEGER DEFAULT 0,
    ical_data TEXT,
    status TEXT DEFAULT 'pending',
    attempts INTEGER DEFAULT 0,
    last_attempt DATETIME,
    error_message TEXT,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    processed_at DATETIME
);

INSERT OR IGNORE INTO schema_version (version, description)
VALUES (5, 'v5: Meeting workflow - state machine, attendee tracking, RFC 6638 scheduling queue');

CREATE INDEX IF NOT EXISTS idx_meeting_state_owner ON meeting_state(owner);
CREATE INDEX IF NOT EXISTS idx_meeting_state_uid ON meeting_state(uid);
CREATE INDEX IF NOT EXISTS idx_meeting_state_organizer ON meeting_state(owner, organizer_email);
CREATE INDEX IF NOT EXISTS idx_meeting_state_time ON meeting_state(owner, start_time, end_time);
CREATE INDEX IF NOT EXISTS idx_meeting_attendee_meeting ON meeting_attendee(owner, meeting_uid);
CREATE INDEX IF NOT EXISTS idx_meeting_attendee_email ON meeting_attendee(owner, email);
CREATE INDEX IF NOT EXISTS idx_meeting_scheduling_pending ON meeting_scheduling_queue(owner, status);
CREATE INDEX IF NOT EXISTS idx_meeting_scheduling_uid ON meeting_scheduling_queue(meeting_uid);


CREATE TABLE IF NOT EXISTS calendar_permission (
    id TEXT PRIMARY KEY,
    folder_id TEXT NOT NULL,
    owner TEXT NOT NULL,
    user_email TEXT NOT NULL,
    user_name TEXT,
    rights INTEGER NOT NULL DEFAULT 0,
    is_default INTEGER NOT NULL DEFAULT 0,
    is_anonymous INTEGER NOT NULL DEFAULT 0,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, folder_id, user_email)
);

CREATE TABLE IF NOT EXISTS calendar_delegate (
    id TEXT PRIMARY KEY,
    delegator TEXT NOT NULL,
    delegate_email TEXT NOT NULL,
    delegate_name TEXT,
    calendar_permission INTEGER NOT NULL DEFAULT 0,
    inbox_permission INTEGER NOT NULL DEFAULT 0,
    tasks_permission INTEGER NOT NULL DEFAULT 0,
    contacts_permission INTEGER NOT NULL DEFAULT 0,
    notes_permission INTEGER NOT NULL DEFAULT 0,
    journal_permission INTEGER NOT NULL DEFAULT 0,
    receive_copies INTEGER NOT NULL DEFAULT 0,
    receive_infos INTEGER NOT NULL DEFAULT 0,
    view_private INTEGER NOT NULL DEFAULT 0,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(delegator, delegate_email)
);

CREATE TABLE IF NOT EXISTS permission_audit (
    id TEXT PRIMARY KEY,
    folder_id TEXT NOT NULL,
    owner TEXT NOT NULL,
    actor_email TEXT NOT NULL,
    target_email TEXT NOT NULL,
    operation TEXT NOT NULL,
    old_rights INTEGER,
    new_rights INTEGER,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

INSERT OR IGNORE INTO schema_version (version, description)
VALUES (6, 'v6: Calendar permissions, delegate management, and audit trail based on MS-OXCPERM');

CREATE INDEX IF NOT EXISTS idx_permission_folder ON calendar_permission(owner, folder_id);
CREATE INDEX IF NOT EXISTS idx_permission_user ON calendar_permission(owner, user_email);
CREATE INDEX IF NOT EXISTS idx_permission_default ON calendar_permission(owner, folder_id, is_default);
CREATE INDEX IF NOT EXISTS idx_permission_anonymous ON calendar_permission(owner, folder_id, is_anonymous);

CREATE INDEX IF NOT EXISTS idx_delegate_delegator ON calendar_delegate(delegator);
CREATE INDEX IF NOT EXISTS idx_delegate_email ON calendar_delegate(delegate_email);
CREATE INDEX IF NOT EXISTS idx_delegate_delegator_email ON calendar_delegate(delegator, delegate_email);

CREATE INDEX IF NOT EXISTS idx_permission_audit_folder ON permission_audit(owner, folder_id);
CREATE INDEX IF NOT EXISTS idx_permission_audit_actor ON permission_audit(actor_email);
CREATE INDEX IF NOT EXISTS idx_permission_audit_target ON permission_audit(target_email);
CREATE INDEX IF NOT EXISTS idx_permission_audit_time ON permission_audit(owner, created_at);

CREATE TABLE IF NOT EXISTS calendar_attachment (
    id TEXT PRIMARY KEY,
    parent_item_server_id TEXT NOT NULL,
    owner TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    content_type TEXT NOT NULL DEFAULT 'application/octet-stream',
    content_size INTEGER NOT NULL DEFAULT 0,
    content_base64 TEXT NOT NULL DEFAULT '',
    is_inline INTEGER NOT NULL DEFAULT 0,
    content_id TEXT,
    content_location TEXT,
    attachment_type TEXT NOT NULL DEFAULT 'file',
    last_modified_time DATETIME,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_attachment_parent ON calendar_attachment(owner, parent_item_server_id);
CREATE INDEX IF NOT EXISTS idx_attachment_owner ON calendar_attachment(owner);
CREATE INDEX IF NOT EXISTS idx_attachment_id ON calendar_attachment(id);

INSERT OR IGNORE INTO schema_version (version, description) VALUES (7, 'v7: Attachment support - CreateAttachment/GetAttachment/DeleteAttachment with base64 content storage');

CREATE TABLE IF NOT EXISTS room_list (
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL,
    name TEXT NOT NULL,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(email)
);

CREATE TABLE IF NOT EXISTS room (
    id TEXT PRIMARY KEY,
    room_list_email TEXT,
    email TEXT NOT NULL,
    name TEXT NOT NULL,
    capacity INTEGER DEFAULT 0,
    is_available INTEGER NOT NULL DEFAULT 1,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(email)
);

CREATE INDEX IF NOT EXISTS idx_room_list_email ON room_list(email);
CREATE INDEX IF NOT EXISTS idx_room_email ON room(email);
CREATE INDEX IF NOT EXISTS idx_room_room_list ON room(room_list_email);

INSERT OR IGNORE INTO schema_version (version, description) VALUES (8, 'v8: Room/resource booking - GetRoomLists/GetRooms with room mailbox support');

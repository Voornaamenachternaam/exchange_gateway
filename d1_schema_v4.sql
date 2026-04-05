-- d1_schema_v4.sql
-- Enhanced schema for protocol 16.1 compatibility

-- Migration from v3 to v4:
-- Run these ALTER statements if upgrading from v3:
-- ALTER TABLE sync_state ADD COLUMN protocol_version TEXT DEFAULT '16.1';
-- ALTER TABLE item_map ADD COLUMN instance_id TEXT;
-- ALTER TABLE device_info ADD COLUMN os_version TEXT;
-- ALTER TABLE device_info ADD COLUMN protocol_version TEXT DEFAULT '16.1';
-- ALTER TABLE provision_state ADD COLUMN policy_type TEXT DEFAULT 'MS-EAS-Provisioning-WBXML';

DROP TABLE IF EXISTS sync_state;
DROP TABLE IF EXISTS item_map;
DROP TABLE IF EXISTS ews_sync_state;
DROP TABLE IF EXISTS device_info;
DROP TABLE IF EXISTS provision_state;
DROP TABLE IF EXISTS deleted_item_tombstone;
DROP TABLE IF EXISTS change_journal;
DROP TABLE IF EXISTS client_sync_command;
DROP TABLE IF EXISTS api_idempotency;
DROP TABLE IF EXISTS schema_version;
DROP TABLE IF EXISTS meeting_response;
DROP TABLE IF EXISTS calendar_exceptions;

CREATE TABLE sync_state (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL,
    collection_id TEXT NOT NULL,
    sync_key TEXT NOT NULL,
    token TEXT,
    protocol_version TEXT DEFAULT '16.1',
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, collection_id)
);

CREATE TABLE item_map (
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

CREATE TABLE deleted_item_tombstone (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL,
    server_id TEXT NOT NULL,
    instance_id TEXT,
    deleted_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, server_id)
);

CREATE TABLE change_journal (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL,
    server_id TEXT NOT NULL,
    op TEXT NOT NULL,
    resource_href TEXT,
    instance_id TEXT,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE client_sync_command (
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

CREATE TABLE provision_state (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL,
    device_id TEXT NOT NULL,
    policy_key TEXT NOT NULL,
    policy_status TEXT NOT NULL,
    policy_type TEXT DEFAULT 'MS-EAS-Provisioning-WBXML',
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, device_id)
);

CREATE TABLE ews_sync_state (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_email TEXT NOT NULL,
    folder_id TEXT NOT NULL,
    sync_state TEXT NOT NULL,
    jmap_state TEXT,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(user_email, folder_id)
);

CREATE TABLE device_info (
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
    UNIQUE(device_id)
);

CREATE TABLE api_idempotency (
    idempotency_key TEXT PRIMARY KEY,
    route_name TEXT NOT NULL,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE schema_version (
    version INTEGER PRIMARY KEY,
    description TEXT NOT NULL,
    applied_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE meeting_response (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL,
    request_id TEXT NOT NULL,
    calendar_id TEXT NOT NULL,
    user_response INTEGER NOT NULL,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, request_id)
);

CREATE TABLE calendar_exceptions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL,
    parent_server_id TEXT NOT NULL,
    exception_start TEXT NOT NULL,
    server_id TEXT,
    is_deleted INTEGER DEFAULT 0,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, parent_server_id, exception_start)
);

INSERT INTO schema_version (version, description) VALUES (1, 'initial gateway typed schema');

CREATE INDEX idx_api_idempotency_created ON api_idempotency(created_at);
CREATE INDEX idx_item_map_owner_time ON item_map(owner, updated_at);
CREATE INDEX idx_item_map_resource ON item_map(owner, resource_href);
CREATE INDEX idx_item_map_uid ON item_map(owner, uid);
CREATE INDEX idx_item_map_instance ON item_map(owner, instance_id);
CREATE INDEX idx_deleted_owner_time ON deleted_item_tombstone(owner, deleted_at);
CREATE INDEX idx_change_journal_owner ON change_journal(owner, id);
CREATE INDEX idx_change_journal_op ON change_journal(owner, op, id);
CREATE INDEX idx_change_journal_server ON change_journal(owner, server_id);
CREATE INDEX idx_ews_sync_lookup ON ews_sync_state(user_email, folder_id);
CREATE INDEX idx_provision_lookup ON provision_state(owner, device_id);
CREATE INDEX idx_device_info_owner ON device_info(user_email);
CREATE INDEX idx_meeting_response_owner ON meeting_response(owner);
CREATE INDEX idx_calendar_exceptions_parent ON calendar_exceptions(owner, parent_server_id);

INSERT INTO schema_version (version, description)
VALUES (2, 'v2: change_journal.resource_href inline; additional indexes');

INSERT INTO schema_version (version, description)
VALUES (3, 'v3: device_info expanded with model, os, phone_number, imei, user_agent columns');

INSERT INTO schema_version (version, description)
VALUES (4, 'v4: Added protocol_version, instance_id, meeting_response, calendar_exceptions for protocol 16.1 compatibility');
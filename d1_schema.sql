
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

CREATE TABLE sync_state (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    owner         TEXT    NOT NULL,
    collection_id TEXT    NOT NULL,
    sync_key      TEXT    NOT NULL,
    token         TEXT,
    updated_at    DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, collection_id)
);

CREATE TABLE item_map (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    owner         TEXT    NOT NULL,
    caldav_href   TEXT,
    resource_href TEXT    NOT NULL,
    server_id     TEXT    NOT NULL,
    uid           TEXT,
    etag          TEXT,
    updated_at    DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, server_id)
);

CREATE TABLE deleted_item_tombstone (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    owner      TEXT    NOT NULL,
    server_id  TEXT    NOT NULL,
    deleted_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, server_id)
);

CREATE TABLE change_journal (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    owner         TEXT    NOT NULL,
    server_id     TEXT    NOT NULL,
    op            TEXT    NOT NULL,
    resource_href TEXT,
    created_at    DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE client_sync_command (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    owner         TEXT    NOT NULL,
    collection_id TEXT    NOT NULL,
    client_id     TEXT    NOT NULL,
    server_id     TEXT,
    status        TEXT    NOT NULL,
    created_at    DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at    DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, collection_id, client_id)
);

CREATE TABLE provision_state (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    owner         TEXT    NOT NULL,
    device_id     TEXT    NOT NULL,
    policy_key    TEXT    NOT NULL,
    policy_status TEXT    NOT NULL,
    updated_at    DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, device_id)
);

CREATE TABLE ews_sync_state (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    user_email TEXT    NOT NULL,
    folder_id  TEXT    NOT NULL,
    sync_state TEXT    NOT NULL,
    jmap_state TEXT,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(user_email, folder_id)
);

CREATE TABLE device_info (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    user_email    TEXT    NOT NULL,
    device_id     TEXT    NOT NULL,
    friendly_name TEXT,
    last_seen     DATETIME DEFAULT CURRENT_TIMESTAMP,
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

INSERT INTO schema_version (version, description) VALUES (1, "initial gateway typed schema");

CREATE TABLE api_idempotency (
    idempotency_key TEXT PRIMARY KEY,
    route_name TEXT NOT NULL,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_api_idempotency_created ON api_idempotency(created_at);
CREATE INDEX idx_item_map_owner_time   ON item_map(owner, updated_at);
CREATE INDEX idx_item_map_resource     ON item_map(owner, resource_href);
CREATE INDEX idx_item_map_uid          ON item_map(owner, uid);
CREATE INDEX idx_deleted_owner_time    ON deleted_item_tombstone(owner, deleted_at);
CREATE INDEX idx_change_journal_owner  ON change_journal(owner, id);
CREATE INDEX idx_change_journal_op     ON change_journal(owner, op, id);
CREATE INDEX idx_client_sync_lookup    ON client_sync_command(owner, collection_id, client_id);
CREATE INDEX idx_ews_sync_lookup       ON ews_sync_state(user_email, folder_id);
CREATE INDEX idx_provision_lookup      ON provision_state(owner, device_id);
CREATE INDEX idx_item_map_owner_time   ON item_map(owner, updated_at);
CREATE INDEX idx_item_map_resource     ON item_map(owner, resource_href);
CREATE INDEX idx_item_map_uid          ON item_map(owner, uid);
CREATE INDEX idx_deleted_owner_time    ON deleted_item_tombstone(owner, deleted_at);
CREATE INDEX idx_change_journal_owner  ON change_journal(owner, id);
CREATE INDEX idx_change_journal_op     ON change_journal(owner, op, id);
CREATE INDEX idx_client_sync_lookup    ON client_sync_command(owner, collection_id, client_id);
CREATE INDEX idx_ews_sync_lookup       ON ews_sync_state(user_email, folder_id);
CREATE INDEX idx_provision_lookup      ON provision_state(owner, device_id);
CREATE INDEX idx_api_idempotency_created ON api_idempotency(created_at);

INSERT INTO schema_version (version, description)
VALUES (2, 'v2: change_journal.resource_href inline; additional indexes');

DROP TABLE IF EXISTS sync_state;
DROP TABLE IF EXISTS item_map;
DROP TABLE IF EXISTS ews_sync_state;
DROP TABLE IF EXISTS device_info;
DROP TABLE IF EXISTS provision_state;

-- ActiveSync sync-key tracking used by /api/set_sync_key
CREATE TABLE sync_state (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL,
    collection_id TEXT NOT NULL,
    sync_key TEXT NOT NULL,
    token TEXT,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, collection_id)
);

-- CalDAV resource <-> ActiveSync server-id mapping used by sync pipeline
CREATE TABLE item_map (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL,
    caldav_href TEXT,
    resource_href TEXT NOT NULL,
    server_id TEXT NOT NULL,
    uid TEXT,
    etag TEXT,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, server_id)
);


-- Provisioning policy state by owner/device used by /api/set_provision_policy and /api/get_provision_policy
CREATE TABLE provision_state (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL,
    device_id TEXT NOT NULL,
    policy_key TEXT NOT NULL,
    policy_status TEXT NOT NULL,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(owner, device_id)
);

-- Minimal EWS sync state persistence (reserved for future EWS expansion)
CREATE TABLE ews_sync_state (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_email TEXT NOT NULL,
    folder_id TEXT NOT NULL,
    sync_state TEXT NOT NULL,
    jmap_state TEXT,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(user_email, folder_id)
);

-- Device metadata table (future provisioning/policy enrichment)
CREATE TABLE device_info (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_email TEXT NOT NULL,
    device_id TEXT NOT NULL,
    friendly_name TEXT,
    last_seen DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(device_id)
);

CREATE INDEX idx_sync_lookup ON sync_state(owner, collection_id);
CREATE INDEX idx_item_map_owner_time ON item_map(owner, updated_at);
CREATE INDEX idx_item_map_resource ON item_map(owner, resource_href);
CREATE INDEX idx_ews_sync_lookup ON ews_sync_state(user_email, folder_id);

CREATE INDEX idx_provision_lookup ON provision_state(owner, device_id);

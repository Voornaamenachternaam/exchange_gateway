DROP TABLE IF EXISTS sync_state;
DROP TABLE IF EXISTS ews_sync_state;
DROP TABLE IF EXISTS device_info;
DROP TABLE IF EXISTS device_policy;

CREATE TABLE sync_state (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_email TEXT NOT NULL,
    device_id TEXT NOT NULL,
    collection_id TEXT NOT NULL,
    sync_key TEXT NOT NULL UNIQUE,
    jmap_state TEXT NOT NULL,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(user_email, device_id, collection_id)
);

CREATE TABLE ews_sync_state (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_email TEXT NOT NULL,
    folder_id TEXT NOT NULL,
    sync_state TEXT NOT NULL,
    jmap_state TEXT NOT NULL,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(user_email, folder_id)
);

CREATE TABLE device_info (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_email TEXT NOT NULL,
    device_id TEXT NOT NULL,
    friendly_name TEXT,
    last_seen DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(device_id)
);

CREATE INDEX idx_sync_lookup ON sync_state(user_email, device_id, collection_id);
CREATE INDEX idx_ews_sync_lookup ON ews_sync_state(user_email, folder_id);


CREATE TABLE device_policy (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_email TEXT NOT NULL,
    device_id TEXT NOT NULL,
    current_policy_key TEXT,
    pending_policy_key TEXT,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(user_email, device_id)
);

CREATE INDEX idx_device_policy_lookup ON device_policy(user_email, device_id);

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS users (
  username TEXT PRIMARY KEY,
  last_seen INTEGER DEFAULT (strftime('%s','now'))
);

CREATE TABLE IF NOT EXISTS calendars (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  owner TEXT NOT NULL,
  caldav_href TEXT NOT NULL,
  collection_id TEXT NOT NULL,
  UNIQUE(owner, caldav_href)
);

CREATE TABLE IF NOT EXISTS items_map (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  owner TEXT NOT NULL,
  caldav_href TEXT NOT NULL,
  resource_href TEXT NOT NULL,
  server_id TEXT NOT NULL UNIQUE,
  item_id TEXT,
  change_key TEXT,
  uid TEXT,
  etag TEXT,
  sequence INTEGER DEFAULT 0,
  last_sync INTEGER
);

CREATE TABLE IF NOT EXISTS sync_state (
  owner TEXT NOT NULL,
  collection_id TEXT NOT NULL,
  sync_key TEXT NOT NULL,
  last_sync_token TEXT,
  last_sync_ts INTEGER,
  PRIMARY KEY(owner, collection_id)
);

CREATE INDEX IF NOT EXISTS idx_items_map_owner ON items_map(owner);
CREATE INDEX IF NOT EXISTS idx_calendars_owner ON calendars(owner);

use sqlx::{SqlitePool, sqlite::SqlitePoolOptions, Row, Transaction};
use std::path::Path;
use anyhow::Result;
use chrono::Utc;

pub struct Storage {
    pub pool: SqlitePool,
    pub db_path: String,
}

impl Storage {
    pub async fn new(db_path: &str) -> Result<Self> {
        if let Some(parent) = Path::new(db_path).parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let db_url = format!("sqlite://{}?mode=rwc", db_path);
        let pool = SqlitePoolOptions::new().max_connections(5).connect(&db_url).await?;
        Ok(Self { pool, db_path: db_path.to_string() })
    }

    pub async fn run_migrations(&self) -> Result<()> {
        let sql = include_str!("../migrations/001_init.sql");
        sqlx::query(sql).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn get_sync_key(&self, owner: &str, collection_id: &str) -> Result<Option<String>> {
        let row = sqlx::query("SELECT sync_key FROM sync_state WHERE owner = ? AND collection_id = ?")
            .bind(owner).bind(collection_id)
            .fetch_optional(&self.pool).await?;
        Ok(row.map(|r| r.get::<String,_>("sync_key")))
    }

    pub async fn set_sync_key(&self, owner: &str, collection_id: &str, sync_key: &str, token: Option<&str>) -> Result<()> {
        let token = token.unwrap_or("");
        sqlx::query("INSERT INTO sync_state (owner, collection_id, sync_key, last_sync_token, last_sync_ts) VALUES (?, ?, ?, ?, strftime('%s','now')) ON CONFLICT(owner, collection_id) DO UPDATE SET sync_key=excluded.sync_key, last_sync_token=excluded.last_sync_token, last_sync_ts=strftime('%s','now')")
            .bind(owner).bind(collection_id).bind(sync_key).bind(token)
            .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn upsert_item_map(&self, owner: &str, caldav_href: &str, resource_href: &str, server_id: &str, uid: &str, etag: &str) -> Result<()> {
        sqlx::query("INSERT INTO items_map (owner, caldav_href, resource_href, server_id, uid, etag, last_sync) VALUES (?, ?, ?, ?, ?, ?, strftime('%s','now')) ON CONFLICT(server_id) DO UPDATE SET resource_href=excluded.resource_href, uid=excluded.uid, etag=excluded.etag, last_sync=strftime('%s','now')")
            .bind(owner).bind(caldav_href).bind(resource_href).bind(server_id).bind(uid).bind(etag)
            .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn get_item_by_server_id(&self, server_id: &str) -> Result<Option<(i64, String)>> {
        let row = sqlx::query("SELECT id, resource_href FROM items_map WHERE server_id = ?")
            .bind(server_id)
            .fetch_optional(&self.pool).await?;
        Ok(row.map(|r| (r.get::<i64,_>("id"), r.get::<String,_>("resource_href"))))
    }

    pub async fn delete_item_by_server_id(&self, server_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM items_map WHERE server_id = ?").bind(server_id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn list_changes_since(&self, owner: &str, since_unix_ts: i64) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query("SELECT server_id, resource_href FROM items_map WHERE owner = ? AND last_sync >= ?")
            .bind(owner).bind(since_unix_ts)
            .fetch_all(&self.pool).await?;
        let mut res = Vec::new();
        for r in rows {
            res.push((r.get::<String,_>("server_id"), r.get::<String,_>("resource_href")));
        }
        Ok(res)
    }

    pub async fn transaction<F, T>(&self, f: F) -> Result<T>
    where
        F: for<'c> FnOnce(Transaction<'c, sqlx::Sqlite>) -> futures::future::BoxFuture<'c, Result<T>>,
    {
        let mut tx = self.pool.begin().await?;
        let res = f(tx).await?;
        tx.commit().await?;
        Ok(res)
    }
}

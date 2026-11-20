use std::time::Duration;

use exchange_gateway::storage::{FolderUpsertParams, Storage};
use sqlx::{Connection, Executor, SqliteConnection};

#[tokio::test]
async fn sqlite_lock_during_journaled_rename_rolls_back_without_data_loss() {
    let path = std::env::temp_dir().join(format!(
        "exchange-gateway-lock-fault-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let database_url = format!("sqlite://{}?mode=rwc", path.display());
    let storage = Storage::new(&database_url).await.unwrap();
    storage.init_schema().await.unwrap();
    storage
        .upsert_folder_journaled(
            FolderUpsertParams {
                owner: "u@example.com",
                server_id: "mb-locked",
                parent_id: "0",
                display_name: "Original",
                folder_type: 12,
                backend_kind: "jmap",
                backend_id: "mb-locked",
            },
            "folder-add",
        )
        .await
        .unwrap();
    let (journal_head, journal_rows, _) = storage
        .snapshot_folder_sync_data("u@example.com", 0)
        .await
        .unwrap();
    assert_eq!(journal_rows.len(), 1);

    let mut locker = SqliteConnection::connect(&database_url).await.unwrap();
    locker.execute("BEGIN EXCLUSIVE").await.unwrap();
    let rename = tokio::time::timeout(
        Duration::from_secs(15),
        storage.update_folder_journaled(
            "u@example.com",
            "mb-locked",
            None,
            Some("Must not commit"),
        ),
    )
    .await
    .expect("SQLite busy timeout must bound the blocked rename");
    let error = rename.expect_err("an exclusively locked database must reject the write");
    let message = error.to_string().to_ascii_lowercase();
    assert!(
        message.contains("locked") || message.contains("busy"),
        "unexpected write failure: {error}"
    );
    locker.execute("ROLLBACK").await.unwrap();

    let row = storage
        .get_folder("u@example.com", "mb-locked")
        .await
        .unwrap()
        .expect("the original mapping must survive");
    assert_eq!(row.display_name, "Original");
    assert_eq!(row.parent_id, "0");
    let (new_head, new_rows, _) = storage
        .snapshot_folder_sync_data("u@example.com", 0)
        .await
        .unwrap();
    assert_eq!(new_head, journal_head, "no journal row may be acknowledged");
    assert_eq!(new_rows.len(), journal_rows.len());
    assert_eq!(new_rows[0].op, "folder-add");

    locker.close().await.unwrap();
    storage.pool().close().await;
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

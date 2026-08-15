use std::{str::FromStr, sync::Arc};

use sqlx::{
    ConnectOptions, Error, Pool, Sqlite, SqlitePool,
    migrate::MigrateError,
    sqlite::{SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePoolOptions},
};
use tokio::{
    sync::broadcast,
    time::{Duration, sleep},
};
use utils::assets::asset_dir;
use uuid::Uuid;

use crate::models::execution_process::{ExecutionProcess, ExecutionProcessStatus};

pub mod models;

async fn run_migrations(pool: &Pool<Sqlite>) -> Result<(), Error> {
    use std::collections::HashSet;

    let migrator = sqlx::migrate!("./migrations");
    let mut processed_versions: HashSet<i64> = HashSet::new();

    loop {
        match migrator.run(pool).await {
            Ok(()) => return Ok(()),
            Err(MigrateError::VersionMismatch(version)) => {
                if cfg!(debug_assertions) {
                    // return the error in debug mode to catch migration issues early
                    return Err(sqlx::Error::Migrate(Box::new(
                        MigrateError::VersionMismatch(version),
                    )));
                }

                if !cfg!(windows) {
                    // On non-Windows platforms, we do not attempt to auto-fix checksum mismatches
                    return Err(sqlx::Error::Migrate(Box::new(
                        MigrateError::VersionMismatch(version),
                    )));
                }

                // Guard against infinite loop
                if !processed_versions.insert(version) {
                    return Err(sqlx::Error::Migrate(Box::new(
                        MigrateError::VersionMismatch(version),
                    )));
                }

                // On Windows, there can be checksum mismatches due to line ending differences
                // or other platform-specific issues. Update the stored checksum and retry.
                tracing::warn!(
                    "Migration version {} has checksum mismatch, updating stored checksum (likely platform-specific difference)",
                    version
                );

                // Find the migration with the mismatched version and get its current checksum
                if let Some(migration) = migrator.iter().find(|m| m.version == version) {
                    // Update the checksum in _sqlx_migrations to match the current file
                    sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
                        .bind(&*migration.checksum)
                        .bind(version)
                        .execute(pool)
                        .await?;
                } else {
                    // Migration not found in current set, can't fix
                    return Err(sqlx::Error::Migrate(Box::new(
                        MigrateError::VersionMismatch(version),
                    )));
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
}

#[derive(Clone)]
pub struct DBService {
    pub pool: Pool<Sqlite>,
    execution_completions: broadcast::Sender<Uuid>,
}

impl DBService {
    pub async fn new() -> Result<DBService, Error> {
        let database_url = format!(
            "sqlite://{}",
            asset_dir().join("db.v2.sqlite").to_string_lossy()
        );
        let options = SqliteConnectOptions::from_str(&database_url)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Delete);
        let pool = SqlitePool::connect_with(options).await?;
        run_migrations(&pool).await?;
        Ok(DBService::from_pool(pool))
    }

    pub async fn new_migration_pool() -> Result<Pool<Sqlite>, Error> {
        let database_url = format!(
            "sqlite://{}",
            asset_dir().join("db.v2.sqlite").to_string_lossy()
        );
        let options = SqliteConnectOptions::from_str(&database_url)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Delete)
            .disable_statement_logging();
        SqlitePoolOptions::new()
            .max_connections(64)
            .connect_with(options)
            .await
    }

    pub async fn new_with_after_connect<F>(after_connect: F) -> Result<DBService, Error>
    where
        F: for<'a> Fn(
                &'a mut SqliteConnection,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<(), Error>> + Send + 'a>,
            > + Send
            + Sync
            + 'static,
    {
        let pool = Self::create_pool(Some(Arc::new(after_connect))).await?;
        Ok(DBService::from_pool(pool))
    }

    pub fn from_pool(pool: Pool<Sqlite>) -> Self {
        let (execution_completions, _) = broadcast::channel(1024);
        Self {
            pool,
            execution_completions,
        }
    }

    /// Subscribe to terminal execution updates. Notifications are published
    /// only after the database update has completed successfully.
    pub fn subscribe_execution_completions(&self) -> broadcast::Receiver<Uuid> {
        self.execution_completions.subscribe()
    }

    pub async fn update_execution_completion(
        &self,
        id: Uuid,
        status: ExecutionProcessStatus,
        exit_code: Option<i64>,
    ) -> Result<(), Error> {
        const MAX_BUSY_ATTEMPTS: usize = 4;
        let mut attempt = 0;
        loop {
            match ExecutionProcess::update_completion(&self.pool, id, status.clone(), exit_code)
                .await
            {
                Ok(()) => break,
                Err(error) if is_sqlite_busy(&error) && attempt + 1 < MAX_BUSY_ATTEMPTS => {
                    attempt += 1;
                    tracing::warn!(
                        execution_id = %id,
                        attempt,
                        "SQLite busy while finalizing execution; retrying"
                    );
                    sleep(Duration::from_millis(25 * attempt as u64)).await;
                }
                Err(error) => return Err(error),
            }
        }
        let _ = self.execution_completions.send(id);
        Ok(())
    }

    async fn create_pool<F>(after_connect: Option<Arc<F>>) -> Result<Pool<Sqlite>, Error>
    where
        F: for<'a> Fn(
                &'a mut SqliteConnection,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<(), Error>> + Send + 'a>,
            > + Send
            + Sync
            + 'static,
    {
        let database_url = format!(
            "sqlite://{}",
            asset_dir().join("db.v2.sqlite").to_string_lossy()
        );
        let options = SqliteConnectOptions::from_str(&database_url)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Delete);

        let pool = if let Some(hook) = after_connect {
            SqlitePoolOptions::new()
                .after_connect(move |conn, _meta| {
                    let hook = hook.clone();
                    Box::pin(async move {
                        hook(conn).await?;
                        Ok(())
                    })
                })
                .connect_with(options)
                .await?
        } else {
            SqlitePool::connect_with(options).await?
        };

        run_migrations(&pool).await?;
        Ok(pool)
    }
}

fn is_sqlite_busy(error: &Error) -> bool {
    matches!(error, Error::Database(database_error) if database_error.code().as_deref() == Some("5"))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    async fn execution_service() -> DBService {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE execution_processes (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                exit_code INTEGER,
                completed_at TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        DBService::from_pool(pool)
    }

    #[tokio::test]
    async fn completion_notification_is_visible_after_database_update() {
        let db = execution_service().await;
        let execution_id = Uuid::new_v4();
        sqlx::query("INSERT INTO execution_processes (id, status) VALUES (?, 'running')")
            .bind(execution_id)
            .execute(&db.pool)
            .await
            .unwrap();
        let mut completions = db.subscribe_execution_completions();

        db.update_execution_completion(execution_id, ExecutionProcessStatus::Completed, Some(0))
            .await
            .unwrap();

        assert_eq!(completions.recv().await.unwrap(), execution_id);
        let status: String =
            sqlx::query_scalar("SELECT status FROM execution_processes WHERE id = ?")
                .bind(execution_id)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(status, "completed");
    }

    #[tokio::test]
    async fn nonexistent_completion_update_fails_without_notification() {
        let db = execution_service().await;
        let mut completions = db.subscribe_execution_completions();

        let result = db
            .update_execution_completion(Uuid::new_v4(), ExecutionProcessStatus::Completed, Some(0))
            .await;

        assert!(matches!(result, Err(sqlx::Error::RowNotFound)));
        assert!(matches!(
            completions.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn completion_retries_a_transient_sqlite_busy_lock() {
        let path = std::env::temp_dir().join(format!("vk-db-busy-{}.sqlite", Uuid::new_v4()));
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .busy_timeout(Duration::ZERO);
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE execution_processes (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                exit_code INTEGER,
                completed_at TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        let execution_id = Uuid::new_v4();
        sqlx::query("INSERT INTO execution_processes (id, status) VALUES (?, 'running')")
            .bind(execution_id)
            .execute(&pool)
            .await
            .unwrap();

        let mut lock = pool.acquire().await.unwrap();
        sqlx::query("BEGIN EXCLUSIVE")
            .execute(&mut *lock)
            .await
            .unwrap();
        let db = DBService::from_pool(pool.clone());
        let update = tokio::spawn(async move {
            db.update_execution_completion(execution_id, ExecutionProcessStatus::Completed, Some(0))
                .await
        });
        tokio::time::sleep(Duration::from_millis(35)).await;
        sqlx::query("ROLLBACK").execute(&mut *lock).await.unwrap();

        update.await.unwrap().unwrap();
        let status: String =
            sqlx::query_scalar("SELECT status FROM execution_processes WHERE id = ?")
                .bind(execution_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "completed");

        drop(lock);
        pool.close().await;
        std::fs::remove_file(&path).unwrap();
    }
}

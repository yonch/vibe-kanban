use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool, Type};
use ts_rs::TS;
use uuid::Uuid;

use super::pull_request::PullRequest;

#[derive(Debug, Clone, Serialize, Deserialize, TS, Type)]
#[sqlx(type_name = "merge_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum MergeStatus {
    Open,
    Merged,
    Closed,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Merge {
    Direct(DirectMerge),
    Pr(PrMerge),
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DirectMerge {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub repo_id: Uuid,
    pub merge_commit: String,
    pub target_branch_name: String,
    pub created_at: DateTime<Utc>,
}

/// PR merge - represents a pull request merge
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct PrMerge {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub repo_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub target_branch_name: String,
    pub pr_info: PullRequestInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct PullRequestInfo {
    pub number: i64,
    pub url: String,
    pub status: MergeStatus,
    pub merged_at: Option<chrono::DateTime<chrono::Utc>>,
    pub merge_commit_sha: Option<String>,
}

/// Active workspace-repo pair used for auto-detecting PRs created outside of VK.
#[derive(Debug, Clone, FromRow)]
pub struct ActiveWorkspaceRepo {
    pub workspace_id: Uuid,
    pub repo_id: Uuid,
    pub workspace_branch: String,
    pub target_branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum MergeType {
    Direct,
    Pr,
}

/// Row type for direct merges only (PR data now lives in pull_requests).
struct DirectMergeRow {
    id: Uuid,
    workspace_id: Uuid,
    repo_id: Uuid,
    merge_commit: Option<String>,
    target_branch_name: String,
    created_at: DateTime<Utc>,
}

impl Merge {
    pub fn merge_commit(&self) -> Option<String> {
        match self {
            Merge::Direct(direct) => Some(direct.merge_commit.clone()),
            Merge::Pr(pr) => pr.pr_info.merge_commit_sha.clone(),
        }
    }

    /// Create a direct merge record
    pub async fn create_direct(
        pool: &SqlitePool,
        workspace_id: Uuid,
        repo_id: Uuid,
        target_branch_name: &str,
        merge_commit: &str,
    ) -> Result<DirectMerge, sqlx::Error> {
        let id = Uuid::new_v4();
        let now = Utc::now();

        sqlx::query!(
            "INSERT INTO merges (id, workspace_id, repo_id, merge_type, merge_commit, created_at, target_branch_name)
            VALUES (?, ?, ?, 'direct', ?, ?, ?)",
            id,
            workspace_id,
            repo_id,
            merge_commit,
            now,
            target_branch_name,
        )
        .execute(pool)
        .await?;

        Ok(DirectMerge {
            id,
            workspace_id,
            repo_id,
            merge_commit: merge_commit.to_string(),
            target_branch_name: target_branch_name.to_string(),
            created_at: now,
        })
    }

    /// Find all active (non-archived) workspace-repo pairs.
    /// Used by the PR monitor to discover PRs created outside of VK.
    /// When `updated_since` is provided, only returns workspaces that have been
    /// updated since that time, reducing unnecessary GitHub API calls.
    ///
    /// Takes `NaiveDateTime` rather than `DateTime<Utc>` so the bound value is
    /// encoded as `"YYYY-MM-DD HH:MM:SS.fff"`, matching the format SQLite writes
    /// for `workspaces.updated_at` (via `datetime('now', 'subsec')`). Binding
    /// `DateTime<Utc>` would produce RFC3339 `"YYYY-MM-DDTHH:MM:SS.fff+00:00"`,
    /// which sorts lexicographically *after* the stored format because `'T'`
    /// (0x54) > `' '` (0x20), so the `>= ?` predicate would never match a
    /// same-day row.
    pub async fn get_active_workspace_repos(
        pool: &SqlitePool,
        updated_since: Option<NaiveDateTime>,
    ) -> Result<Vec<ActiveWorkspaceRepo>, sqlx::Error> {
        sqlx::query_as!(
            ActiveWorkspaceRepo,
            r#"SELECT
                wr.workspace_id AS "workspace_id!: Uuid",
                wr.repo_id AS "repo_id!: Uuid",
                w.branch AS "workspace_branch!",
                wr.target_branch AS "target_branch!"
            FROM workspace_repos wr
            JOIN workspaces w ON w.id = wr.workspace_id
            WHERE w.archived = FALSE
              AND (? IS NULL OR w.updated_at >= ?)
            ORDER BY w.updated_at DESC"#,
            updated_since,
            updated_since,
        )
        .fetch_all(pool)
        .await
    }

    /// Find all merges for a workspace (returns both direct merges and PRs).
    /// Direct merges come from the `merges` table, PRs from `pull_requests`.
    pub async fn find_by_workspace_id(
        pool: &SqlitePool,
        workspace_id: Uuid,
    ) -> Result<Vec<Self>, sqlx::Error> {
        let direct_rows = sqlx::query_as!(
            DirectMergeRow,
            r#"SELECT
                id AS "id!: Uuid",
                workspace_id AS "workspace_id!: Uuid",
                repo_id AS "repo_id!: Uuid",
                merge_commit,
                target_branch_name,
                created_at AS "created_at!: DateTime<Utc>"
            FROM merges
            WHERE workspace_id = ? AND merge_type = 'direct'
            ORDER BY created_at DESC"#,
            workspace_id,
        )
        .fetch_all(pool)
        .await?;

        let pull_requests = PullRequest::find_by_workspace_id(pool, workspace_id).await?;

        let mut merges: Vec<Merge> = direct_rows.into_iter().map(|row| row.into()).collect();
        merges.extend(pull_requests.iter().map(|pr| pr.to_merge()));

        // Sort by created_at descending (matching previous behavior)
        merges.sort_by(|a, b| {
            let a_time = match a {
                Merge::Direct(d) => d.created_at,
                Merge::Pr(p) => p.created_at,
            };
            let b_time = match b {
                Merge::Direct(d) => d.created_at,
                Merge::Pr(p) => p.created_at,
            };
            b_time.cmp(&a_time)
        });

        Ok(merges)
    }

    /// Update the status of a PR merge record by id.
    pub async fn update_status(
        pool: &SqlitePool,
        id: Uuid,
        status: MergeStatus,
        merge_commit_sha: Option<String>,
    ) -> Result<(), sqlx::Error> {
        let now = Utc::now();
        let (status_str, merged_at): (&str, Option<DateTime<Utc>>) = match status {
            MergeStatus::Open => ("open", None),
            MergeStatus::Merged => ("merged", Some(now)),
            MergeStatus::Closed => ("closed", None),
            MergeStatus::Unknown => ("unknown", None),
        };
        let id_str = id.to_string();
        sqlx::query(
            "UPDATE pull_requests SET pr_status = ?, merged_at = ?, merge_commit_sha = ?, updated_at = ?, synced_at = NULL WHERE id = ?",
        )
        .bind(status_str)
        .bind(merged_at)
        .bind(merge_commit_sha)
        .bind(now)
        .bind(id_str)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Count open PRs for a workspace.
    pub async fn count_open_prs_for_workspace(
        pool: &SqlitePool,
        workspace_id: Uuid,
    ) -> Result<i64, sqlx::Error> {
        PullRequest::count_open_for_workspace(pool, workspace_id).await
    }

    /// Find all merges for a workspace and specific repo
    pub async fn find_by_workspace_and_repo_id(
        pool: &SqlitePool,
        workspace_id: Uuid,
        repo_id: Uuid,
    ) -> Result<Vec<Self>, sqlx::Error> {
        let direct_rows = sqlx::query_as!(
            DirectMergeRow,
            r#"SELECT
                id AS "id!: Uuid",
                workspace_id AS "workspace_id!: Uuid",
                repo_id AS "repo_id!: Uuid",
                merge_commit,
                target_branch_name,
                created_at AS "created_at!: DateTime<Utc>"
            FROM merges
            WHERE workspace_id = ? AND repo_id = ? AND merge_type = 'direct'
            ORDER BY created_at DESC"#,
            workspace_id,
            repo_id,
        )
        .fetch_all(pool)
        .await?;

        let pull_requests =
            PullRequest::find_by_workspace_and_repo_id(pool, workspace_id, repo_id).await?;

        let mut merges: Vec<Merge> = direct_rows.into_iter().map(|row| row.into()).collect();
        merges.extend(pull_requests.iter().map(|pr| pr.to_merge()));

        merges.sort_by(|a, b| {
            let a_time = match a {
                Merge::Direct(d) => d.created_at,
                Merge::Pr(p) => p.created_at,
            };
            let b_time = match b {
                Merge::Direct(d) => d.created_at,
                Merge::Pr(p) => p.created_at,
            };
            b_time.cmp(&a_time)
        });

        Ok(merges)
    }
}

impl From<DirectMergeRow> for DirectMerge {
    fn from(row: DirectMergeRow) -> Self {
        DirectMerge {
            id: row.id,
            workspace_id: row.workspace_id,
            repo_id: row.repo_id,
            merge_commit: row
                .merge_commit
                .expect("direct merge must have merge_commit"),
            target_branch_name: row.target_branch_name,
            created_at: row.created_at,
        }
    }
}

impl From<DirectMergeRow> for Merge {
    fn from(row: DirectMergeRow) -> Self {
        Merge::Direct(DirectMerge::from(row))
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::{
        SqlitePool,
        sqlite::{SqliteConnectOptions, SqliteJournalMode},
    };

    use super::*;

    /// In-memory SQLite with just enough schema to exercise the watermarked
    /// `get_active_workspace_repos` query.
    async fn test_pool() -> SqlitePool {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Memory);
        let pool = SqlitePool::connect_with(options).await.unwrap();
        sqlx::query(
            r#"CREATE TABLE workspaces (
                id BLOB PRIMARY KEY NOT NULL,
                branch TEXT NOT NULL,
                archived BOOLEAN NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL DEFAULT (datetime('now', 'subsec'))
            )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"CREATE TABLE workspace_repos (
                workspace_id BLOB NOT NULL,
                repo_id BLOB NOT NULL,
                target_branch TEXT NOT NULL,
                PRIMARY KEY (workspace_id, repo_id)
            )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn insert_workspace(pool: &SqlitePool, branch: &str) -> Uuid {
        let workspace_id = Uuid::new_v4();
        let repo_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO workspaces (id, branch, updated_at) VALUES (?, ?, datetime('now', 'subsec'))",
        )
        .bind(workspace_id)
        .bind(branch)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO workspace_repos (workspace_id, repo_id, target_branch) VALUES (?, ?, ?)",
        )
        .bind(workspace_id)
        .bind(repo_id)
        .bind("origin/main")
        .execute(pool)
        .await
        .unwrap();
        workspace_id
    }

    // Stored `workspaces.updated_at` is space-separated (from
    // `datetime('now', 'subsec')`); a `DateTime<Utc>` bound parameter would be
    // RFC3339 ("T"-separated, "+00:00" suffix) and sort *after* the stored
    // value in text comparison, hiding every same-day workspace from the
    // watermark. Binding `NaiveDateTime` produces the same space-separated
    // format, so the comparison works.
    #[tokio::test(flavor = "current_thread")]
    async fn watermark_matches_same_day_workspace() {
        let pool = test_pool().await;
        let workspace_id = insert_workspace(&pool, "auto/test").await;

        // Watermark a full second before the insert; the workspace must appear.
        let watermark = (Utc::now() - chrono::Duration::seconds(1)).naive_utc();
        let candidates = Merge::get_active_workspace_repos(&pool, Some(watermark))
            .await
            .unwrap();

        assert_eq!(
            candidates.len(),
            1,
            "same-day workspace must be included when updated after the watermark"
        );
        assert_eq!(candidates[0].workspace_id, workspace_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn watermark_excludes_workspace_updated_before_watermark() {
        let pool = test_pool().await;
        insert_workspace(&pool, "auto/test").await;

        // Watermark well in the future; the workspace must be excluded.
        let watermark = (Utc::now() + chrono::Duration::hours(1)).naive_utc();
        let candidates = Merge::get_active_workspace_repos(&pool, Some(watermark))
            .await
            .unwrap();

        assert!(
            candidates.is_empty(),
            "workspaces updated before the watermark must be excluded"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn null_watermark_returns_all_active_workspaces() {
        let pool = test_pool().await;
        insert_workspace(&pool, "auto/a").await;
        insert_workspace(&pool, "auto/b").await;

        let candidates = Merge::get_active_workspace_repos(&pool, None)
            .await
            .unwrap();

        assert_eq!(candidates.len(), 2);
    }

    /// Pinpoints the encoding mismatch that caused the original bug: binding a
    /// `DateTime<Utc>` produces RFC3339 text (`'T'`, `+00:00`) that sorts after
    /// the space-separated form SQLite writes for `updated_at`, so the
    /// `>= ?` predicate excludes a same-day row even when the row was updated
    /// *after* the watermark in real time. Binding `NaiveDateTime` produces
    /// matching text and includes the row.
    #[tokio::test(flavor = "current_thread")]
    async fn datetime_utc_bind_breaks_same_day_comparison() {
        let pool = test_pool().await;
        insert_workspace(&pool, "auto/test").await;

        let watermark = Utc::now() - chrono::Duration::seconds(1);
        let sql = "SELECT COUNT(*) FROM workspaces w \
                   WHERE w.archived = FALSE AND (? IS NULL OR w.updated_at >= ?)";

        let buggy: i64 = sqlx::query_scalar(sql)
            .bind(Some(watermark))
            .bind(Some(watermark))
            .fetch_one(&pool)
            .await
            .unwrap();
        let fixed: i64 = sqlx::query_scalar(sql)
            .bind(Some(watermark.naive_utc()))
            .bind(Some(watermark.naive_utc()))
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!(buggy, 0, "DateTime<Utc> bind hides the same-day workspace");
        assert_eq!(fixed, 1, "NaiveDateTime bind matches the stored format");
    }
}

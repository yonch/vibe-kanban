use std::time::Duration;

use api_types::{PullRequestStatus, UpsertPullRequestRequest};
use chrono::Utc;
use db::{
    DBService,
    models::{
        merge::{ActiveWorkspaceRepo, Merge, MergeStatus, PrMerge},
        repo::Repo,
        workspace::{Workspace, WorkspaceError},
    },
};
use git::GitServiceError;
use git_host::{GitHostError, GitHostProvider, GitHostService};
use serde_json::json;
use sqlx::error::Error as SqlxError;
use thiserror::Error;
use tokio::time::interval;
use tracing::{debug, error, info, warn};

use crate::services::{
    analytics::AnalyticsContext, container::ContainerService, remote_client::RemoteClient,
    remote_sync,
};

#[derive(Debug, Error)]
enum PrMonitorError {
    #[error(transparent)]
    GitHostError(#[from] GitHostError),
    #[error(transparent)]
    GitServiceError(#[from] GitServiceError),
    #[error(transparent)]
    WorkspaceError(#[from] WorkspaceError),
    #[error(transparent)]
    Sqlx(#[from] SqlxError),
}

impl PrMonitorError {
    fn is_environmental(&self) -> bool {
        matches!(
            self,
            PrMonitorError::GitHostError(
                GitHostError::CliNotInstalled { .. } | GitHostError::NotAGitRepository(_)
            ) | PrMonitorError::GitServiceError(_)
        )
    }
}

/// Service to monitor PRs and update task status when they are merged
pub struct PrMonitorService<C: ContainerService> {
    db: DBService,
    poll_interval: Duration,
    analytics: Option<AnalyticsContext>,
    container: C,
    remote_client: Option<RemoteClient>,
}

impl<C: ContainerService + Send + Sync + 'static> PrMonitorService<C> {
    pub async fn spawn(
        db: DBService,
        analytics: Option<AnalyticsContext>,
        container: C,
        remote_client: Option<RemoteClient>,
    ) -> tokio::task::JoinHandle<()> {
        let service = Self {
            db,
            poll_interval: Duration::from_secs(60), // Check every minute
            analytics,
            container,
            remote_client,
        };
        tokio::spawn(async move {
            service.start().await;
        })
    }

    async fn start(&self) {
        info!(
            "Starting PR monitoring service with interval {:?}",
            self.poll_interval
        );

        let mut interval = interval(self.poll_interval);

        loop {
            interval.tick().await;
            // Discover new PRs first so check_all_open_prs can handle
            // their status updates in the same cycle.
            if let Err(e) = self.discover_new_prs().await {
                error!("Error discovering new PRs: {}", e);
            }
            if let Err(e) = self.check_all_open_prs().await {
                error!("Error checking open PRs: {}", e);
            }
        }
    }

    /// Check all open PRs for updates with the provided GitHub token
    async fn check_all_open_prs(&self) -> Result<(), PrMonitorError> {
        let open_prs = Merge::get_open_prs(&self.db.pool).await?;

        if open_prs.is_empty() {
            debug!("No open PRs to check");
            return Ok(());
        }

        info!("Checking {} open PRs", open_prs.len());

        for pr_merge in open_prs {
            if let Err(e) = self.check_pr_status(&pr_merge).await {
                if e.is_environmental() {
                    warn!(
                        "Skipping PR #{} for workspace {} due to environmental error: {}",
                        pr_merge.pr_info.number, pr_merge.workspace_id, e
                    );
                } else {
                    error!(
                        "Error checking PR #{} for workspace {}: {}",
                        pr_merge.pr_info.number, pr_merge.workspace_id, e
                    );
                }
            }
        }
        Ok(())
    }

    /// Discover PRs created outside of VK on workspace branches.
    /// Scans all active workspaces, queries GitHub for PRs on their branch,
    /// and links any not already known. Runs before check_all_open_prs so
    /// newly discovered PRs get their status checked in the same cycle.
    async fn discover_new_prs(&self) -> Result<(), PrMonitorError> {
        let candidates = Merge::get_active_workspace_repos(&self.db.pool).await?;

        if candidates.is_empty() {
            debug!("No active workspace repos to check for PRs");
            return Ok(());
        }

        debug!(
            "Checking {} workspace-repo pairs for new PRs",
            candidates.len()
        );

        for candidate in &candidates {
            if let Err(e) = self.discover_prs_for_workspace_repo(candidate).await {
                if e.is_environmental() {
                    debug!(
                        "Skipping PR discovery for workspace {} repo {} due to environmental error: {}",
                        candidate.workspace_id, candidate.repo_id, e
                    );
                } else {
                    warn!(
                        "Error discovering PR for workspace {} repo {}: {}",
                        candidate.workspace_id, candidate.repo_id, e
                    );
                }
            }
        }
        Ok(())
    }

    /// Query GitHub for PRs on a workspace branch and link any we don't already know about.
    /// Only creates merge records; status updates and archival are handled by
    /// check_all_open_prs later in the same cycle.
    async fn discover_prs_for_workspace_repo(
        &self,
        candidate: &ActiveWorkspaceRepo,
    ) -> Result<(), PrMonitorError> {
        let repo = Repo::find_by_id(&self.db.pool, candidate.repo_id)
            .await?
            .ok_or_else(|| {
                PrMonitorError::GitHostError(GitHostError::PullRequest(format!(
                    "Repo {} not found",
                    candidate.repo_id
                )))
            })?;

        let git = self.container.git();
        let remote = git.resolve_remote_for_branch(&repo.path, &candidate.target_branch)?;

        let git_host = GitHostService::from_url(&remote.url)?;
        let prs = git_host
            .list_prs_for_branch(&repo.path, &remote.url, &candidate.workspace_branch)
            .await?;

        if prs.is_empty() {
            return Ok(());
        }

        let known_pr_numbers = Merge::get_known_pr_numbers(
            &self.db.pool,
            candidate.workspace_id,
            candidate.repo_id,
        )
        .await?;

        for pr_info in prs {
            if known_pr_numbers.contains(&pr_info.number) {
                continue;
            }

            info!(
                "Auto-discovered PR #{} for workspace {} repo {}",
                pr_info.number, candidate.workspace_id, candidate.repo_id
            );

            Merge::create_pr(
                &self.db.pool,
                candidate.workspace_id,
                candidate.repo_id,
                &candidate.target_branch,
                pr_info.number,
                &pr_info.url,
            )
            .await?;
        }

        Ok(())
    }

    /// Check the status of a specific PR
    async fn check_pr_status(&self, pr_merge: &PrMerge) -> Result<(), PrMonitorError> {
        let git_host = GitHostService::from_url(&pr_merge.pr_info.url)?;
        let pr_status = git_host.get_pr_status(&pr_merge.pr_info.url).await?;

        debug!(
            "PR #{} status: {:?} (was open)",
            pr_merge.pr_info.number, pr_status.status
        );

        // Update the PR status in the database
        if !matches!(&pr_status.status, MergeStatus::Open) {
            // Update merge status with the latest information from git host
            Merge::update_status(
                &self.db.pool,
                pr_merge.id,
                pr_status.status.clone(),
                pr_status.merge_commit_sha.clone(),
            )
            .await?;

            self.sync_pr_to_remote(pr_merge, &pr_status.status, pr_status.merge_commit_sha)
                .await;

            // If the PR was merged, archive the workspace
            if matches!(&pr_status.status, MergeStatus::Merged)
                && let Some(workspace) =
                    Workspace::find_by_id(&self.db.pool, pr_merge.workspace_id).await?
            {
                let open_pr_count =
                    Merge::count_open_prs_for_workspace(&self.db.pool, workspace.id).await?;

                if open_pr_count == 0 {
                    info!(
                        "PR #{} was merged, archiving workspace {}",
                        pr_merge.pr_info.number, workspace.id
                    );
                    if !workspace.pinned
                        && let Err(e) = self.container.archive_workspace(workspace.id).await
                    {
                        error!("Failed to archive workspace {}: {}", workspace.id, e);
                    }
                } else {
                    info!(
                        "PR #{} was merged, leaving workspace {} active with {} open PR(s)",
                        pr_merge.pr_info.number, workspace.id, open_pr_count
                    );
                }

                // Track analytics event
                if let Some(analytics) = &self.analytics {
                    analytics.analytics_service.track_event(
                        &analytics.user_id,
                        "pr_merged",
                        Some(json!({
                            "workspace_id": workspace.id.to_string(),
                        })),
                    );
                }
            }
        }

        Ok(())
    }

    /// Sync PR status to remote server
    async fn sync_pr_to_remote(
        &self,
        pr_merge: &PrMerge,
        status: &MergeStatus,
        merge_commit_sha: Option<String>,
    ) {
        let Some(client) = &self.remote_client else {
            return;
        };

        let pr_status = match status {
            MergeStatus::Open => PullRequestStatus::Open,
            MergeStatus::Merged => PullRequestStatus::Merged,
            MergeStatus::Closed => PullRequestStatus::Closed,
            MergeStatus::Unknown => return,
        };

        let merged_at = if matches!(status, MergeStatus::Merged) {
            Some(Utc::now())
        } else {
            None
        };

        let client = client.clone();
        let request = UpsertPullRequestRequest {
            url: pr_merge.pr_info.url.clone(),
            number: pr_merge.pr_info.number as i32,
            status: pr_status,
            merged_at,
            merge_commit_sha,
            target_branch_name: pr_merge.target_branch_name.clone(),
            local_workspace_id: pr_merge.workspace_id,
        };
        tokio::spawn(async move {
            remote_sync::sync_pr_to_remote(&client, request).await;
        });
    }
}

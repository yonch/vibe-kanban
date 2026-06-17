use std::time::{Duration, Instant};

use db::models::{requests::UpdateWorkspace, workspace::Workspace};
use rmcp::{
    ErrorData, handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool,
    tool_router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{ApiResponseEnvelope, McpServer, ToolError};

const DEFAULT_TIMEOUT_SECONDS: u64 = 1800;
const WAIT_EXECUTION_HTTP_RETRIES: usize = 3;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct McpListWorkspacesRequest {
    #[schemars(description = "Filter by archived state")]
    archived: Option<bool>,
    #[schemars(description = "Filter by pinned state")]
    pinned: Option<bool>,
    #[schemars(description = "Filter by branch name (exact match, case-insensitive)")]
    branch: Option<String>,
    #[schemars(description = "Case-insensitive substring match against workspace name")]
    name_search: Option<String>,
    #[schemars(description = "Maximum number of workspaces to return (default: 50)")]
    limit: Option<i32>,
    #[schemars(description = "Number of results to skip before returning rows (default: 0)")]
    offset: Option<i32>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct WorkspaceSummary {
    #[schemars(description = "Workspace ID")]
    id: String,
    #[schemars(description = "Workspace branch")]
    branch: String,
    #[schemars(description = "Whether the workspace is archived")]
    archived: bool,
    #[schemars(description = "Whether the workspace is pinned")]
    pinned: bool,
    #[schemars(description = "Optional workspace display name")]
    name: Option<String>,
    #[schemars(description = "Creation timestamp")]
    created_at: String,
    #[schemars(description = "Last update timestamp")]
    updated_at: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct McpListWorkspacesResponse {
    workspaces: Vec<WorkspaceSummary>,
    total_count: usize,
    returned_count: usize,
    limit: usize,
    offset: usize,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct McpUpdateWorkspaceRequest {
    #[schemars(
        description = "Workspace ID to update. Optional if running inside that workspace context."
    )]
    workspace_id: Option<Uuid>,
    #[schemars(description = "Set archived state")]
    archived: Option<bool>,
    #[schemars(description = "Set pinned state")]
    pinned: Option<bool>,
    #[schemars(description = "Set workspace display name (empty string clears it)")]
    name: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct McpUpdateWorkspaceResponse {
    success: bool,
    workspace_id: String,
    archived: bool,
    pinned: bool,
    name: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct McpDeleteWorkspaceRequest {
    #[schemars(
        description = "Workspace ID to delete. Optional if running inside that workspace context."
    )]
    workspace_id: Option<Uuid>,
    #[schemars(
        description = "Also delete linked remote workspace when available (default: false)"
    )]
    delete_remote: Option<bool>,
    #[schemars(description = "Also delete workspace branches from repos (default: false)")]
    delete_branches: Option<bool>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct McpDeleteWorkspaceResponse {
    success: bool,
    workspace_id: String,
    delete_remote: bool,
    delete_branches: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct McpWaitExecutionRequest {
    #[schemars(
        description = "One or more execution IDs to wait on. When multiple IDs are provided, returns as soon as any one reaches a terminal state."
    )]
    execution_ids: Vec<Uuid>,
    #[schemars(
        description = "Maximum time to wait in seconds before returning a timeout response (default: 1800). If your MCP harness times out first, call again with the same IDs and the remaining intended wait budget, e.g. intended 900s wait - elapsed 120s harness timeout = 780s."
    )]
    timeout_seconds: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
struct McpWaitExecutionResponse {
    #[schemars(
        description = "The execution ID that reached a terminal state (or first ID on timeout)"
    )]
    completed_execution_id: String,
    #[schemars(description = "The session ID that owns the completed execution")]
    session_id: String,
    #[schemars(description = "Terminal status: 'completed', 'failed', 'killed', or 'timeout'")]
    status: String,
    #[schemars(description = "Timestamp when the execution completed (if available)")]
    completed_at: Option<String>,
    #[schemars(
        description = "Final assistant message/summary from the completed execution (if available)"
    )]
    output: Option<String>,
    #[schemars(description = "Whether the agent accepted the execution before it completed")]
    accepted_by_agent: bool,
}

#[tool_router(router = workspaces_tools_router, vis = "pub")]
impl McpServer {
    #[tool(description = "List local workspaces with optional filters and pagination.")]
    async fn list_workspaces(
        &self,
        Parameters(McpListWorkspacesRequest {
            archived,
            pinned,
            branch,
            name_search,
            limit,
            offset,
        }): Parameters<McpListWorkspacesRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let url = self.url("/api/workspaces");
        let mut workspaces: Vec<Workspace> = match self.send_json(self.client.get(&url)).await {
            Ok(ws) => ws,
            Err(e) => return Ok(Self::tool_error(e)),
        };

        if let Some(archived_filter) = archived {
            workspaces.retain(|w| w.archived == archived_filter);
        }
        if let Some(pinned_filter) = pinned {
            workspaces.retain(|w| w.pinned == pinned_filter);
        }
        if let Some(branch_filter) = branch.as_deref() {
            workspaces.retain(|w| w.branch.eq_ignore_ascii_case(branch_filter));
        }
        if let Some(name_search) = name_search.as_deref() {
            let needle = name_search.to_ascii_lowercase();
            workspaces.retain(|w| {
                w.name
                    .as_deref()
                    .map(|name| name.to_ascii_lowercase().contains(&needle))
                    .unwrap_or(false)
            });
        }

        // Keep ordering deterministic after filtering.
        workspaces.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        let total_count = workspaces.len();
        let offset = offset.unwrap_or(0).max(0) as usize;
        let limit = limit.unwrap_or(50).max(0) as usize;

        let workspace_summaries = workspaces
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|workspace| WorkspaceSummary {
                id: workspace.id.to_string(),
                branch: workspace.branch,
                archived: workspace.archived,
                pinned: workspace.pinned,
                name: workspace.name,
                created_at: workspace.created_at.to_rfc3339(),
                updated_at: workspace.updated_at.to_rfc3339(),
            })
            .collect::<Vec<_>>();

        McpServer::success(&McpListWorkspacesResponse {
            returned_count: workspace_summaries.len(),
            total_count,
            limit,
            offset,
            workspaces: workspace_summaries,
        })
    }

    #[tool(
        description = "Update a workspace's archived, pinned, or name fields. `workspace_id` is optional if running inside that workspace context."
    )]
    async fn update_workspace(
        &self,
        Parameters(McpUpdateWorkspaceRequest {
            workspace_id,
            archived,
            pinned,
            name,
        }): Parameters<McpUpdateWorkspaceRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let workspace_id = match self.resolve_workspace_id(workspace_id) {
            Ok(id) => id,
            Err(error_result) => return Ok(Self::tool_error(error_result)),
        };
        if let Err(error_result) = self.scope_allows_workspace(workspace_id) {
            return Ok(Self::tool_error(error_result));
        }

        let url = self.url(&format!("/api/workspaces/{}", workspace_id));
        let payload = UpdateWorkspace {
            archived,
            pinned,
            name,
        };

        let updated: Workspace = match self.send_json(self.client.put(&url).json(&payload)).await {
            Ok(ws) => ws,
            Err(e) => return Ok(Self::tool_error(e)),
        };

        McpServer::success(&McpUpdateWorkspaceResponse {
            success: true,
            workspace_id: updated.id.to_string(),
            archived: updated.archived,
            pinned: updated.pinned,
            name: updated.name,
        })
    }

    #[tool(
        description = "Delete a local workspace. `workspace_id` is optional if running inside that workspace context."
    )]
    async fn delete_workspace(
        &self,
        Parameters(McpDeleteWorkspaceRequest {
            workspace_id,
            delete_remote,
            delete_branches,
        }): Parameters<McpDeleteWorkspaceRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let workspace_id = match self.resolve_workspace_id(workspace_id) {
            Ok(id) => id,
            Err(error_result) => return Ok(Self::tool_error(error_result)),
        };
        if let Err(error_result) = self.scope_allows_workspace(workspace_id) {
            return Ok(Self::tool_error(error_result));
        }

        let delete_remote = delete_remote.unwrap_or(false);
        let delete_branches = delete_branches.unwrap_or(false);

        let url = self.url(&format!("/api/workspaces/{}", workspace_id));
        if let Err(e) = self
            .send_empty_json(self.client.delete(&url).query(&[
                ("delete_remote", delete_remote),
                ("delete_branches", delete_branches),
            ]))
            .await
        {
            return Ok(Self::tool_error(e));
        }

        McpServer::success(&McpDeleteWorkspaceResponse {
            success: true,
            workspace_id: workspace_id.to_string(),
            delete_remote,
            delete_branches,
        })
    }

    #[tool(
        description = "Block until an execution reaches a terminal state (completed, failed, or killed) or timeout elapses. If your MCP client/harness times out before this tool returns, call it again with the same execution ID(s) and the remaining intended wait budget, e.g. intended 900s wait - elapsed 120s client timeout = 780s; do not treat a client/harness timeout as an execution timeout. When multiple execution IDs are provided, returns as soon as any one reaches a terminal state — call again with the remaining IDs to wait for the next completion."
    )]
    async fn wait_execution(
        &self,
        Parameters(McpWaitExecutionRequest {
            execution_ids,
            timeout_seconds,
        }): Parameters<McpWaitExecutionRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        if execution_ids.is_empty() {
            return Self::err("At least one execution_id must be provided", None::<&str>);
        }

        let timeout_secs = timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS);
        let url = self.url("/api/execution-processes/wait");

        let response: McpWaitExecutionResponse = match self
            .send_wait_execution_json(&url, &execution_ids, timeout_secs)
            .await
        {
            Ok(r) => r,
            Err(e) => return Ok(Self::tool_error(e)),
        };

        McpServer::success(&response)
    }
}

impl McpServer {
    async fn send_wait_execution_json(
        &self,
        url: &str,
        execution_ids: &[Uuid],
        timeout_secs: u64,
    ) -> Result<McpWaitExecutionResponse, ToolError> {
        let mut last_error = None;
        let started_at = Instant::now();
        let wait_budget = Duration::from_secs(timeout_secs);
        let http_budget = Duration::from_secs(timeout_secs.saturating_add(30));

        for attempt in 1..=WAIT_EXECUTION_HTTP_RETRIES {
            let remaining_timeout_secs = if attempt == 1 {
                timeout_secs
            } else if let Some(remaining) = remaining_wait_execution_secs(started_at, wait_budget) {
                remaining
            } else {
                return Err(last_error.unwrap_or_else(|| {
                    ToolError::message("wait_execution retry budget exhausted")
                }));
            };

            let http_timeout = remaining_wait_execution_duration(started_at, http_budget)
                .unwrap_or_else(|| Duration::from_secs(1));
            let payload = serde_json::json!({
                "execution_ids": execution_ids,
                "timeout_seconds": remaining_timeout_secs,
            });

            let response = self
                .client
                .post(url)
                .json(&payload)
                .timeout(http_timeout)
                .send()
                .await;

            let response = match response {
                Ok(response) => response,
                Err(error) if attempt < WAIT_EXECUTION_HTTP_RETRIES => {
                    last_error = Some(ToolError::new(
                        "Failed to connect to VK API",
                        Some(error.to_string()),
                    ));
                    tokio::time::sleep(wait_execution_retry_delay(attempt)).await;
                    continue;
                }
                Err(error) => {
                    return Err(ToolError::new(
                        "Failed to connect to VK API",
                        Some(error.to_string()),
                    ));
                }
            };

            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                let error = ToolError::new(
                    format!("VK API returned error status: {status}"),
                    (!body.is_empty()).then_some(body),
                );

                if status.is_server_error() && attempt < WAIT_EXECUTION_HTTP_RETRIES {
                    last_error = Some(error);
                    tokio::time::sleep(wait_execution_retry_delay(attempt)).await;
                    continue;
                }

                return Err(error);
            }

            let api_response = response
                .json::<ApiResponseEnvelope<McpWaitExecutionResponse>>()
                .await
                .map_err(|error| {
                    ToolError::new("Failed to parse VK API response", Some(error.to_string()))
                })?;

            if !api_response.success {
                let msg = api_response.message.as_deref().unwrap_or("Unknown error");
                return Err(ToolError::new("VK API returned error", Some(msg)));
            }

            return api_response
                .data
                .ok_or_else(|| ToolError::message("VK API response missing data field"));
        }

        Err(last_error.unwrap_or_else(|| ToolError::message("VK API request failed")))
    }
}

fn wait_execution_retry_delay(attempt: usize) -> Duration {
    Duration::from_millis(250 * attempt as u64)
}

fn remaining_wait_execution_secs(started_at: Instant, budget: Duration) -> Option<u64> {
    let remaining = remaining_wait_execution_duration(started_at, budget)?;
    let secs = remaining.as_secs();
    let rounded_up = if remaining.subsec_nanos() > 0 {
        secs.saturating_add(1)
    } else {
        secs
    };
    (rounded_up > 0).then_some(rounded_up)
}

fn remaining_wait_execution_duration(started_at: Instant, budget: Duration) -> Option<Duration> {
    let remaining = budget.checked_sub(started_at.elapsed())?;
    (!remaining.is_zero()).then_some(remaining)
}

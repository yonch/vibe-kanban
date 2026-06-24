use anyhow;
use axum::{
    Extension, Router,
    extract::{Path, Query, State, ws::Message},
    middleware::from_fn_with_state,
    response::{IntoResponse, Json as ResponseJson},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use db::models::{
    coding_agent_turn::CodingAgentTurn,
    execution_process::{ExecutionProcess, ExecutionProcessStatus},
    execution_process_repo_state::ExecutionProcessRepoState,
};
use deployment::Deployment;
use futures_util::{StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use services::services::container::ContainerService;
use sqlx::SqlitePool;
use utils::{log_msg::LogMsg, response::ApiResponse};
use uuid::Uuid;

use crate::{
    DeploymentImpl,
    error::ApiError,
    middleware::{
        load_execution_process_middleware,
        signed_ws::{MaybeSignedWebSocket, SignedWsUpgrade},
    },
};

#[derive(Debug, Deserialize)]
struct SessionExecutionProcessQuery {
    pub session_id: Uuid,
    /// If true, include soft-deleted (dropped) processes in results/stream
    #[serde(default)]
    pub show_soft_deleted: Option<bool>,
}

async fn get_execution_process_by_id(
    Extension(execution_process): Extension<ExecutionProcess>,
    State(_deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<ExecutionProcess>>, ApiError> {
    Ok(ResponseJson(ApiResponse::success(execution_process)))
}

#[derive(Debug, Serialize)]
struct ExecutionSummaryResponse {
    summary: Option<String>,
}

async fn get_execution_summary(
    Extension(execution_process): Extension<ExecutionProcess>,
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<ExecutionSummaryResponse>>, ApiError> {
    let pool = &deployment.db().pool;
    let summary = CodingAgentTurn::find_by_execution_process_id(pool, execution_process.id)
        .await?
        .and_then(|turn| turn.summary);
    Ok(ResponseJson(ApiResponse::success(
        ExecutionSummaryResponse { summary },
    )))
}

async fn stream_raw_logs_ws(
    ws: SignedWsUpgrade,
    State(deployment): State<DeploymentImpl>,
    Path(exec_id): Path<Uuid>,
) -> impl IntoResponse {
    // Always accept the WebSocket upgrade — handle "not found" inside the
    // connection by sending `finished` and closing cleanly, instead of
    // rejecting with HTTP 404 which the browser surfaces as an opaque
    // connection failure.
    ws.on_upgrade(move |socket| async move {
        if let Err(e) = handle_raw_logs_ws(socket, deployment, exec_id).await {
            tracing::warn!("raw logs WS closed: {}", e);
        }
    })
}

async fn handle_raw_logs_ws(
    mut socket: MaybeSignedWebSocket,
    deployment: DeploymentImpl,
    exec_id: Uuid,
) -> anyhow::Result<()> {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use executors::logs::utils::patch::ConversationPatch;
    use utils::log_msg::LogMsg;

    // Get the raw stream — if not found, send finished and close cleanly
    let raw_stream = match deployment.container().stream_raw_logs(&exec_id).await {
        Some(stream) => stream,
        None => {
            // No logs available: send finished so the client gets a clean
            // close instead of retrying endlessly.
            let _ = socket
                .send(LogMsg::Finished.to_ws_message_unchecked())
                .await;
            let _ = socket.close().await;
            return Ok(());
        }
    };

    let counter = Arc::new(AtomicUsize::new(0));
    let mut stream = raw_stream.map_ok({
        let counter = counter.clone();
        move |m| match m {
            LogMsg::Stdout(content) => {
                let index = counter.fetch_add(1, Ordering::SeqCst);
                let patch = ConversationPatch::add_stdout(index, content);
                LogMsg::JsonPatch(patch).to_ws_message_unchecked()
            }
            LogMsg::Stderr(content) => {
                let index = counter.fetch_add(1, Ordering::SeqCst);
                let patch = ConversationPatch::add_stderr(index, content);
                LogMsg::JsonPatch(patch).to_ws_message_unchecked()
            }
            LogMsg::Finished => LogMsg::Finished.to_ws_message_unchecked(),
            _ => unreachable!("Raw stream should only have Stdout/Stderr/Finished"),
        }
    });

    loop {
        tokio::select! {
            item = stream.next() => {
                match item {
                    Some(Ok(msg)) => {
                        if socket.send(msg).await.is_err() {
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        tracing::error!("stream error: {}", e);
                        break;
                    }
                    None => break,
                }
            }
            inbound = socket.recv() => {
                match inbound {
                    Ok(Some(Message::Close(_))) => break,
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        }
    }
    // Send a proper close frame so the client sees code 1000 (normal closure)
    // instead of an abnormal TCP drop that triggers reconnection attempts.
    let _ = socket.close().await;
    Ok(())
}

async fn stream_normalized_logs_ws(
    ws: SignedWsUpgrade,
    State(deployment): State<DeploymentImpl>,
    Path(exec_id): Path<Uuid>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| async move {
        let stream = deployment
            .container()
            .stream_normalized_logs(&exec_id)
            .await;

        match stream {
            Some(stream) => {
                let stream = stream.err_into::<anyhow::Error>().into_stream();
                if let Err(e) = handle_normalized_logs_ws(socket, stream).await {
                    tracing::warn!("normalized logs WS closed: {}", e);
                }
            }
            None => {
                // No logs available: send finished and close cleanly
                let mut socket = socket;
                let _ = socket
                    .send(utils::log_msg::LogMsg::Finished.to_ws_message_unchecked())
                    .await;
                let _ = socket.close().await;
            }
        }
    })
}

async fn handle_normalized_logs_ws(
    mut socket: MaybeSignedWebSocket,
    stream: impl futures_util::Stream<Item = anyhow::Result<LogMsg>> + Unpin + Send + 'static,
) -> anyhow::Result<()> {
    let mut stream = stream.map_ok(|msg| msg.to_ws_message_unchecked());
    loop {
        tokio::select! {
            item = stream.next() => {
                match item {
                    Some(Ok(msg)) => {
                        if socket.send(msg).await.is_err() {
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        tracing::error!("stream error: {}", e);
                        break;
                    }
                    None => break,
                }
            }
            inbound = socket.recv() => {
                match inbound {
                    Ok(Some(Message::Close(_))) => break,
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        }
    }
    let _ = socket.close().await;
    Ok(())
}

async fn stop_execution_process(
    Extension(execution_process): Extension<ExecutionProcess>,
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<()>>, ApiError> {
    deployment
        .container()
        .stop_execution(&execution_process, ExecutionProcessStatus::Killed)
        .await?;

    Ok(ResponseJson(ApiResponse::success(())))
}

async fn stream_execution_processes_by_session_ws(
    ws: SignedWsUpgrade,
    State(deployment): State<DeploymentImpl>,
    Query(query): Query<SessionExecutionProcessQuery>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| async move {
        if let Err(e) = handle_execution_processes_by_session_ws(
            socket,
            deployment,
            query.session_id,
            query.show_soft_deleted.unwrap_or(false),
        )
        .await
        {
            tracing::warn!("execution processes by session WS closed: {}", e);
        }
    })
}

async fn handle_execution_processes_by_session_ws(
    mut socket: MaybeSignedWebSocket,
    deployment: DeploymentImpl,
    session_id: uuid::Uuid,
    show_soft_deleted: bool,
) -> anyhow::Result<()> {
    // Get the raw stream and convert LogMsg to WebSocket messages
    let mut stream = deployment
        .events()
        .stream_execution_processes_for_session_raw(session_id, show_soft_deleted)
        .await?
        .map_ok(|msg| msg.to_ws_message_unchecked());

    loop {
        tokio::select! {
            item = stream.next() => {
                match item {
                    Some(Ok(msg)) => {
                        if socket.send(msg).await.is_err() {
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        tracing::error!("stream error: {}", e);
                        break;
                    }
                    None => break,
                }
            }
            inbound = socket.recv() => {
                match inbound {
                    Ok(Some(Message::Close(_))) => break,
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        }
    }
    Ok(())
}

async fn get_execution_process_repo_states(
    Extension(execution_process): Extension<ExecutionProcess>,
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<Vec<ExecutionProcessRepoState>>>, ApiError> {
    let pool = &deployment.db().pool;
    let repo_states =
        ExecutionProcessRepoState::find_by_execution_process_id(pool, execution_process.id).await?;
    Ok(ResponseJson(ApiResponse::success(repo_states)))
}

#[derive(Debug, Deserialize)]
pub struct WaitForExecutionsRequest {
    pub execution_ids: Vec<Uuid>,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
}

fn default_timeout_seconds() -> u64 {
    1800
}

#[derive(Debug, Serialize)]
pub struct WaitForExecutionsResponse {
    pub completed_execution_id: Uuid,
    pub session_id: Uuid,
    pub status: String,
    pub completed_at: Option<DateTime<Utc>>,
    pub output: Option<String>,
    pub accepted_by_agent: bool,
}

fn coding_agent_turn_accepted_by_agent(turn: Option<&CodingAgentTurn>) -> bool {
    turn.is_some_and(|turn| turn.agent_session_id.is_some())
}

async fn completed_wait_response(
    pool: &SqlitePool,
    id: Uuid,
) -> Result<Option<WaitForExecutionsResponse>, sqlx::Error> {
    let Some(ep) = ExecutionProcess::find_by_id(pool, id).await? else {
        return Ok(None);
    };

    if ep.status == ExecutionProcessStatus::Running {
        return Ok(None);
    }

    let status = match ep.status {
        ExecutionProcessStatus::Completed => "completed",
        ExecutionProcessStatus::Failed => "failed",
        ExecutionProcessStatus::Killed => "killed",
        ExecutionProcessStatus::Running => unreachable!(),
    };

    let turn = CodingAgentTurn::find_by_execution_process_id(pool, ep.id).await?;
    let output = turn.as_ref().and_then(|turn| turn.summary.clone());
    let accepted_by_agent = coding_agent_turn_accepted_by_agent(turn.as_ref());

    Ok(Some(WaitForExecutionsResponse {
        completed_execution_id: ep.id,
        session_id: ep.session_id,
        status: status.to_string(),
        completed_at: ep.completed_at,
        output,
        accepted_by_agent,
    }))
}

async fn timeout_wait_response(
    pool: &SqlitePool,
    first_id: Uuid,
) -> Result<WaitForExecutionsResponse, sqlx::Error> {
    let first_execution = ExecutionProcess::find_by_id(pool, first_id).await?;
    let session_id = first_execution
        .as_ref()
        .map(|ep| ep.session_id)
        .unwrap_or(first_id);
    let accepted_by_agent = match first_execution {
        Some(ep) => {
            let turn = CodingAgentTurn::find_by_execution_process_id(pool, ep.id).await?;
            coding_agent_turn_accepted_by_agent(turn.as_ref())
        }
        None => false,
    };

    Ok(WaitForExecutionsResponse {
        completed_execution_id: first_id,
        session_id,
        status: "timeout".to_string(),
        completed_at: None,
        output: None,
        accepted_by_agent,
    })
}

/// Long-poll endpoint: holds the connection open until any of the requested executions
/// reaches a terminal state (not running) or the timeout elapses.
async fn wait_for_executions(
    State(deployment): State<DeploymentImpl>,
    axum::Json(request): axum::Json<WaitForExecutionsRequest>,
) -> Result<ResponseJson<ApiResponse<WaitForExecutionsResponse>>, ApiError> {
    let pool = &deployment.db().pool;
    let response = wait_for_executions_with_pool(pool, request).await?;
    Ok(ResponseJson(ApiResponse::success(response)))
}

async fn wait_for_executions_with_pool(
    pool: &SqlitePool,
    request: WaitForExecutionsRequest,
) -> Result<WaitForExecutionsResponse, ApiError> {
    use std::time::Duration;

    if request.execution_ids.is_empty() {
        return Err(ApiError::BadRequest(
            "execution_ids must not be empty".to_string(),
        ));
    }

    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(request.timeout_seconds.min(3600));
    let poll_interval = Duration::from_millis(500);

    loop {
        let mut poll_error = None;

        for id in &request.execution_ids {
            match completed_wait_response(pool, *id).await {
                Ok(Some(response)) => return Ok(response),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        execution_id = %id,
                        error = ?error,
                        "wait_for_executions poll failed; retrying until deadline"
                    );
                    poll_error = Some(error);
                }
            }
        }

        if tokio::time::Instant::now() + poll_interval > deadline {
            if let Some(error) = poll_error {
                tracing::warn!(
                    error = ?error,
                    "wait_for_executions deadline reached after database errors"
                );
                return Err(ApiError::ServiceUnavailable(
                    "Execution status is temporarily unavailable. Please retry.".to_string(),
                ));
            }

            let first_id = request.execution_ids[0];
            return match timeout_wait_response(pool, first_id).await {
                Ok(response) => Ok(response),
                Err(error) => {
                    tracing::warn!(
                        error = ?error,
                        "wait_for_executions deadline reached after database errors"
                    );
                    Err(ApiError::ServiceUnavailable(
                        "Execution status is temporarily unavailable. Please retry.".to_string(),
                    ))
                }
            };
        }

        tokio::time::sleep(poll_interval).await;
    }
}

pub(super) fn router(deployment: &DeploymentImpl) -> Router<DeploymentImpl> {
    let workspace_id_router = Router::new()
        .route("/", get(get_execution_process_by_id))
        .route("/stop", post(stop_execution_process))
        .route("/repo-states", get(get_execution_process_repo_states))
        .route("/summary", get(get_execution_summary))
        .route("/raw-logs/ws", get(stream_raw_logs_ws))
        .route("/normalized-logs/ws", get(stream_normalized_logs_ws))
        .layer(from_fn_with_state(
            deployment.clone(),
            load_execution_process_middleware,
        ));

    let workspaces_router = Router::new()
        .route("/wait", post(wait_for_executions))
        .route(
            "/stream/session/ws",
            get(stream_execution_processes_by_session_ws),
        )
        .nest("/{id}", workspace_id_router);

    Router::new().nest("/execution-processes", workspaces_router)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use db::models::coding_agent_turn::CodingAgentTurn;
    use sqlx::sqlite::SqlitePoolOptions;
    use uuid::Uuid;

    use super::{
        WaitForExecutionsRequest, coding_agent_turn_accepted_by_agent,
        wait_for_executions_with_pool,
    };
    use crate::error::ApiError;

    fn turn(agent_session_id: Option<&str>) -> CodingAgentTurn {
        CodingAgentTurn {
            id: Uuid::new_v4(),
            execution_process_id: Uuid::new_v4(),
            agent_session_id: agent_session_id.map(str::to_string),
            agent_message_id: None,
            prompt: None,
            summary: None,
            seen: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn accepted_by_agent_requires_agent_session_id() {
        let accepted = turn(Some("agent-session"));
        let not_accepted = turn(None);

        assert!(coding_agent_turn_accepted_by_agent(Some(&accepted)));
        assert!(!coding_agent_turn_accepted_by_agent(Some(&not_accepted)));
        assert!(!coding_agent_turn_accepted_by_agent(None));
    }

    #[tokio::test]
    async fn wait_for_executions_returns_unavailable_after_poll_errors_reach_deadline() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        pool.close().await;

        let result = wait_for_executions_with_pool(
            &pool,
            WaitForExecutionsRequest {
                execution_ids: vec![Uuid::new_v4()],
                timeout_seconds: 0,
            },
        )
        .await;

        assert!(matches!(
            result,
            Err(ApiError::ServiceUnavailable(message))
                if message == "Execution status is temporarily unavailable. Please retry."
        ));
    }
}

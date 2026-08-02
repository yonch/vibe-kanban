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
use tokio::sync::broadcast;
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
    // Subscribe before taking the initial snapshot so a completion committed
    // during the query is queued for us instead of being missed.
    let updates = deployment.events().msg_store().get_receiver();
    let response = wait_for_executions_with_pool(pool, updates, request).await?;
    Ok(ResponseJson(ApiResponse::success(response)))
}

async fn find_completed_execution(
    pool: &SqlitePool,
    execution_ids: &[Uuid],
) -> Result<Option<WaitForExecutionsResponse>, sqlx::Error> {
    for id in execution_ids {
        if let Some(response) = completed_wait_response(pool, *id).await? {
            return Ok(Some(response));
        }
    }

    Ok(None)
}

fn is_requested_execution_update(msg: &LogMsg, execution_ids: &[Uuid]) -> bool {
    let LogMsg::JsonPatch(patch) = msg else {
        return false;
    };

    patch.0.iter().any(|operation| {
        operation
            .path()
            .strip_prefix("/execution_processes/")
            .is_some_and(|updated_id| {
                execution_ids
                    .iter()
                    .any(|execution_id| updated_id == execution_id.to_string())
            })
    })
}

async fn wait_for_executions_with_pool(
    pool: &SqlitePool,
    mut updates: broadcast::Receiver<LogMsg>,
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

    match find_completed_execution(pool, &request.execution_ids).await {
        Ok(Some(response)) => return Ok(response),
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(
                error = ?error,
                "wait_for_executions initial snapshot failed; waiting for an update"
            );
        }
    }

    loop {
        match tokio::time::timeout_at(deadline, updates.recv()).await {
            Ok(Ok(msg)) if !is_requested_execution_update(&msg, &request.execution_ids) => {}
            Ok(Ok(_)) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                // A matching update may be non-terminal. A lagged receiver may
                // have lost the terminal update, so both cases reconcile once.
                match find_completed_execution(pool, &request.execution_ids).await {
                    Ok(Some(response)) => return Ok(response),
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(
                            error = ?error,
                            "wait_for_executions update reconciliation failed"
                        );
                    }
                }
            }
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                tracing::warn!("wait_for_executions event stream closed before the deadline");
                return Err(ApiError::ServiceUnavailable(
                    "Execution status is temporarily unavailable. Please retry.".to_string(),
                ));
            }
            Err(_) => {
                // Reconcile at the deadline in case an update hook failed or a
                // notification was otherwise lost.
                match find_completed_execution(pool, &request.execution_ids).await {
                    Ok(Some(response)) => return Ok(response),
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(
                            error = ?error,
                            "wait_for_executions deadline reconciliation failed"
                        );
                        return Err(ApiError::ServiceUnavailable(
                            "Execution status is temporarily unavailable. Please retry."
                                .to_string(),
                        ));
                    }
                }

                return match timeout_wait_response(pool, request.execution_ids[0]).await {
                    Ok(response) => Ok(response),
                    Err(error) => {
                        tracing::warn!(
                            error = ?error,
                            "wait_for_executions timeout response failed"
                        );
                        Err(ApiError::ServiceUnavailable(
                            "Execution status is temporarily unavailable. Please retry."
                                .to_string(),
                        ))
                    }
                };
            }
        }
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
    use std::{sync::Arc, time::Duration};

    use chrono::Utc;
    use db::models::coding_agent_turn::CodingAgentTurn;
    use sqlx::sqlite::SqlitePoolOptions;
    use utils::{log_msg::LogMsg, msg_store::MsgStore};
    use uuid::Uuid;

    use super::{
        WaitForExecutionsRequest, coding_agent_turn_accepted_by_agent,
        is_requested_execution_update, wait_for_executions_with_pool,
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

    #[test]
    fn execution_update_filter_only_matches_requested_processes() {
        let requested_id = Uuid::new_v4();
        let other_id = Uuid::new_v4();
        let requested_patch = serde_json::from_value(serde_json::json!([{
            "op": "replace",
            "path": format!("/execution_processes/{requested_id}"),
            "value": {}
        }]))
        .unwrap();
        let other_patch = serde_json::from_value(serde_json::json!([{
            "op": "replace",
            "path": format!("/execution_processes/{other_id}"),
            "value": {}
        }]))
        .unwrap();

        assert!(is_requested_execution_update(
            &LogMsg::JsonPatch(requested_patch),
            &[requested_id]
        ));
        assert!(!is_requested_execution_update(
            &LogMsg::JsonPatch(other_patch),
            &[requested_id]
        ));
        assert!(!is_requested_execution_update(
            &LogMsg::Ready,
            &[requested_id]
        ));
    }

    #[tokio::test]
    async fn wait_for_executions_wakes_on_matching_update() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE execution_processes (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                run_reason TEXT NOT NULL,
                executor_action TEXT NOT NULL,
                status TEXT NOT NULL,
                exit_code INTEGER,
                dropped INTEGER NOT NULL DEFAULT 0,
                started_at TEXT NOT NULL,
                completed_at TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE coding_agent_turns (
                id TEXT PRIMARY KEY,
                execution_process_id TEXT NOT NULL,
                agent_session_id TEXT,
                agent_message_id TEXT,
                prompt TEXT,
                summary TEXT,
                seen INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        let execution_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO execution_processes (
                id, session_id, run_reason, executor_action, status,
                started_at, created_at, updated_at
            ) VALUES (?, ?, 'codingagent', '{}', 'running', ?, ?, ?)",
        )
        .bind(execution_id)
        .bind(session_id)
        .bind(now)
        .bind(now)
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();

        let msg_store = Arc::new(MsgStore::new());
        let wait = tokio::spawn({
            let pool = pool.clone();
            let updates = msg_store.get_receiver();
            async move {
                wait_for_executions_with_pool(
                    &pool,
                    updates,
                    WaitForExecutionsRequest {
                        execution_ids: vec![execution_id],
                        timeout_seconds: 60,
                    },
                )
                .await
            }
        });

        sqlx::query(
            "UPDATE execution_processes
             SET status = 'completed', exit_code = 0, completed_at = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(now)
        .bind(now)
        .bind(execution_id)
        .execute(&pool)
        .await
        .unwrap();
        let patch = serde_json::from_value(serde_json::json!([{
            "op": "replace",
            "path": format!("/execution_processes/{execution_id}"),
            "value": {}
        }]))
        .unwrap();
        msg_store.push_patch(patch);

        let response = tokio::time::timeout(Duration::from_secs(1), wait)
            .await
            .expect("wait should wake on the execution update")
            .unwrap()
            .unwrap();
        assert_eq!(response.completed_execution_id, execution_id);
        assert_eq!(response.session_id, session_id);
        assert_eq!(response.status, "completed");
    }

    #[tokio::test]
    async fn wait_for_executions_returns_unavailable_after_database_errors_reach_deadline() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        pool.close().await;
        let msg_store = MsgStore::new();

        let result = wait_for_executions_with_pool(
            &pool,
            msg_store.get_receiver(),
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

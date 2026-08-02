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
    // Subscribe before taking the initial snapshot so a completion committed
    // during the query is queued for us instead of being missed.
    let updates = deployment.db().subscribe_execution_completions();
    let response = wait_for_executions_with_pool(&deployment.db().pool, updates, request).await?;
    Ok(ResponseJson(ApiResponse::success(response)))
}

async fn find_completed_execution(
    pool: &SqlitePool,
    execution_ids: &[Uuid],
) -> Result<Option<WaitForExecutionsResponse>, sqlx::Error> {
    let mut last_error = None;

    for id in execution_ids {
        match completed_wait_response(pool, *id).await {
            Ok(Some(response)) => return Ok(Some(response)),
            Ok(None) => {}
            Err(error) => last_error = Some(error),
        }
    }

    match last_error {
        Some(error) => Err(error),
        None => Ok(None),
    }
}

fn schedule_database_retry(
    retry_at: &mut Option<tokio::time::Instant>,
    retry_delay: &mut std::time::Duration,
) {
    *retry_at = Some(tokio::time::Instant::now() + *retry_delay);
    *retry_delay = (*retry_delay * 2).min(std::time::Duration::from_secs(5));
}

async fn wait_for_executions_with_pool(
    pool: &SqlitePool,
    mut completions: broadcast::Receiver<Uuid>,
    request: WaitForExecutionsRequest,
) -> Result<WaitForExecutionsResponse, ApiError> {
    use std::{future::pending, time::Duration};

    if request.execution_ids.is_empty() {
        return Err(ApiError::BadRequest(
            "execution_ids must not be empty".to_string(),
        ));
    }

    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(request.timeout_seconds.min(3600));

    let mut retry_at = None;
    let mut retry_delay = Duration::from_millis(100);
    match find_completed_execution(pool, &request.execution_ids).await {
        Ok(Some(response)) => return Ok(response),
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(
                error = ?error,
                "wait_for_executions initial snapshot failed; scheduling retry"
            );
            schedule_database_retry(&mut retry_at, &mut retry_delay);
        }
    }

    loop {
        let retry = async {
            match retry_at {
                Some(retry_at) => tokio::time::sleep_until(retry_at).await,
                None => pending().await,
            }
        };

        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
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

                return timeout_wait_response(pool, request.execution_ids[0])
                    .await
                    .map_err(|error| {
                        tracing::warn!(
                            error = ?error,
                            "wait_for_executions timeout response failed"
                        );
                        ApiError::ServiceUnavailable(
                            "Execution status is temporarily unavailable. Please retry."
                                .to_string(),
                        )
                    });
            }
            result = completions.recv() => match result {
                Ok(execution_id) if !request.execution_ids.contains(&execution_id) => {}
                Ok(_) => {
                    // Completion notifications are sent after the terminal
                    // update commits, so this read normally succeeds once.
                    match find_completed_execution(pool, &request.execution_ids).await {
                        Ok(Some(response)) => return Ok(response),
                        Ok(None) => {
                            tracing::warn!(
                                "wait_for_executions completion was not visible; scheduling retry"
                            );
                            schedule_database_retry(&mut retry_at, &mut retry_delay);
                        }
                        Err(error) => {
                            tracing::warn!(
                                error = ?error,
                                "wait_for_executions completion reconciliation failed; scheduling retry"
                            );
                            schedule_database_retry(&mut retry_at, &mut retry_delay);
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Reconcile once because a requested completion may have
                    // been among the dropped notifications.
                    match find_completed_execution(pool, &request.execution_ids).await {
                        Ok(Some(response)) => return Ok(response),
                        Ok(None) => {}
                        Err(error) => {
                            tracing::warn!(
                                error = ?error,
                                "wait_for_executions lag reconciliation failed; scheduling retry"
                            );
                            schedule_database_retry(&mut retry_at, &mut retry_delay);
                        }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => {
                    tracing::warn!("wait_for_executions completion stream closed before the deadline");
                    return Err(ApiError::ServiceUnavailable(
                        "Execution status is temporarily unavailable. Please retry.".to_string(),
                    ));
                }
            },
            _ = retry => {
                retry_at = None;
                match find_completed_execution(pool, &request.execution_ids).await {
                    Ok(Some(response)) => return Ok(response),
                    Ok(None) => {
                        // The database recovered. Return to notification-only
                        // waiting without scheduling another read.
                        retry_delay = Duration::from_millis(100);
                    }
                    Err(error) => {
                        tracing::warn!(
                            error = ?error,
                            "wait_for_executions database retry failed"
                        );
                        schedule_database_retry(&mut retry_at, &mut retry_delay);
                    }
                }
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
    use std::time::Duration;

    use chrono::Utc;
    use db::models::coding_agent_turn::CodingAgentTurn;
    use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
    use tokio::sync::broadcast;
    use uuid::Uuid;

    use super::{
        WaitForExecutionsRequest, coding_agent_turn_accepted_by_agent, find_completed_execution,
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

    async fn create_wait_test_schema(pool: &SqlitePool) {
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
        .execute(pool)
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
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_execution(
        pool: &SqlitePool,
        execution_id: Uuid,
        session_id: Uuid,
        status: &str,
    ) {
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO execution_processes (
                id, session_id, run_reason, executor_action, status,
                started_at, completed_at, created_at, updated_at
            ) VALUES (?, ?, 'codingagent', '{}', ?, ?, ?, ?, ?)",
        )
        .bind(execution_id)
        .bind(session_id)
        .bind(status)
        .bind(now)
        .bind((status != "running").then_some(now))
        .bind(now)
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn wait_for_executions_wakes_on_post_commit_completion() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        create_wait_test_schema(&pool).await;

        let execution_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let now = Utc::now();
        insert_execution(&pool, execution_id, session_id, "running").await;

        let (completion_tx, completion_rx) = broadcast::channel(16);
        let wait = tokio::spawn({
            let pool = pool.clone();
            async move {
                wait_for_executions_with_pool(
                    &pool,
                    completion_rx,
                    WaitForExecutionsRequest {
                        execution_ids: vec![execution_id],
                        timeout_seconds: 60,
                    },
                )
                .await
            }
        });
        tokio::task::yield_now().await;
        assert!(!wait.is_finished());

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
        completion_tx.send(execution_id).unwrap();

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
    async fn completed_execution_is_returned_after_an_unreadable_execution() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        create_wait_test_schema(&pool).await;

        let unreadable_id = Uuid::new_v4();
        let completed_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        insert_execution(&pool, unreadable_id, session_id, "invalid-status").await;
        insert_execution(&pool, completed_id, session_id, "completed").await;

        let response = find_completed_execution(&pool, &[unreadable_id, completed_id])
            .await
            .expect("a later healthy completion should take precedence over a row error")
            .expect("the completed execution should be returned");

        assert_eq!(response.completed_execution_id, completed_id);
        assert_eq!(response.status, "completed");
    }

    #[tokio::test]
    async fn healthy_wait_does_not_reconcile_without_notification() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        create_wait_test_schema(&pool).await;

        let execution_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        insert_execution(&pool, execution_id, session_id, "running").await;

        let (completion_tx, completion_rx) = broadcast::channel(16);
        let wait = tokio::spawn({
            let pool = pool.clone();
            async move {
                wait_for_executions_with_pool(
                    &pool,
                    completion_rx,
                    WaitForExecutionsRequest {
                        execution_ids: vec![execution_id],
                        timeout_seconds: 60,
                    },
                )
                .await
            }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!wait.is_finished());

        let now = Utc::now();
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

        // The former implementation polled every 500 ms. Remaining pending
        // beyond that interval proves a healthy wait needs a notification.
        tokio::time::sleep(Duration::from_millis(750)).await;
        assert!(
            !wait.is_finished(),
            "healthy wait must not discover completion through periodic reconciliation"
        );

        completion_tx.send(execution_id).unwrap();
        let response = wait.await.unwrap().unwrap();
        assert_eq!(response.completed_execution_id, execution_id);
        assert_eq!(response.status, "completed");
    }

    #[tokio::test]
    async fn wait_for_executions_retries_only_after_database_error() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let execution_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let (_completion_tx, completion_rx) = broadcast::channel(16);
        let wait = tokio::spawn({
            let pool = pool.clone();
            async move {
                wait_for_executions_with_pool(
                    &pool,
                    completion_rx,
                    WaitForExecutionsRequest {
                        execution_ids: vec![execution_id],
                        timeout_seconds: 60,
                    },
                )
                .await
            }
        });

        tokio::time::sleep(Duration::from_millis(25)).await;
        create_wait_test_schema(&pool).await;
        insert_execution(&pool, execution_id, session_id, "completed").await;

        let response = tokio::time::timeout(Duration::from_secs(1), wait)
            .await
            .expect("database-error retry should observe the completion")
            .unwrap()
            .unwrap();
        assert_eq!(response.completed_execution_id, execution_id);
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
        let (_completion_tx, completion_rx) = broadcast::channel(16);

        let result = wait_for_executions_with_pool(
            &pool,
            completion_rx,
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

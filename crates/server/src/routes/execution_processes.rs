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
    execution_process::{ExecutionProcess, ExecutionProcessError, ExecutionProcessStatus},
    execution_process_repo_state::ExecutionProcessRepoState,
};
use deployment::Deployment;
use futures_util::{StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use services::services::container::ContainerService;
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

async fn stream_raw_logs_ws(
    ws: SignedWsUpgrade,
    State(deployment): State<DeploymentImpl>,
    Path(exec_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    // Check if the stream exists before upgrading the WebSocket
    let _stream = deployment
        .container()
        .stream_raw_logs(&exec_id)
        .await
        .ok_or_else(|| {
            ApiError::ExecutionProcess(ExecutionProcessError::ExecutionProcessNotFound)
        })?;

    Ok(ws.on_upgrade(move |socket| async move {
        if let Err(e) = handle_raw_logs_ws(socket, deployment, exec_id).await {
            tracing::warn!("raw logs WS closed: {}", e);
        }
    }))
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

    // Get the raw stream and convert to JSON patches on-the-fly
    let raw_stream = deployment
        .container()
        .stream_raw_logs(&exec_id)
        .await
        .ok_or_else(|| anyhow::anyhow!("Execution process not found"))?;

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
    Ok(())
}

async fn stream_normalized_logs_ws(
    ws: SignedWsUpgrade,
    State(deployment): State<DeploymentImpl>,
    Path(exec_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let stream = deployment
        .container()
        .stream_normalized_logs(&exec_id)
        .await
        .ok_or_else(|| {
            ApiError::ExecutionProcess(ExecutionProcessError::ExecutionProcessNotFound)
        })?;

    // Convert the error type to anyhow::Error and turn TryStream -> Stream<Result<_, _>>
    let stream = stream.err_into::<anyhow::Error>().into_stream();

    Ok(ws.on_upgrade(move |socket| async move {
        if let Err(e) = handle_normalized_logs_ws(socket, stream, exec_id).await {
            tracing::warn!(
                execution_process_id = %exec_id,
                "normalized logs WS closed: {}", e
            );
        }
    }))
}

async fn handle_normalized_logs_ws(
    mut socket: MaybeSignedWebSocket,
    stream: impl futures_util::Stream<Item = anyhow::Result<LogMsg>> + Unpin + Send + 'static,
    exec_id: Uuid,
) -> anyhow::Result<()> {
    let mut stream = stream.map_ok(|msg| msg.to_ws_message_unchecked());
    let mut sent_finished = false;
    loop {
        tokio::select! {
            item = stream.next() => {
                match item {
                    Some(Ok(msg)) => {
                        // Track if the last message we're sending is Finished
                        if let Message::Text(ref text) = msg {
                            if text.contains("\"finished\"") {
                                sent_finished = true;
                            }
                        }
                        if socket.send(msg).await.is_err() {
                            tracing::warn!(
                                execution_process_id = %exec_id,
                                "normalized logs WS: client disconnected during send"
                            );
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        tracing::error!(
                            execution_process_id = %exec_id,
                            "normalized logs stream error: {}", e
                        );
                        break;
                    }
                    None => break,
                }
            }
            inbound = socket.recv() => {
                match inbound {
                    Ok(Some(Message::Close(_))) => {
                        tracing::debug!(
                            execution_process_id = %exec_id,
                            "normalized logs WS: client sent Close frame"
                        );
                        break;
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(
                            execution_process_id = %exec_id,
                            "normalized logs WS: inbound error (likely network drop): {}", e
                        );
                        break;
                    }
                }
            }
        }
    }
    if !sent_finished {
        tracing::warn!(
            execution_process_id = %exec_id,
            "normalized logs WS: stream ended without sending Finished"
        );
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
}

/// Long-poll endpoint: holds the connection open until any of the requested executions
/// reaches a terminal state (not running) or the timeout elapses.
async fn wait_for_executions(
    State(deployment): State<DeploymentImpl>,
    axum::Json(request): axum::Json<WaitForExecutionsRequest>,
) -> Result<ResponseJson<ApiResponse<WaitForExecutionsResponse>>, ApiError> {
    use std::time::Duration;

    if request.execution_ids.is_empty() {
        return Err(ApiError::BadRequest(
            "execution_ids must not be empty".to_string(),
        ));
    }

    let pool = &deployment.db().pool;
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(request.timeout_seconds.min(3600));
    let poll_interval = Duration::from_millis(500);

    loop {
        for id in &request.execution_ids {
            if let Some(ep) = ExecutionProcess::find_by_id(pool, *id).await? {
                if ep.status != ExecutionProcessStatus::Running {
                    let status = match ep.status {
                        ExecutionProcessStatus::Completed => "completed",
                        ExecutionProcessStatus::Failed => "failed",
                        ExecutionProcessStatus::Killed => "killed",
                        ExecutionProcessStatus::Running => unreachable!(),
                    };

                    return Ok(ResponseJson(ApiResponse::success(
                        WaitForExecutionsResponse {
                            completed_execution_id: ep.id,
                            session_id: ep.session_id,
                            status: status.to_string(),
                            completed_at: ep.completed_at,
                        },
                    )));
                }
            }
        }

        if tokio::time::Instant::now() + poll_interval > deadline {
            let first_id = request.execution_ids[0];
            let session_id = ExecutionProcess::find_by_id(pool, first_id)
                .await?
                .map(|ep| ep.session_id)
                .unwrap_or(first_id);

            return Ok(ResponseJson(ApiResponse::success(
                WaitForExecutionsResponse {
                    completed_execution_id: first_id,
                    session_id,
                    status: "timeout".to_string(),
                    completed_at: None,
                },
            )));
        }

        tokio::time::sleep(poll_interval).await;
    }
}

pub(super) fn router(deployment: &DeploymentImpl) -> Router<DeploymentImpl> {
    let workspace_id_router = Router::new()
        .route("/", get(get_execution_process_by_id))
        .route("/stop", post(stop_execution_process))
        .route("/repo-states", get(get_execution_process_repo_states))
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

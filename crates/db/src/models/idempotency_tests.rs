use std::str::FromStr;

use executors::{
    actions::{
        ExecutorAction, ExecutorActionType, coding_agent_initial::CodingAgentInitialRequest,
    },
    executors::BaseCodingAgent,
    profile::ExecutorConfig,
};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use uuid::Uuid;

use super::{
    execution_process::{CreateExecutionProcess, ExecutionProcess, ExecutionProcessRunReason},
    session::{CreateSession, Session},
    workspace::{CreateWorkspace, Workspace},
};

async fn test_pool() -> SqlitePool {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Memory);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    crate::run_migrations(&pool).await.unwrap();
    pool
}

async fn create_workspace(pool: &SqlitePool, key: Option<&str>) -> Workspace {
    Workspace::create(
        pool,
        &CreateWorkspace {
            branch: format!("workspace/{}", Uuid::new_v4()),
            name: Some("Test workspace".to_string()),
            idempotency_key: key.map(str::to_string),
        },
        Uuid::new_v4(),
    )
    .await
    .unwrap()
}

async fn create_session(pool: &SqlitePool, workspace_id: Uuid, key: Option<&str>) -> Session {
    Session::create(
        pool,
        &CreateSession {
            executor: Some("CODEX".to_string()),
            name: Some("Test session".to_string()),
            idempotency_key: key.map(str::to_string),
        },
        Uuid::new_v4(),
        workspace_id,
    )
    .await
    .unwrap()
}

fn coding_agent_action(prompt: &str) -> ExecutorAction {
    ExecutorAction::new(
        ExecutorActionType::CodingAgentInitialRequest(CodingAgentInitialRequest {
            prompt: prompt.to_string(),
            executor_config: ExecutorConfig::new(BaseCodingAgent::Codex),
            working_dir: None,
        }),
        None,
    )
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_idempotency_key_finds_existing_row_after_duplicate_insert() {
    let pool = test_pool().await;
    let workspace = create_workspace(&pool, Some("workspace-key")).await;

    let duplicate = Workspace::create(
        &pool,
        &CreateWorkspace {
            branch: "workspace/duplicate".to_string(),
            name: Some("Duplicate".to_string()),
            idempotency_key: Some("workspace-key".to_string()),
        },
        Uuid::new_v4(),
    )
    .await;

    assert!(duplicate.is_err());
    let replay = Workspace::find_by_idempotency_key(&pool, "workspace-key")
        .await
        .unwrap()
        .expect("idempotency key should find original workspace");
    assert_eq!(replay.id, workspace.id);
}

#[tokio::test(flavor = "current_thread")]
async fn session_idempotency_key_is_scoped_to_workspace() {
    let pool = test_pool().await;
    let first_workspace = create_workspace(&pool, None).await;
    let second_workspace = create_workspace(&pool, None).await;
    let first_session = create_session(&pool, first_workspace.id, Some("session-key")).await;
    let second_session = create_session(&pool, second_workspace.id, Some("session-key")).await;

    let duplicate = Session::create(
        &pool,
        &CreateSession {
            executor: Some("CODEX".to_string()),
            name: Some("Duplicate".to_string()),
            idempotency_key: Some("session-key".to_string()),
        },
        Uuid::new_v4(),
        first_workspace.id,
    )
    .await;

    assert!(duplicate.is_err());
    let replay =
        Session::find_by_workspace_and_idempotency_key(&pool, first_workspace.id, "session-key")
            .await
            .unwrap()
            .expect("idempotency key should find original session in workspace");
    assert_eq!(replay.id, first_session.id);

    let scoped_replay =
        Session::find_by_workspace_and_idempotency_key(&pool, second_workspace.id, "session-key")
            .await
            .unwrap()
            .expect("same key should be usable in a different workspace");
    assert_eq!(scoped_replay.id, second_session.id);
}

#[tokio::test(flavor = "current_thread")]
async fn execution_idempotency_key_finds_existing_row_after_duplicate_insert() {
    let pool = test_pool().await;
    let workspace = create_workspace(&pool, None).await;
    let session = create_session(&pool, workspace.id, None).await;
    let create = CreateExecutionProcess {
        session_id: session.id,
        executor_action: coding_agent_action("first prompt"),
        run_reason: ExecutionProcessRunReason::CodingAgent,
        idempotency_key: Some("execution-key".to_string()),
    };
    let execution = ExecutionProcess::create(&pool, &create, Uuid::new_v4(), &[])
        .await
        .unwrap();

    let duplicate = ExecutionProcess::create(&pool, &create, Uuid::new_v4(), &[]).await;

    assert!(duplicate.is_err());
    let replay =
        ExecutionProcess::find_by_session_and_idempotency_key(&pool, session.id, "execution-key")
            .await
            .unwrap()
            .expect("idempotency key should find original execution");
    assert_eq!(replay.id, execution.id);
}

#[tokio::test(flavor = "current_thread")]
async fn null_idempotency_keys_do_not_collide() {
    let pool = test_pool().await;
    create_workspace(&pool, None).await;
    create_workspace(&pool, None).await;

    let workspace = create_workspace(&pool, None).await;
    create_session(&pool, workspace.id, None).await;
    create_session(&pool, workspace.id, None).await;
}

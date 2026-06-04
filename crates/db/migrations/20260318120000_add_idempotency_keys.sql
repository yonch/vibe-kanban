ALTER TABLE workspaces ADD COLUMN idempotency_key TEXT;
CREATE UNIQUE INDEX idx_workspaces_idempotency_key
ON workspaces(idempotency_key)
WHERE idempotency_key IS NOT NULL;

ALTER TABLE sessions ADD COLUMN idempotency_key TEXT;
CREATE UNIQUE INDEX idx_sessions_workspace_idempotency_key
ON sessions(workspace_id, idempotency_key)
WHERE idempotency_key IS NOT NULL;

ALTER TABLE execution_processes ADD COLUMN idempotency_key TEXT;
CREATE UNIQUE INDEX idx_execution_processes_session_idempotency_key
ON execution_processes(session_id, idempotency_key)
WHERE idempotency_key IS NOT NULL;

# Workspace Frontend-Backend Communication

This document describes how the Vibe Kanban frontend and backend communicate to display workspace conversations (chat entries, tool calls, agent output) and diffs (file changes in the worktree).

## Transport: WebSocket with JSON Patch

All real-time data flows over **WebSockets**. There is no long-polling or Server-Sent Events path for workspace UI state. The backend upgrades HTTP connections to WebSocket via `SignedWsUpgrade` (an Axum extractor that validates an HMAC-signed handshake).

Every WebSocket stream speaks the same wire protocol — a sequence of JSON text frames carrying one of three message types, defined by the `LogMsg` Rust enum (`crates/utils/src/log_msg.rs`):

| Message | JSON shape | Purpose |
|---------|-----------|---------|
| `JsonPatch` | `{"JsonPatch": [...]}` | An array of RFC 6902 JSON Patch operations (`add`, `replace`, `remove`) applied against the client-side state tree. |
| `Ready` | `{"Ready": true}` | Signals that the initial snapshot has been fully sent. The frontend uses this to flip `isInitialized` and start rendering. |
| `Finished` | `{"finished": true}` | The stream is done (process exited, data exhausted). The frontend closes the socket with code 1000 and does **not** reconnect. |

REST endpoints exist alongside these streams, but only for actions (creating sessions, pushing branches, starting workspaces) and for supplementary metadata (workspace summaries, queue status). The live data that drives the conversation and diff views is entirely WebSocket-based.

## WebSocket Endpoints

Six WebSocket endpoints serve the workspace UI. Each is mounted under `/api/` in the Axum router.

### 1. Execution processes for a session
`/api/execution-processes/stream/session/ws?session_id=<uuid>`
(`crates/server/src/routes/execution_processes.rs`)

Streams the set of execution processes (coding agent runs, setup scripts, follow-ups) belonging to a session. The server sends an initial `replace /execution_processes` patch containing a map keyed by process ID, then live `add`/`replace`/`remove` patches as processes start, update status, or get soft-deleted.

### 2. Normalized logs for one execution process
`/api/execution-processes/<id>/normalized-logs/ws`

Streams the conversation entries for a single execution process: user messages, assistant messages, tool use/result pairs, thinking blocks, etc. Each entry arrives as a `JsonPatch` adding to `/entries/<index>`. The frontend consumes these to build the chat timeline.

### 3. Raw logs for one execution process
`/api/execution-processes/<id>/raw-logs/ws`

Streams stdout/stderr output for script-type processes (setup, cleanup, archive scripts). The patch values carry `STDOUT` and `STDERR` typed entries instead of normalized conversation entries.

### 4. Workspace diff
`/api/workspaces/<id>/git/diff/ws?stats_only=<bool>`
(`crates/server/src/routes/workspaces/streams.rs`)

Streams the live git diff of the workspace's worktree against its base commit. Covered in detail in the "Diff communication" section below.

### 5. Workspaces list
`/api/workspaces/streams/ws?archived=<bool>&limit=<n>`

Streams the full set of workspaces (active or archived), with live patches as workspaces are created, renamed, archived, or deleted.

### 6. Scratch stream
`/api/scratch/<scratchType>/<id>/stream/ws`

Streams small per-workspace key-value state (e.g. editor scratch pads).

## How the Conversation Is Assembled

The conversation view is built from multiple WebSocket connections composed together.

### Step 1 — Execution process list

When a session is selected, `useExecutionProcesses` (`packages/web-core/src/shared/hooks/useExecutionProcesses.ts`) opens a single WebSocket to endpoint (1). This provides the list of all execution processes for the session — their IDs, statuses, creation times, and run reasons.

The server sends the full process map as the initial `replace` patch, then incremental updates. The hook sorts processes by `created_at` and filters to the current `session_id` to guard against stale buffered data when switching sessions quickly.

### Step 2 — Conversation history (most-recent-first loading)

`useConversationHistory` (`packages/web-core/src/features/workspace-chat/model/hooks/useConversationHistory.ts`) takes the execution process list and loads actual conversation entries by opening **per-process WebSocket connections** to endpoints (2) or (3).

History is loaded **most-recent-first**. The hook iterates the process list in reverse chronological order:

1. **Initial batch**: Load the most recent completed processes until at least `MIN_INITIAL_ENTRIES` (10) conversation entries are accumulated. Each historic process opens a short-lived WebSocket to its normalized-logs endpoint, collects all patches until the `Finished` message, and closes.

2. **Background batches**: After the initial batch renders, remaining older processes are loaded in batches of `REMAINING_BATCH_SIZE` (50 entries) to avoid blocking the UI. The `isLoadingHistory` flag stays true during this phase.

3. **Running processes**: Any currently-running execution process gets a **long-lived** WebSocket to its normalized-logs endpoint. Entries stream in live and are emitted immediately as they arrive (the `onEntries` callback fires on each patch application). If the stream isn't ready yet (process just spawned), the hook retries with exponential backoff up to 20 attempts.

4. **Status transitions**: When a process transitions from `running` to a terminal status, the hook re-fetches its complete log via a fresh WebSocket to get the final, canonical entry list and replaces the live-streamed entries.

### Step 3 — Timeline derivation

The raw execution process entries are merged into a flat `ConversationTimelineSource` model and handed to the rendering layer via the `onTimelineUpdated` callback. The rendering layer further transforms this into the visual chat timeline with message grouping, file change aggregation, and plan detection.

## Diff Communication

Diffs are delivered via a dedicated, long-lived WebSocket (endpoint 4). They are **never** fetched via REST.

### Backend pipeline

The diff stream is produced by `DiffStreamManager` (`crates/services/src/services/diff_stream.rs`):

1. **Initial reset**: On connection, the manager computes a full diff of every workspace repo against its base commit using `GitService::get_diffs()` (`crates/git/src/lib.rs`). It emits a `ConversationPatch::replace_repo_diffs()` — a single `replace` operation at `/entries/<repo_key>` containing all file diffs.

2. **Filesystem watcher**: A `notify-debouncer-full` watcher (200 ms debounce for git events) monitors the worktree. When files change, `process_file_changes()` computes diffs only for the affected paths and emits targeted `add`/`replace`/`remove` patches at `/entries/<repo_key>/<escaped_file_path>`.

3. **Git state watcher**: The manager also watches `.git/HEAD` and `.git/logs/HEAD` for branch switches, rebases, or resets. These trigger a full re-diff (debounced).

4. **Periodic reconciliation**: Every 30 seconds, a `Reconcile` event triggers a full directory scan to catch any changes the filesystem watcher may have missed (e.g. operations inside nested `.git` directories).

5. **Size limits**: Per-file content is omitted if it exceeds `MAX_INLINE_DIFF_BYTES` (2 MB), and the entire stream stops including content after `MAX_CUMULATIVE_DIFF_BYTES` (200 MB) to prevent memory exhaustion. Line-count statistics are always included.

Each diff entry is a `Diff` struct: change kind (Added/Deleted/Modified/Renamed/Copied), old/new paths, old/new content (if not omitted), additions/deletions counts, and repo ID.

### Frontend consumption

`useDiffStream` (`packages/web-core/src/shared/hooks/useDiffStream.ts`) wraps `useJsonPatchWsStream` with a `DiffStreamEvent` shape:

```
{ entries: { [repoName]: { [filePath]: PatchType } } }
```

The hook flattens the nested map into a `Diff[]` array for the UI. The connection stays open for the lifetime of the workspace detail view.

## The Core WebSocket Machinery

All WebSocket consumption funnels through two layers:

### `useJsonPatchWsStream<T>` (hook, long-lived connections)

`packages/web-core/src/shared/hooks/useJsonPatchWsStream.ts`

Used for streams that stay open (diffs, execution process list, workspaces list). Key behaviors:

- **Structural sharing**: Patches are applied with `immer.produce()`, so only modified subtrees get new React references.
- **Upsert semantics**: `applyUpsertPatch()` (`packages/web-core/src/shared/lib/jsonPatch.ts`) catches `MissingError` on `replace` ops and converts them to `add`. This makes the client resilient to out-of-order or duplicate patches.
- **Reconnection**: On unclean close, exponential backoff (1s, 2s, 4s, 8s cap). After 6 failed retries with no data received, an error is surfaced. A `Finished` message or clean close (code 1000) suppresses reconnection.
- **Connection URL**: `openLocalApiWebSocket()` converts relative paths to absolute `ws://` or `wss://` URLs, with optional `/api/host/<hostId>` scoping for multi-host deployments.

### `streamJsonPatchEntries<E>` (imperative, short-lived connections)

`packages/web-core/src/shared/lib/streamJsonPatchEntries.ts`

Used for one-shot history loads (fetch all entries for a completed process, then close). Key behaviors:

- **rAF batching**: Incoming `JsonPatch` operations are accumulated and flushed once per `requestAnimationFrame`, avoiding per-message React re-renders.
- **Deduplication within a batch**: If multiple ops touch the same JSON Pointer path within one frame, only the last write is applied.
- **Synchronous flush on finish**: When `Finished` arrives, any pending rAF is cancelled and ops are flushed synchronously before calling `onFinished`, ensuring no entries are lost.
- Returns a `StreamController` with `getEntries()`, `isConnected()`, `onChange()`, and `close()`.

## Connection Lifecycle Summary

| Event | What happens |
|-------|-------------|
| User opens a workspace | `useExecutionProcesses` opens a WS to the session's execution-processes stream. `useDiffStream` opens a WS to the workspace diff stream. |
| Execution process list arrives | `useConversationHistory` opens short-lived WSs to each completed process's normalized-logs endpoint (most-recent-first), then long-lived WSs to any running processes. |
| User sends a follow-up message | A new execution process appears in the process-list stream. The history hook detects the new running process and opens a live normalized-logs WS for it. |
| Agent finishes | The execution process status transitions in the list stream. The history hook re-fetches the final log and replaces the live-streamed entries. |
| User switches sessions | All WS connections from the previous session are closed. Fresh connections are opened for the new session. Scope tokens prevent stale async callbacks from contaminating the new state. |
| Network interruption | `useJsonPatchWsStream` reconnects with exponential backoff. On reconnect, the server replays the full snapshot (initial `replace` patch + `Ready`), so the client converges to correct state without needing to track missed messages. |
| Process not yet ready | `loadRunningAndEmitWithBackoff` retries up to 20 times at 500ms intervals, waiting for the execution process's log stream to become available. |

## Out-of-Order Message Handling

The system does **not** implement sequence numbers or explicit reordering. Instead, it uses structural properties of JSON Patch to be resilient:

- **Snapshot-then-patch**: Every new connection begins with a full `replace` snapshot followed by a `Ready` signal. Incremental patches are only applied after the snapshot, so there is no window where partial state can confuse ordering.
- **Upsert fallback**: `applyUpsertPatch` converts failed `replace` ops to `add` ops, so if a patch for a not-yet-existing path arrives, it's created rather than dropped.
- **Last-write-wins dedup**: Within a rAF batch, duplicate patches to the same path are deduplicated to the last write, preventing stale intermediate states from rendering.
- **History replay on reconnect**: Raw-log streams (`useLogStream`) detect reconnections and replace the entire log array on the first message of a new connection, avoiding duplicates from history replay.
- **Scope tokens**: `useConversationHistory` uses opaque scope tokens to discard results from async loads that belong to a previous session/scope, preventing cross-session contamination.

These properties mean that while messages within a single WebSocket are always delivered in order (per the WebSocket spec), the system also handles the multi-connection case gracefully: each connection carries its own state tree, and the composition layer (`useConversationHistory`) reconciles them by execution process ID and creation timestamp.

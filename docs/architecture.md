---
title: "Architecture"
description: "Understand how the Vibe Kanban frontend, backend, local services, remote services, and external integrations fit together."
---

## Overview

Vibe Kanban is a local-first application with optional cloud and relay services. The local app serves a React frontend, exposes an Axum API, stores state in SQLite, manages git worktrees, runs coding agents as child processes, and proxies preview traffic from workspace dev servers.

The remote deployment adds organisation, issue, attachment, GitHub App, ElectricSQL, and relay-facing services for Vibe Kanban Cloud. Local installations can connect to those services when the shared API and relay endpoints are configured.

```mermaid
flowchart LR
  User["User"]
  Browser["Browser or Tauri webview"]
  LocalWeb["Local web app\npackages/local-web"]
  WebCore["Shared React UI\npackages/web-core"]
  Server["Local Axum server\ncrates/server"]
  Deployment["LocalDeployment\ncrates/local-deployment"]
  DB[("SQLite\ndb.v2.sqlite")]
  Worktrees["Git worktrees\nworkspace-manager\nworktree-manager"]
  Agents["Coding agents\nClaude, Codex, Gemini, Amp, etc."]
  Preview["Preview proxy\ncrates/preview-proxy"]
  Remote["Remote API\ncrates/remote"]
  Relay["Relay services\nrelay-* crates"]
  GitHost["Git hosts\nGitHub and Azure Repos"]

  User --> Browser
  Browser --> LocalWeb
  LocalWeb --> WebCore
  WebCore -->|HTTP, SSE, WebSocket| Server
  Server --> Deployment
  Deployment --> DB
  Deployment --> Worktrees
  Deployment --> Agents
  Deployment --> Preview
  Deployment -->|optional| Remote
  Deployment -->|optional| Relay
  Deployment --> GitHost
  Remote --> Relay
  Remote --> GitHost
```

## Backend architecture

The backend is split between transport, deployment wiring, service logic, and persistence. `crates/server` owns startup and Axum routing. `crates/deployment` defines the shared `Deployment` trait used by routes and services. `crates/local-deployment` builds the concrete local deployment by initialising configuration, SQLite, git, file storage, event streaming, approvals, executor state, relay state, and preview services.

```mermaid
flowchart TB
  Main["server binary\ncrates/server/src/main.rs"]
  Startup["startup.rs\nbind main and preview listeners"]
  Router["routes/mod.rs\nAxum route tree"]
  Middleware["middleware\norigin checks, relay signatures,\nerror logging, compression"]
  DeploymentTrait["Deployment trait\ncrates/deployment"]
  LocalDeployment["LocalDeployment\ncrates/local-deployment"]
  Services["Service layer\ncrates/services"]
  DBService["DBService\ncrates/db + SQLx migrations"]
  GitService["GitService\ncrates/git"]
  Executors["Executor profiles and runners\ncrates/executors"]
  Events["EventService\nSSE history + live stream"]
  PreviewProxy["PreviewProxyService\nsubdomain preview routes"]
  RemoteClient["RemoteClient\ncloud API handoff and sync"]
  RelayControl["Relay control, hosts,\nWebRTC, signing"]
  Files["Filesystem and attachments"]

  Main --> Startup
  Startup --> LocalDeployment
  Startup --> Router
  Router --> Middleware
  Router --> DeploymentTrait
  DeploymentTrait --> LocalDeployment
  LocalDeployment --> Services
  LocalDeployment --> DBService
  LocalDeployment --> GitService
  LocalDeployment --> Executors
  LocalDeployment --> Events
  LocalDeployment --> PreviewProxy
  LocalDeployment --> RemoteClient
  LocalDeployment --> RelayControl
  LocalDeployment --> Files
```

### Backend request layers

Most HTTP traffic enters through the Axum router in `crates/server`. The route
tree keeps transport concerns at the edge, loads request-scoped models where
needed, and delegates all durable behaviour through the `Deployment` interface.

```mermaid
flowchart TB
  Browser["Browser, Tauri webview,\nor relay client"]
  FrontendRoutes["Static frontend routes\n/ and /{*path}"]
  ApiRoot["/api router"]
  EdgeMiddleware["Edge middleware\ncompression, origin validation,\nerror logging"]
  RelayMiddleware["Relay middleware\nrequest signature verification\nresponse signing"]
  RouteGroups["Route groups\nworkspaces, sessions, repo,\nfilesystem, terminal, preview,\nevents, approvals, remote"]
  ModelLoaders["Model loader middleware\nworkspace lookup for /workspaces/{id}"]
  Handlers["Route handlers\nsmall transport adapters"]
  Deployment["Deployment trait\nshared backend contract"]
  Services["Local services\nDB, container, git, files,\nevents, remote, relay"]

  Browser --> FrontendRoutes
  Browser --> ApiRoot
  ApiRoot --> EdgeMiddleware
  EdgeMiddleware --> RelayMiddleware
  RelayMiddleware --> RouteGroups
  RouteGroups --> ModelLoaders
  RouteGroups --> Handlers
  ModelLoaders --> Handlers
  Handlers --> Deployment
  Deployment --> Services
```

### Local deployment composition

`LocalDeployment::new` wires long-lived services once at startup. The same
deployment instance is cloned into every route, so handlers share service
handles while each service keeps its own internal locks, background tasks, and
connection pools.

```mermaid
flowchart TB
  Startup["startup.rs\ncreate shutdown token\nbind listeners"]
  Config["Config\nconfig file + RwLock"]
  DB["DBService\nSQLite pool + migrations\nupdate hooks"]
  Events["EventService\nMsgStore for SSE patches"]
  Container["LocalContainerService\nprocess and workspace runtime"]
  WorkspaceManager["WorkspaceManager\nmulti-repo workspace orchestration"]
  WorktreeManager["WorktreeManager\ngit worktree lifecycle"]
  Git["GitService\ngit CLI operations"]
  Files["FileService and FilesystemService\nattachments, search, cleanup"]
  Auth["AuthContext + OAuth credentials"]
  Remote["RemoteClient\noptional cloud API"]
  Relay["RelayControl, RelaySigning,\nRelayHosts, TrustedKeyAuth"]
  Preview["PreviewProxyService\nworkspace preview routing"]
  Background["Background tasks\norphan file cleanup,\nworkspace cleanup,\nPR monitor"]

  Startup --> Config
  Startup --> DB
  DB --> Events
  Startup --> Container
  Container --> WorkspaceManager
  WorkspaceManager --> WorktreeManager
  Container --> Git
  Container --> Files
  Startup --> Auth
  Auth --> Remote
  Startup --> Relay
  Remote --> Relay
  Startup --> Preview
  Startup --> Background
  Background --> DB
  Background --> Container
```

### Workspace creation and execution flow

Creating and starting a workspace crosses three boundaries: the route creates
durable database records, the container service claims an execution, and the
local container implementation prepares worktrees before spawning the selected
agent or script process.

```mermaid
sequenceDiagram
  participant UI as Web UI
  participant Route as workspaces/create.rs
  participant DB as SQLite models
  participant Container as ContainerService
  participant Workspace as WorkspaceManager
  participant Git as GitService
  participant Executor as Executor action
  participant Child as Agent or script process

  UI->>Route: POST /api/workspaces/start
  Route->>DB: create workspace and session records
  Route->>DB: attach repositories and imported attachments
  Route->>Container: ensure_container_exists(workspace)
  Container->>Workspace: ensure_workspace_exists(...)
  Workspace->>Git: create or reuse per-repo worktrees
  Workspace-->>Container: workspace directory
  Container->>DB: store container_ref and clear deleted flag
  Route->>Container: start_execution(...)
  Container->>DB: claim execution_process with before-head repo state
  Container->>Executor: spawn(current_dir, approvals, env)
  Executor-->>Child: start coding agent or script
  Container->>Container: track child, cancellation token, MsgStore
  Container-->>Route: execution_process
  Route-->>UI: workspace, session, execution response
```

### Execution logs, UI streams, and state events

Execution logs and model-state events use different buses. Each running
execution process owns a per-process `MsgStore`; that store is the live fan-out
point for raw stdout/stderr and executor-normalised conversation patches. Raw
stdout and stderr are also persisted to local JSONL files under the asset
directory. The global `EventService` store is separate: it carries JSON Patch
state updates derived from SQLite hooks for workspace, process, and scratch
metadata. SQLite remains the durable source of execution metadata and the legacy
fallback for logs migrated from older versions.

In other words, chat and raw-log subscriptions are coordinated by the
container/execution layer and its per-execution `MsgStore`s, not by
`EventService`. `EventService` only coordinates database-state notifications.
The frontend listener split follows that boundary: raw log viewers subscribe to
raw stdout/stderr patches, the chat timeline subscribes to normalised
conversation patches for coding-agent and review processes, and workspace or
process list views subscribe to state patches derived from SQLite hooks.

```mermaid
flowchart TB
  Child["Agent or script child process"]
  Stdout["stdout/stderr streams"]
  ProcessMsgStore["Per-execution MsgStore\nraw chunks, session ids,\nmessage ids, JsonPatch entries"]
  JsonlWriter["ExecutionLogWriter\nsessions/.../processes/{execution_id}.jsonl"]
  Normalizer["Executor normaliser\nCodex, Claude, ACP, Cursor, etc."]
  RawWS["/api/execution-processes/{id}/raw-logs/ws\nWebSocket: STDOUT/STDERR JsonPatch entries"]
  NormalizedWS["/api/execution-processes/{id}/normalized-logs/ws\nWebSocket: NORMALIZED_ENTRY JsonPatch entries"]
  DB[("SQLite\nworkspace, session,\nexecution_process,\ncoding_agent_turn metadata")]
  UpdateHook["SQLite update/preupdate hooks\nEventService::create_hook"]
  GlobalMsgStore["EventService MsgStore\nworkspace/process state patches"]
  EventsSSE["GET /api/events\nSSE history + live stream"]
  WorkspacesWS["/api/workspaces/streams/ws\nWebSocket: workspace state patches"]
  ProcessesWS["/api/execution-processes/stream/session/ws\nWebSocket: execution_process state patches"]
  LegacyLogs["Legacy execution_process_logs\nread fallback + startup migration"]
  RawLogUI["Raw log listeners\nuseLogStream"]
  ChatUI["Chat timeline\nuseConversationHistory"]
  StateUI["State listeners\nuseWorkspaces, useExecutionProcesses"]

  Child --> Stdout
  Stdout --> ProcessMsgStore
  ProcessMsgStore --> JsonlWriter
  LegacyLogs -.->|"migrated to JSONL"| JsonlWriter
  ProcessMsgStore --> Normalizer
  Normalizer -->|"push JsonPatch"| ProcessMsgStore
  ProcessMsgStore --> RawWS
  ProcessMsgStore --> NormalizedWS
  ProcessMsgStore -->|"SessionId/MessageId"| DB
  DB --> UpdateHook
  UpdateHook --> GlobalMsgStore
  GlobalMsgStore --> EventsSSE
  GlobalMsgStore --> WorkspacesWS
  GlobalMsgStore --> ProcessesWS
  RawWS --> RawLogUI
  NormalizedWS --> ChatUI
  WorkspacesWS --> StateUI
  ProcessesWS --> StateUI
  DB -->|"workspace status lookups"| UpdateHook
```

The live execution-log stores are held by the container service, not by the
individual executor adapters or by Axum. `crates/utils` defines the reusable
`MsgStore` type. `crates/local-deployment` owns the concrete
`LocalContainerService` instance with the `msg_stores` lookup map
(`Arc<RwLock<HashMap<ExecutionProcessId, Arc<MsgStore>>>>`).
`crates/services` exposes that map through the `ContainerService` trait and
implements the common `get_msg_store_by_id`, `stream_raw_logs`, and
`stream_normalized_logs` methods. Axum routes in `crates/server` call those
trait methods through `Deployment::container()`, so the lookup path is shared
across Codex, Claude, Cursor, ACP-style agents, scripts, and other executors.

```mermaid
flowchart TB
  subgraph UtilsCrate["crates/utils"]
    MsgStoreType["MsgStore\nbounded history + broadcast sender"]
    LogMsg["LogMsg\nstdout, stderr, JsonPatch, finished"]
  end

  subgraph LocalDeploymentCrate["crates/local-deployment"]
    LocalDeployment["LocalDeployment\nstartup-owned service graph"]
    LocalContainer["LocalContainerService\nconcrete container service"]
    StoreMap["msg_stores\nHashMap<ExecutionProcessId, Arc<MsgStore>>"]
    ChildMaps["child_store, cancellation_tokens,\nDB stream handles, exit monitors"]
  end

  subgraph ServicesCrate["crates/services"]
    ContainerTrait["ContainerService trait"]
    ClaimExecution["claim_execution_with_idempotency_key\ncreates execution row + MsgStore"]
    CommonLookup["get_msg_store_by_id\nstream_raw_logs\nstream_normalized_logs"]
    Persistence["spawn_stream_raw_logs_to_storage\nsubscribes to same MsgStore"]
  end

  subgraph ExecutorsCrate["crates/executors"]
    ExecutorTrait["StandardCodingAgentExecutor\nspawn + normalize_logs"]
    ExecutorAdapters["Codex, Claude, Cursor,\nACP, scripts, etc."]
  end

  subgraph ServerCrate["crates/server"]
    AxumRoutes["Axum routes\nexecution_processes.rs"]
    RawRoute["raw-logs/ws"]
    NormalizedRoute["normalized-logs/ws"]
  end

  LocalDeployment --> LocalContainer
  LocalContainer --> StoreMap
  LocalContainer --> ChildMaps
  LocalContainer -.implements.-> ContainerTrait
  StoreMap --> MsgStoreType
  MsgStoreType --> LogMsg

  ContainerTrait --> ClaimExecution
  ContainerTrait --> CommonLookup
  ContainerTrait --> Persistence
  ClaimExecution -->|"insert execution_id -> Arc<MsgStore>"| StoreMap
  Persistence -->|"subscribe for JSONL persistence"| StoreMap

  ExecutorAdapters --> ExecutorTrait
  ExecutorTrait -->|"spawn child"| LocalContainer
  ExecutorTrait -->|"normalize_logs(Arc<MsgStore>)"| StoreMap
  ChildMaps -->|"stdout/stderr forwarder"| StoreMap

  AxumRoutes --> RawRoute
  AxumRoutes --> NormalizedRoute
  RawRoute -->|"Deployment::container().stream_raw_logs(id)"| CommonLookup
  NormalizedRoute -->|"Deployment::container().stream_normalized_logs(id)"| CommonLookup
  CommonLookup -->|"read execution_id"| StoreMap
```

The main listener split is:

| Frontend listener | Endpoint and protocol | Payload shape | Intended data |
| --- | --- | --- | --- |
| `useLogStream` in process, script, and preview log views | `/api/execution-processes/{id}/raw-logs/ws` over WebSocket | `LogMsg::JsonPatch` entries whose values are `STDOUT` or `STDERR`, followed by `finished` | Raw execution logs for scripts, dev servers, and process-detail views. This stream does not carry normalised chat entries. |
| `useConversationHistory` for workspace chat timeline | `/api/execution-processes/{id}/normalized-logs/ws` over WebSocket | `LogMsg::JsonPatch` entries whose values are normalised conversation entries, followed by `finished` | Agent/review conversation history: assistant text, tool use, token usage, todos, questions, errors, and related normalised entries. Script processes are routed to the raw-log stream instead. |
| `useExecutionProcesses` via `ExecutionProcessesProvider` | `/api/execution-processes/stream/session/ws?session_id=...` over WebSocket | Initial `replace /execution_processes`, `Ready`, then add/replace/remove patches keyed by process id | Execution-process metadata for a session: status, timestamps, executor action, run reason, soft-delete state, and similar model fields. |
| `useWorkspaces` via `WorkspaceProvider` | `/api/workspaces/streams/ws?archived=...` over WebSocket | Initial `replace /workspaces`, `Ready`, then workspace add/replace/remove patches | Workspace list/cache state, including computed workspace status. |
| Legacy/global event consumers | `/api/events` over SSE | `LogMsg` encoded as SSE events, normally JSON patches from the EventService store | Global history + live state events from the SQLite hook bus. Current React state hooks use the filtered WebSocket endpoints above rather than `EventSource`. |

The SQLite update stream is not an execution-log stream. `DBService` installs
SQLite update and preupdate hooks for `workspaces`, `execution_processes`, and
`scratch`. For inserts and updates, SQLite gives the hook a `rowid`; the async
hook task reloads the changed row, converts the row into JSON Patch operations,
and pushes the patch into the `EventService` `MsgStore`. Add and replace patches
therefore carry the row payload, not only the row id. Deletes are handled by the
preupdate hook while old key values are still available, so remove patches carry
the identity needed to remove client-side state. Filtered WebSocket routes then
expose per-view slices of that patch bus, and `/api/events` exposes the same
store as SSE for global consumers.

#### EventService ownership, threading, and filtering

The global state-event store is an in-memory `utils::MsgStore`, not a durable
database table. `MsgStore` combines a bounded history buffer with a Tokio
`broadcast::Sender<LogMsg>`. Calling `push_patch` appends the patch to history
and broadcasts it to every current subscriber. New subscribers can therefore
receive an initial in-memory replay through `history_plus_stream`, while live
subscribers receive subsequent broadcasts through their own broadcast receiver.
The history is process-local and bounded by bytes, so SQLite remains the durable
source of truth for model state.

`crates/local-deployment` creates the shared event `MsgStore` during
`LocalDeployment::new`. It passes the same `Arc<MsgStore>` into
`EventService::create_hook` before opening the hooked `DBService`, then stores
it inside the long-lived `EventService`. `crates/services` owns the
`EventService` type and the patch-building logic. `crates/server` does not write
to the event store directly; routes ask the deployment for `events()` or
`stream_events()` and turn the returned `LogMsg` stream into SSE or WebSocket
frames.

The only normal producers for this global event store are SQLite hooks installed
by `EventService::create_hook`. The preupdate hook handles deletes while the old
row values are still available, producing remove patches for `workspaces`,
`execution_processes`, and `scratch`. The update hook handles inserts and
updates. Because SQLite invokes the hook synchronously on the connection thread,
the hook captures the current Tokio runtime handle and immediately spawns an
async task. That task reloads the affected row by `rowid`, builds the JSON Patch,
and calls `msg_store.push_patch`. Execution-process changes also trigger a
derived workspace-status patch so workspace lists update when process state
changes. This means the hook is the bridge from synchronous SQLite mutation
notification into the async broadcast bus.

Consumers register by subscribing to the `EventService` store, not by
registering with SQLite. `/api/events` uses `Deployment::stream_events`, which
exposes the store's history plus live stream as SSE without view-specific
filtering. The WebSocket routes for workspaces, execution processes, and scratch
state call `EventService` stream helpers instead. Each helper first queries
SQLite for a fresh snapshot, emits that snapshot as a `replace` patch, emits
`Ready`, and then subscribes to the shared broadcast receiver for live patches.

Filtering for model-state WebSockets is owned by these `EventService` stream
helpers in `crates/services/src/services/events/streams.rs`, with transport
handled by `crates/server`. Clients are not expected to consume a complete
unfiltered firehose for the main workspace and execution-process views.
`stream_workspaces_raw` filters by patch path and optional `archived` state,
converting some replacements to adds or removes so a filtered client cache stays
coherent when a workspace enters or leaves the current view.
`stream_execution_processes_for_session_raw` filters execution-process patches
by `session_id` and `show_soft_deleted`; remove patches that cannot be
session-verified are allowed through and the client cache ignores irrelevant
ids. `stream_scratch_raw` filters `/scratch` patches by scratch id and scratch
type embedded in the patch value. Frontend stores therefore receive view-shaped
patch streams, while the global store itself remains a coarse event bus for all
hooked table changes.

This global `EventService` store is separate from the per-execution `MsgStore`
instances kept by `LocalContainerService`. Per-execution stores are created when
an execution process is started and are written by executor stdout/stderr
forwarders and normalisers. They feed the raw and normalised log WebSockets:
`/api/execution-processes/{id}/raw-logs/ws` maps stdout/stderr messages into
raw log patches, while `/api/execution-processes/{id}/normalized-logs/ws`
streams the normalised conversation patches pushed by executor normalisers. The
global event store only carries model-state patches derived from SQLite hook
notifications.

Running execution-log streams read from memory first. If the process still has a
live `MsgStore`, both raw and normalised endpoints replay its in-memory history
and then continue with live broadcast messages. Once the live store is gone, the
raw endpoint reads the process JSONL file from disk and appends `finished`; if no
file exists it falls back to legacy `execution_process_logs` rows. Historical
normalised replay also starts from the JSONL raw messages, populates a temporary
`MsgStore`, reruns the executor normaliser, deduplicates the resulting patches,
and then emits `finished`.

### Threading and synchronization

Most backend concurrency is cooperative Tokio task concurrency. The process has
one Tokio runtime, Axum spawns a task per request/connection, and long-lived
services spawn additional tasks for event fan-out, process monitoring, diff
watching, relay registration, and cleanup. Blocking filesystem, git, shell,
tarball, PTY, and notification work is moved to Tokio's blocking pool with
`tokio::task::spawn_blocking` or, for a few callback-style integrations, a
dedicated `std::thread::spawn`.

```mermaid
flowchart TB
  subgraph Tokio["Tokio async runtime"]
    Axum["Axum request, SSE,\nand WebSocket tasks"]
    GlobalEvents["Global EventService\nMsgStore + broadcast"]
    ExecStores["Per-execution MsgStore\nraw and normalised logs"]
    Container["LocalContainerService\nchild/process lifecycle maps"]
    Approvals["Approvals\nDashMap + oneshot waiters\n+ broadcast patches"]
    DiffStreams["Diff streams\nmpsc queue + watcher task"]
    Relay["Relay/WebRTC\nmpsc command queues,\noneshot responses, Notify"]
    PRMonitor["PR monitor\ninterval + Notify trigger"]
  end

  subgraph Blocking["Blocking OS work"]
    Git["git and GitHub/Azure CLI\nspawn_blocking"]
    FS["filesystem scan and watch callbacks"]
    PTY["PTY reader thread\nunbounded mpsc output"]
    Child["Agent/script child process\nprocess group"]
  end

  subgraph Pools["Resource pools"]
    SQLite["SQLite SQLx pool"]
    Postgres["Postgres SQLx pools\nremote and relay server"]
    HTTP["reqwest client pools\npreview, remote, relay"]
  end

  subgraph BrowserRuntime["Browser runtime"]
    Browser["React UI"]
    BrowserWorkers["Frontend diff Worker pool\nsize 3"]
  end

  Browser --> Axum
  Browser --> BrowserWorkers
  Axum --> Container
  Container --> Child
  Child --> ExecStores
  ExecStores --> Axum
  SQLite -->|"update/preupdate hooks"| GlobalEvents
  GlobalEvents --> Axum
  Axum --> Approvals
  Approvals --> Container
  Axum --> DiffStreams
  DiffStreams --> Git
  DiffStreams --> FS
  Relay --> Axum
  PRMonitor --> Container
  Container --> SQLite
  Axum --> SQLite
  Axum --> HTTP
  Relay --> HTTP
  Relay --> Postgres
```

The main synchronization domains are:

| Domain | Cross-task or cross-thread boundary | Synchronization mechanism | Notes and contention risks |
| --- | --- | --- | --- |
| Server lifetime | `crates/server/src/main.rs` runs the main API listener and preview proxy listener as sibling tasks. | A process-wide `CancellationToken` is cloned into the deployment and listener graceful-shutdown futures. | Shutdown is cooperative. Tasks that own child tokens should stop when the root token is cancelled, while short spawned tasks may simply finish best-effort cleanup. |
| SQLite state events | SQLite update/preupdate hooks run synchronously on SQLx connection threads but need to publish async UI events. | `EventService::create_hook` captures the Tokio runtime handle and spawns an async patch-building task; `MsgStore` uses a `std::sync::RwLock` for bounded history and a Tokio `broadcast::Sender` for live subscribers. | Hook callbacks must stay small because they run on the SQLite connection thread. Row reload and patch fan-out happen after the hook returns, reducing the chance that a database connection waits on async subscribers. |
| Execution process lifecycle | Each agent or script has an OS child process, stdout/stderr forwarding, optional executor exit signal, DB log persistence, and an exit monitor. | `LocalContainerService` keeps `child_store`, `cancellation_tokens`, `msg_stores`, DB stream handles, and exit monitor handles in `Arc<tokio::sync::RwLock<HashMap<...>>>`. Executor completion uses `oneshot` exit signals plus `CancellationToken`s. | Process cleanup takes entries out of maps before awaiting long work where possible. The highest-risk area is nested access to child handles and lifecycle maps during stop/exit races, so new code should avoid holding a map lock while awaiting process IO, DB writes, or another service call. |
| Execution logs | Child stdout/stderr, executor protocol normalisers, JSONL persistence, raw-log WebSockets, and chat-log WebSockets all share per-execution state. | Per-execution `MsgStore` instances combine bounded in-memory history with `broadcast::Sender<LogMsg>`. Log persistence subscribes to the same store and exits on `LogMsg::Finished`. | Slow subscribers can lag and miss broadcast entries, but they get an error and the durable fallback is the JSONL file after process completion. |
| Approvals and questions | Executor adapters wait for UI approval responses while routes and WebSockets expose pending state. | `Approvals` stores pending/completed entries in `DashMap`, gives each request a `oneshot::Sender<ApprovalOutcome>`, and broadcasts JSON Patch updates on a bounded `broadcast::channel(64)`. Timeout watchers are spawned Tokio tasks. | This avoids holding a mutex across the executor wait. If approval patch subscribers lag, they receive a fresh pending snapshot. |
| Queued follow-ups | Users can submit a follow-up while a session is already running. | `QueuedMessageService` is an in-memory `DashMap<SessionId, QueuedMessage>`. Exit-monitor finalization consumes the queued message and starts the next execution. | The queue is process-local and intentionally one item per session. The DB remains the durable source for execution records; queued drafts are not a multi-item durable work queue. |
| Auth, config, relay credentials, and profile caches | Request handlers and background tasks share mutable configuration and auth state. | Mostly `Arc<tokio::sync::RwLock<...>>`; auth refresh has a `tokio::sync::Mutex<()>` guard so only one refresh runs at a time. | Treat these locks as short critical sections around in-memory data. Do not hold them while making remote HTTP requests unless the code explicitly needs to serialize that request. |
| Diff and filesystem streams | Diff views combine file-watcher callbacks, periodic git checks, and git diff computation. | `diff_stream` sends `LogMsg`s through a bounded `mpsc::channel(1000)`, stores sent-file metadata in `std::sync::RwLock`s, uses `watch::channel(())` for git state notifications, and aborts the watcher task when `DiffStreamHandle` is dropped. Filesystem repo scans use a `CancellationToken` soft timeout plus `JoinHandle::abort` hard timeout. | Backpressure is local to each diff stream. Heavy git operations run in the blocking pool so they do not pin async workers. |
| PTY terminal sessions | Terminal routes interact with blocking PTY readers and writers. | `PtyService` uses `Arc<std::sync::Mutex<HashMap<Uuid, PtySession>>>`; session creation runs in `spawn_blocking`; each PTY reader uses a dedicated OS thread and forwards bytes over an unbounded Tokio `mpsc` receiver. | The synchronous mutex protects portable-pty handles. Keep writes/resizes short; avoid adding async awaits while holding the mutex. |
| Relay and WebRTC | Relay host/client code multiplexes HTTP requests and WebSocket streams over data channels and SSH tunnels. | Relay control uses `CancellationToken` child tokens. WebRTC clients use bounded `mpsc` command and data-channel queues, per-request `oneshot` response waiters, `tokio::sync::Mutex<HashMap<...>>` pending maps, and `Notify` for connection-open wakeups. Relay signing sessions use an `RwLock<HashMap<...>>`. | Pending maps are locked briefly to insert/remove waiters. Data-channel request timeouts prevent permanent waits. TunnelManager serializes tunnel creation with a mutex and double-checks before inserting an active tunnel. |
| Remote/cloud services | The remote API and relay server are separate Axum processes backed by Postgres. | SQLx `PgPool`s are configured with `max_connections(10)` in both `crates/remote` and relay server DB setup. Request transaction metadata is propagated with a Tokio task-local `TX_CONTEXT`. | The pool size is the primary remote-side concurrency limit in this code. ElectricSQL and Postgres handle cross-process synchronization; app code should keep transactions scoped tightly. |
| Preview proxy and HTTP clients | Preview iframe traffic and relay fallback requests proxy to local or remote HTTP/WebSocket targets. | `PreviewProxyService` owns a cloneable reqwest `Client`; WebSocket proxying delegates to bridge helpers. Reqwest manages its own connection pool. | There is no app-level preview lock. Backpressure and connection reuse are handled by Hyper/reqwest and the WebSocket bridge tasks. |
| Frontend diff rendering | Large file diffs are parsed/highlighted off the main browser thread. | `ChangesPanelContainer` creates a `WorkerPoolContextProvider` for `@pierre/diffs` with `poolSize: 3`. | This is the only explicit frontend thread pool found in the app code search. |

No explicit Tokio semaphores or barriers are currently used in the searched
backend and frontend code. The practical concurrency limits are therefore the
SQLx connection pools, bounded broadcast/mpsc queues, Tokio's blocking-thread
pool, the browser worker pool, and external services or child processes.

Potential deadlock patterns to avoid:

- Do not hold `tokio::sync::RwLock` or `Mutex` guards across calls that can
  re-enter the same service, wait on child processes, perform DB work, or await
  network IO.
- Do not call async code from SQLite hook callbacks. Use the existing runtime
  spawn bridge so SQLite connection threads are not blocked by async work.
- Keep `std::sync` locks in `MsgStore`, PTY sessions, filesystem watcher
  state, and executor caches small and non-async. These locks are safe because
  current code only guards in-memory structures or synchronous handles.
- Prefer bounded channels for new long-lived streams unless losing
  backpressure is intentional. Existing unbounded channels are used for PTY
  output and some executor protocol/control paths where the producer is tied to
  a local process or protocol callback.

### Preview proxy flow

Preview traffic is separate from normal API traffic. The main server exposes
preview configuration APIs, while the preview listener uses
`PreviewProxyService` to route browser requests to the dev-server process
running inside a workspace.

```mermaid
flowchart LR
  UI["Workspace UI"]
  Api["/api/preview and\n/workspaces/{id}/execution/dev-server/start"]
  Container["LocalContainerService"]
  DevProcess["Dev-server execution process"]
  PreviewService["PreviewProxyService"]
  PreviewListener["Preview listener\nstartup.rs"]
  Browser["Preview browser frame"]
  WorkspaceServer["Workspace dev server\nlocalhost port in worktree"]

  UI --> Api
  Api --> Container
  Container --> DevProcess
  DevProcess --> WorkspaceServer
  Api --> PreviewService
  Browser --> PreviewListener
  PreviewListener --> PreviewService
  PreviewService --> WorkspaceServer
  WorkspaceServer --> PreviewService
  PreviewService --> Browser
```

## Executor architecture

Executors are adapters around coding-agent CLIs and protocols. The backend
stores the desired action as an `ExecutorAction`, resolves it through
`ExecutorConfigs`, and then calls the `StandardCodingAgentExecutor` trait
implemented by each agent. The container service owns process lifecycle,
workspace paths, approval bridges, environment injection, log capture, and
durable execution records.

```mermaid
flowchart TB
  Request["Workspace request\ninitial prompt, follow-up,\nreview, script"]
  ExecutorAction["ExecutorAction\ncoding agent or script"]
  Profiles["ExecutorConfigs\nprofile, model, permissions,\nvariant overrides"]
  Container["LocalContainerService\nstart_execution_inner"]
  Trait["StandardCodingAgentExecutor trait\nspawn, spawn_follow_up,\nspawn_review,\nnormalize_logs,\ndiscover_options"]
  Enum["CodingAgent enum\nenum_dispatch"]
  Approvals["ExecutorApprovalBridge\nCodex, Claude, Gemini,\nQwen, Opencode"]
  Env["ExecutionEnv\nrepo context, VK ids,\ncommit reminders"]
  Child["SpawnedChild\nprocess group, exit signal,\ncancellation token"]
  MsgStore["Per-execution MsgStore"]
  Storage["JSONL execution log file"]
  UIStreams["Raw and normalised\nWebSocket streams"]

  Request --> ExecutorAction
  ExecutorAction --> Profiles
  Profiles --> Enum
  Container --> Trait
  Enum --> Trait
  Container --> Approvals
  Container --> Env
  Trait -->|"spawn*()"| Child
  Child -->|"stdout/stderr"| MsgStore
  Trait -->|"normalize_logs()"| MsgStore
  MsgStore --> Storage
  MsgStore --> UIStreams
```

### Executor adapters

Most adapters share the same container contract, but differ in how they launch
the agent, resume sessions, request approvals, and translate native output into
normalised conversation entries.

```mermaid
flowchart LR
  subgraph Common["Common executor contract"]
    Trait["StandardCodingAgentExecutor"]
    CommandBuilder["CommandBuilder\nbase command + overrides"]
    SpawnedChild["SpawnedChild\nAsyncGroupChild + optional\nexit/cancel channels"]
    Normalized["ConversationPatch\nnormalised UI entries"]
  end

  subgraph Codex["Codex"]
    CodexCmd["npx @openai/codex app-server"]
    AppServer["AppServerClient\nJSON-RPC over stdin/stdout"]
    CodexLog["LogWriter\nprotocol events, approvals,\nerrors as raw log lines"]
    CodexParser["codex::normalize_logs\ncodex/event parser"]
  end

  subgraph Claude["Claude Code"]
    ClaudeCmd["npx @anthropic-ai/claude-code\nor claude-code-router"]
    ClaudeResume["--resume\n--resume-session-at"]
    ClaudeJson["JSON stdout lines\nsession_id + message uuid"]
    ClaudeParser["ClaudeLogProcessor\nstdout JSON + stderr parser"]
  end

  subgraph ACP["Gemini and ACP-style agents"]
    AcpHarness["AcpAgentHarness\nsession protocol wrapper"]
    AcpEvents["ACP stdout events\nsession, message, tool, error"]
    AcpParser["acp::normalize_logs\nstreaming text + tool state"]
  end

  subgraph Cursor["Cursor"]
    CursorCmd["Cursor agent command\ninitial + --resume"]
    CursorTrust["MCP trust setup"]
    CursorParser["Cursor normaliser\nplain text/stderr handling"]
  end

  Trait --> CommandBuilder
  CommandBuilder --> CodexCmd
  CommandBuilder --> ClaudeCmd
  CommandBuilder --> AcpHarness
  CommandBuilder --> CursorCmd
  CodexCmd --> AppServer
  AppServer --> CodexLog
  CodexLog --> CodexParser
  ClaudeCmd --> ClaudeResume
  ClaudeResume --> ClaudeJson
  ClaudeJson --> ClaudeParser
  AcpHarness --> AcpEvents
  AcpEvents --> AcpParser
  CursorCmd --> CursorTrust
  CursorTrust --> CursorParser
  CodexParser --> Normalized
  ClaudeParser --> Normalized
  AcpParser --> Normalized
  CursorParser --> Normalized
  Trait --> SpawnedChild
```

Codex runs as an app-server subprocess and uses a JSON-RPC client inside the
executor adapter. The adapter starts or forks Codex threads, forwards approval
requests through Vibe Kanban's approval bridge, writes Codex protocol events
back into the captured log stream, and normalises `codex/event` notifications
for the conversation UI.

Claude Code runs as a CLI process that emits structured JSON lines on stdout.
The Claude adapter builds initial and resumed commands, supports
`--resume-session-at` for resetting to a previous message, extracts Claude
session and message identifiers from the JSON stream, and normalises both
stdout JSON and stderr into conversation entries.

Gemini uses the shared ACP harness and normaliser. The harness manages
agent-client-protocol sessions, while the ACP normaliser turns session,
message, tool-call, and error events into streaming conversation patches.

Cursor follows the same trait contract with Cursor-specific command building,
resume arguments, MCP trust setup, and log normalisation. Its normaliser handles
plain text and stderr-oriented output, including login and setup errors.

## Frontend architecture

The detailed frontend architecture, including app shells, feature modules, local
and remote entrypoints, connection ownership, and workspace data flows, lives in
[Frontend architecture](frontend-architecture.md).

## Runtime flow

1. The server starts, creates the asset directory, migrates SQLite, initialises `LocalDeployment`, and binds the main API listener and preview proxy listener.
2. The frontend loads from the local server in production or from Vite in development.
3. UI features call `/api` routes for projects, workspaces, sessions, git operations, previews, approvals, terminal access, and configuration.
4. Backend routes delegate through the `Deployment` trait to database, git, filesystem, executor, event, preview, remote, and relay services.
5. A workspace creates or reuses git worktrees, starts agent or script processes, stores execution metadata in SQLite, persists raw execution logs as JSONL files, and streams raw logs, normalised conversation patches, and state changes back to the UI.
6. Optional cloud configuration enables remote project, issue, host pairing, relay, and sync flows through `crates/remote` and the relay crates.

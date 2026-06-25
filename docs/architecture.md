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

Execution output uses a per-process `MsgStore` as the live fan-out point. Raw
stdout and stderr are persisted to local JSONL files under the asset directory,
while executor-specific normalisers read the same stream and push conversation
patches back into the `MsgStore`. UI listeners are split by payload: raw log
viewers subscribe to raw stdout/stderr patches, the chat timeline subscribes to
normalised conversation patches for coding-agent and review processes, and
workspace/process list views subscribe to state patches derived from SQLite
hooks. SQLite remains the source of durable execution metadata and the legacy
fallback for logs migrated from older versions.

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
`scratch`. Those hooks fetch the changed row, convert it into JSON Patch
operations, and push the patch into the `EventService` `MsgStore`. Filtered
WebSocket routes then expose per-view slices of that patch bus, and `/api/events`
exposes the same store as SSE for global consumers.

Running execution-log streams read from memory first. If the process still has a
live `MsgStore`, both raw and normalised endpoints replay its in-memory history
and then continue with live broadcast messages. Once the live store is gone, the
raw endpoint reads the process JSONL file from disk and appends `finished`; if no
file exists it falls back to legacy `execution_process_logs` rows. Historical
normalised replay also starts from the JSONL raw messages, populates a temporary
`MsgStore`, reruns the executor normaliser, deduplicates the resulting patches,
and then emits `finished`.

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

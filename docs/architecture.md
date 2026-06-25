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

### Execution logs and live updates

Execution output uses a per-process `MsgStore` before being normalised and
persisted. Database writes then trigger SQLite hooks registered by
`EventService`, which convert changed rows into JSON patches for the global
SSE stream consumed by the frontend.

```mermaid
flowchart LR
  Child["Agent or script child process"]
  Stdout["stdout/stderr streams"]
  ProcessMsgStore["Per-execution MsgStore\nraw and normalised log entries"]
  LogPersister["DB stream task\nexecution_process logs"]
  DB[("SQLite\nexecution_processes,\nturns, scratch,\nworkspaces")]
  UpdateHook["SQLite update/preupdate hooks\nEventService::create_hook"]
  GlobalMsgStore["EventService MsgStore\nhistory + live patches"]
  SSE["GET /api/events\nSSE keep-alive stream"]
  UI["Frontend query cache\nworkspace and process state"]

  Child --> Stdout
  Stdout --> ProcessMsgStore
  ProcessMsgStore --> LogPersister
  LogPersister --> DB
  DB --> UpdateHook
  UpdateHook --> GlobalMsgStore
  GlobalMsgStore --> SSE
  SSE --> UI
  DB -->|"workspace status lookups"| UpdateHook
```

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

## Frontend architecture

The frontend has thin app entrypoints and a larger shared UI package. `packages/local-web` provides the local Vite shell, TanStack Router route files, and app-level providers. `packages/remote-web` provides the cloud entrypoint. `packages/web-core` contains the shared features, dialogs, hooks, stores, API helpers, keyboard handling, onboarding, workspace UI, kanban UI, and shared layouts used by both app shells.

```mermaid
flowchart TB
  LocalEntrypoint["Local entrypoint\npackages/local-web"]
  RemoteEntrypoint["Remote entrypoint\npackages/remote-web"]
  Router["TanStack Router\nroute files and generated tree"]
  Providers["App providers\nhost, workspace, processes,\nlogs, actions, terminal, modals"]
  WebCore["Shared UI library\npackages/web-core"]
  Features["Feature modules\nkanban, workspace,\nworkspace chat, onboarding, export"]
  Shared["Shared modules\ncomponents, dialogs, hooks,\nstores, keyboard, i18n"]
  API["API helpers\nshared/lib/api.ts\nTanStack Query client"]
  Types["Generated TypeScript types\nshared/types.ts\nshared/remote-types.ts"]
  LocalAPI["Local backend API\n/api"]
  RemoteAPI["Remote cloud API"]
  Realtime["Realtime channels\nSSE, WebSocket, ElectricSQL"]

  LocalEntrypoint --> Router
  RemoteEntrypoint --> Router
  Router --> Providers
  Providers --> WebCore
  WebCore --> Features
  WebCore --> Shared
  Features --> API
  Shared --> API
  API --> Types
  API --> LocalAPI
  API --> RemoteAPI
  API --> Realtime
```

## Runtime flow

1. The server starts, creates the asset directory, migrates SQLite, initialises `LocalDeployment`, and binds the main API listener and preview proxy listener.
2. The frontend loads from the local server in production or from Vite in development.
3. UI features call `/api` routes for projects, workspaces, sessions, git operations, previews, approvals, terminal access, and configuration.
4. Backend routes delegate through the `Deployment` trait to database, git, filesystem, executor, event, preview, remote, and relay services.
5. A workspace creates or reuses git worktrees, starts agent or script processes, stores execution metadata, and streams logs and state changes back to the UI.
6. Optional cloud configuration enables remote project, issue, host pairing, relay, and sync flows through `crates/remote` and the relay crates.

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

---
title: "Dioxus Frontend Migration Guide"
description: "A comprehensive guide to incrementally migrating the Vibe Kanban frontend from React to Dioxus, outlining the strategy, costs, and key decision points."
---

## Table of Contents

- [Executive Summary](#executive-summary)
- [1. Understanding the Current Frontend](#1-understanding-the-current-frontend)
- [2. The Dioxus Proposition: What Changes?](#2-the-dioxus-proposition-what-changes)
- [3. Incremental Migration Strategies](#3-incremental-migration-strategies)
  - [Option 1: TypeScript Host with Dioxus Islands](#option-1-typescript-host-with-dioxus-islands)
  - [Option 2: Feature-by-Feature Dioxus Pages](#option-2-feature-by-feature-dioxus-pages)
  - [Option 3: Parallel Dioxus App Shell](#option-3-parallel-dioxus-app-shell)
  - [Option 4: Big-Bang Rewrite (Not Recommended)](#option-4-big-bang-rewrite-not-recommended)
- [4. A Practical Migration Path (Phased Approach)](#4-a-practical-migration-path-phased-approach)
  - [Phase 0: Build and Contract Spike](#phase-0-build-and-contract-spike)
  - [Phase 1: Define Frontend Facades](#phase-1-define-frontend-facades)
  - [Phase 2: Migrate Low-Risk Leaf Features](#phase-2-migrate-low-risk-leaf-features)
  - [Phase 3: Migrate Whole Routes](#phase-3-migrate-whole-routes)
  - [Phase 4: Migrate the Core Workspace](#phase-4-migrate-the-core-workspace)
  - [Phase 5: Invert the Host](#phase-5-invert-the-host)
- [5. Analysis of High-Cost & Difficulty Areas](#5-analysis-of-high-cost--difficulty-areas)
  - [High-Difficulty Areas (Chart)](#high-difficulty-areas-chart)
- [6. Technical Deep Dives](#6-technical-deep-dives)
  - [State Management Strategy](#state-management-strategy)
  - [Styling Strategy](#styling-strategy)
  - [Developer Experience (DX)](#developer-experience-dx)
  - [Testing Strategy](#testing-strategy)
- [7. Final Decision Guidance](#7-final-decision-guidance)

## Executive Summary

An incremental migration from the current React/TypeScript frontend to Dioxus is feasible. However, it should be approached as a **phased rewrite** rather than a simple component-for-component translation. The cost and complexity of bridging two different paradigms (React's virtual DOM vs. Dioxus's Rust-based reactivity) are non-trivial.

**Recommendation:**

1.  **Start with a TypeScript-hosted Dioxus architecture.** The existing React shell (`local-web`, `remote-web`) should remain the application's entry point. New or self-contained features can be built as isolated Dioxus "islands" (WebAssembly components). This approach allows the team to validate the Dioxus workflow (builds, styling, interop) with minimal disruption.
2.  **Defer the "host inversion"** (a Dioxus app hosting TypeScript components) until a significant portion of the application's state and routing is managed by Rust. Attempting this too early would introduce immense complexity.

**Key Takeaways:**

-   **Don't rewrite `web-core` first.** The shared components and hooks are deeply integrated with the React ecosystem.
-   **Begin with a small "spike"** on a low-risk UI element to establish a clear interop contract.
-   **Introduce explicit frontend "facades"** (for APIs, navigation, etc.) to create a shared foundation for both React and Dioxus parts of the app.
-   **Acknowledge the high-cost areas:** shared state, routing, and complex UI widgets (Kanban board, chat, terminal) will be the most challenging to migrate.

## 1. Understanding the Current Frontend

Our frontend is structured into three primary layers, all built on React:

-   `packages/local-web`: The local application shell (including Tauri). Manages local routing, providers, and API transport.
-   `packages/remote-web`: The cloud application shell. Manages remote authentication, account-level routing, and WebRTC/relay transport for remote hosts.
-   `packages/web-core`: The shared product experience. Contains the vast majority of UI components, hooks, stores, and business logic.

The dependency graph is deeply rooted in the React ecosystem, relying on:

-   **Component Model:** React Context, Hooks
-   **Data Fetching & State:** TanStack Query, Zustand
-   **Routing:** TanStack Router
-   **UI Libraries:** Radix UI, shadcn/ui, React DnD, Lexical (editor), CodeMirror, xterm.js
-   **Virtualization:** React-specific virtualization libraries.

This tight coupling means that simply translating JSX to Dioxus's RSX is not enough. We must architect new Rust-native equivalents for the contracts and patterns these libraries provide.

## 2. The Dioxus Proposition: What Changes?

Dioxus web applications compile to WebAssembly (WASM) and manipulate the browser's DOM. Dioxus provides excellent support for its web target, offering several "escape hatches" to interact with JavaScript. This enables two-way interoperability:

-   **React hosts Dioxus:** A React component can load a WASM bundle and mount it into a DOM node. This is the "island" model.
-   **Dioxus hosts React:** A Dioxus component can render a custom element (web component) that encapsulates a React widget, or use `wasm-bindgen` to call JavaScript functions.

While interop is possible, it's not free. The main challenges lie at the boundary: managing component lifecycles, sharing state, typing events, handling focus, unifying styling, and configuring the build process.

## 3. Incremental Migration Strategies

Here are four potential strategies, ordered from most to least recommended.

### Option 1: TypeScript Host with Dioxus Islands

The existing Vite/React app remains the host. New Dioxus crates compile to WASM and expose `mount` and `unmount` functions. A React component renders a container `<div>` and manages the lifecycle of the Dioxus island.

-   **Best For:** New settings panels, read-only visualizations, or standalone forms that don't rely on shared React context.
-   **Advantages:**
    -   Lowest initial blast radius.
    -   Core application remains stable.
    -   Allows for evaluation of Dioxus's performance, developer experience, and build tooling.
-   **Costs:**
    -   Requires writing and maintaining "glue" code for the boundary.
    -   State must be explicitly passed (serialized) between React and Dioxus.
    -   Increases bundle size with two frontend runtimes.

### Option 2: Feature-by-Feature Dioxus Pages

The React app and its router remain in control, but an entire page/route body is delegated to a Dioxus component.

-   **Best For:** Routes with clear data boundaries that can function with minimal shared UI state.
-   **Advantages:**
    -   Reduces the amount of fine-grained interop code compared to the island model.
    -   Allows a full feature to be built idiomatically in Dioxus.
-   **Costs:**
    -   Core services like navigation, auth, and API transport must be bridged.
    -   Requires building Rust equivalents of any shared UI components used on that page.

### Option 3: Parallel Dioxus App Shell

A new Dioxus application shell is developed in parallel to the existing React shells. This new shell would have its own router, state management, and transport logic. It could be deployed behind a feature flag or at a separate URL for development.

-   **Advantages:**
    -   Enables building the "ideal" Dioxus architecture from the start.
    -   Avoids complicating the existing React app with excessive interop layers.
-   **Costs:**
    -   Duplicates a significant amount of work for an extended period.
    -   High risk of behavior drifting between the two versions.

### Option 4: Big-Bang Rewrite (Not Recommended)

Rewriting the entire frontend in Dioxus at once. This is technically possible but carries an unacceptably high risk due to the complexity and maturity of the current application.

## 4. A Practical Migration Path (Phased Approach)

We recommend a multi-phase approach that starts small and progressively transfers ownership from React to Dioxus.

### Phase 0: Build and Contract Spike

The goal of this phase is to establish a working interop pattern, not to ship a feature. Create a small Dioxus crate that proves the following:

-   **Development:** A smooth local development workflow with hot-reloading for both React and Dioxus.
-   **Build:** Successful integration into the production Vite build.
-   **Interop Contract:** A clear, documented way to pass props from React to Dioxus and emit events from Dioxus back to React.
-   **Integration:** CSS/Tailwind reuse, basic API calls, and WASM loading in all environments (local, remote, Tauri).

**Example: React Host Component**
```tsx
import React, { useEffect, useRef } from 'react';
// These functions would be from your WASM bundle
import { mount, unmount } from 'my-dioxus-widget';

const DioxusWidgetHost = ({ someData }) => {
  const containerRef = useRef(null);

  useEffect(() => {
    if (containerRef.current) {
      // Mount the Dioxus component with initial props
      mount(containerRef.current, { initialData: someData });
    }

    // Define a custom event listener to get data back from Dioxus
    const handleDioxusEvent = (event) => {
      console.log('Event from Dioxus:', event.detail);
    };
    window.addEventListener('dioxus-custom-event', handleDioxusEvent);

    return () => {
      if (containerRef.current) {
        unmount(containerRef.current);
      }
      window.removeEventListener('dioxus-custom-event', handleDioxusEvent);
    };
  }, [someData]);

  return <div ref={containerRef} />;
};
```

### Phase 1: Define Frontend Facades

Before migrating complex features, create a set of framework-agnostic services ("facades"). These will act as a stable bridge that both React and Dioxus can use.

-   **API Transport:** A unified interface for `fetch`/`WebSocket` calls that handles local, remote WebRTC, and relay logic internally.
-   **Navigation:** A service for programmatic routing (`navigateTo('/path')`) that abstracts away the underlying router implementation.
-   **Runtime Services:** Access to auth status, user info, themes, and notifications.
-   **UI Tokens:** A single source of truth for design tokens (colors, spacing, etc.) consumable by both CSS (for React) and Rust (for Dioxus).

### Phase 2: Migrate Low-Risk Leaf Features

With the facades in place, begin migrating isolated pages or panels. Good candidates are features that are self-contained and don't rely heavily on global state.

### Phase 3: Migrate Whole Routes

Move entire route bodies to Dioxus. At this stage, Dioxus owns the page-local state and data loading for the migrated route. React's role is reduced to providing the mount point and routing parameters.

### Phase 4: Migrate the Core Workspace

This is the most complex phase and should only be attempted after the previous phases have proven successful. This involves porting the core real-time features of the application: data streams, chat, terminal, diffs, etc. This is a major undertaking that should be planned as a dedicated project.

### Phase 5: Invert the Host

The final step is to switch from a TypeScript-hosted app to a Dioxus-hosted app. This becomes viable only when Dioxus manages the majority of:

-   Top-level routing
-   Core application state and providers
-   Long-lived data streams
-   API transport

At this point, React becomes a compatibility layer for a few complex widgets that are not worth rewriting (e.g., the Lexical editor).

## 5. Analysis of High-Cost & Difficulty Areas

The migration of certain features will be significantly more complex than others.

### High-Difficulty Areas (Chart)

| Area | Difficulty | Reason | Recommended Strategy |
| :--- | :--- | :--- | :--- |
| **Chat Editor** | **Very High** | Deeply integrated with Lexical, a complex JS library. | Embed the JS widget in Dioxus. Do not rewrite. |
| **Routing** | **High** | TanStack Router is tied to the React component tree. | Create a navigation facade. Replace router late in migration. |
| **Transport/Remote** | **High** | Complex logic for WebRTC/relay connections. | Create a transport facade early. |
| **Shared State** | **High** | State is spread across multiple React-specific libraries. | Use facades and migrate state ownership incrementally. |
| **UI Library (`ui`)** | **High** | Relies on Radix and React-specific accessibility patterns. | Rewrite components incrementally. Budget significant time. |
| **Kanban Board** | **High** | Complex drag-and-drop interactions. | Embed a mature JS library or rewrite as a late-stage goal. |
| **Diffs & Logs** | **High** | Performance-sensitive virtualization and rendering. | Migrate only after stream and virtualization patterns are proven. |
| **Terminal** | **Medium** | Based on xterm.js. | Embed the JS widget in Dioxus. Do not rewrite. |
| **Tooling & DX** | **Medium** | Introduces a parallel Rust toolchain (cargo, rust-analyzer). | Address in Phase 0 to ensure a smooth developer workflow. |

## 6. Technical Deep Dives

### State Management Strategy

A key challenge is bridging the state management paradigms.

| React Concept | Dioxus Equivalent | Notes |
| :--- | :--- | :--- |
| `useState` / `useReducer` | `use_signal` / `use_reducer` | Dioxus signals are the foundation of its reactive system. |
| `useContext` | Dioxus Context API | Similar concept for providing state down the component tree. |
| Zustand | Dioxus `use_store` / Global Signals | Dioxus provides built-in primitives for global state. |
| TanStack Query | Dioxus `use_resource` | `use_resource` is used for managing async operations like data fetching. |

The strategy should be to gradually move state ownership into Dioxus's reactive system, using the facades to abstract the state source from the components.

### Styling Strategy

The document mentions Tailwind CSS. Sharing styles is critical.

-   **Initial Phase:** Run the Tailwind CLI as part of the main web application's build process. Dioxus components can use the generated CSS classes via the `class` attribute in RSX. The `build.rs` script in the Dioxus crate can be configured to ensure the Tailwind process runs when Rust code changes.
-   **Host Inversion Phase:** When Dioxus becomes the host, its build process (e.g., via `dioxus-cli`) would take ownership of running the Tailwind CLI.

The key is to ensure both the React and Dioxus parts of the app are pointing to the same generated CSS file.

### Developer Experience (DX)

Introducing a second language and toolchain has significant DX implications.

-   **Tooling:** Developers will need to be comfortable with both `npm`/`pnpm` and `cargo`, as well as their respective language servers (TypeScript LS and `rust-analyzer`).
-   **Hot-Reloading:** Dioxus has its own hot-reloading system. The goal of Phase 0 is to create a seamless experience where changes in both React and Dioxus code trigger the correct updates in the browser.
-   **Debugging:** Debugging WASM can be more challenging than debugging JavaScript. Developers will need to become familiar with browser devtools for WASM.

### Testing Strategy

The testing strategy must evolve with the migration.

-   **Unit & Integration Tests:** Dioxus has its own test harness for components. New Dioxus code should have comprehensive tests written in Rust.
-   **Contract Tests:** The interop boundaries (facades) must have strict contract tests to prevent regressions.
-   **End-to-End (E2E) Tests:** Use a tool like Playwright to run E2E tests on migrated user flows. These tests are framework-agnostic and are crucial for ensuring functional parity.
-   **Visual Regression Tests:** For complex UI, visual regression testing can catch subtle layout and styling issues.

## 7. Final Decision Guidance

This migration is a strategic investment in the long-term health and performance of the frontend.

**Use Dioxus for:**

-   New, isolated features where the performance and type-safety of Rust are a clear advantage.
-   Pages that can be built directly against our backend API contracts.
-   Future-proofing our frontend by aligning it with the Rust-based backend.

**Keep React/TypeScript (for now) for:**

-   The core workspace UI, which is complex and stable.
-   Areas with heavy reliance on mature JavaScript libraries (e.g., Lexical editor, xterm.js terminal).
-   The main application shell, until Dioxus is ready to take over hosting responsibilities.

The economic case for this migration rests on the desire for a more robust, performant, and unified (Rust-first) frontend platform. If the primary goal is simply to use Rust for a few UI components, then stopping at the "Dioxus Islands" stage is a valid and much lower-cost option. If the ambition is a full architectural evolution, then the phased rewrite is the recommended path forward.

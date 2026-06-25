---
title: "Dioxus Frontend Migration"
description: "Evaluate whether the Vibe Kanban React frontend can move to Dioxus incrementally, where the migration is expensive, and when the host runtime should switch from TypeScript to Rust."
---

## Executive Summary

An incremental migration from the current React and TypeScript frontend to
Dioxus is possible, but it should be treated as a multi-phase rewrite rather
than a component-by-component translation with low switching cost.

The most practical starting point is TypeScript-hosted Dioxus: keep
`packages/local-web`, `packages/remote-web`, and the existing React route tree as
the app shell, then mount isolated Dioxus/WebAssembly islands for new or
self-contained UI. That lets the team validate Dioxus build, packaging, styling,
browser API access, and Rust-to-JavaScript interop without disturbing the core
workspace, chat, diff, terminal, remote relay, or cloud flows.

The harder and more strategically important migration is Dioxus-hosted
TypeScript: a Rust/Dioxus app shell owns routing, providers, long-lived
connections, shared state, and page composition, while selected TypeScript
widgets remain embedded for mature browser libraries that are not worth
rewriting immediately. That inversion is worth considering only after most
product state and page composition has moved to Rust. Until then, switching the
host would duplicate the existing shell and force the highest-risk work early.

Recommended posture:

- Do not start by rewriting `web-core` wholesale.
- Start with a Dioxus spike in one leaf surface with narrow state and no
  dependency on React context.
- Keep the existing backend API and generated Rust source types as the durable
  contract.
- Introduce an explicit frontend facade layer before moving large features:
  transport, route navigation, query/cache semantics, runtime services, and
  design tokens.
- Expect the high-cost areas to be shared state, routing, drag/drop kanban,
  chat/editor, diffs, terminal, virtualization, and remote host transport.

## Current Frontend Shape

The current frontend has three important layers:

- `packages/local-web`: local browser/Tauri React app shell. It owns bootstrap,
  Sentry/PostHog, Tauri listeners, local route files, local providers, and the
  local API transport.
- `packages/remote-web`: cloud React app shell. It owns remote auth, account and
  invitation routes, host navigation, remote API setup, and the local API
  transport override for WebRTC or relay access to a selected host.
- `packages/web-core`: shared React product experience. It contains workspace
  UI, kanban UI, chat, create flows, dialogs, hooks, stores, API helpers,
  keyboard handling, i18n glue, and shared layouts.

The dependency graph is React-shaped all the way through the product layer.
`web-core` and `packages/ui` depend on React context, hooks, TanStack Query,
TanStack Router integration, Zustand stores, Radix/shadcn-style components,
Lexical, CodeMirror, xterm, React DnD libraries, React virtualization, and
React-specific diff and markdown renderers.

That means the existing split between app shells and shared feature modules is a
good migration boundary, but not a free one. A Dioxus migration needs new Rust
equivalents for the product shell contracts, not just RSX versions of JSX
components.

## What Dioxus Changes

Dioxus web apps compile to WebAssembly and render into the browser DOM. Current
Dioxus documentation describes the web target as its best-supported target and
notes that browser APIs are available through `wasm-bindgen`. Dioxus also
supports running JavaScript through `document::eval`, and Dioxus RSX can render
web components directly. These capabilities make interop possible in both
directions:

- React/TypeScript can host a Dioxus island by loading a WASM bundle and
  mounting it into a DOM node.
- Dioxus can host TypeScript widgets through custom elements, JavaScript
  modules, `eval`, `wasm-bindgen` bindings, or imperative mount/unmount wrappers.

Interop is possible, but it is not the same as native composition. The expensive
parts are lifecycle, state ownership, event typing, focus management, styling,
bundling, and test coverage across the boundary.

References:

- Dioxus web guide: https://dioxuslabs.com/learn/0.7/guides/platforms/web/
- Dioxus JavaScript and DOM escape hatches:
  https://dioxuslabs.com/learn/0.7/essentials/ui/escape/
- Dioxus project structure notes:
  https://dioxuslabs.com/learn/0.7/beyond/project_structure/

## Incremental Migration Options

### Option 1: TypeScript Host With Dioxus Islands

Keep the current Vite/React app as the page owner. Add one or more Dioxus crates
that compile to WASM and expose mount/unmount functions. React components render
a host `<div>`, call the Dioxus mount function, pass serialized props, and listen
for custom DOM events or callbacks.

Best candidates:

- A new settings panel or diagnostic panel.
- A read-only visualization.
- A standalone workflow that talks directly to HTTP endpoints.
- A component that does not need React context, TanStack Router, drag/drop,
  Lexical, xterm, or React virtualization.

Advantages:

- Lowest initial blast radius.
- Existing local and remote apps keep working.
- Existing React routes, auth, transport, query cache, and Tauri setup remain
  intact.
- The team can evaluate Dioxus WASM size, load behavior, hot reload,
  accessibility, Tailwind/CSS reuse, CI, and packaging.

Costs:

- Boundary glue must be written and maintained.
- Dioxus islands cannot consume React context directly.
- Shared app state must be copied, serialized, or exposed through an adapter.
- Two frontend runtimes ship at once.
- Fine-grained islands can become more expensive than a normal rewrite because
  every island needs a boundary contract.

This option is best for evaluation and new isolated surfaces. It is not a good
long-term way to migrate every component in `packages/ui`.

### Option 2: Feature-by-Feature Dioxus Pages

Keep the React app shell and route tree, but replace an entire route body with a
Dioxus page. The route still comes from TanStack Router, but React delegates page
content to Dioxus. The Dioxus page owns its local state and calls backend APIs
through a Rust or JavaScript transport facade.

Best candidates:

- A route with a clear URL and data boundary.
- A feature whose data can be loaded directly from backend endpoints.
- A page that can avoid shared React providers or can receive enough context as
  explicit props.

Advantages:

- Fewer interop boundaries than component islands.
- Lets a whole feature use Dioxus state and RSX naturally.
- Preserves current app shell risk controls.
- Creates a realistic migration path for non-core pages before workspace pages.

Costs:

- Navigation, auth, runtime mode, route params, and transport must be bridged.
- Query cache behavior may diverge between React and Dioxus versions.
- Shared UI components need Rust equivalents or TypeScript wrappers.
- Feature-level parity testing becomes necessary.

This option is the most useful migration middle ground.

### Option 3: Parallel Dioxus App Shell

Create a new Dioxus app shell next to `packages/local-web` and
`packages/remote-web`. It owns its own router, providers, generated bindings,
transport adapters, and page layout. The React app continues to serve production
traffic while Dioxus reaches parity behind a flag or alternate development URL.

Advantages:

- Avoids distorting the React app with too many interop layers.
- Lets Dioxus own routing and state from the beginning.
- Good for proving the final architecture before committing to a full switch.

Costs:

- Duplicates product shell work for a long time.
- Requires parallel local and remote runtime behavior.
- Needs strong test fixtures to prevent behavior drift.
- The highest-risk systems still need to be rewritten eventually.

This option is useful after early spikes succeed, especially if the team wants a
clear Rust-first target without repeatedly embedding small islands.

### Option 4: Big-Bang Rewrite

Rewrite the app shell, `web-core`, and `packages/ui` in Dioxus before switching
users.

This is technically possible but not recommended. The current frontend has too
many mature flows and live connection types for a big-bang rewrite to be
predictable.

## Practical Migration Path

### Phase 0: Build and Contract Spike

Add a small Dioxus crate that compiles to web WASM and can be loaded from the
current Vite app. The spike should prove:

- Local development workflow.
- Production build integration.
- WASM asset loading under local web, remote web, and Tauri.
- CSS and design token reuse.
- Prop passing from React to Dioxus.
- Event passing from Dioxus to React.
- Basic HTTP call to the local API.
- CI checks for the Rust/WASM package.

The output of this phase should be a documented interop contract, not a product
rewrite.

### Phase 1: Frontend Facades

Before moving any complex feature, define frontend contracts that both React and
Dioxus can use:

- API transport: direct local fetch/WebSocket, remote WebRTC, and relay fallback.
- Navigation: workspace, host, project, issue, draft, and VS Code routes.
- Runtime services: auth, user system, theme, notifications, telemetry, Tauri.
- Generated API types: continue deriving from Rust and generating TypeScript,
  but also expose Rust-native frontend types for Dioxus.
- UI tokens: colors, spacing, typography, layout primitives, and responsive
  breakpoints.

This prevents the Dioxus implementation from re-learning product behavior by
reading React hooks one at a time.

### Phase 2: Low-Risk Leaf Features

Move isolated pages or panels first. Good candidates are surfaces that load data
once, submit a form, and close. Avoid the workspace screen, chat composer,
terminal, kanban board, and diff viewer at this stage.

Success criteria:

- Same behavior in local browser, Tauri, and remote web where applicable.
- No global React state dependency except explicit props and events.
- No duplicated backend endpoint semantics.
- Clear rollback path to the React version.

### Phase 3: Whole-Route Migration

Move a route body at a time. At this point, Dioxus should own the page-local
state and data loading for that route. React should only provide route params,
runtime services, and a mount point.

Good route candidates are pages with clear URL ownership and limited long-lived
connection state. Cloud account, settings, and non-workspace project management
surfaces are better candidates than active workspace execution.

### Phase 4: Workspace Runtime Migration

Move the core workspace experience only after transport, state, and UI
primitives are proven. This phase includes:

- Workspace list and detail streams.
- Session and execution-process streams.
- Chat timeline and normalized logs.
- Raw logs.
- Diff stream and diff store.
- Terminal tabs.
- Preview controls and preview browser.
- Workspace actions, approvals, retry, merge, push, rebase, and branch changes.

This is the point where the migration becomes a product-platform rewrite. It
should be planned as a major project with parity tests and staged rollout.

### Phase 5: Host Inversion

Switch from TypeScript-hosted Dioxus to Dioxus-hosted TypeScript when Dioxus owns
most of these:

- Top-level routing.
- Runtime providers.
- Workspace and project page composition.
- Long-lived stream lifecycle.
- API transport selection.
- Design-system primitives.
- Modal/dialog ownership.
- Keyboard command ownership.

After that point, React becomes the compatibility layer for remaining widgets
instead of the application host.

## High-Cost and High-Difficulty Areas

### Routing and App Shells

Current local and remote route trees are generated by TanStack Router. Route
structure is deeply tied to local versus remote runtime behavior, host-scoped
URLs, project and issue context, workspace drafts, VS Code routes, auth, and
navigation helpers.

Dioxus has its own router model. A migration must either bridge from TanStack
Router into Dioxus route state or replace route ownership. Bridging is good
early; replacing route ownership is a host-inversion milestone.

Difficulty: high.

### Transport and Remote Host Access

The local app can call `/api` directly. The remote app installs a transport
override so host-scoped local API traffic can go through WebRTC or signed relay
HTTP/WebSocket sessions. This behavior is central to remote workspace usage.

Any Dioxus page that touches workspace data must share the same logical
transport semantics. Reimplementing this too early risks subtle local/remote
divergence.

Difficulty: high.

### Long-Lived Streams

The workspace UI uses multiple concurrent streams: workspace lists, workspace
diffs, execution processes, normalized logs, raw logs, terminal tabs, and remote
Electric shapes. Existing hooks own reconnect behavior, initialization state,
JSON Patch application, batching, and lifecycle cleanup.

Dioxus can model these flows, but the migration is not mechanical. The team must
port stream state machines and cache semantics carefully.

Difficulty: high.

### Shared State and Cache Semantics

React state is spread across TanStack Query, Zustand, React context providers,
custom hooks, local storage scratch state, and derived route state. Dioxus uses a
different reactive model with signals, stores, resources, and component-scoped
state.

The hard part is not choosing Dioxus equivalents. The hard part is preserving
invalidation, optimistic updates, derived state, and component lifetime behavior.

Difficulty: high.

### UI Library and Accessibility

`packages/ui` is React-specific and uses Radix primitives, shadcn-style wrappers,
React refs, React event types, Lexical plugins, drag/drop libraries, and
React-only virtualization. Rewriting this package in Dioxus means rebuilding a
large amount of accessible interaction behavior.

Simple display components are cheap. Dialogs, dropdowns, comboboxes, command
menus, typeahead, keyboard navigation, focus traps, and mobile drawers are not.

Difficulty: high.

### Kanban Drag and Drop

The kanban board depends on mature drag/drop behavior, hit testing, keyboard
interaction, auto-scroll, multi-select, and remote issue updates. Recreating this
in Dioxus will likely require either a Rust implementation or a JavaScript
library wrapper.

This is a poor first migration target.

Difficulty: high.

### Chat Composer and Rich Text Editing

The current editor stack uses Lexical and many custom plugins for markdown,
mentions/typeahead, attachments, images, code, PR comments, paste handling, and
keyboard behavior. This is one of the least attractive areas to rewrite early.

The practical Dioxus strategy is to embed the existing TypeScript editor as a
custom element or imperative widget until a Rust-native editor story is proven.

Difficulty: very high.

### Diffs, Logs, and Virtualization

Diff and log views depend on specialized rendering, syntax highlighting,
virtualization, scroll sync, and high-volume incremental updates. Performance and
scroll behavior are user-visible.

These can move to Dioxus, but only after the stream model and virtualization
strategy are clear.

Difficulty: high.

### Terminal

The terminal uses xterm and addons. Rewriting terminal behavior in Rust is not a
good use of time. The likely long-term strategy is to embed xterm as a
TypeScript/JavaScript widget inside Dioxus and keep the Rust side responsible for
connection lifecycle and typed events.

Difficulty: medium if embedded, very high if rewritten.

### Tauri and Desktop Behavior

The local app also runs inside Tauri. Existing desktop behavior includes
notification navigation, update handling, zoom, clipboard paths, drag regions,
and platform-specific behavior.

Dioxus can run in a browser/Tauri-style setup, and Dioxus documentation points
to Tauri when direct web APIs are needed across desktop and mobile. Still, this
repo already has a Tauri shell. Migration should preserve the existing Tauri
contract before considering any larger desktop runtime change.

Difficulty: medium to high.

### Tooling and Developer Experience

The current frontend uses Vite, TypeScript, Prettier, ESLint, TanStack Router
generation, and React Fast Refresh. Dioxus introduces Rust compile times, WASM
packaging, Dioxus hot reload behavior, Rust formatting, and potentially another
asset pipeline.

This is manageable, but it must be made boring before product work depends on
it.

Difficulty: medium.

## When to Invert the Host

The migration starts as TypeScript with embedded Dioxus because the current
React app owns the shell and most product behavior. Inverting too early would
force the Dioxus app to duplicate:

- Local and remote route trees.
- Auth and user runtime.
- WebRTC and relay transport selection.
- Workspace provider stack.
- Modal and command ownership.
- Keyboard handling.
- Shared UI primitives.
- Tauri-specific behavior.

The inversion point arrives when the TypeScript host is mostly a compatibility
wrapper. A practical threshold is:

- At least one major workspace or project route is fully Dioxus-owned.
- Dioxus owns the API transport facade and long-lived stream lifecycle.
- Most new feature work is happening in Rust.
- Remaining React code is limited to expensive embedded widgets such as Lexical,
  xterm, CodeMirror, or a drag/drop board.
- The Dioxus shell can run local browser, Tauri, and remote cloud entrypoints
  with equivalent navigation and auth behavior.

Before that threshold, keep React as the host. After that threshold, Dioxus as
the host reduces duplicated route, provider, and state ownership.

## TypeScript Embedded in Dioxus

Assuming the project reaches Dioxus-hosted TypeScript, the likely embedding
patterns are:

- Custom elements for widgets with clear attribute/event boundaries.
- Imperative mount/unmount wrappers for React widgets that need a React root.
- `wasm-bindgen` bindings for focused JavaScript APIs.
- Dioxus `document::eval` for small escape hatches, not primary architecture.
- Direct web components for third-party browser widgets when available.

Good embedded TypeScript candidates:

- Lexical editor.
- xterm terminal.
- CodeMirror editor.
- Mature drag/drop board if a Dioxus-native version is not ready.
- Complex markdown/diff renderers where JavaScript libraries remain better.

The Dioxus side should own data, transport, and page lifecycle. The embedded
TypeScript widget should receive explicit props and emit explicit events.

## Testing Strategy

Migration should add parity tests before moving core flows:

- Contract tests for transport behavior: local direct, remote WebRTC, and relay
  fallback.
- Stream reducer tests for JSON Patch snapshots, patches, reconnects, and
  finished states.
- Route parity tests for local and remote URL generation.
- Playwright tests for migrated user workflows.
- Visual checks for dense workspace, kanban, chat, and mobile layouts.
- Tauri smoke tests for desktop-only behavior after any shell change.

For early Dioxus islands, smoke tests are enough. For workspace migration, parity
tests are mandatory.

## Decision Guidance

Use Dioxus for:

- New isolated surfaces where Rust ownership is valuable.
- Pages that can directly consume backend API contracts.
- Future Rust-first product areas where shared types and backend behavior matter
  more than JavaScript ecosystem libraries.

Keep React/TypeScript for now for:

- Core workspace execution UI.
- Chat composer and rich text editing.
- Terminal.
- Kanban drag/drop.
- Diff and log rendering until stream and virtualization behavior is proven.
- Remote relay/WebRTC transport until a shared facade exists.

The migration is feasible, but the economic case depends on whether the project
wants Rust to own the frontend product platform, not just some UI components. If
the goal is only smaller pockets of Rust UI, Dioxus islands are enough. If the
goal is a Rust-first frontend, plan for a staged shell inversion and budget the
workspace runtime as the main cost center.

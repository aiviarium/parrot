# Parrot protocol, version 1

This is the specification of the Parrot protocol and runtime. [Implementation profiles](#40-implementation-profiles)
say which parts are built first. Appendix E amends the sections it names and wins where they differ.

## Contents

- Part I, Core: [1](#1-overview) overview, [2](#2-conventions) conventions,
  [3](#3-transports) transports, [4](#4-handshake-versions-and-features) handshake,
  [5](#5-principals-scopes-and-isolation) principals, scopes and isolation, [6](#6-agents) agents,
  [7](#7-environments) environments, [8](#8-workspaces) workspaces,
  [9](#9-sessions-and-bindings) sessions and bindings, [10](#10-subscriptions-and-updates)
  subscriptions, [11](#11-state-and-turns) state and turns, [12](#12-items) items,
  [13](#13-prompts-and-the-queue) prompts and queue, [14](#14-requests) requests,
  [15](#15-options-and-permission-levels) options and levels, [16](#16-actions) actions,
  [17](#17-usage-and-budgets) usage and budgets
- Part II, Orchestration: [18](#18-the-session-tree) tree, [19](#19-tasks) tasks,
  [20](#20-waking-and-waiting) waking and waiting, [21](#21-requests-in-a-tree) requests in a tree,
  [22](#22-ceilings) ceilings, [23](#23-seeding-and-bundles) seeding, [24](#24-links) links
- Part III, Functions: [25](#25-functions) functions, [26](#26-exposure-to-harnesses) exposure,
  [27](#27-calls-from-harness-code) calls from harness code, [28](#28-the-session-functions) session functions,
  [29](#29-the-desktop-functions) desktop
- Part IV, Drivers and runtime: [30](#30-driver-protocol) driver protocol,
  [31](#31-driver-data) driver data, [32](#32-sources-and-tui-control) sources and TUI,
  [33](#33-degradations) degradations, [34](#34-durability-and-restarts) durability,
  [35](#35-storage) storage, [36](#36-notifications) notifications, [37](#37-errors) errors,
  [38](#38-extensions-schema-and-conformance) extensions, schema and conformance,
  [39](#39-method-index) method index, [40](#40-implementation-profiles) profiles
- Appendices: [A](#appendix-a-differences-from-acp-v2) ACP v2, [B](#appendix-b-t3-code) T3 Code,
  [C](#appendix-c-harness-mapping-sketch) harness mapping, [D](#appendix-d-bundle-rendering) bundle
  rendering

# Part I: Core

## 1. Overview

| Peer | Is |
| --- | --- |
| client | A UI, CLI, phone, bridge or orchestrator that drives sessions |
| runtime | `parrot serve` or the embedded library. It owns sessions and their logs, queues, subscriptions, principals, ceilings, workspaces and functions, and links to other runtimes |
| driver | One harness made into an agent: built into the runtime, or a separate process |

- A runtime speaks the client protocol (Parts I to III) to clients and the
  [driver protocol](#30-driver-protocol) to drivers. They share framing, errors, items, requests,
  options and actions; the driver protocol has its own methods and uses driver keys instead of ids.
- Every built-in driver can run alone over stdio as `parrot driver <name>`, so built-in and process
  drivers are tested the same way.

| Word | Meaning |
| --- | --- |
| session | A conversation Parrot owns. Stable id, log, workspace, place in a tree, and over time one or more bindings |
| binding | One harness run of a session: an agent in an environment, with the harness's own session reference. Handing a session to another harness or machine makes a new binding; the session id stays |
| environment | A machine or container where bindings run and workspaces live. Each runtime has exactly one |
| workspace | A folder a session works in, tracked by the runtime |
| principal | Who makes a call: a connection, a session acting through its agent, or a linked runtime |
| function | A typed call offered by the runtime, a client or a linked runtime |
| effect | What a call may change: `read`, `write`, `turn` (starts agent work), `destructive`. One vocabulary for actions, functions and ceilings |

## 2. Conventions

- Types are TypeScript notation; `?` is optional. Every object may carry `_meta`
  ([38](#38-extensions-schema-and-conformance)). A string union marked *open* may receive other
  values, which peers keep and pass on.
- `Json` is any JSON value; `JsonSchema` is JSON Schema 2020-12. Times are RFC 3339 in UTC.
- **Ids** are a type prefix, `_`, and 26 Crockford base32 characters. Ids the runtime creates are
  ULIDs. Ids derived from harness data are the first 128 bits of SHA-256 over their inputs
  ([30.4](#304-keys-and-ids)), so the same history always gets the same ids. Prefixes: `ses`, `bnd`,
  `msg`, `itm`, `req`, `trn`, `q`, `wsp`, `env`, `con`, `rt`, `tsk`, `sub` (subscription), `lnk`,
  `tok`. Clients don't parse ids. Ids are never rewritten when crossing a link.
- **Paths** are absolute and native to their environment. On Windows drive letters are upper case and
  comparison ignores case; on macOS comparison follows the volume. `file:` URIs follow RFC 8089.
  Filters and fences compare normalised paths after resolving `..` and symbolic links.
- **Patterns** (in ceilings and tokens) match dot-separated names: `*` matches one segment, `**` any
  number, as in `parrot.sessions.*` or `rookery.**`.
- **Keyed** methods take `key?: string`. A repeat of the same method with the same key by the same
  principal identity (not connection) within 24 hours returns the first result.

## 3. Transports

### 3.1 Messages and framing

- JSON-RPC 2.0; batches are rejected with `-32600`. The runtime sends requests to a client only to
  call functions the client provides; everything else it sends is a notification.
- stdio and local sockets carry one JSON message per line, UTF-8. Readers accept `\n` and `\r\n`. A
  line over the limit (default 32 MiB) closes the connection. A process driver's standard error is
  logs, drained continuously, last 1 MiB kept.
- WebSocket carries one message per text frame, same limit.

### 3.2 Transports

| Transport | Used by | Identity |
| --- | --- | --- |
| stdio | Process drivers, `parrot driver`, `parrot connect` (below) | The user at both ends |
| Link carriers | Links between runtimes ([24.2](#242-carriers)) | The peer runtime's key, never the carrier |
| Local socket | Clients of a local `parrot serve` | The connecting OS user, by peer credentials |
| WebSocket | Remote clients and phones | A client token |

`parrot connect` relays its standard input and output to the local runtime's socket, as the user who
runs it. `parrot connect --link` does the same for a link carrier: the connection starts as an
unauthenticated link candidate with no scopes until the link handshake succeeds ([24.3](#243-pairing-and-authentication)).

### 3.3 Local socket

| OS | Path |
| --- | --- |
| Linux | `$XDG_RUNTIME_DIR/parrot/sock`; if unset, `~/.local/state/parrot/sock` |
| macOS | `~/Library/Application Support/parrot/sock`; if over 104 bytes the runtime refuses to start and names the path |
| Windows | `\\.\pipe\parrot-<user SID>` |

- The Unix directory has mode `0700` and must be owned by the user. The runtime accepts only peers
  with its own uid (`SO_PEERCRED`, `getpeereid`).
- The Windows pipe has an ACL for the user only, `PIPE_REJECT_REMOTE_CLIENTS` and
  `FILE_FLAG_FIRST_PIPE_INSTANCE`.
- A stale socket is removed at start.

### 3.4 WebSocket and client tokens

- Path `/parrot`, subprotocol `parrot.1`, TLS required unless bound to loopback.
- A client token is passed as a second subprotocol, `parrot.token.<token>`, because browsers can't set
  headers on an upgrade.

```ts
interface Token {
  id: string
  name: string
  scopes: Scope[]
  sessions?: string[]              // limits the token to these sessions' subtrees
  expires?: string
  created: string
  lastUsed?: string
}
```

| Method | Params | Result |
| --- | --- | --- |
| `token/create` | `{ name, scopes, sessions?, expires? }` | `{ token: Token, secret: string }` (shown once) |
| `token/list` | `{}` | `{ tokens }` |
| `token/revoke` | `{ token }` | `{}`; its connections close |

Token methods are for the owner on a local connection only. Secrets are stored hashed.

## 4. Handshake, versions and features

The client sends `initialize` first; anything else fails with `-32010`.

```ts
interface InitializeParams {
  protocol: number[]
  peer: PeerInfo
  features: Features
  provides?: FunctionSpec[]          // functions this client offers (25.2)
}
interface InitializeResult {
  protocol: number
  peer: PeerInfo
  runtime: { id: string; env: string }
  features: Features                 // includes the profiles implemented (40)
  connection: { id: string; principal: string; scopes: Scope[]; sessions?: string[] }
}
interface PeerInfo { kind: "client" | "runtime" | "driver"; name: string; version: string }
type Features = Record<string, object>
```

- No shared version fails with `-32011` and `data.supported`.
- Within a version, changes are additive: optional fields, new methods announced as features, new
  values of open unions. Anything else is a new version, served beside the old one for at least six
  months once released.
- Peers ignore what they don't know and keep unknown fields when storing and forwarding.
- Feature values are objects; absent means unsupported. Experimental features use `x.parrot/`, vendor
  features a reverse domain (`dev.helight/`).

Client features: `client.forms`, `client.notify`, `client.functions`.

## 5. Principals, scopes and isolation

### 5.1 Principals

```ts
type Principal = string   // "con_…" connection, "ses_…" session acting through its agent, "lnk_…" linked runtime
```

- Every call has a principal, recorded in the `origin` of what it causes.
- A **connection** has the scopes of its identity: every scope for the owner on stdio or the local
  socket; a token's scopes, limited to its `sessions`, on WebSocket.
- A **session** acts only on its own subtree: itself and its descendants. Its scopes are its `grant`,
  narrowed by its ceiling. A root's grant is empty unless its creator gives one, never beyond the
  creator's scopes. A child's grant is at most its parent's.
- A **link** acts with the scopes its peer granted, on what the peer offered ([24](#24-links)).

### 5.2 Scopes

```ts
type Scope = "read" | "prompt" | "answer" | "configure" | "manage" | "delegate" | "workspace" | "destructive" | "functions"
```

| Scope | Allows |
| --- | --- |
| `read` | Listing and reading; subscribing; `session/bundle` without a summary |
| `prompt` | Prompts, cancel, the queue, actions whose effects are only `turn` or `read` |
| `answer` | Answering requests |
| `configure` | Setting options, actions with `write` effects |
| `manage` | Root sessions, starting and stopping bindings, renaming, archiving, adopting, agent-scoped actions such as login |
| `delegate` | Children, tasks, seeding, rebinding, `session/bundle` with a summary |
| `workspace` | Creating, merging and transferring workspaces |
| `destructive` | Deleting sessions, removing workspaces, rewinding, `destructive` effects |
| `functions` | Providing functions; calling functions other than the session functions |

Scopes combine freely. Calls outside the principal's scopes, subtree or ceiling fail with `-32003`.
[39](#39-method-index) gives the scope of every method.

### 5.3 Threat model and isolation

Parrot's principals, ceilings and fences are enforced by the runtime on calls it receives. An agent
whose harness can run arbitrary commands as the owner's OS user can bypass them: it can open the local
socket, which grants the owner every scope, or read the runtime's state directory. What Parrot
guarantees therefore depends on a binding's isolation:

```ts
type Isolation =
  | "none"        // runs as the owner, unsandboxed: limits are advisory
  | "sandbox"     // the harness's own OS-enforced sandbox denies the socket and the state directory
  | "user"        // runs as another OS user without access to the socket or state directory
  | "container"   // runs in a container or VM that has neither
```

- Each binding reports its isolation (`Binding.isolation`). The driver computes it from driver data
  and the options it starts the harness with; when sandboxing, it adds the socket path and the state
  directory to the sandbox's deny list.
- A ceiling may require a minimum isolation (`Ceiling.isolation`). A child whose agent can't provide
  it fails to start with `-32003`.
- Below `sandbox`, the spec's limits hold only against agents that act through Parrot's functions; the
  runtime sets `Session.advisory: true` and clients show it.
- `user` and `container` isolation come from the environment: Rookery can create the OS user or
  container, and the runtime starts the harness there through a configured launcher.

## 6. Agents

```ts
interface Agent {
  id: string                       // "claude", or a configured variant like "claude-work"
  env: string
  name: string
  driver: { name: string; version: string; kind: "builtin" | "process"; data: string }   // data: driver data version
  harness?: { version: string; verified: boolean }   // verified: driver data covers this version
  installed: boolean
  auth: "signed_in" | "signed_out" | "unknown"
  usable: boolean
  detail?: string
  controls: ("native" | "tui")[]   // the first is the default
  features: Features
  levels: PermissionLevel[]        // levels this agent can be held to (15.2)
  isolation: Isolation[]           // isolation it can run with (5.3)
  options: Option[]                // defaults for a new binding
  actions: Action[]                // agent-scoped actions, such as login
}
```

| Feature | Meaning |
| --- | --- |
| `session.steer` | A prompt during a turn reaches it. Option `when: "next_step" \| "after_tools"` |
| `session.stream` | Items arrive while they happen; without it, in batches as the driver reads them |
| `session.resume` | A stopped harness session can be resumed |
| `session.history` | The driver reads sessions the harness stored outside Parrot (9.5) |
| `session.fork` | The harness forks natively. Option `at: ("turn" \| "message")[]` |
| `session.subagents` | The driver reports subagents the harness runs (18.2). Options `prompt`, `cancel` |
| `request.redeliver` | Open requests are redelivered after the driver reconnects to the harness |
| `attachments` | Options `kinds: ("image" \| "file")[]`, `maxBytes` |
| `usage` | Options `cost`, `context`, `rateLimits` |
| `code` | The harness runs model-written code that calls tools, and reports it ([27.3](#273-observing-harness-code)) |
| `functions.mcp` | The harness accepts an injected MCP server (26) |
| `hooks` | The driver reports hook runs as items |
| `mcp.status` | The driver reports the harness's MCP servers (9) |

| Method | Params | Result |
| --- | --- | --- |
| `agent/list` | `{ env? }` | `{ agents, hint?: string }`; `hint` says how to install a driver egg when there is none (31) |
| `agent/refresh` | `{ agent, env? }` | `{ agent }` |

## 7. Environments

```ts
interface Environment {
  id: string
  runtime: string
  name: string
  link?: string                    // absent for the local environment
  os: "linux" | "macos" | "windows"
  arch: string
  status: "online" | "offline"
  roots: string[]                  // folders workspaces may be in; default the owner's home
  features: Features               // "git", "workspace.worktree", "isolation.user", "isolation.container", "desktop"
}
```

| Method | Params | Result |
| --- | --- | --- |
| `env/list` | `{}` | `{ envs }` |

A runtime has one environment. Others appear through links. A call that needs an offline environment
fails with `-32030`.

## 8. Workspaces

```ts
interface Workspace {
  id: string
  env: string
  kind: "dir" | "worktree" | "external"   // external: managed by another system (a T3 project); no git operations
  path: string
  repo?: string
  branch?: string
  base?: string
  owner?: string                   // the session that created a worktree
  sessions: string[]               // sessions using it now
  created: string
}
type WorkspaceRequest =
  | { kind: "dir"; path: string }
  | { kind: "worktree"; repo: string; branch?: string; base?: string }   // branch defaults to parrot/<session id>
  | { kind: "use"; workspace: string }
type Carry = "commits" | "staged" | "unstaged" | "untracked"
```

| Method | Params | Result |
| --- | --- | --- |
| `workspace/create` | `{ env, request, key? }` | `{ workspace }` |
| `workspace/list` | `{ env?, session?, cursor?, limit? }` | `{ workspaces, next? }` |
| `workspace/status` | `{ workspace }` | `{ branch?, commit?, ahead?, behind?, dirty, changed: string[] }` |
| `workspace/merge` | `{ from, into, method: "merge" \| "squash", message?, key? }` | `{ commit?, conflicts?: string[] }` |
| `workspace/transfer` | `{ workspace, to: string, include?: Carry[], key? }` | `{ workspace }` |
| `workspace/remove` | `{ workspace, force?: boolean }` | `{}` |

- **Fences.** A workspace must be inside one of its environment's `roots` and inside the creating
  principal's ceiling `paths`. A session may only use workspaces inside its ceiling's paths.
- **Removal** refuses a workspace that sessions use, that has uncommitted changes, or that has commits
  not reachable from another branch, unless `force` (which needs `destructive`). `dir` and `external`
  workspaces are only forgotten, never deleted.
- **Lifetime.** Worktrees live until removed. Stopping or deleting a session never removes a workspace.
- **Transfer** sends commits as a git bundle over the link, plus carried uncommitted changes as a
  patch. Untracked files only with `untracked`; ignored files and Parrot's checkpoint refs never.
  Nothing is pushed to a git remote. Non-git workspaces can't be transferred (`-32004`).
- **Merge** is between workspaces of one repository in one environment. Conflicts leave `into`
  unchanged and are listed.

## 9. Sessions and bindings

```ts
interface Session {
  id: string
  runtime: string                  // the owning runtime: where the current binding runs
  movedTo?: string                 // set on a runtime the session left (9.3)
  title?: string
  workspace: string
  cwd: string
  state: State
  background: Background[]
  binding?: Binding
  bindings: string[]
  kind: "root" | "child" | "embedded"
  parent?: { session: string; item?: string }
  task?: Task
  seed?: Seed
  origin: Origin
  grant: Scope[]
  ceiling: Ceiling
  advisory: boolean                // the binding's isolation is below sandbox (5.3)
  budget?: BudgetState
  mcp?: { name: string; status: "connected" | "failed" | "pending" | "needs_auth"; tools?: number }[]
  created: string
  updated: string
  archived: boolean
}
interface SessionSummary {         // what lists and the index feed carry
  id: string; runtime: string; title?: string; state: State; kind: Session["kind"]
  parent?: string; agent?: string; env?: string; task?: Task["status"]; archived: boolean; updated: string
}
interface Binding {
  id: string
  session: string
  agent: string
  env: string
  control: "native" | "tui"
  ref?: Ref
  isolation: Isolation
  level?: PermissionLevel          // the current permission level (15.2)
  network?: boolean
  status: "starting" | "running" | "stopped" | "lost" | "failed"
  via: "new" | "resume" | "adopt" | "fork" | "bundle"
  pending?: "bundle"               // a bundle waits to be prepended to the next prompt (23.2)
  started: string
  stopped?: string
}
interface Ref { id: string; strength: "strong" | "weak" }
interface Background { id: string; kind: "task" | "subagent" | "command" | string; title: string; item?: string }
```

### 9.1 Creating and listing

| Method | Params | Result |
| --- | --- | --- |
| `session/new` | `NewSession` (keyed) | `{ session, subscription, head }` |
| `session/list` | `{ parent?, workspace?, agent?, env?, archived?, cursor?, limit? }` | `{ sessions: SessionSummary[], next? }` |
| `session/get` | `{ session }` | `{ session }` |
| `session/tree` | `{ session, cursor?, limit? }` | `{ nodes: { session: SessionSummary; depth: number }[], next? }` |

```ts
interface NewSession {
  agent: string
  env?: string                     // default: the parent's env, else the local env
  workspace?: WorkspaceRequest     // required unless a seed carries the workspace (23.1): where work happens is always an explicit choice
  cwd?: string
  options?: Record<string, Json>
  title?: string
  control?: "native" | "tui"
  start?: boolean                  // default true
  grant?: Scope[]
  parent?: string                  // 18
  task?: TaskRequest               // 19
  seed?: SeedRequest               // 23
  ceiling?: Ceiling                // narrows the inherited one
  wake?: Wake                      // 20
  onParentEnd?: "continue" | "stop"    // default "continue"
  key?: string
}
```

- Returns when the session exists; its binding is `starting`. The caller is subscribed to it.
- A session on another environment is created by that environment's runtime, which owns it
  ([24](#24-links)). `session/list` merges the runtime's sessions with those its links offer.

### 9.2 Starting and stopping

| Method | Params | Result |
| --- | --- | --- |
| `session/start` | `{ session, options?, key? }` | `{ binding }` |
| `session/stop` | `{ session, tree?: boolean }` | `{}` |

- `session/start` resumes the last binding's harness session with `session.resume`; otherwise it
  starts a new binding with a pending bundle ([23.2](#232-bundles)).
- `session/stop` ends the harness process. The session, workspace and log stay.

### 9.3 Rebinding to another harness or machine

| Method | Params | Result |
| --- | --- | --- |
| `session/rebind` | `{ session, agent?, env?, workspace?: WorkspaceRequest, require?: "exact" \| "any", interrupt?: boolean, key? }` | `{ binding }` |

Steps, each recorded in the log before it runs ([34](#34-durability-and-restarts)):

1. `require: "exact"` that can't be met fails with `-32001` now. The target agent, environment, level
   and isolation must fit the session's ceiling.
2. If a turn runs, the call waits for it to end, or interrupts it with `interrupt: true`.
3. The queue pauses and the old binding stops. Two bindings never run in one workspace at once.
4. With `env` and no `workspace`, the workspace is transferred with commits, staged and unstaged
   changes.
5. The new binding starts: native resume when agent and harness session allow it, else a pending
   bundle.
6. If the new binding fails to start, the old one starts again (resumed where possible) and the call
   fails.
7. **To another environment:** the session moves to that environment's runtime with its log, queue,
   requests, links to parent and children, and task. The old runtime keeps a redirect
   (`Session.movedTo`) and answers calls for the session with `-32050` and `data.runtime`. Clients
   reconnect there, through the link or directly.
8. A `handoff` item records the change; the queue resumes unless it was paused before.

### 9.4 Rewind, rename, archive, delete

| Method | Params | Result |
| --- | --- | --- |
| `session/rewind` | `{ session, turn, files: boolean, key? }` | `{}` |
| `session/rename` | `{ session, title }` | `{}` |
| `session/archive` | `{ session, archived: boolean }` | `{}` |
| `session/delete` | `{ session }` | `{}` |

- `session/rewind` removes items after `turn` (they get `removed`) and restarts the binding by native
  fork at that turn where the agent has `session.fork`, else with a pending bundle cut at `turn`.
- With `files: true`, the workspace is restored from the runtime's checkpoint taken at the start of
  the turn after `turn` ([35](#35-storage)). Refused (`-32004`) for `dir` workspaces, workspaces other
  sessions use, and when there is no checkpoint. Harness file restore is never used: it misses changes
  made by commands.
- `session/delete` stops the session and deletes its log and checkpoints. Refused while it has running
  children.

### 9.5 Adopting harness sessions

| Method | Params | Result |
| --- | --- | --- |
| `session/discover` | `{ agent, cwd?, cursor?, limit? }` | `{ found: { ref, title?, cwd, updated }[], next? }` |
| `session/adopt` | `{ agent, ref, workspace?: WorkspaceRequest, start?: boolean, key? }` | `{ session, subscription, head }` |

- With `session.history`, `discover` lists harness sessions no Parrot session has, and `adopt` makes
  one from a harness session: its history becomes items with `source: "transcript"`; `start` resumes
  it. Without `workspace`, a `dir` workspace at the harness session's folder.
- Turns sent through the harness's own UI or remote control appear with `origin.kind` `"harness"` or
  `"remote"`.

## 10. Subscriptions and updates

### 10.1 The sequence

Every update a runtime emits takes the next number of one runtime-wide sequence, `seq`. A connection
receives its subscriptions' updates in `seq` order. Gaps are normal; connections are reliable and
ordered, so clients replay only after reconnecting.

### 10.2 Subscriptions

| Method | Params | Result |
| --- | --- | --- |
| `subscribe` | `{ target: Target, replayFrom?: number, items?: number }` | `{ subscription, head, snapshot? }` |
| `unsubscribe` | `{ subscription }` | `{}` |

```ts
type Target =
  | { session: string }
  | { tree: string }               // a session and its descendants in one stream
  | { index: true }                // 10.3
interface Snapshot {
  sessions?: SessionSnapshot[]     // the session, or the tree's nodes up to 50 (more through session/tree)
  index?: { sessions: SessionSummary[]; next?: string; agents: Agent[]; envs: Environment[] }
}
interface SessionSnapshot {
  session: Session
  items: Item[]                    // the newest `items` (default 50), oldest first
  before?: string
  requests: Request[]              // open requests of the session and its descendants
  queue: QueueState
  options: Option[]
  actions: Action[]
  usage?: Usage
  subscribers: ConnectionInfo[]
}
interface ConnectionInfo { id: string; name: string; principal: string; device?: string }
```

- Without `replayFrom`, the snapshot reflects every update up to `head`; later ones follow.
- With `replayFrom`, there is no snapshot; the runtime sends the target's updates after `replayFrom`
  up to `head`, then live ones, or fails with `-32012` if it no longer has them.
- A subscription to a session also delivers `request` updates of its descendants' open requests, so a
  client watching only the root sees every question its tree asks.
- A tree subscription includes sessions that join later and delivers each update once.
- A subscription to a session a linked runtime owns is forwarded over the link; its `seq` and `head`
  are the owner's. Clients keep one cursor per runtime.

### 10.3 The index feed

Reports what a session list shows: a `SessionSummary` when a session is created or changes state,
title, binding, task status or archive flag, and `deleted` when it goes; agents; environments;
workspaces created and removed. The snapshot pages sessions (`next`, continued with `session/list`).

### 10.4 Updates

```ts
interface UpdateParams {           // notification "update"
  subscription: string
  seq: number
  at: string
  session?: string
  update: Update
}
type Update =
  | { type: "state"; state: State; background: Background[]; detail?: string }
  | { type: "session"; session: Session }
  | { type: "item"; item: ItemPatch }
  | { type: "turn"; turn: Turn }
  | { type: "request"; request: Request }
  | { type: "queue"; queue: QueueState }
  | { type: "options"; options: Option[]; level?: PermissionLevel; network?: boolean }
  | { type: "actions"; actions: Action[] }
  | { type: "usage"; usage: Usage }
  | { type: "notice"; level: "info" | "warning" | "error"; text: string; code?: string }
  | { type: "summary"; session: SessionSummary }        // index
  | { type: "agent"; agent: Agent }                     // index
  | { type: "env"; env: Environment }                   // index
  | { type: "workspace"; workspace: Workspace }         // index
  | { type: "deleted"; kind: "session" | "workspace"; id: string }   // index
  | { type: "functions"; pinned: string[] }             // sessions with an MCP server (26)
```

- Only items are patched; everything else is sent whole.
- Unknown update types are ignored.

### 10.5 Reading

| Method | Params | Result |
| --- | --- | --- |
| `session/read` | `{ session, cursor?, direction?: "back" \| "forward", limit? }` | `{ items, before?, after? }` |
| `item/get` | `{ session, item }` | `{ item }`, untruncated |

## 11. State and turns

### 11.1 State

```ts
type State = "starting" | "idle" | "running" | "requires_action" | "stopped" | "lost" | "unrecognised" | "unknown"
```

- States are observations; clients accept any change.
- `requires_action` holds while the session, or an embedded descendant running in its process, has an
  open request with `blocking: true`.
- `unrecognised`: under TUI control, the screen matches no profile state ([32.2](#322-tui-control)).
- Background work doesn't change the state.

### 11.2 Turns

```ts
interface Turn {
  id: string
  binding: string
  status: "running" | "completed" | "interrupted" | "failed"
  stop?: "end" | "max_tokens" | "turn_limit" | "refusal" | "cancelled" | "budget" | "error" | string   // open
  continuable: boolean             // stopped short: max_tokens, turn_limit, interrupted, or a retryable error
  input?: string
  started: string
  ended?: string
  error?: { code: string; message: string; retryable: boolean }
  usage?: Usage
}
```

`continuable` turns are what the `continue` action is for ([16.1](#161-well-known-actions)).

## 12. Items

### 12.1 Patches

```ts
interface ItemPatch {
  id: string
  kind?: ItemKind                  // required on the first patch
  turn?: string
  binding?: string
  parent?: string                  // the script or tool this belongs under
  source?: "native" | "transcript" | "tui" | "inferred" | "runtime"
  ref?: Ref
  origin?: Origin
  set?: Record<string, Json>       // replace fields; null removes one
  append?: Record<string, string>  // append to string fields
  streaming?: boolean              // false: its text stream ended
  removed?: true                   // final
}
```

- The first patch creates the item; `kind` never changes. `set` applies before `append`; `_meta` in
  `set` merges by key.
- `streaming: false` ends the stream; later patches may still change fields.
- Text is UTF-8, split at character boundaries.
- Stored logs consolidate streaming: once an item's stream ends, its `append` patches are replaced by
  one `set` with the `seq` of the last of them ([35](#35-storage)). Replay sends the consolidated
  patch.

### 12.2 Kinds

Unknown kinds are kept and shown by `title` and `text` if present.

| Kind | Fields |
| --- | --- |
| `user` | `text`, `context?`, `attachments?`, `status: "queued" \| "sent" \| "handled" \| "uncertain"` |
| `assistant` | `text` (Markdown) |
| `reasoning` | `text`, `summary?: boolean` |
| `tool` | `tool`, `title`, `input`, `status: "pending" \| "running" \| "paused" \| "ok" \| "error" \| "cancelled"`, `output?`, `server?`, `caller: "model" \| "script"`, `function?` |
| `script` | `language`, `code?` (absent when the harness doesn't expose it), `status: "running" \| "ok" \| "error" \| "cancelled"`, `background: boolean`, `result?`, `error?` |
| `command` | `command`, `cwd`, `status`, `output?`, `exit?` |
| `file_change` | `changes: { path, op: "add" \| "modify" \| "delete" \| "move", from?, diff? }[]`, `status` |
| `plan` | `entries?: { text, status: "pending" \| "active" \| "done" }[]`, `text?` |
| `subagent` | `title`, `session`, `status` |
| `web` | `query?`, `url?`, `status`, `results?` |
| `review` | `target`, `findings?` |
| `compaction` | `trigger: "manual" \| "auto"`, `summary?` |
| `hook` | `event`, `name`, `outcome: "ok" \| "blocked" \| "error"`, `output?` |
| `handoff` | `from`, `to` (bindings), `via` |
| `error` | `message`, `code?` |

```ts
interface Origin {
  kind: "client" | "harness" | "remote" | "hook" | "session" | "wake" | "runtime"   // open
  principal?: string
  name?: string
  cause?: { session: string; item?: string }
  action?: string                  // set when a well-known action produced the item ("continue", "retry")
}
```

- `uncertain`: the runtime sent the prompt, then crashed before the harness confirmed it
  ([34](#34-durability-and-restarts)).
- Long `output` is truncated; `_meta["x.parrot/truncated"]` gives the full size, `item/get` all of it.

## 13. Prompts and the queue

### 13.1 Prompts

| Method | Params | Result |
| --- | --- | --- |
| `session/prompt` | `PromptParams` (keyed) | `PromptResult` |
| `session/cancel` | `{ session, tree?: boolean }` | `{ interrupted: boolean, queued: number }` |

```ts
interface PromptParams {
  session: string
  text: string
  context?: { uri: string; title?: string; text: string }[]
  attachments?: { name: string; type: string; data?: string; uri?: string }[]
  delivery?: "auto" | "queue" | "steer" | "interrupt"   // default "auto"
  key?: string
}
interface PromptResult { message: string; disposition: "started" | "steered" | "queued" | "handled"; queued?: string }
```

| `delivery` | Idle | Running |
| --- | --- | --- |
| `auto` | Starts a turn | Steers with `session.steer`, else queues |
| `queue` | Starts a turn | Queues |
| `steer` | Starts a turn | Steers, else `-32001` |
| `interrupt` | Starts a turn | Interrupts, then starts a turn with this message |

- A `stopped` session starts its binding first, which needs `manage` too.
- A pending bundle is prepended to the first prompt a new binding gets ([23.2](#232-bundles)).
- A refused steer goes to the front of the queue; its user item becomes `queued`, with a `notice`.
- A prompt that fails after acceptance ends its turn as `failed`; the call succeeded.
- `session/cancel` interrupts the turn and pauses the queue; with `tree`, across the subtree.

### 13.2 The queue

The runtime owns the queue; drivers never queue.

```ts
interface QueueState {
  paused: boolean
  entries: { id: string; message?: string; text: string; origin: Origin; kind: "prompt" | "wake" }[]
}
```

| Method | Params | Result |
| --- | --- | --- |
| `queue/remove` | `{ session, id }` | `{}` |
| `queue/edit` | `{ session, id, text }` | `{}` |
| `queue/move` | `{ session, id, index }` | `{}` |
| `queue/clear` | `{ session }` | `{}` |
| `queue/resume` | `{ session }` | `{}` |

- When the session is `idle` and the queue isn't paused, the first entry is sent.
- Only `queue/resume` and a client prompt with `delivery` other than `queue` unpause it. Wakes never
  do.
- A removed entry's user item gets `removed`.

## 14. Requests

```ts
interface Request {
  id: string
  session: string
  path: string[]                   // sessions from the root to this one
  ref?: Ref
  turn?: string
  item?: string
  kind: "approval" | "question" | "plan" | "input" | "auth" | string   // open
  title: string
  description?: string
  subject?: Subject
  level: PermissionLevel           // the lowest level that allows this without asking; "host" when unknown
  network?: boolean                // it needs network access
  choices?: Choice[]
  input?: JsonSchema               // enums as oneOf with const and title
  blocking: boolean
  answerable: "connection" | "process" | "message"
  expires?: string
  fallback?: Answer
  notify: "none" | "normal" | "urgent"
  status: "open" | "answered" | "expired" | "cancelled"
  answer?: Answer
  answeredBy?: string              // a principal, or "harness", "fallback", "ceiling"
}
interface Choice {
  id: string
  label: string
  intent?: "allow" | "deny" | "neutral"
  scope?: "once" | "session" | "always"
  amends?: Json
  description?: string
}
type Subject =                     // open
  | { type: "tool"; tool: string; input: Json; editable?: boolean }
  | { type: "command"; command: string; cwd: string }
  | { type: "files"; paths: string[]; diff?: string }
  | { type: "network"; host: string }
  | { type: "plan"; text: string }
  | { type: "function"; function: string; input: Json }
interface Answer { choice?: string; value?: Json; input?: Json; message?: string }
```

| Method | Params | Result |
| --- | --- | --- |
| `request/answer` | `{ request, answer, key? }` | `{}` |
| `request/list` | `{ tree?: string, session?: string, status?: "open", cursor? }` | `{ requests, next? }` |

| `answerable` | A client leaves | The driver reconnects to the harness | The harness process ends |
| --- | --- | --- | --- |
| `connection` | survives | expires | expires |
| `process` | survives | survives (deduplicated by `ref`) | expires |
| `message` | survives | survives | survives; the answer is sent as a follow-up prompt |

- Questions with several parts are one request with one `input` property per question.
- The first valid answer wins and goes to every subscriber; later ones fail with `-32013`, except a
  repeat with the same key.
- Under a ceiling, choices with `scope` other than `once` are removed before the request is shown, so
  an answer can't raise the session's level for later.

## 15. Options and permission levels

### 15.1 Options

```ts
interface Option {
  id: string
  role?: "model" | "effort" | "mode" | "sandbox" | "speed" | string   // open
  label: string
  description?: string
  schema: JsonSchema               // enums as oneOf: [{ const, title, description?, "x-details"? }]
  value: Json
  default?: Json
  applies: "start" | "now" | "next_turn"
  dependsOn?: { option: string; schemas: Record<string, JsonSchema> }
  complete?: boolean
}
```

| Method | Params | Result |
| --- | --- | --- |
| `option/set` | `{ session, values: Record<string, Json>, key? }` | `{ options, reset: string[], level?: PermissionLevel }` |
| `option/complete` | `{ session?, agent?, option, prefix, limit? }` | `{ values: { const: Json; title: string }[] }` |

- Options come from the harness at runtime where it reports them; driver data fills the rest.
- Several options may share a role.
- `dependsOn`: a dependent value outside its new schema resets to that schema's default, listed in
  `reset`. `dependsOn` and `complete` don't combine.
- Values apply together. `applies: "start"` options can't change while a binding runs (`-32004`).

### 15.2 Permission levels

A level says what the agent can do **without asking anyone**:

```ts
type PermissionLevel =
  | "read"        // read files in the workspace
  | "write"       // also edit files in the workspace
  | "sandboxed"   // also run commands in an OS sandbox limited to the workspace
  | "host"        // also run commands outside a sandbox, or touch files outside the workspace
```

Network access is a separate flag, `network`, on bindings, requests and ceilings.

- The driver computes the level from everything the harness enforces: options, and the settings
  sources it starts the harness with. Under a ceiling, the driver pins those sources (Claude's
  `--setting-sources`, Codex's `-c` overrides) so repository or user allow-rules can't raise the level
  unseen. Driver data maps each combination ([31](#31-driver-data)).
- An unmapped combination counts as `host` with network, so a mode a CLI update adds can't slip under
  a ceiling.
- `Agent.levels` lists the levels an agent can be held to. pi has no permission system and lists only
  `host`.
- The runtime checks the level after every options update the driver reports, including changes the
  harness makes itself. A level above the ceiling makes the runtime set the options back if it can,
  else interrupt the turn and stop the binding, with a `notice`.

## 16. Actions

```ts
interface Action {
  id: string
  name?: string                    // "/compact"
  label: string
  description?: string
  input?: JsonSchema
  hint?: string
  scope: "agent" | "session"
  when: ("idle" | "running")[]
  effects: ("read" | "write" | "turn" | "destructive")[]
  requires?: ("terminal")[]
  source: "harness" | "driver" | "skill" | "extension" | "runtime" | string
}
```

| Method | Params | Result |
| --- | --- | --- |
| `action/invoke` | `{ session?, agent?, env?, action, input?, args?, delivery?, key? }` | `{ status: "done" \| "started" \| "queued" \| "requested", message?, request?, result? }` |

- `args` is raw argument text for actions with only a `hint`.
- `turn` actions follow `delivery` like prompts.
- Slash commands sent as prompt text are not parsed by the runtime.

### 16.1 Well-known actions

Well-known ids have fixed meanings. `continue` and `retry` are offered for every session: no harness
in the research has a native continue, so the runtime's fallback ([33](#33-degradations)) is the
feature. The others are offered only where the harness supports them.

| Id | Effects | Meaning |
| --- | --- | --- |
| `continue` | turn | Carry on where the last turn stopped. Input `{ note?: string }`. Offered when the last turn is `continuable` or the task is `awaiting` |
| `retry` | turn | Run the failed turn's input again. Offered after a turn with a retryable error. To retry with other options, set them first with `option/set` |
| `compact` | turn | Compact the context. Input `{ instructions?: string }` where supported |
| `review` | turn | Review changes |
| `clear` | destructive | A fresh context in the same session |
| `summarize` | turn | Write a summary of the session; result `{ text }` |
| `login`, `logout` | write | Agent scope; `login` raises an `auth` request (a URL, a device code or a form) |
| `status` | read | The harness's own status report |

`retry` resends input to a harness whose earlier attempt may already have changed files. Its
description says so, and clients show the failed turn's file changes beside it.

## 17. Usage and budgets

### 17.1 Usage

```ts
interface Usage {
  tokens?: { input: number; output: number; cacheRead?: number; cacheWrite?: number; reasoning?: number }   // input excludes cache reads
  cost?: { amount: number; currency: string }
  context?: { used: number; window: number }
  rateLimits?: { name: string; used: number; limit?: number; resets?: string }[]
  countedIn?: string               // for embedded sessions: the session whose usage already includes this
}
```

### 17.2 Budgets

```ts
interface Budget {
  tokens?: number                  // input (without cache reads) plus output
  cost?: { amount: number; currency: string }
  seconds?: number                 // wall-clock time spent running
  turns?: number
  sessions?: number                // descendants created
}
interface BudgetState { limit: Budget; used: Budget }
```

- A ceiling's budget counts the whole subtree. Usage counts once, at the binding that reports it:
  embedded sessions whose usage the parent's harness already reports carry `countedIn` and add
  nothing.
- The runtime checks budgets on every usage update. When one runs out, it interrupts running turns in
  the subtree, fails their tasks with `stop: "budget"` and pauses their queues. A step already in
  flight can overshoot by its own size.
- A `cost` budget on an agent without `usage.cost` fails at creation with `-32001`.

# Part II: Orchestration

Orchestration uses Part I's entities: a child is a session, a handoff is a binding, a delegated agent
is a session principal. T3 Code keeps a separate MCP tool surface for agents, links servers by
signing one in to another's MCP endpoint, and models work as threads, runs and nodes. Parrot has one
surface, one tree and one identity per conversation.

## 18. The session tree

### 18.1 Kinds

| `Session.kind` | Is | Prompted by | Stopped by |
| --- | --- | --- | --- |
| `root` | Created by a connection without `parent` | Clients | Clients |
| `child` | Created with `parent`, by a client or by the parent's agent | The parent's agent and clients | The parent's agent and clients |
| `embedded` | A subagent the harness itself started: Claude's Task tool (in its process), Codex collab agents (threads with their own ids) | With `session.subagents` option `prompt` | With option `cancel` |

- The parent has a `subagent` item whose `session` is the child, created in the same update. Children
  the parent's agent created link back to the tool item that created them (`Session.parent.item`).
- `session/list { parent }`, `session/tree` and tree subscriptions give the subtree.
- A tree may span runtimes: a child on another environment is owned by that environment's runtime and
  its parent link points across the link. `onParentEnd: "stop"` is delivered when the link is up;
  while it is down, the child keeps running within its ceiling and budget.

### 18.2 Embedded sessions

- With `session.subagents`, the driver reports subagents ([30.3](#303-subagents)). The runtime creates
  an `embedded` session per subagent, with the parent's workspace, ceiling and environment, and the
  subagent's harness id as `Binding.ref` where it has one.
- Without the feature, subagent work stays as `tool` items in the parent.

## 19. Tasks

```ts
interface TaskRequest {
  brief: string                    // the child's first prompt
  title?: string
  result?: JsonSchema
  completeOn?: "finish" | "idle"   // default "finish" if the agent has functions.mcp, else "idle"
}
interface Task {
  id: string
  brief: string
  title?: string
  result?: JsonSchema
  completeOn: "finish" | "idle"
  status: "running" | "awaiting" | "done" | "failed" | "cancelled"
  output?: Json                    // structured if `result` was given, else { text }
  error?: string
  history: { id: string; status: Task["status"]; output?: Json }[]   // earlier tasks of this session
}
```

| Method | Params | Result |
| --- | --- | --- |
| `task/assign` | `{ session, task: TaskRequest, key? }` | `{ task }`. Gives a child a new task after its last one ended; the brief is prompted |
| `task/finish` | `{ session, output }` | `{}`. By the session's own principal only |
| `task/fail` | `{ session, error }` | `{}`. By the session's own principal, or its parent's |
| `task/wait` | `{ sessions: string[], since?: number, timeout?: number }` | `{ changes: { session, task }[], cursor: number, timedOut: boolean }` |

- `parrot.task.finish` validates the output against `result`; a wrong shape is returned to the agent
  as a function error so it can try again.
- With `completeOn: "finish"`, a child that goes idle without finishing becomes `awaiting`: it stopped
  without a result, usually to ask in prose. A prompt or `continue` sets it back to `running`.
- With `completeOn: "idle"`, going idle with no open request completes the task with the last
  assistant message as `{ text }`.
- A prompt to a child whose task is `done`, `failed` or `cancelled` doesn't change the task. New work
  with a result is a new task, given with `task/assign`.
- Interrupting a child's turn doesn't end its task. Stopping a child with a running task cancels it.

## 20. Waking and waiting

### 20.1 Waiting

`task/wait` returns task changes of the named sessions after `since`, a cursor from an earlier call
(the first call omits it and gets the current state). It returns at once if there are changes, else
when one happens or after `timeout` seconds. The default and maximum timeout come from driver data:
the harness's tool-call timeout minus a margin, so a wait called from a tool returns before the harness
gives up. The agent calls again with the cursor; nothing is lost between calls.

### 20.2 Waking

```ts
interface Wake { on: ("done" | "awaiting" | "failed" | "requires_action")[] }
```

- When a child's task or state reaches a value in its parent's `wake.on`, the change is recorded for
  the parent. When the parent is idle with an unpaused queue, the runtime sends one **wake entry**
  listing every recorded change not yet returned by a `task/wait` of the parent, and marks them
  delivered. Changes a wait already returned are never sent again.
- Wake entries are queue entries (`kind: "wake"`, `origin.kind: "wake"`, `origin.cause` the child).
  They never unpause a queue and never start a stopped parent; they wait until the parent next runs.
- The entry's text names the children, their new status and how to read more
  (`parrot.tasks.wait`, `parrot.sessions.read`).

## 21. Requests in a tree

- Only people answer requests: connections with `answer` whose token or scope covers the session.
  Parent agents never answer their children's requests; a parent's agent sees them
  (`parrot.requests.list`) and can wake on them, so it can tell the user.
- A child's requests reach every subscriber of its ancestors (10.2), with `path`.
- With no connection able to answer for `ceiling.unattended` seconds, a request that has a choice with
  `intent: "deny"` is denied with `answeredBy: "ceiling"`. Without `unattended`, it waits.

## 22. Ceilings

```ts
interface Ceiling {
  level?: PermissionLevel          // the highest permission level
  network?: boolean                // whether network access may be allowed without asking
  isolation?: Isolation            // the lowest isolation (5.3)
  effects?: ("read" | "write" | "turn" | "destructive")[]   // for actions and functions
  functions?: string[]             // function name patterns allowed
  agents?: string[]
  envs?: string[]
  paths?: string[]                 // folders its workspaces must be inside
  depth?: number                   // levels of children below it
  children?: number                // children running at once
  budget?: Budget                  // for the whole subtree
  unattended?: number              // seconds before denying requests nobody can answer
}
```

- The empty ceiling allows everything the principal's grant allows.
- A child's ceiling is its parent's intersected with the one requested: the lower level, `network`
  only if both allow it, the higher isolation, intersected lists and paths, smaller numbers, `depth`
  minus one, the remaining budget.
- A session runs only on an agent that can be held to its ceiling's `level` and provide its
  `isolation`; otherwise `session/new` or `session/rebind` fails with `-32003` and says why.
- Options, actions, function calls and workspaces outside the ceiling fail with `-32003`. Levels are
  enforced as in [15.2](#152-permission-levels). With isolation below `sandbox`, ceilings are
  advisory ([5.3](#53-threat-model-and-isolation)).

## 23. Seeding and bundles

A seed gives a session history from another session.

| Operation | Call | Session id |
| --- | --- | --- |
| Fork | `session/new { seed }` | New session |
| Delegate with context | `session/new { parent, task, seed: { session: parent } }` | New child |
| Hand off to another harness, move to another machine | `session/rebind` ([9.3](#93-rebinding-to-another-harness-or-machine)) | Same session |

```ts
interface SeedRequest {
  session: string                  // the source
  at?: string                      // a turn id; default the latest
  carry?: ("history" | "workspace" | "plan")[]   // default all three
  brief?: string
  require?: "exact" | "any"        // default "any"
  summary?: "none" | "source"      // default "none"
  source?: "keep" | "stop" | "archive"   // default "keep"
}
interface Seed { session: string; at?: string; via: "fork" | "bundle" }
```

### 23.1 Method

| Target | How |
| --- | --- |
| Same agent and environment as the source's binding, and the agent has `session.fork` | Native fork: exact history |
| Anything else | A bundle. With `require: "exact"`, fails with `-32001` instead |

- `source` is applied after the target binding has started; if the target fails, the source is
  untouched.
- With `carry` including `workspace` and no `workspace` given, the new session gets a worktree on the
  source's current commit in the same environment, or a transfer to another.

### 23.2 Bundles

```ts
interface ContextBundle {
  version: 1
  from: { session: string; binding: string; agent: string; env: string; at?: string }
  title?: string
  summary?: string
  items: Item[]
  plan?: Item
  workspace?: { repo?: string; branch?: string; commit?: string; changed?: string[] }
  level?: PermissionLevel
}
```

| Method | Params | Result |
| --- | --- | --- |
| `session/bundle` | `{ session, at?, summary? }` | `{ bundle, rendered: string }` |

- **Trimming.** If the history doesn't fit the target's context window (the `model` option's
  `x-details.contextWindow`, else 100,000 tokens) with half of it left for work, the runtime keeps the
  first user message and the newest items that fit.
- **Summary.** `source` invokes the source's `summarize` action, which adds a turn to the source;
  agents without it fail with `-32001`. The summary goes first, then the kept items.
- **Pending.** The rendered bundle ([Appendix D](#appendix-d-bundle-rendering)) is held on the new
  binding (`pending: "bundle"`) and prepended to its first prompt: the seed's `brief`, a task's brief,
  or the next prompt a client sends. The new harness doesn't start working unasked.
- Reasoning items are dropped; tool calls are rendered as a line with their title and outcome.
- The target's options are set to the target agent's values for the source's permission level,
  lowered to the target's ceiling.

## 24. Links

A link connects two runtimes. Through it, each sees the other's environment, agents and offered
sessions, and sessions can be created in or moved to the other environment. The bytes travel over a
**carrier**: SSH, any command, a direct TCP connection, or a platform's own channel where nothing else
reaches the machine. Trust never comes from the carrier: both runtimes authenticate each other by key
and encrypt end to end, so a carrier may be a third party's relay.

### 24.1 Link records

```ts
interface Link {
  id: string
  name: string
  peer: { runtime: string; key: string }     // the other runtime's id and public key, pinned at pairing
  carrier?: CarrierRef                        // how this runtime reaches the peer; absent if it only accepts
  grant: Scope[]                             // what the peer may do here
  offer: { env: boolean; sessions: "none" | "linked" | "all" }   // "linked": only sessions created over this link
  containment: "restricted" | "shell" | "unknown"   // 24.4
  status: "connected" | "connecting" | "down" | "unpaired"
  via?: string                               // the carrier instance of the current connection
  since?: string
}
interface CarrierRef {
  kind: string                               // "ssh", "command", "tcp", or a carrier egg's name
  target: Record<string, Json>               // validated against the carrier's target schema
  fallback?: CarrierRef[]                    // tried in order when this one fails
}
```

| Method | Params | Result |
| --- | --- | --- |
| `link/list` | `{}` | `{ links }` |
| `carrier/list` | `{}` | `{ carriers: { kind, title, target: JsonSchema, listens: boolean, containment, maxMessage }[] }` |
| `link/invite` | `{ name, grant, offer, expires? }` | `{ code, link }`: a pairing code, and the record in status `unpaired` |
| `link/add` | `{ name, carrier, code?, grant, offer }` | `{ link }`: dials, pairs with `code` (or over the carrier, 24.3) |
| `link/update` | `{ link, carrier?, grant?, offer? }` | `{ link }` |
| `link/remove` | `{ link }` | `{}`; the peer sees the link `down` and pairing revoked |

Either side may dial; a link with a carrier on both sides uses whichever connects first and keeps one
connection. Only the side behind NAT needs a carrier.

### 24.2 Carriers

A carrier gives the runtime an ordered, reliable, bidirectional channel to the peer's
`parrot connect --link` for the life of one connection. Loss, reordering or duplication must close the
channel; Parrot reconnects and resumes with cursors, so carriers don't need their own recovery.

| Kind | Built in | Channel |
| --- | --- | --- |
| `ssh` | Yes | `ssh <destination> parrot connect --link` with the user's SSH config |
| `command` | Yes | Any command whose standard input and output reach the peer's `parrot connect --link`: `kubectl exec -i`, `docker exec -i`, `wsl.exe`, `gh codespace ssh`, `aws ssm start-session` with a command document |
| `tcp` | Yes | A TCP connection to the peer's listener (`parrot serve --link-listen <addr>`). Only for networks where the peer is reachable; the link handshake does all authentication |
| Carrier egg | No | A platform's own channel (Rookery's control plane, a vendor's device channel, a message relay) |

**Carrier eggs** are Hatchery eggs with the `parrot` target and kind `carrier`: a process in any
language, with KDL data naming its target schema, whether it can listen, its `maxMessage` and its
containment. The runtime starts it per connection:

- Dial: `<carrier> dial` with the target as JSON on its first input line. After it answers
  `{"ready":true}` or `{"error":…}` on its first output line, its standard input and output are the
  channel.
- Listen (for channels that deliver inbound connections, such as a control plane that can't start a
  process on demand): `<carrier> listen` runs beside the runtime and, for each inbound channel, starts
  `parrot connect --link --fd <n>` or relays to the local socket the same way.
- Message-based channels carry each link frame (below) as one message; the carrier states its
  `maxMessage`, at least 4 KiB.

**Link frames.** The channel carries a Noise protocol session
(`Noise_XX_25519_ChaChaPoly_BLAKE2s`, or `Noise_XXpsk3` when pairing with a code). On byte-stream
channels each Noise message is prefixed by its length as two bytes, big-endian. Parrot JSON-RPC
messages are split into frames that fit `min(65,535, maxMessage)` after encryption, each with a flag
for the last part, and joined before parsing; the 32 MiB message limit still applies. Workspace
transfers and bundles go as `blob/put` chunks of at most 1 MiB, so slow channels can interleave other
messages.

### 24.3 Pairing and authentication

- Each runtime has an Ed25519 identity key in its state directory, converted to X25519 for Noise. Its
  runtime id is derived from the public key, so ids can't be claimed.
- **Pairing with a code.** `link/invite` on one side returns a code holding its runtime id, a one-time
  secret and its addresses if it listens, valid for 10 minutes by default. `link/add` with that code on
  the other side dials, runs `XXpsk3` with the secret, and both sides pin each other's key. The inviting
  side's record moves from `unpaired` to `connected` with the grant and offer it set in the invite.
- **Pairing over the carrier.** When the carrier gives the dialling user a shell on the peer's machine
  (`ssh`, most `command` carriers), `link/add` without a code first runs `parrot link invite --json`
  through the carrier, with the grant and offer the caller asked the peer to give. That needs no copy and
  paste, and trusts the carrier exactly once.
- **Every connection** after pairing runs `XX` with pinned keys. A key that doesn't match fails with
  `-32040` and `data.reason: "key"`, and nothing is retried until the owner acts.
- After the handshake both sides send `initialize` as peers of kind `runtime`, and each acts as the
  link principal with the grant the other gave it.
- **Calls on behalf of a tree.** A call that the peer makes for a session in a cross-runtime tree carries
  `onBehalfOf: <session>`. The owner checks that the session is the parent (or an ancestor) it recorded
  when the child was created over this link, and applies that session's ceiling inside the link's grant.

### 24.4 Containment

A link grant only holds if the peer can't reach the runtime some other way.

- `restricted`: the carrier can only run `parrot connect --link` on this machine: an SSH key with
  `command="parrot connect --link",restrict` in `authorized_keys` (written by `parrot link invite
  --authorized-key <key>`), a listener-only carrier egg, or `tcp`.
- `shell`: the carrier's credentials give a shell as the runtime's user. Anyone holding them is the
  owner, whatever the grant says. Clients show such links as full access.
- `unknown`: a carrier egg that doesn't declare it, treated as `shell`.

### 24.5 Behaviour

- **No transitivity.** A runtime shows peers only its own environment and sessions.
- **Ownership.** A session is owned by the runtime of the environment its binding runs in. Creating a
  session in a linked environment asks that runtime to create it; moving one transfers it (9.3).
  Calls for a session go to its owner; clients of another runtime reach it through the link, with the
  link's grant narrowing what they may do.
- **Failover.** When a connection drops, the dialling side tries the carrier, then its `fallback` list,
  with backoff from 1 second to 5 minutes. `via` says which one is in use.
- **Reconnects.** Subscriptions forwarded over a link resume with `replayFrom` after a drop. The
  environment shows `offline` while the link is down.
- **Credentials.** Harness sign-in is per agent and environment. Git credentials stay with each
  environment. Only protocol messages, bundles, git bundles and patches cross a link, encrypted.
- Link and carrier methods are for the owner on a local connection only.

# Part III: Functions

## 25. Functions

```ts
interface FunctionSpec {
  name: string                     // namespaced; "parrot." is reserved for this spec
  description: string
  input: JsonSchema
  output?: JsonSchema
  effects: ("read" | "write" | "turn" | "destructive")[]
  examples?: { input: Json; output?: Json }[]
  env?: string                     // for functions that act on one environment
  pinned?: boolean                 // listed directly to harnesses (26)
}
interface FunctionInfo extends FunctionSpec { provider: string }   // "runtime", a connection id, or a link id
interface FunctionResult {
  output?: Json
  content?: ({ type: "text"; text: string } | { type: "image"; data: string; mime: string } | { type: "resource"; uri: string })[]
  error?: { code: string; message: string }
}
```

### 25.1 Calling

| Method | Params | Result |
| --- | --- | --- |
| `function/list` | `{ session?, cursor? }` | `{ functions: FunctionInfo[], next? }` |
| `function/search` | `{ session?, query, limit? }` | `{ functions: FunctionInfo[] }` |
| `function/describe` | `{ names: string[], env? }` | `{ functions: FunctionInfo[], declarations: string }` |
| `function/call` | `{ function, env?, input, session?, key? }` | `FunctionResult` |

- A function is identified by `name` and `env`. `env` is required when the name is offered by more
  than one environment.
- A call for a session (by its agent, or by a client naming `session`) runs as the session's principal,
  within its ceiling's `functions` and `effects`.
- A session's call with `write`, `turn` or `destructive` effects that its ceiling's `effects` doesn't
  list opens a request with `subject: { type: "function" }` and waits for it; a denial returns an error
  result.
- `function/search` ranks by name and description match; the ranking is the runtime's own and stable
  for the same catalog.
- Calls by a session's agent appear as `tool` items with `function` set.

### 25.2 Providing

| Method | Params | Result |
| --- | --- | --- |
| `function/provide` | `{ functions: FunctionSpec[] }` | `{}`; replaces this connection's set |

- A client provides functions in `initialize` or with `function/provide` (needs `functions` and
  `client.functions`). The runtime calls them with a JSON-RPC request `function/call` to the client.
  They go away when the connection closes.
- A name and environment can have one provider. A second provider of the same pair is refused with
  `-32004` until the first goes away.
- A linked runtime offers its environment's functions when its offer includes `env`; they carry its
  `env`.

## 26. Exposure to harnesses

- With `functions.mcp`, the driver injects one MCP server named `parrot` into each binding, for that
  binding only and never in the harness's global configuration. It runs in the binding's environment,
  reached over loopback HTTP with a per-binding token, or through `parrot mcp --binding <id>` on stdio.
- MCP tool names are function names with `.` replaced by `__` (`parrot__sessions__spawn`). Function
  names may not contain `__`.
- **Listing.** Sorted by name, so the harness's prompt cache survives changes elsewhere:
  - the session functions the session may use ([28](#28-the-session-functions));
  - functions marked `pinned` that the session may call;
  - `parrot__functions__search`, `parrot__functions__describe` and `parrot__functions__call`, which
    reach every other function the session may call without listing it.
- The listing changes only when the session's scopes or ceiling change, or a pinned function is
  provided or removed. The runtime then sends a `functions` update on the stream of each session
  with an MCP server, and the server sends `notifications/tools/list_changed` once its listing has
  changed.
- The server checks arguments against the input schema of a tool it lists before calling, and a
  bad call returns `invalid_input` with the path of the first problem.
- Without `functions.mcp`, the session has no functions; tasks complete on `idle`.

## 27. Calls from harness code

Parrot is an orchestrator, not a harness: it runs no model-written code. Harnesses increasingly let the
model write code that calls tools (Claude Code's Code Mode, the API's programmatic tool calling, pi,
Cloudflare-style code mode over MCP). Parrot's functions are shaped so such code can call them like any
other tool, and so the calls can be seen and checked.

### 27.1 Shape of functions

Every function, including those provided by clients, follows these rules; `function/provide` refuses a
spec that breaks a checkable one with `-32602`.

- **Names.** Dot-separated segments matching `[a-z][a-z0-9_]*`, no segment containing `__`, at most
  four segments; the MCP name (dots as `__`) is at most 64 characters.
- **Inputs.** One JSON object. Schemas use no recursive `$ref` (programmatic tool calling rejects
  them) and no constructs beyond JSON Schema 2020-12's `type`, `properties`, `required`, `items`,
  `enum`, `const`, `oneOf` of objects with a `const` discriminator, `minimum`/`maximum`, `pattern`,
  `format`, `default` and `description`. Optional fields have defaults that do the common thing.
- **Results.** Every result has `output` as JSON, and the MCP tool result carries the same JSON as its
  only `text` content (and in `structuredContent`). Code can `JSON.parse` the text on every harness;
  programmatic tool calling accepts only text results. Images and files are referenced by `uri` in the
  JSON, never returned as image blocks.
- **Output schemas** are required and are rendered into the description as a short TypeScript type,
  since some code bindings don't see `outputSchema`.
- **Errors** are results with `isError: true` and the JSON `{ "error": { "code", "message", ... } }`,
  with codes from a fixed list per function. Code branches on `code`, never on prose.
- **Batches.** Reads that a script would loop over take arrays: `parrot.sessions.read { sessions: [] }`,
  `parrot.tasks.wait { sessions: [] }`. Results are keyed by input, with a per-entry error.
- **Size.** List and read results take `limit`, `cursor` and `fields` (the top-level fields to return);
  a result over 256 KiB is cut at an entry boundary and returns `next`.
- **Keys.** Every function with `write`, `turn` or `destructive` effects takes an optional `key`. A
  repeated key returns the first call's result, so a script that retries after a timeout doesn't spawn
  twice.

### 27.2 Time

- No call blocks longer than the binding's tool-call timeout from driver data (`limits.kdl`) minus a
  margin, and never longer than 3 minutes: programmatic tool calling times a call out after about 4
  minutes, and Claude Code's Code Mode stops foreground code after 10.
- `parrot.tasks.wait` and other waiting calls take `timeout`, capped at that limit, and return a cursor.
- **Approvals.** A call that needs a request ([25.1](#251-calling)) opens it and waits within the same
  limit. If it isn't answered in time, the call returns the error `pending_approval` with `request`.
  Calling again with the same `key` after the request is answered returns the result; while it is open,
  the same error comes back at once. A denial returns `denied`. Scripts therefore never hang on a
  person, and the request stays visible to clients the whole time.

### 27.3 Observing harness code

- With the agent feature `code`, the driver reports each code execution as a `script` item and the
  calls it makes as `tool` items with `caller: "script"` and `parent` set to the script item.
- Where the harness doesn't expose the code or the link between a call and its script, the driver says
  so in driver data, and items are reported with what is known; `caller` is `"model"` when unknown.
- Calls to Parrot functions from harness code arrive through the injected MCP server like any other
  call, as the session's principal, with the same scopes, ceiling, effects and requests. Parrot never
  relies on a harness's own allow-list of callers.
- Turning a harness's code mode on or off is an option of that agent, from driver data.

## 28. The session functions

Provided by the runtime to every session, filtered by its scopes and ceiling. Each is a projection of a
method, called as the session's principal on its subtree.

| Function | Method | Needs |
| --- | --- | --- |
| `parrot.sessions.spawn` | `session/new` with `parent` set to the caller | `delegate` |
| `parrot.sessions.send` | `session/prompt` | `prompt` |
| `parrot.sessions.act` | `action/invoke`, such as `continue` on a child | the scope of the action's effects |
| `parrot.sessions.cancel` | `session/cancel` (interrupts without stopping) | `prompt` |
| `parrot.sessions.stop` | `session/stop` | `manage` |
| `parrot.sessions.read` | `session/read`, `session/get` | `read` |
| `parrot.sessions.tree` | `session/tree` of the caller | `read` |
| `parrot.sessions.handoff` | `session/rebind` of the caller | `delegate` |
| `parrot.tasks.assign` | `task/assign` | `delegate` |
| `parrot.tasks.wait` | `task/wait` | `read` |
| `parrot.task.finish` | `task/finish` of the caller | the caller has a running task |
| `parrot.requests.list` | `request/list` of the subtree | `read` |
| `parrot.workspaces.status` | `workspace/status` | `read` |
| `parrot.workspaces.merge` | `workspace/merge` | `workspace` |
| `parrot.agents.list`, `parrot.envs.list` | `agent/list`, `env/list` | `read` |

## 29. The desktop functions

An environment with the feature `desktop` offers `parrot.desktop.*`, provided by Rookery's desktop
service or a client on that machine. Coordinates are pixels of the named display, origin top left, at
the display's native resolution; screenshots report their scale. Key names are W3C UI Events `key`
values (`Enter`, `Control`, `a`).

| Function | Input | Output | Effects |
| --- | --- | --- | --- |
| `parrot.desktop.displays` | `{}` | `{ displays: { id, width, height, scale, primary }[] }` | read |
| `parrot.desktop.screenshot` | `{ display?, region?: {x,y,width,height}, maxWidth? }` | image content, `{ scale }` | read |
| `parrot.desktop.tree` | `{ window?, depth? }` | `{ nodes: { id, role, name?, value?, bounds, children? }[] }` | read |
| `parrot.desktop.windows` | `{}` | `{ windows: { id, title, app, bounds, focused }[] }` | read |
| `parrot.desktop.pointer` | `{ display?, action: "move" \| "click" \| "double" \| "down" \| "up" \| "scroll", x, y, button?: "left" \| "right" \| "middle", dx?, dy? }` | `{}` | write |
| `parrot.desktop.keys` | `{ text?: string; press?: string[] }` (press: keys held together) | `{}` | write |
| `parrot.desktop.focus` | `{ window }` | `{}` | write |

A harness's own computer use is not routed through these; it appears as its own tool items.

# Part IV: Drivers and runtime

## 30. Driver protocol

### 30.1 Methods the driver implements

| Method | Params | Result |
| --- | --- | --- |
| `initialize` | as in 4, `peer.kind: "driver"` on the driver side | as in 4 |
| `agent/list` | `{}` | `{ agents }` (without `env`; the runtime sets it) |
| `binding/start` | `BindingStart` | `{ ref?: Ref, isolation: Isolation, level: PermissionLevel, network: boolean, options: Option[], actions: Action[] }` |
| `binding/stop` | `{ binding }` | `{}` |
| `binding/prompt` | `{ binding, subagent?: string, message: string, text, context?, attachments?, steer: boolean }` | `{ disposition: "started" \| "steered" \| "handled" }` |
| `binding/cancel` | `{ binding, subagent? }` | `{ interrupted: boolean }` |
| `binding/answer` | `{ binding, request: string, answer }` | `{}` |
| `binding/options` | `{ binding, values }` | `{ options, level, network }` |
| `binding/action` | `{ binding?, agent?, action, input?, args? }` | `{ status, request?, result? }` |
| `binding/fork` | `{ binding, at?: string /* turn key */ }` (with `session.fork`) | `{ ref: Ref }`; then `binding/start` with `resume` set to it |
| `history/discover` | `{ agent, cwd?, cursor?, limit? }` (with `session.history`) | `{ found, next? }` |
| `history/read` | `{ agent, ref, cursor? }` | `{ updates: DriverUpdate[], next? }`: the stored history as updates |
| `binding/screen` | `{ binding }` (TUI control) | `{ text: string, cursor: {row, col}, size: {rows, cols} }` |
| `binding/keys` | `{ binding, text?: string, press?: string[] }` (TUI control) | `{}` |

```ts
interface BindingStart {
  session: string
  binding: string
  agent: string
  workspace: string                // its path
  cwd: string
  options: Record<string, Json>
  control: "native" | "tui"
  resume?: Ref                     // resume this harness session
  pin?: { level: PermissionLevel; network: boolean }   // hold the harness to at most this (15.2)
  isolation?: Isolation            // the minimum required
  deny: string[]                   // paths the harness must not reach: the runtime's socket and state directory
  mcp?: { url?: string; command?: string[]; token: string }
}
```

### 30.2 Updates the driver sends

The driver sends the notification `update` with `DriverUpdate` params:

```ts
interface DriverUpdate {
  binding: string
  subagent?: string                // a subagent key (30.3)
  update:
    | { type: "state"; state: State; background: Background[]; detail?: string }
    | { type: "item"; item: ItemPatch }          // ids are driver keys
    | { type: "turn"; turn: Turn }               // id is a driver key; `binding` omitted
    | { type: "request"; request: Request }      // id is a driver key; `session`, `path` omitted
    | { type: "options"; options: Option[]; level: PermissionLevel; network: boolean }
    | { type: "actions"; actions: Action[] }
    | { type: "usage"; usage: Usage }
    | { type: "ref"; ref: Ref }                  // the harness session id, once known
    | { type: "mcp"; servers: Session["mcp"] }
    | { type: "notice"; level: "info" | "warning" | "error"; text: string; code?: string }
    | { type: "frame"; source: string; data: Json }   // raw source output for storage (35)
}
```

Queue, session, summary and index updates come only from the runtime.

### 30.3 Subagents

With `session.subagents`, the driver sends an item `{ kind: "subagent", set: { key, title, ref? } }`
in the parent, then the subagent's own updates with `subagent: <key>`. `binding/prompt` and
`binding/cancel` with `subagent` address it, where the feature's options allow.

### 30.4 Keys and ids

- Driver keys are strings unique within a binding (items, turns, requests) or a session (subagents).
- The runtime derives ids: with a strong harness ref, from `(session, kind, harness ref)`; otherwise
  from `(session, binding, kind, key)`. History read again after a restart, or replayed into a new
  binding, therefore keeps its ids where the harness has stable ids.
- The user item of a runtime prompt uses the `message` id the runtime passed. A driver creates user
  items of its own only for input that didn't come from the runtime.
- The runtime keeps the map from Parrot turn ids to driver keys, so `session/rewind` and forks pass
  the harness's own reference down.

### 30.5 Processes

- A process driver serves many bindings unless its egg says `bindings = 1`.
- The runtime starts process drivers, and drivers start harness processes, so they die with their
  parent: a process group with a parent-death signal on Linux, a kqueue watch on macOS, a job object
  with kill-on-close on Windows. A crashed runtime leaves no harness process running.
- Stopping: `binding/stop` per binding, then standard input closes, then the process tree is killed
  after 10 seconds.
- The runtime builds in only the general cores, `acp` and `tui`; `parrot driver <name>` serves them on
  stdio. Every other driver is a program in its egg (31).

## 31. Driver data

Driver data is the part of a driver that changes with harness releases. It ships as a Hatchery egg
with the `parrot` target, versioned separately from the runtime, as KDL files:

| File | Holds |
| --- | --- |
| `driver.kdl` | Driver name, runtime versions it needs, the harness versions it covers, how to find and start the harness, default control |
| `options.kdl` | Options the harness doesn't report itself: ids, roles, schemas, `applies`, defaults; renames between harness versions |
| `levels.kdl` | Each combination of permission-related option values and settings sources, mapped to a level and `network`; how to pin settings sources; which isolation each sandbox setting gives |
| `actions.kdl` | Actions the harness doesn't report; well-known ids mapped to native forms |
| `limits.kdl` | Tool-call timeout, turn limits and similar numbers |
| `correlate.kdl` | Per item kind: how items from two sources match, which wins ([32](#32-sources-and-tui-control)) |
| `tui/*.toml` | TUI profiles, per harness version range ([32.2](#322-tui-control)) |
| `probe/*.kdl` | Probe scenarios for nightly checks |
| `fixtures/` | Conformance fixtures |

- Each file can be scoped to harness version ranges with a version shim: `shim "<range>" dir="<dir>"` in
  `driver.kdl`. When the driver detects a harness version in the range, the files in `<dir>` replace
  the egg's files of the same name. Lines go from the oldest range to the newest; the last that holds
  the version wins.
- The runtime starts a driver with `PARROT_DRIVER_DATA` (the egg's `parrot` folder),
  `PARROT_DRIVER_AGENT` (the egg's package id) and `PARROT_DRIVER_AGENT_NAME` (its display name). The
  driver reports that id and name in `agent/list`, so a data-only egg on a built-in core, such as
  `gemini` on `driver "builtin" name="acp"`, is its own agent.
- With no egg installed the runtime has no agents; `agent/list` then also returns `hint`, a sentence
  that says how to install one.
- A harness version newer than every range uses the newest blocks, and the agent reports
  `harness.verified: false`.
- The schemas of these files are part of this spec's generated schema.

## 32. Sources and TUI control

### 32.1 Sources

- Each binding has one **control source**: the native protocol or the TUI. A process without a screen
  can't be driven by both.
- Any number of **passive sources**: the transcript on disk, CLI subcommands, config files, git.
- `correlate.kdl` says how items of two sources match (by harness id, else by turn and position) and
  which wins: the control source while the binding runs, the transcript when history is read again.
- In shadow mode (development and nightly checks), passive readers run beside the control source and
  differences are reported as `notice` updates with code `x.parrot/shadow`.

### 32.2 TUI control

- Under TUI control the driver maps the screen to states and actions with the TUI profile. Content
  comes from the transcript where the harness writes one.
- A dialog the profile knows becomes a request with choices; answering presses the mapped keys.
- A screen no profile state matches makes the session `unrecognised`, after a hold of 2 seconds. The
  driver then opens a request of kind `input` with the screen text in `description` and a free `value`
  field; clients may also show the live screen with `session/screen` and send keys with
  `session/keys` (both need `prompt`). The session returns to a matched state on its own.

| Method | Params | Result |
| --- | --- | --- |
| `session/screen` | `{ session }` | `{ text, cursor, size }` |
| `session/keys` | `{ session, text?, press? }` | `{}` |

## 33. Degradations

| Missing | Runtime does |
| --- | --- |
| `session.steer` | `auto` queues while running; `steer` fails with `-32001` |
| `session.stream` | Items arrive in batches |
| `session.resume` | `session/start` makes a new binding with a pending bundle |
| `session.fork` | Seeds and rewinds by bundle; `require: "exact"` fails |
| `session.subagents` | Subagents stay `tool` items in the parent |
| `request.redeliver` | Requests are `connection`; they expire when the driver reconnects |
| `functions.mcp` | No functions; tasks complete on `idle` |
| `code` | Calls made from harness code appear as plain `tool` items with `caller: "model"`; nothing else changes |
| Native `continue` (every harness so far) | A prompt with the text `Continue.`, plus the note on a new line; the user item has `origin.action: "continue"` and clients show it as an action |
| Native `retry` | The failed turn's input sent again as a new prompt, `origin.action: "retry"` |

## 34. Durability and restarts

- The runtime writes each intent to its log before acting on it: prompts and queue entries, wakes,
  rebind and seed steps, task changes, workspace operations. After a crash it continues or rolls back
  each from the log.
- After a restart, every binding that was running is `stopped`; nothing is started again by itself.
  The next prompt, action or `session/start` starts it, resuming the harness session where the agent
  can. Queues keep their entries and stay paused until then.
- A prompt sent to a harness without confirmation before the crash is marked `uncertain` and not sent
  again; clients offer to resend it.
- Wakes are recomputed from task states. `connection` and `process` requests expire; `message`
  requests stay open.
- Calls in progress end with the connection; callers retry with their cursor or key.

## 35. Storage

- The runtime keeps sessions, bindings, the update log, the item projection, queues, requests,
  workspaces, links and tokens (hashed) in a local database with numbered schema migrations, in its
  state directory (mode `0700`).
- **Update log.** Kept 30 days by default. Streamed `append` patches are consolidated when the stream
  ends (12.1). `replayFrom` older than the log fails with `-32012`.
- **Frames.** Raw source output per binding, with the driver data version, kept 30 days. Items can be
  rebuilt from frames when a driver changes its projection; ids stay stable (30.4). Drivers drop fields
  known to carry secrets before sending frames.
- **Checkpoints.** For `worktree` workspaces, the runtime records the workspace at the start of each
  turn as a commit on a hidden ref (`refs/parrot/<session>/<turn>`), with tracked changes and untracked
  files that aren't ignored, without touching the index or working tree. They're pruned with the
  session or after 30 days, and never transferred.

## 36. Notifications

- Requests with `notify` other than `none`, tasks that end, and sessions that become `lost` are
  notification events.
- Clients with `client.notify` receive them as updates and show them.
- The runtime also delivers them to configured push targets: a webhook URL that receives a small JSON
  body (`{ event, session, title, text, url? }`), or an ntfy topic. Targets are configuration, not
  protocol. Bodies carry titles and short text only, never items or code.

## 37. Errors

| Code | Name | When |
| --- | --- | --- |
| -32600 | invalid request | Malformed, or a batch |
| -32601 | method not found | |
| -32602 | invalid params | Includes values that fail their schema |
| -32001 | unsupported | `data.feature` |
| -32002 | not found | |
| -32003 | forbidden | Outside scopes, subtree or ceiling; `data.reason` |
| -32004 | wrong state | `data.state` |
| -32010 | not initialized | |
| -32011 | no shared version | `data.supported` |
| -32012 | replay unavailable | |
| -32013 | already answered | |
| -32020 | harness failed | `data.detail` |
| -32030 | environment offline | |
| -32032 | budget exhausted | |
| -32040 | link failed | `data.detail` |
| -32050 | moved | `data.runtime` |

## 38. Extensions, schema and conformance

- Every object may carry `_meta: Record<string, Json>` with namespaced keys, kept when unknown. New
  values of open unions, methods and functions outside this spec use namespaced names; new methods are
  announced as features.
- The runtime's Rust types are the source. `schemars` generates the JSON Schema (including driver data
  files), which is committed; the TypeScript client is generated from it; CI fails on a stale schema.
  The schema's `x-methods` lists the methods a client or runtime calls, `x-client-methods` the
  requests a runtime sends to a client, and `x-notifications` the notifications.
- Open unions use a custom `Open<T>` (a known variant, or the raw `serde_json::Value`), since
  `#[serde(other)]` covers only unit variants. Every struct keeps unknown fields in
  `#[serde(flatten)] extra`. Patches apply to `serde_json::Map`.
- Golden fixtures are JSONL of `{ "dir": "in" | "out", "msg": … }` with placeholders for `seq`, ids and
  times, and marks where the projection is compared: items after patches, requests, options, queue,
  state, tasks. Message streams aren't compared, so drivers may chunk differently.
- `parrot conform <command>` runs a driver against the driver fixtures; `parrot conform --runtime`
  runs the client fixtures against a runtime.

## 39. Method index

| Method | Scope |
| --- | --- |
| `initialize` | none |
| `agent/list`, `env/list`, `session/list`, `session/get`, `session/tree`, `session/read`, `item/get`, `workspace/list`, `workspace/status`, `request/list`, `link/list`, `subscribe`, `unsubscribe`, `session/discover`, `session/bundle` (no summary), `task/wait`, `function/list`, `function/search`, `function/describe`, `session/screen` | read |
| `session/prompt`, `session/cancel`, `queue/*`, `action/invoke` (turn, read), `session/keys` | prompt |
| `request/answer` | answer |
| `option/set`, `option/complete`, `action/invoke` (write), `agent/refresh` | configure |
| `session/new` (root), `session/start`, `session/stop`, `session/rename`, `session/archive`, `session/adopt`, agent-scoped actions | manage |
| `session/new` (with `parent` or `seed`), `session/rebind`, `task/assign`, `task/fail`, `session/bundle` (summary) | delegate |
| `task/finish` | the session's own principal |
| `workspace/create`, `workspace/merge`, `workspace/transfer` | workspace |
| `workspace/remove`, `session/delete`, `session/rewind`, `action/invoke` (destructive) | destructive |
| `function/call` (other than session functions), `function/provide` | functions |
| `blob/put` | a link principal, for transfers it started |
| `token/*`, `link/invite`, `link/add`, `link/update`, `link/remove`, `carrier/list` | owner on a local connection |

## 40. Implementation profiles

The spec is complete; profiles order the work. A runtime lists the profiles it implements in
`features` (`profile.core`, …); calls outside them fail with `-32001`.

| Profile | Sections | Proves |
| --- | --- | --- |
| `core` | 1–17 for local agents: `dir` workspaces, root sessions, subscriptions and the index, prompts, queue, requests, options with levels, actions with `continue` and `retry`, usage; 30–35 | Two clients on one session; one disconnects, the other answers; models discovered and set; a restart leaves nothing running |
| `tree` | 18–22, worktrees and merge, 25–26, 28 | An orchestrator delegates to two children in worktrees, waits, and merges |
| `seed` | 9.3 within one machine, 9.4, 23, Appendix D, checkpoints | Fork, handoff between Claude and Codex, rewind with files |
| `link` | 24 with `ssh`, `command` and `tcp`; carrier eggs; 9.3 across machines, workspace transfer | A session moves to a server and keeps running after the laptop closes |
| `remote` | 3.4, 36 | A phone answers a request from a push notification |
| `desktop` | 29 | An agent uses the desktop of a Rookery host |
| `tui` | 32.2 | A harness driven through its TUI, with an unknown dialog answered from a client |

# Appendices

## Appendix A: Differences from ACP v2

| Parrot | ACP v2 | Reason |
| --- | --- | --- |
| Prompt returns a message id; state on its own stream | Same | Copied |
| Upsert items by id | Same idea | Copied, with one generic patch rule |
| `subscribe` with `replayFrom` and a runtime-wide `seq` | `session/resume` with `replayFrom`, `replay_complete` | Session, tree and index views share one sequence; starting is a separate call so read-only clients can attach |
| Sessions with bindings | One agent per session | Handoff and move keep the session id |
| Requests with choices, forms, `answerable`, `level` | `request_permission` | Questions, plans, forms and logins in one shape |
| Options with roles, `applies`, permission levels | Config options | Pickers and ceilings work across harnesses |
| Actions with schemas and effects | `available_commands` with a hint | Structured commands, policy by effect |
| Principals, scopes, ceilings, isolation | None | Several clients, phones and delegated agents |
| Session functions | MCP-over-ACP draft | Orchestration with the protocol's own semantics |
| No client file system or terminal methods | Removed too | Copied |

`parrot acp` serves a Parrot session as an ACP agent on stdio and negotiates ACP v1 or v2.

## Appendix B: T3 Code

### B.1 T3 seen from Parrot

A T3 driver talks to a T3 server's `/mcp` tools. Each T3 provider instance is an agent (`t3:<instance>`)
in the environment of the machine T3 runs on. T3 owns its projects, so workspaces are `external`: no
checkpoints, merges or transfers.

| T3 | Parrot |
| --- | --- |
| Provider instance | `Agent` |
| Thread | A binding's `ref` |
| `t3_thread_launch` (with worktree mode) | `binding/start` |
| `t3_thread_send` | `binding/prompt` |
| `t3_thread_wait`, `t3_thread_read` | Driver-internal; the agent lacks `session.stream`, items arrive in batches |
| `t3_pending_request` and its answer | Requests and `binding/answer` |

T3's `delegate_task` isn't available to outside clients; children of a T3-hosted session are separate
T3 threads started by Parrot.

### B.2 Parrot seen from T3

`parrot acp` lets T3's ACP provider run a Parrot session. ACP has no tree, tasks or seeds, so
orchestration stays inside Parrot and children show in T3 as tool calls. A native T3 provider
(`ProviderAdapterV2`) would map the tree onto T3's subagent nodes and bindings onto its provider
sessions.

## Appendix C: Harness mapping sketch

Each driver gets a full table before it is built: every native message, its Parrot form, what is
dropped. Entries marked *unverified* need checking against the harness first.

| Harness | Mapping |
| --- | --- |
| Claude Code stream-json | `control_request can_use_tool` → requests (`approval`, permission suggestions as choices with `scope` and `amends`, `subject.editable` for `updatedInput`; `question` for AskUserQuestion; `plan` for ExitPlanMode); `answerable: "process"` with redelivery by `ref`; `initialize` → options and actions; `stream_event` deltas → `append`; `result` → turn end, `stop` (including `turn_limit`), usage; Task subagents → embedded sessions; permission mode plus pinned setting sources → level; no code mode: Claude Code's Code Mode runs only in the interactive terminal, so it is seen only under TUI control (below) |
| Claude Code TUI | Code Mode (`CLAUDE_CODE_EXPERIMENTAL_CODE_MODE=1`) is an option with `applies: "restart"`; a session started with it resumes only with it, so rebinds and resumes keep the value. Each `REPL` tool call → a `script` item with `code` from `tool_input.code`; calls inside it pass through permission rules and `PreToolUse` hooks under their own names → `tool` items with `caller: "script"`. How the transcript links inner calls to their `REPL` call is *unverified*; without a link they go under the newest running script. Background code (up to two hours) → `script` with `background: true` and a `background` state entry. Code Mode excludes Remote Control and computer use |
| Codex app-server | thread → `ref`; `turn/start`, `turn/steer` (refused steers go back to the queue), `turn/interrupt`; items by kind; `model/list` → `model` with `dependsOn` efforts, `applies: "next_turn"`; approval policy with sandbox → level, sandbox → isolation; approvals with decisions as choices and policy amendments as `amends`; `autoResolutionMs` → `expires`, `fallback`; collab agents → embedded sessions with their own refs; `thread/fork` → native fork; `/compact`, `/review` → actions |
| pi RPC | `steer` (`after_tools`); models and thinking levels → `model`, `effort`; `extension_ui_request` → `input` requests and approvals; input handled without a turn → `handled`; no permission system → levels `["host"]`; branching history → native fork at a message |
| ACP v2 | Appendix A in reverse. Levels come from driver data per ACP agent; an agent without a mapping is `host` and can't run under a ceiling below it |

## Appendix D: Bundle rendering

```markdown
# Continuing an earlier session

This continues work from another agent session. Its history follows.

## Summary
<summary, if any>

## History
**User:** <text>
**Assistant:** <text>
- Tool <title>: <ok | error: message>
- Changed files: <paths>

## Open plan
- [ ] <entry>
- [x] <entry>

## Workspace
Repository <repo>, branch <branch>, at <commit>. Changed since the history started: <paths>.

## Next
<the brief, the task's brief, or the user's prompt>
```

Sections without content are left out. Items are rendered oldest first; text is only shortened by
trimming whole items ([23.2](#232-bundles)).

## Appendix E: Amendments

These override the sections they name. Implementations follow this appendix where it differs.

**E1. Moving a session (9.3, 24).** Two-phase over the link: `session/offer { session, log, queue,
requests, turnKeys, tasks }` from the owner, the target starts the binding idle with a pending bundle or
resume ref, then `session/commit { session }` transfers ownership; the old owner keeps a redirect
(`-32050`). Until commit the old owner owns it; if the link drops before commit the offer expires after
60 seconds and nothing moved. `seq`, frames and checkpoints don't move; clients re-snapshot; rewind with
files is not possible to turns before the move.

**E2. Trees across runtimes (17, 19, 20, 22).** Budgets are allotments: a child is created with an
allotment deducted from its parent's remaining budget and returns the unspent part when it ends; the
owner of each session enforces its own allotment. Task changes for a parent on another runtime go to a
durable per-link outbox and are delivered on reconnect. Cursors (`task/wait`, subscriptions) are opaque
strings, and every update carries `runtime`.

**E3. Link authentication** is as in 24.3 (Noise with pinned keys, containment).

**E4. Driver details (30, 31).**
- A user item a driver reports carries `message` when it is the echo of a runtime prompt; history read
  later is deduplicated by harness ref, so ids don't duplicate.
- `binding/start` returns when the harness is ready for a prompt or has raised a request; start-time
  dialogs (trust, login) are requests on the starting binding.
- `history/read` updates use `binding: ""`.
- `binding/action` takes `message` for turn actions; queue entries have `kind: "prompt" | "action" | "wake"`.
- The runtime renders answers to `message` requests into the next prompt; drivers set `countedIn`.
- Drivers start harnesses through `parrot guard -- <cmd>`, which owns process groups, parent-death and job
  objects. The launcher of 5.3 is `parrot guard`.
- Driver data files are KDL, TUI profiles included; each file's schema is generated with the protocol
  schema, with one example per file in `spec/examples/driver-data/`.
- `levels.kdl` also maps tools to levels; a request whose level is above the ceiling's is denied with
  `answeredBy: "ceiling"`.
- TUI control lives in the runtime (`parrot-tui`); a driver under TUI control supplies the profile and
  the transcript reader, and `binding/screen`/`binding/keys` are runtime methods, not driver methods.

**E5. Functions have levels (25, 29).** `FunctionSpec.level: PermissionLevel` is required (desktop
functions are `host`, desktop screenshots `write`). A call outside the ceiling's `functions` patterns
fails with `-32003`; a call above the session's level, or with effects the ceiling doesn't list, opens a
request (27.2 applies).

**E6. Rebind order (9.3).** Set `rebinding` (blocks queue dispatch), start the new binding idle with its
pending bundle or resume, then stop the old one, then clear `rebinding`. `parrot.sessions.handoff`
returns at once; the outcome arrives as an update.

**E7. Restarts (34).** Tasks that were `running` become `interrupted` (a new task status), which wakes
and notifies. Prompting a stopped session in one's own subtree needs only `prompt`. `Wake.start: boolean`
lets a wake start a stopped parent.

**E8. Smaller fixes.**
- `completeOn: "idle"` completes only after a turn with `status: "completed"`.
- `session/keys` needs `answer` and `manage`.
- Tokens: `sessions` filters the index, blocks roots and unrelated workspaces; tokens carry `provides`
  patterns for functions.
- `advisory` is computed from isolation and level: `host` is always advisory below `container`.
- Tokens may be created over a link by its owner.
- New methods: `session/configure { session, ceiling?, budget?, wake?, onParentEnd? }` (`delegate` on a
  subtree, `manage` on a root), `queue/list`, `option/list`, `action/list`; `retry` resends an
  `uncertain` prompt.
- `task/fail` by the own principal needs nothing; by a parent, `delegate`; exposed as `parrot.task.fail`.
  `login` is agent-scoped, needs `manage`. Spawning with a new worktree needs `workspace`.
- `workspace/merge`: `from` is the caller's own or a descendant's workspace, `into` is its own; refused
  while `into` is dirty or has a running turn.
- The profile of each method is a column in the method index; profile `core` is 1–17 without worktrees,
  rebind, rewind, budgets, subagents, TUI and checkpoints.
- `request.redeliver` and `answerable: "process"` are removed; requests are `connection` or `message`.
- Function names follow 27.1; TypeScript in harness code is not Parrot's concern.
- Item status values: `script`, `subagent`, `command`, `file_change`, `web` use
  `"running" | "ok" | "error" | "cancelled"`.
- `retry` needs a turn with exactly one input; otherwise `-32004`.
- Budget exhaustion fails tasks with `error: "budget"`.
- Tree snapshots are paged with the same cursor as `session/tree`.

**E9. Links, moves and budgets, made precise.**
- Noise terminates in the runtime. `parrot connect --link` only relays bytes to the runtime's link
  endpoint (`link.sock` beside `sock`, the same directory rules), never to the owner socket; `tcp`
  connects to the same endpoint over TCP.
- The static key is X25519 (no Ed25519); the runtime id is `rt_` plus the first 26 characters of the
  base32 SHA-256 of the public key.
- When both sides dial at once, the connection dialled by the lower runtime id is kept.
- `session/commit` is keyed and idempotent: the target owns the session on receipt; the old owner
  dispatches nothing for it from `session/offer` until the target confirms, and on a lost answer asks
  again. The offer's log travels as `blob/put { transfer, index, data, last }` chunks of at most
  1 MiB, referenced from the offer by `transfer`.
- Rebind order: set `rebinding`; end the running turn (wait up to 30 seconds, then interrupt); with a
  bundle, start the new binding then stop the old; with a resume ref of the same harness session, stop
  the old binding first, then start the new one.
- A child's allotment returns to its parent when its task ends or it is deleted. When the parent has a
  budget, `session/new` with a parent requires an explicit `budget`.

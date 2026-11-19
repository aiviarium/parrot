# Protocol

Parrot's protocol is JSON-RPC 2.0, version 1. [The specification](../../spec/protocol.md) is the authority: it has every message, type, error and rule. This page is the map, with the parts a client meets first.

The Rust types in `parrot-proto` generate [the JSON Schema](../../spec/schema/parrot.schema.json). The TypeScript client's types are generated from that schema, and CI fails when a generated file is stale.

## Peers

| Peer | Is |
| --- | --- |
| Client | A UI, command line, phone, bridge or orchestrator that drives sessions |
| Runtime | `parrot serve` or the embedded library. It owns sessions, logs, queues, requests, workspaces, functions and links |
| Driver | One agent CLI made into an agent: a built-in core or a process from an egg |

A runtime speaks the client protocol to clients and the driver protocol to drivers. They share framing, errors, items, requests, options and actions.

## Transports and framing

One JSON message per line, UTF-8, for stdio and the local socket. A WebSocket carries one message per text frame. The default message limit is 32 MiB, and a longer line closes the connection. Batches are rejected with `-32600`.

| Transport | Used by | Identity |
| --- | --- | --- |
| stdio | Process drivers, `parrot driver`, `parrot connect` | The user at both ends |
| Link carriers | Links between runtimes | The peer runtime's key |
| Local socket | Clients of a local runtime | The connecting OS user, by peer credentials |
| WebSocket | Remote clients | A client token |

The local socket is `$XDG_RUNTIME_DIR/parrot/sock` on Linux, or `~/.local/state/parrot/sock` when `XDG_RUNTIME_DIR` is unset. On macOS it is `~/Library/Application Support/parrot/sock`, and the runtime refuses a path over 104 bytes. On Windows it is the pipe `\\.\pipe\parrot-<user SID>`.

WebSocket is served at `/parrot` with subprotocol `parrot.1`, and the token as a second subprotocol, `parrot.token.<secret>`. A listener off loopback needs TLS. `parrot serve --ws` refuses an address that isn't loopback.

## Handshake

A client sends `initialize` first. Anything else fails with `-32010`.

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol":[1],"peer":{"kind":"client","name":"notes","version":"1.0.0"},"features":{}}}
```

The answer carries the runtime's id and environment, its features and the connection's principal and scopes. No shared protocol version fails with `-32011` and `data.supported`. Changes inside version 1 are additive, and peers keep fields they don't know.

The runtime advertises `profile.core`, `profile.tree`, `profile.seed`, `profile.link`, `profile.remote` and `profile.tui`, plus `profile.desktop` when `box` is available. [Implementation profiles](#implementation-profiles) says what those cover.

## Principals and scopes

| Principal | Is |
| --- | --- |
| `con_…` | One connection |
| `ses_…` | A session acting through its agent, limited to its own subtree |
| `lnk_…` | A linked runtime, with the grant it was given |

| Scope | Allows |
| --- | --- |
| `read` | Listing and reading, subscribing, and bundles without a summary |
| `prompt` | Prompts, cancel, the queue, and actions with only `turn` or `read` effects |
| `answer` | Answering requests |
| `configure` | Setting options and actions with `write` effects |
| `manage` | Root sessions, starting and stopping bindings, renaming, archiving and agent actions |
| `delegate` | Children, tasks, seeding, rebinding and bundles with a summary |
| `workspace` | Creating, merging and transferring workspaces |
| `destructive` | Deleting sessions, removing workspaces, rewinding and destructive effects |
| `functions` | Providing functions and calling functions other than the session functions |

The owner on stdio or the local socket has every scope. A WebSocket token has the scopes it was made with, limited to its sessions. A call outside the principal's scopes, subtree or ceiling fails with `-32003`.

## Methods

These are the methods the schema defines, grouped as a client meets them.

| Area | Methods | Scope |
| --- | --- | --- |
| Handshake | `initialize` | none |
| Agents | `agent/list`, `agent/refresh` | `read`, `configure` |
| Environments | `env/list` | `read` |
| Sessions | `session/new`, `session/list`, `session/get`, `session/tree`, `session/start`, `session/stop`, `session/rename`, `session/archive`, `session/delete`, `session/configure` | `read`, `manage` or `delegate` by call |
| Reading | `session/read`, `item/get`, `subscribe`, `unsubscribe` | `read` |
| Prompts and queue | `session/prompt`, `session/cancel`, `queue/list`, `queue/pause`, `queue/resume`, `queue/remove`, `queue/edit`, `queue/move`, `queue/clear` | `prompt` |
| Requests | `request/list`, `request/answer` | `read`, `answer` |
| Options | `option/list`, `option/set`, `option/complete` | `read`, `configure` |
| Actions | `action/list`, `action/invoke` | `read`, then the action's effects |
| Workspaces | `workspace/create`, `workspace/list`, `workspace/status`, `workspace/merge`, `workspace/transfer`, `workspace/remove` | `workspace` or `destructive` |
| Tasks and tree | `task/assign`, `task/wait`, `task/finish`, `task/fail`, `session/bundle`, `session/rebind`, `session/rewind` | `delegate`, `read` or `destructive` |
| Functions | `function/list`, `function/search`, `function/describe`, `function/call`, `function/provide` | `read`, `functions` |
| Links | `link/list`, `link/invite`, `link/add`, `link/update`, `link/remove`, `carrier/list` | owner only |
| Tokens | `token/create`, `token/list`, `token/revoke` | owner only |
| TUI | `session/screen`, `session/keys` | `prompt` |

The runtime also has methods that only a link uses, `session/offer`, `session/commit`, `workspace/receive` and `blob/put`, and methods it sends to drivers.

`session/discover` and `session/adopt` are not implemented and answer `-32001` with `data.feature`. The runtime leaves `session.history` out of what its agents report for that reason, so a client doesn't offer to import a CLI session. `session/rebind` to another environment and a child session in a linked environment also answer `unsupported`. A root session can be created in a peer's environment, and a workspace can be transferred to it.

### Driver methods

A runtime sends these to a driver, and a built-in core answers them the same way:

| Method | Does |
| --- | --- |
| `binding/start` | Starts the CLI and returns its ref, isolation, level, network, options and actions |
| `binding/prompt` | Sends a prompt, or steers one |
| `binding/cancel` | Interrupts the turn |
| `binding/answer` | Answers a request |
| `binding/options` | Changes options |
| `binding/action` | Invokes an action |
| `binding/fork` | Forks the CLI session where it can |
| `binding/stop` | Stops the CLI and ends its process |
| `binding/screen`, `binding/keys` | Reads and drives a TUI screen |
| `history/discover`, `history/read` | Finds and reads CLI sessions outside Parrot |

A driver sends `update` notifications with `{ binding, update }`. Ids in them are the driver's own keys; the runtime maps them to Parrot ids. [Writing a driver](../writing-drivers.md) has the update sequence and the egg files.

## Subscriptions and updates

```json
{"jsonrpc":"2.0","id":2,"method":"subscribe","params":{"target":{"session":"ses_01J8…"},"items":50}}
```

| Target | Watches |
| --- | --- |
| `{ "session": "ses_…" }` | One session |
| `{ "tree": "ses_…" }` | A session and its descendants in one stream |
| `{ "index": true }` | Session summaries, agents, environments and workspaces |

The answer has a `subscription` id and a `head` sequence number. With `replayFrom`, the runtime sends the updates after that sequence and no snapshot; if the log no longer has them, it fails with `-32012`.

An update names its subscription, its `seq`, a time and one change: `state`, `session`, `item`, `turn`, `request`, `queue`, `options`, `actions`, `usage`, `notice`, `summary`, `agent`, `env`, `workspace`, `deleted` or `functions`. Only items are patches. Unknown update types are ignored.

A subscription to a session also delivers the open requests of its descendants, so a client watching the root sees every question in the tree.

## Items, state and requests

An item is a patch with an id and a kind: `user`, `assistant`, `reasoning`, `tool`, `script`, `command`, `file_change`, `plan`, `subagent`, `web`, `review`, `compaction`, `hook`, `handoff` or `error`. The session state is one of `starting`, `idle`, `running`, `requires_action`, `stopped`, `lost`, `unrecognised` or `unknown`.

A request asks a person something. Its kind is `approval`, `question`, `plan`, `input`, `auth` or an open vendor kind. `request/answer` takes a choice id or a value. The first valid answer wins; a later one fails with `-32013`.

[Items and state](../items.md) covers the model, delivery, options, levels and budgets.

## Errors

| Code | Name |
| --- | --- |
| `-32600` | Invalid request, including a batch |
| `-32601` | Method not found |
| `-32602` | Invalid params |
| `-32001` | Unsupported, with `data.feature` |
| `-32002` | Not found |
| `-32003` | Forbidden, outside scopes, subtree or ceiling |
| `-32004` | Wrong state |
| `-32010` | Not initialized |
| `-32011` | No shared protocol version, with `data.supported` |
| `-32012` | Replay unavailable |
| `-32013` | Already answered |
| `-32020` | `harness failed`, with `data.detail` |
| `-32030` | Environment offline |
| `-32032` | Budget exhausted |
| `-32040` | Link failed, with `data.detail` |
| `-32050` | Moved, with `data.runtime` |

## Schema and conformance

Every object may carry `_meta` with namespaced keys, and peers keep unknown fields. Open unions keep an unknown value instead of dropping it.

`parrot conform` runs golden fixtures in two directions: a driver on stdio, and a runtime through its client methods. `parrot conform --runtime` starts a fresh runtime per fixture or uses one that is running. Each fixture is JSONL of messages to send and messages expected, with placeholders for ids, sequence numbers, times and paths.

## Implementation profiles

| Profile | Covers |
| --- | --- |
| `core` | Local agents, `dir` workspaces, root sessions, subscriptions and the index, prompts, queue, requests, options, levels, actions, usage |
| `tree` | Children, tasks, worktrees and merge, functions |
| `seed` | Bundles, forks, rewinding, checkpoints, rebinding on one machine |
| `link` | Pairing, carriers and calls between runtimes |
| `remote` | WebSocket tokens and push notifications |
| `tui` | TUI control, an unknown screen answered from a client |
| `desktop` | The `parrot.desktop.*` functions, where `box` is present |

A call outside the implemented profiles fails with `-32001` rather than returning a different shape.

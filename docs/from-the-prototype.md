# From the prototype

Parrot is the agent part of `helightdev/crow`, taken out of the shell and given a runtime of its own. The
prototype's kernel started agent CLIs and read their screens on the environments it managed. A runtime does
that now, on the machine the agent runs on, and any client can talk to it.

| In the prototype | In Parrot |
| --- | --- |
| `packages/crow-term` | `crates/parrot-term` |
| `packages/crow-tui`, and the TUI profiles in `plugins/agent-*/tui/*.toml` | `crates/parrot-tui`, with each egg's `profile.kdl` |
| `packages/crow-models` | `crates/parrot-models` |
| The drivers in `plugins/agent-claude`, `agent-codex`, `agent-pi` and `agent-acp`, in TypeScript in the host half of a plugin | `parrot-driver-claude`, `-codex` and `-pi`, programs in the eggs, and the ACP and TUI cores in the binary. `-opencode` and `-t3` are new |
| The session model of `crow-kernel-agents` | The runtime, with sessions, bindings, turns, items and requests |
| The kernel's `/mcp` endpoint for an agent's tools | `parrot-mcp`, which the runtime injects into the agent CLI |
| The `agentDrivers` contribution of a plugin | An egg the runtime reads at start ([Concepts](concepts.md#eggs)) |
| `plugins/agents`, the chat UI | Crow's `agents` plugin, a client of the runtime ([TypeScript client](reference/client.md)) |
| The kernel's wire types for all of it | `parrot-proto`, specified in the [protocol](reference/protocol.md) |

## The model that changed

| The prototype | Parrot |
| --- | --- |
| `working`, `awaiting approval`, `awaiting input`, `ended` | `running`, `requires_action`, `stopped` |
| A session belonged to one driver for its life | A session has bindings, one CLI run each, so it can be handed to another driver or another machine and keep its id ([Sessions and bindings](concepts.md#sessions-and-bindings)) |
| A tool call and its result were two items | One tool item has the input, the status and the output, and a command, a file change and a subagent have kinds of their own ([Items and state](items.md)) |
| A driver wrote items into the session | A driver reports patches, and the runtime keeps the log and the ids, so a replay is the same |

## What is new

- `parrot serve` is the runtime on a socket of its own, for the user who started it, and `--ws` adds
  WebSocket ([Install](install.md)).
- Agents are installed as eggs, Hatchery packages with a `parrot` target that the release registry publishes.
- A link pairs two runtimes and encrypts what crosses it, so a workspace can move from one to the other
  ([Links](links.md)).
- A session works in a folder, a git worktree or an external workspace, with merges, checkpoints and moving
  between runtimes ([Workspaces](workspaces.md)).
- `@aiviarium/parrot-client` is the client library, and `@aiviarium/parrot-agents` translates between agent
  wire protocols and one model of items and state.
- `parrot conform` runs the recorded traces of Claude Code and Codex ([Writing a driver](writing-drivers.md)).

## What is not here

| Not here | Where it is |
| --- | --- |
| A user interface | Crow's `agents` plugin, or a client of your own |
| Hosts, provisioning and the agent CLIs' sign-in | Rookery ([Behind Rookery](rookery.md)) |

The release's own list of what is left out is Rookery's [Not in this
version](https://rookery.helight.dev/docs/reference/not-in-this-version).

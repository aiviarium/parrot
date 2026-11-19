# Items and state

Every driver, whatever protocol it speaks, reports the same things: items, turns, state changes, requests, options and usage. A client can then render a Claude Code turn and a Codex turn with the same code.

## Items

An item is one part of a conversation. It arrives as a patch that creates or changes it:

```json
{ "type": "item", "item": { "id": "itm_01J8...", "kind": "assistant", "turn": "trn_01J8...", "set": { "text": "The files are" }, "streaming": true } }
```

The first patch names the `kind`, which never changes. `set` replaces fields, `append` adds to a string field and `removed` takes the item away. The runtime maps the driver's own keys to Parrot ids, so the same history gets the same ids after a restart.

| Kind | Holds |
| --- | --- |
| `user` | `text`, optional context and attachments, and `status`: `queued`, `sent`, `handled` or `uncertain` |
| `assistant` | `text` as Markdown |
| `reasoning` | Thinking `text`, and `summary` when it is a summary rather than the raw stream |
| `tool` | `tool`, `title`, `input`, `status`, `output`, the `server` for MCP tools and whether a model or a script called it |
| `script` | A program the agent CLI ran: `language`, optional `code`, `status` and `result` |
| `command` | A shell command, its `cwd`, `status`, `output` and `exit` code |
| `file_change` | `changes` with a path, an operation and an optional diff |
| `plan` | `entries` with `text` and `status`, or plain `text` |
| `subagent` | A child the CLI started: `title`, `session` and `status` |
| `web` | A search or fetch: `query`, `url`, `status` and `results` |
| `review` | A code review: `target` and `findings` |
| `compaction` | A context compaction, `manual` or `auto`, with an optional summary |
| `hook` | A CLI hook: `event`, `name`, `outcome` and `output` |
| `handoff` | A change of binding: `from`, `to` and how it happened |
| `error` | A `message` and an optional `code` |

Text streams in patches. Once a stream ends, the stored log replaces its `append` patches with one `set`, so replay is short. Long tool output is truncated in updates; `item/get` returns all of it.

Items may also carry `source`: `native`, `transcript`, `tui`, `inferred` or `runtime`. That says where the content came from, not which CLI produced it.

## Turns

A turn is one run of the agent from a prompt to a stop.

| Turn status | Means |
| --- | --- |
| `running` | The CLI is working |
| `completed` | It stopped normally |
| `interrupted` | A person or a ceiling interrupted it |
| `failed` | It stopped with an error |

A turn also has `stop`, such as `end`, `max_tokens`, `turn_limit`, `refusal`, `cancelled`, `budget` or `error`, and `continuable`. A continuable turn is what the `continue` action is for.

## State

The session state is an observation, not a promise. Clients accept any change.

| State | Means |
| --- | --- |
| `starting` | The binding starts |
| `idle` | The CLI waits for a prompt |
| `running` | A turn runs |
| `requires_action` | A blocking request waits |
| `stopped` | No CLI runs |
| `lost` | The CLI disappeared |
| `unrecognised` | TUI control found no state for the screen |
| `unknown` | Nothing better is known yet |

Background work, such as a subagent or a task, is listed on the session and does not change the state. The state stays `running` while the turn it belongs to runs.

## Requests

A request is how the CLI asks a person something. The turn waits while a request with `blocking: true` is open, and the session reports `requires_action`.

| Kind | Asks |
| --- | --- |
| `approval` | To allow a tool call or a command, with choices such as allow once or always |
| `question` | For an answer, with a JSON Schema in `input` |
| `plan` | To accept or reject a plan |
| `input` | For free input, such as a screen the TUI profile does not know |
| `auth` | To sign in, with a URL, a device code or a form |

Each request has a `subject` describing what it is about: a tool call, a command, files, a network host, a plan or a function call. A choice has an id, a label, an `intent` of `allow`, `deny` or `neutral`, and a `scope` of `once`, `session` or `always`. Under a ceiling, choices with a wider scope are removed before the request is shown.

| Method | Does |
| --- | --- |
| `request/list` | Lists open requests, of one session or of a subtree |
| `request/answer` | Answers with a choice id, a value or an input object |

The first valid answer wins and reaches every subscriber. A later one fails with `-32013`, unless it repeats the same key. A request from a child reaches subscribers of its ancestors, so a client watching the root sees every question in the tree.

Under a ceiling, a request that nobody answers within `ceiling.unattended` seconds is denied with the first `deny` choice, and `answeredBy` says `ceiling`.

## Prompts and the queue

A prompt can start a turn or join the current one. `delivery` says which:

| `delivery` | Idle | Running |
| --- | --- | --- |
| `auto` | Starts a turn | Steers when the agent supports it, else queues |
| `queue` | Starts a turn | Queues |
| `steer` | Starts a turn | Steers, or fails with `unsupported` |
| `interrupt` | Starts a turn | Interrupts, then starts a turn with the new text |

The runtime owns the queue; drivers never queue. `session/cancel` interrupts the turn and pauses the queue. `queue/resume` starts sending entries again, and a client prompt with another delivery unpauses it. Wakes from tasks never do.

## Options and permission levels

An option is a setting the agent CLI has: a model, a reasoning effort, a mode or a sandbox. Options come from the CLI at runtime where it reports them, and from the egg's `options.kdl` otherwise. `applies` says when a new value takes effect: `start`, `now` or `next_turn`.

A permission level says what an agent may do without asking anyone:

| Level | Allows without asking |
| --- | --- |
| `read` | Read files in the workspace |
| `write` | Also edit files in the workspace |
| `sandboxed` | Also run commands in an OS sandbox limited to the workspace |
| `host` | Also run commands outside a sandbox or touch files outside the workspace |

Network access is a separate flag. The driver computes the level from what the CLI actually enforces. An unmapped combination counts as `host` with network, so a new mode in a CLI update can't slip under a ceiling. If the level rises above the session's ceiling, the runtime sets the options back or stops the binding.

## Usage and budgets

Usage carries tokens, cost, context use and rate limits where the CLI reports them. A ceiling can set a budget of tokens, cost, seconds, turns or sessions. When a budget runs out, the runtime interrupts running turns, fails their tasks with `stop: "budget"` and pauses their queues.

## Actions

An action is something a client can invoke, such as `continue`, `retry`, `compact`, `review`, `clear`, `summarize`, `login` or `status`. Some come from the CLI, some from the driver and some from the runtime. `action/list` shows the ones a session or agent offers, with the states they apply in and their effects.

`continue` and `retry` are always offered. No CLI has a native continue, so the runtime sends a prompt that continues the conversation and marks its origin as the action.

# Concepts

Parrot has one runtime, one environment and one model for what agents do. A client talks to the runtime; the runtime talks to drivers; a driver talks to one agent CLI.

| Word | Is |
| --- | --- |
| Runtime | `parrot serve`, or the library. It owns sessions, logs, queues, requests, workspaces, eggs and links |
| Environment | The machine or container a runtime runs on. Each runtime has one; others appear through links |
| Agent | One agent id, such as `claude` or `codex`, with the driver that runs it |
| Driver | The program that connects one CLI to the runtime, either a built-in core or a program in an egg |
| Egg | A Hatchery package with the `parrot` target. It holds a driver program or the data a built-in core needs |
| Session | A conversation with a stable id, a log, a workspace and a place in a tree |
| Binding | One CLI run of a session: an agent, an environment and the CLI's own session reference |
| Workspace | The folder a session works in |
| Item | One thing in a conversation: text, thinking, a tool call, a file change, a plan and so on |
| Request | Something the agent CLI asks a person: an approval, a question, a plan or free input |
| Link | A paired, encrypted connection to another runtime |

## The runtime

`parrot serve` starts the runtime. It serves the [protocol](reference/protocol.md) on the local socket, and on WebSocket with `--ws`. The socket is reachable only by the user who started the runtime: a Unix socket in a `0700` directory, or a named pipe with an ACL for that user on Windows.

The runtime keeps its state in one directory: the SQLite database, the link key, installed eggs and workspaces. [Configuration](reference/configuration.md) lists every file.

A client can also be a program that provides functions. The runtime calls it during a turn, and the agent reaches it through the MCP server that Parrot injects into the agent CLI. The session functions `parrot.sessions.*` and `parrot.tasks.*` are always there; other functions come from clients and linked runtimes.

## Agents and drivers

An agent exists only when its egg is installed. The runtime reads `<state_dir>/eggs` at start and asks each egg's `driver.kdl` how to run it.

| `driver.kdl` | The runtime runs |
| --- | --- |
| `driver "builtin" name="acp"` | `parrot driver acp`, a core built into the binary |
| `driver "builtin" name="tui"` | The TUI core in the runtime, with the egg's `profile.kdl` |
| `driver "command" { run … }` | The program in the egg's variant folder for this OS and architecture |

The Claude, Codex, pi, opencode and T3 eggs each carry a program, `parrot-driver-<agent>`. The ACP and TUI cores are general: a data-only egg names the command to start and the CLI versions its data covers.

Each driver reports the features it has, such as `session.stream`, `session.resume`, `session.fork`, `attachments` or `usage`. The runtime degrades what an agent lacks instead of failing. For example, a client asking to steer a turn on an agent without `session.steer` gets the prompt queued.

A driver can also read the CLI's transcript beside the live protocol. That is how a session keeps its items when the agent CLI is restarted, and how TUI control gets content from a screen with no message stream.

## Sessions and bindings

A session is what a person sees: one conversation, one workspace, one id. A binding is one run of it. Handing a session to another agent CLI, or moving it to another machine, makes a new binding and keeps the session id.

| Session state | Means |
| --- | --- |
| `starting` | The binding is starting |
| `idle` | The CLI is ready for a prompt |
| `running` | A turn runs |
| `requires_action` | A blocking request waits for an answer |
| `stopped` | No CLI runs; the log and workspace stay |
| `lost` | The CLI vanished without an error the runtime could report |
| `unrecognised` | Under TUI control, the screen matches no profile state |
| `unknown` | The runtime can't tell yet |

A session can be a root, a child of another session, or an embedded subagent that the CLI started. The tree matters for requests, tasks, budgets and links. [Items and state](items.md) covers the state machine, turns, the queue and requests.

## Workspaces

A session works in a `dir`, a git `worktree` or an `external` workspace that another system owns. The workspace is tracked by the runtime and can be listed, merged, removed and, across links, transferred. [Workspaces](workspaces.md) has the rules for each.

## Eggs

An egg is a package folder with a `package.kdl` and a `parrot` folder:

```text
claude/
  package.kdl
  parrot/
    driver.kdl
    options.kdl
    levels.kdl
    correlate.kdl
    variants/linux-x86_64/parrot-driver-claude
```

`package.kdl` declares a `parrot` target and one variant per platform. `driver.kdl` says how the driver runs, which CLI versions the data covers and which controls the agent has. `options.kdl`, `levels.kdl`, `limits.kdl` and `correlate.kdl` fill in what the CLI doesn't report itself.

An egg can declare a second target, `rookery`, with Ansible tasks that install the same CLI on a provisioned host ([Behind Rookery](rookery.md)).

Installing, removing and packing eggs is the `parrot eggs` command ([Command line](reference/cli.md#parrot-eggs)). [Writing a driver](writing-drivers.md) has the file formats and the driver protocol.

## Links and carriers

A link connects two runtimes. They authenticate each other by key, encrypt end to end, and see the other's environment and offered sessions. The bytes travel over a carrier: SSH, any command, direct TCP, or a carrier egg for a platform's own channel. [Links](links.md) covers pairing, grants and containment.

## Where Parrot stops

Parrot has no user interface and no opinion about how a person should see a session. It doesn't provision machines, install agent CLIs on a host or sign them in. Rookery does that, and the eggs' `rookery` target is how the same package serves both. It also doesn't read what a package contributes beyond a driver egg or run a package's code as a plugin system.

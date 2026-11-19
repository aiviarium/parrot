# Command line

`parrot` starts a runtime, talks to one, or runs a driver. Every command needs a subcommand:

```bash
parrot --help
parrot serve --help
```

| Command | Does |
| --- | --- |
| [`parrot serve`](#parrot-serve) | Runs the runtime on the local socket, and on WebSocket or a link listener |
| [`parrot connect`](#parrot-connect) | Relays standard input and output to the runtime |
| [`parrot driver`](#parrot-driver) | Serves a built-in driver core on standard input and output |
| [`parrot guard`](#parrot-guard) | Runs a command that dies with its parent |
| [`parrot conform`](#parrot-conform) | Runs the conformance fixtures |
| [`parrot mcp`](#parrot-mcp) | Serves the MCP server for a binding |
| [`parrot link`](#parrot-link) | Pairs with other runtimes and manages links |
| [`parrot eggs`](#parrot-eggs) | Installs, lists, checks and packs driver eggs |
| [`parrot probe`](#parrot-probe) | Records a real CLI through a probe scenario |
| [`parrot acp`](#parrot-acp) | Serves a session as an ACP agent |
| [Client commands](#client-commands) | `agents`, `sessions`, `new`, `prompt`, `watch`, `requests`, `answer`, `cancel`, `stop`, `options`, `set` |

A command exits with `0` on success and `1` on an error, after printing `parrot: <error>` on standard error. A usage error exits with `2`. `parrot guard` exits with its child's status.

## parrot serve

Runs the runtime. It serves the local socket and, with the flags below, more transports.

| Flag | Takes | Does |
| --- | --- | --- |
| `--state-dir <DIR>` | A path | The state directory; `state.db` and the other files go here |
| `--ws <ADDR>` | `host:port` | Serves WebSocket, which must be a loopback address without TLS |
| `--link-listen <ADDR>` | `host:port` | Accepts links from other runtimes over TCP |
| `--workspace-root <DIR>` | A path | Where workspaces that arrive over a link go; `<state-dir>/workspaces` by default |

```bash
parrot serve --state-dir ./state
parrot serve --ws 127.0.0.1:7420 --link-listen 0.0.0.0:7421
```

The runtime stops on Ctrl-C, and on SIGTERM on Unix. The state directory defaults to the [platform path](configuration.md#state-directory). `PARROT_SOCKET` moves the socket.

## parrot connect

Relays this command's standard input and output to the runtime's socket, as the user who runs it.

| Flag | Takes | Does |
| --- | --- | --- |
| `--socket <PATH>` | A path or pipe name | The runtime's socket instead of the default |
| `--link` | | Relays to the link endpoint, `link.sock` beside the socket |
| `--fd <N>` | A descriptor above 2 | Relays this inherited descriptor; needs `--link`, and is Unix only |

A client that speaks the protocol over stdio can run `parrot connect` instead of opening a socket. With `--link`, the bytes are a Noise session that the runtime terminates, which is how SSH and command carriers reach a runtime.

## parrot driver

```bash
parrot driver acp
parrot driver tui
```

Serves one of the two built-in cores on standard input and output: `acp` for an ACP agent, and `tui` for a CLI driven through its terminal. Other names fail with a message that names the two cores. The runtime starts these itself; the command is also how the cores are tested over stdio.

## parrot guard

```bash
parrot guard -- codex exec --skip-git-repo-check
```

Runs a command in its own process group so that it stops when the guard's parent stops. On Linux it uses a parent-death signal, on macOS a kqueue watch and on Windows a job object. A `.ps1` shim that a package manager installed runs through `pwsh` or Windows PowerShell.

## parrot conform

Runs the golden fixtures against a driver or a runtime.

| Flag | Does |
| --- | --- |
| `--runtime` | Runs the client fixtures against a runtime instead of the driver fixtures |
| `--socket <PATH>` | Uses a running runtime; without it, each fixture starts its own `parrot serve` |
| `--fixtures <DIR>` | Runs the `.jsonl` fixtures in this folder instead of the built-in ones |
| `--scenarios <DIR>` | Point the fake CLI at `<dir>/<scenario>.jsonl`; needs `--scenario-var` |
| `--scenario-var <VAR>` | The variable that names the scenario file |
| `--agent <ID>` | The agent the fixtures start, `claude` by default |
| `--only <NAME>` | Runs only this fixture; repeat for more |
| `--pending` | Also runs fixtures marked pending |
| `--wait <SECONDS>` | How long each message has, 10 by default |
| `-v`, `--verbose` | Prints every message a failing fixture received |
| `--state-dir <DIR>` | The state directory whose eggs the fixtures use |

The rest of the arguments are the driver command to test, such as `parrot driver claude`. Each fixture prints `pass`, `FAIL` or `skip`, then a summary, and the command exits with `1` if a fixture fails.

```bash
PARROT_CLAUDE_BIN=target/debug/fake-claude parrot conform \
  --scenarios crates/parrot-driver-claude/fixtures/scenarios \
  --scenario-var PARROT_FAKE_CLAUDE_SCENARIO \
  parrot driver claude
```

## parrot mcp

Serves the MCP server for one binding on standard input and output, or on loopback HTTP with `--listen`. The runtime injects this command into an agent CLI; a person rarely runs it.

| Flag | Default | Does |
| --- | --- | --- |
| `--binding <ID>` | `PARROT_BINDING` | The binding the server serves |
| `--socket <PATH>` | `PARROT_SOCKET`, then the platform path | The runtime's local socket |
| `--endpoint <URL>` | | A runtime WebSocket endpoint, such as `ws://127.0.0.1:7420/parrot` |
| `--ca-file <FILE>` | | PEM roots for a `wss://` endpoint, instead of the public roots |
| `--token-file <FILE>` | `PARROT_MCP_TOKEN` | A file holding the binding's credential |
| `--listen <ADDR>` | | Serves MCP over HTTP on a loopback address instead of stdio |
| `--tool-timeout <SECONDS>` | `PARROT_TOOL_TIMEOUT` | The agent CLI's tool-call timeout |

`--socket` and `--endpoint` conflict. The credential never goes on the command line; it is sent in `initialize` and, over WebSocket, as the `parrot.token.` subprotocol.

## parrot link

Manages links to other runtimes. `--socket` is global to the command. A pairing code is read from standard input, never from an argument. The command is not available on Windows yet.

| Subcommand | Flags | Does |
| --- | --- | --- |
| `invite` | `--name <NAME>`, `--grant <SCOPE,…>`, `--sessions none\|linked\|all`, `--offer-env`, `--expires <RFC 3339>`, `--json`, `--authorized-key <KEY>`, `--authorized-keys <PATH>` | Makes an invite and prints its one-time code |
| `add` | `--name <NAME>`, one of `--ssh <DEST>`, `--tcp <ADDR>`, `--command <ARGV…> ;`, `--carrier <JSON>`; `--grant`, `--sessions`, `--offer-env`, `--code-stdin`, `--ask-grant`, `--ask-sessions`, `--ask-offer-env` | Dials a peer and pairs |
| `list` | | Prints the link records |
| `remove` | `<LINK>` | Removes a link and revokes its pairing |

`--grant` and `--sessions` are what the peer may do here and see here. `--ask-grant`, `--ask-sessions` and `--ask-offer-env` are what this runtime asks the peer for when it pairs over the carrier. `--command` takes every argument up to a `;`, so put it last. `--expires` takes an RFC 3339 time; an invite lasts ten minutes without it.

```bash
parrot link invite --name laptop --grant read,prompt --sessions linked
parrot link add --name studio --ssh ops@192.0.2.10 --ask-grant read,prompt
parrot link list
parrot link remove lnk_01J8Q4T7N3K2M9X8C6V5B4A3S2
```

The scopes are `read`, `prompt`, `answer`, `configure`, `manage`, `delegate`, `workspace`, `destructive` and `functions`.

## parrot eggs

Installs and packs driver eggs.

| Subcommand | Flags | Does |
| --- | --- | --- |
| `install` | `<SPEC>…`, `--registry <URL\|DIR>` (repeat), `--state-dir <DIR>` | Installs packages such as `aiviarium/claude`, `claude@^0.1` or `./eggs/claude` |
| `remove` | `<NAME>`, `--state-dir <DIR>` | Removes an installed package |
| `list` | `--state-dir <DIR>`, `--json` | Lists each installed agent's egg |
| `check` | `[DIR]…`, `--state-dir <DIR>` | Checks package folders, or the eggs the runtime would load |
| `pack` | `<DIR>…`, `--out <DIR>` or `--publish <DIR>`, `--build`, `--target <TRIPLE>`, `--release`, `--workspace <DIR>`, `--sign-key-env <VAR>` | Writes `.egg` files or publishes a static registry |

`--out` is required unless `--publish` is given. `--target`, `--release` and `--workspace` only apply with `--build`; `--sign-key-env` only with `--publish`, and it names a variable that holds the key, never the key itself. Without `--registry`, install searches the registries in `hatchery.kdl`; with no such file, `PARROT_EGGS_REGISTRY`, then `https://aiviarium.github.io/parrot/eggs`.

```bash
parrot eggs pack eggs --build --publish target/eggs-registry
parrot eggs install aiviarium/claude --registry target/eggs-registry
parrot eggs check ./eggs/claude
```

## parrot probe

Runs `probes/<scenario>.kdl` from an installed agent's egg against the real CLI and records what it emits. It is not part of CI.

| Argument | Does |
| --- | --- |
| `<AGENT>` | The agent id, such as `claude` |
| `<SCENARIO>` | The probe file name without `.kdl` |
| `--out <DIR>` | Where to write the recording; required |
| `--dir <PATH>` | The folder the agent CLI works in, the current folder by default |
| `--state-dir <DIR>` | The state directory whose eggs to use |

The output folder gets `transcript.jsonl`, `frames.jsonl` and `probe.json`. The last holds the agent, egg, scenario, CLI version and outcome.

## parrot acp

Serves a Parrot session as an ACP agent on standard input and output.

| Flag | Does |
| --- | --- |
| `--socket <PATH>` | The runtime's socket instead of the default |
| `--session <ID>` | Serves this session instead of creating one on `session/new` |
| `--agent <ID>` | The agent `session/new` starts; the first usable agent by default |
| `--dir <PATH>` | The folder to use when the ACP client sends no `cwd` |

## Client commands

These talk to a running runtime over its socket. Each takes `--socket <PATH>` and `--json`; `--json` prints the raw results and updates instead of the lines.

| Command | Arguments and flags | Does |
| --- | --- | --- |
| `agents` | | Lists the agents the runtime can start |
| `sessions` | | Lists sessions |
| `new` | `<AGENT>`, `--dir <PATH>`, `--title <TITLE>` | Creates a session and starts it; prints its id |
| `prompt` | `<SESSION> <TEXT>`, `--delivery auto\|queue\|steer\|interrupt` | Sends a prompt |
| `watch` | `<SESSION>`, `--until <STATE>` | Subscribes and prints updates until Ctrl-C or the state is reached |
| `requests` | `[SESSION]` | Lists open requests, of one session or of all |
| `answer` | `<REQUEST> <ANSWER>` | Answers with a choice id, or with an answer object as JSON |
| `cancel` | `<SESSION>` | Interrupts the turn and pauses the queue |
| `stop` | `<SESSION>` | Stops the agent CLI; the session and its log stay |
| `options` | `<SESSION>` | Shows the session's options |
| `set` | `<SESSION> <OPTION> <VALUE>` | Sets one option; the value is JSON when it parses, else a string |

```bash
parrot agents
parrot new claude --dir ~/src/notes --title "Notes"
parrot prompt ses_01J8Q4T7N3K2M9X8C6V5B4A3S2 "Summarise the README."
parrot watch ses_01J8Q4T7N3K2M9X8C6V5B4A3S2 --until idle
parrot requests ses_01J8Q4T7N3K2M9X8C6V5B4A3S2
parrot set ses_01J8Q4T7N3K2M9X8C6V5B4A3S2 permissionMode plan
```

`new` canonicalises `--dir`, and uses the current folder when the flag is absent. `watch` prints a snapshot first. `requests` asks for status `open`. `answer` sends an object as the answer, wraps a plain string as `{"choice": "…"}`, and wraps any other JSON as `{"value": …}`.

## Environment

| Variable | Read by | Holds |
| --- | --- | --- |
| `PARROT_SOCKET` | `serve`, `connect`, the client commands, `mcp`, `link` | The socket path or pipe name |
| `PARROT_EGGS_REGISTRY` | `eggs install` | The registry used when no `--registry` and no `hatchery.kdl` applies |
| `PARROT_NOTIFY` | `serve` | Space-separated push targets, `webhook:<url>` or `ntfy:<server>/<topic>` |
| `PARROT_BINDING` | `mcp` | The binding to serve |
| `PARROT_MCP_TOKEN` | `mcp`, drivers | The binding's credential |
| `PARROT_TOOL_TIMEOUT` | `mcp` | The agent CLI's tool-call timeout in seconds |

The rest are on [Configuration](configuration.md#environment-variables).

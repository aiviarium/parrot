# Getting started

This walkthrough starts a runtime, installs one agent, creates a session and answers what the agent asks. It uses Claude Code as the example; any [driver](drivers/claude.md) works the same way.

## 1. Install an agent

```bash
parrot eggs install aiviarium/claude
parrot eggs list
```

With a checkout or a local registry, see [Install](install.md). The agent's CLI, `claude` here, has to be on `PATH` and signed in. Parrot drives the CLI; it doesn't install or sign in to it.

## 2. Start the runtime

```bash
parrot serve
```

`parrot serve` opens the local socket and keeps the state directory in `~/.local/state/parrot` on Linux, `~/Library/Application Support/parrot` on macOS and `%LOCALAPPDATA%\parrot` on Windows. Everything else in this page talks to that socket.

| Flag | Does |
| --- | --- |
| `--state-dir <DIR>` | Uses this state directory instead of the default |
| `--ws <ADDR>` | Also serves WebSocket on a loopback address, such as `127.0.0.1:7420` |
| `--link-listen <ADDR>` | Accepts links from other runtimes over TCP, such as `0.0.0.0:7421` |
| `--workspace-root <DIR>` | Puts workspaces that arrive over a link here, `workspaces` in the state directory by default |

## 3. Look around

In a second terminal:

```bash
parrot agents      # every agent an installed egg provides
parrot sessions    # every session this runtime owns
```

`parrot agents` shows the agent id, its driver and the CLI version the driver data covers. An empty table means no egg is installed; the protocol's `agent/list` also returns a hint that says how to install one.

## 4. Start a session

```bash
parrot new claude --dir ~/src/notes --title "First session"
```

`new` prints the session id, such as `ses_01J8Q4T7N3K2M9X8C6V5B4A3S2`. The agent works in the folder given to `--dir`, which is the current folder when left out. The session starts at once.

Send a prompt:

```bash
parrot prompt ses_01J8Q4T7N3K2M9X8C6V5B4A3S2 "List the files here and explain what this project does."
```

`prompt` returns when the runtime accepts the message, not when the turn ends. The disposition says what happened: `started`, `steered` or `queued`. To follow the session, watch it in another terminal:

```bash
parrot watch ses_01J8Q4T7N3K2M9X8C6V5B4A3S2
```

`watch` prints a snapshot, then a line for each change: items as they finish, states, turns, requests and the queue. Stop it with Ctrl-C, or use `--until idle` to stop once the session is idle.

## 5. Answer a request

An approval, a question or a plan arrives as a request. The agent's turn waits while a blocking request is open.

```bash
parrot requests
parrot answer req_01J8Q4WA1B2C3D4E5F6G7H8J9K allow   # a choice id the request lists
```

`answer` takes a choice id that the request lists, or an object as JSON for a question with parts. The session moves back from `requires_action` to `running` when the CLI accepts it. `parrot watch` shows both changes.

## 6. Change an option

```bash
parrot options ses_01J8Q4T7N3K2M9X8C6V5B4A3S2
parrot set ses_01J8Q4T7N3K2M9X8C6V5B4A3S2 permissionMode plan
```

The value is JSON when it parses, else a string. `options` lists what the agent offers, which values apply at once and which apply at the next turn.

## 7. Stop and restart

```bash
parrot cancel ses_01J8Q4T7N3K2M9X8C6V5B4A3S2   # interrupt the turn and pause the queue
parrot stop ses_01J8Q4T7N3K2M9X8C6V5B4A3S2     # end the CLI; the session and its log stay
```

A later `parrot prompt` to a stopped session starts it again, resuming the CLI session where the agent supports it. Stop the runtime with Ctrl-C.

## Connect a program

`parrot connect` relays standard input and output to the local socket. A client that speaks the protocol over stdio can run `parrot connect` instead of opening the socket itself, as the TypeScript client does with `Client.overStdio("parrot", ["connect"])`.

`parrot connect --link` relays to the link endpoint for a carrier, which is how SSH and command carriers reach a runtime ([Links](links.md)).

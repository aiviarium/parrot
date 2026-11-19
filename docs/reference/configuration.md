# Configuration

A runtime keeps everything in one state directory. A configuration file sits beside it, and a few environment variables change what the runtime and its drivers do.

## State directory

| OS | Default |
| --- | --- |
| Linux | `$XDG_STATE_HOME/parrot`, else `~/.local/state/parrot` |
| macOS | `~/Library/Application Support/parrot` |
| Windows | `%LOCALAPPDATA%\parrot` |

`parrot serve --state-dir <DIR>` and `--state-dir` on the egg commands use another one. The directory is private to the user who runs the runtime.

| Path | Holds |
| --- | --- |
| `state.db` | SQLite: the update log, projections, intent log, links and hashed tokens |
| `config.json` | The runtime's configuration |
| `hatchery.kdl` | Extra registries for `parrot eggs install` |
| `link.key` | The runtime's static link key, mode `0600` |
| `eggs/` | Installed packages, with `eggs.lock` |
| `cache/hatchery/` | Cached registry data |
| `workspaces/` | Default folder for workspaces that arrive over a link |
| `transfers/` | Incoming workspace transfers |
| `files/` | Scratch files, such as screenshots |
| `sock` or the named pipe | The local socket; `PARROT_SOCKET` moves it |
| `link.sock` | The link endpoint, beside the socket on Unix |

An older runtime's `tokens.json` and `links.json` are imported once, then renamed to `*.json.imported`. Updates and frames are kept 30 days by default. A session's latest state and its items stay until the session is deleted.

## config.json

A missing file is an empty configuration. A file that doesn't parse stops the runtime, with the path and the parser's message.

```json
{
  "notify": ["webhook:https://hooks.example.test/parrot"],
  "notifyUrl": "https://app.example.test/sessions/",
  "notifyCa": "hooks-ca.pem",
  "tui": {
    "notes-cli": {
      "command": ["notes", "--interactive"],
      "profile": "/home/me/.config/parrot/notes.kdl",
      "name": "Notes CLI",
      "size": [40, 120]
    }
  }
}
```

| Field | Shape | Does |
| --- | --- | --- |
| `notify` | `string[]` | Push targets, each `webhook:<url>` or `ntfy:<server>/<topic>` |
| `notifyUrl` | `string` | A link put in a push body, with the session id appended |
| `notifyCa` | `string` | A PEM file of the only roots HTTPS push targets are checked against |
| `tui` | `{ <agent>: TuiAgent }` | Agents driven through their terminal |

`notifyCa` is relative to the state directory. Configuration holds no secrets.

### TuiAgent

| Field | Shape | Does |
| --- | --- | --- |
| `command` | `string[]` | The CLI and its arguments |
| `profile` | `string` | A path to the KDL profile |
| `name` | `string` | The display name |
| `env` | `{ [name]: string }` | Extra environment for the CLI |
| `transcript` | `{ format, dir? }` | `format` of `claude`, `codex` or `jsonl`, with an optional folder |
| `mcp` | `string` | `mcp-config` or `config-override` |
| `unrecognisedAfterMs` | `number` | How long an unmatched screen is held, 2000 by default |
| `size` | `[rows, cols]` | The terminal size |
| `noGuard` | `boolean` | Starts the CLI without `parrot guard` |

An entry wins over the egg of the same agent id for its command and profile. [TUI control](../drivers/tui.md) has the rest.

## hatchery.kdl

The file adds registries that every `parrot eggs install` searches, before the default:

```kdl
registries {
  http "https://eggs.example.test" key="ed25519:<public key>"
  dir "/srv/eggs"
  git "https://github.com/example/eggs.git" only="example"
}
```

The node name is the registry kind and the argument its location. `key` makes every egg from it carry a signature by that Ed25519 key. `min-age`, `only` and `token-env` work as they do in Hatchery. With a `hatchery.kdl` present and no `--registry`, Parrot searches its registries and not the default.

The file has no secrets in it: `token-env` names a variable.

## Environment variables

| Variable | Read by | Holds |
| --- | --- | --- |
| `PARROT_SOCKET` | Runtime, clients, `mcp`, `link` | The socket path or pipe name |
| `PARROT_EGGS_REGISTRY` | `eggs install` | A registry used when no `--registry` and no `hatchery.kdl` applies |
| `PARROT_NOTIFY` | `serve` | Space-separated push targets, as in `notify` |
| `PARROT_DRIVER_DATA` | A driver | The installed egg's `parrot` folder |
| `PARROT_DRIVER_AGENT` | A driver | The agent id it serves |
| `PARROT_DRIVER_AGENT_NAME` | A driver | The agent's display name |
| `PARROT_<AGENT>_BIN` | The TUI core | The CLI for an agent with TUI control, such as `PARROT_CLAUDE_BIN` |
| `PARROT_TUI_SIZE` | The TUI core | The terminal size, as `<rows>x<cols>` |
| `PARROT_BIN` | A carrier egg | The `parrot` binary to relay an inbound channel to |
| `PARROT_LINK_SOCKET` | A carrier egg | The link endpoint to relay to |
| `PARROT_MCP_TOKEN` | `mcp`, drivers | The binding's credential |
| `PARROT_BINDING` | `mcp` | The binding to serve |
| `PARROT_TOOL_TIMEOUT` | `mcp`, drivers | The agent CLI's tool-call timeout in seconds |
| `PARROT_BOX` | `serve` | The `box` program for the desktop functions |
| `PARROT_SHADOW` | Drivers | Runs passive readers beside the control source and reports differences |
| `PARROT_ACP_BIN`, `PARROT_ACP_ARGS` | The ACP core | The agent command and its arguments |
| `PARROT_T3_URL`, `PARROT_T3_TOKEN` | The T3 driver | A T3 server's MCP address and bearer token |
| `PARROT_T3_MCP`, `PARROT_T3_MCP_ARGS` | The T3 driver | A T3 MCP server command and its arguments |
| `PARROT_CLAUDE_BIN`, `PARROT_CODEX_BIN`, `PARROT_OPENCODE_BIN`, `PARROT_PI_BIN` | Each driver | The CLI binary, instead of the name on `PATH` |

The per-driver variables are useful for a wrapper, a version manager or a test. A driver otherwise starts the CLI by name.

## Notifications

A request that opens with `notify` other than `none` becomes a push event. The current runtime pushes only requests; the specification also lists task ends and lost sessions, which it does not send yet. A webhook receives a small JSON body:

```json
{
  "event": "request",
  "session": "ses_01J8Q4T7N3K2M9X8C6V5B4A3S2",
  "request": "req_01J8Q4WA1B2C3D4E5F6G7H8J9K",
  "title": "Claude Code asks to run a command",
  "text": "ls -la",
  "url": "https://app.example.test/sessions/ses_01J8Q4T7N3K2M9X8C6V5B4A3S2"
}
```

`url` appears only when `notifyUrl` is set. An ntfy target posts the text to `<server>/<topic>` with the title in a `Title` header. A body carries a title and at most 200 characters of text, never an item or code.

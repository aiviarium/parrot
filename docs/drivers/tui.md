# TUI control

TUI control drives an agent CLI through its terminal. Parrot runs the CLI on a PTY, reads each settled screen, and maps the screen to a state, a dialog or an action with a profile. It is the fallback for a CLI that offers no protocol to speak.

| Part | Value |
| --- | --- |
| Agent id | The egg's package id, or the key in `config.json` |
| Egg | Any egg with `controls "tui"` and a `profile.kdl` |
| Driver | The TUI core, built into the runtime |
| Controls | `tui`, and `native` too for the Claude Code and Codex eggs |

## Which control a session uses

An egg lists its controls in `driver.kdl`, the first being the default:

```kdl
controls "native" "tui"
```

A client picks the other with `control: "tui"` in `session/new`. The Claude Code and Codex eggs offer both; their TUI profiles read the CLI's transcript for items, so a TUI session has the same items as a native one.

An egg whose only control is `tui` has no driver program. Its `parrot` folder holds `profile.kdl`, `correlate.kdl`, the command and any transcript setting:

```kdl
driver "builtin" name="tui"
command "example-cli"
controls "tui"
data-version "1"
transcript "jsonl" dir="~/.example-cli/transcripts"
mcp "mcp-config"
```

| Node | Does |
| --- | --- |
| `command "prog" "arg"…` | The CLI to run |
| `transcript "claude"`, `"codex"` or `"jsonl" dir=…` | Where items come from |
| `mcp "mcp-config"` | Passes the Parrot MCP server as `--mcp-config` JSON |
| `mcp "config-override"` | Passes it as `-c` overrides |

For an egg with TUI control, `PARROT_<AGENT>_BIN` replaces the CLI command, where `<AGENT>` is the agent id in upper case with `_` for `-`. `PARROT_TUI_SIZE`, as `<rows>x<cols>`, replaces the terminal size.

## The screen model

Each settled snapshot becomes a labelled tree. Nodes have a role such as `dialog`, `menu`, `input`, `text` or `status`, their bounds, their text and a stable id across frames. A profile tags nodes, names states and defines actions:

```kdl
state name="idle" {
  when exists="prompt"
}

action "submit" {
  steps { key "Enter" }
  expect { not-exists "busy" }
}
```

The profile sits in the egg as `parrot/profile.kdl`. Its schema is `spec/schema/driver-data/profile.schema.json`. [parrot-tui](https://github.com/aiviarium/parrot/blob/main/crates/parrot-tui/README.md) describes every node rule, matcher, condition and step.

## States and requests

| Screen | Session |
| --- | --- |
| A profile state | Its Parrot state, such as `idle`, `running` or `requires_action` |
| A dialog the profile knows | `requires_action` and a request whose choices are the dialog's options |
| A screen no state matches for two seconds | `unrecognised` and an `input` request with the screen text |
| A dialog left unanswered as the screen moves on | The request is cancelled with `answeredBy: "harness"` |

A client can also read the live screen and send keys:

| Method | Params | Needs |
| --- | --- | --- |
| `session/screen` | `{ session }` | `prompt` |
| `session/keys` | `{ session, text?, press? }` | `prompt` |

## Configuring an agent without an egg

`config.json` can declare a TUI agent directly, under `tui`. The runtime adds it to `agent/list` beside the eggs:

```json
{
  "tui": {
    "notes-cli": {
      "command": ["notes", "--interactive"],
      "profile": "/home/me/.config/parrot/notes.kdl",
      "name": "Notes CLI",
      "size": [40, 120],
      "transcript": { "format": "jsonl", "dir": "~/.notes/transcripts" }
    }
  }
}
```

| Field | Is |
| --- | --- |
| `command` | The CLI and its arguments |
| `profile` | The KDL profile |
| `name` | The display name, `notes-cli` by default |
| `env` | Extra environment for the CLI |
| `transcript` | `format` of `claude`, `codex` or `jsonl`, and an optional `dir` |
| `mcp` | `mcp-config` or `config-override` |
| `unrecognisedAfterMs` | How long an unmatched screen is held, 2000 by default |
| `size` | Rows and columns |
| `noGuard` | Starts the CLI without `parrot guard` |

An agent from an egg is declared the same way only when `config.json` has an entry for its id; the entry wins over the egg's profile and command.

## Limits

- TUI control types into a screen it recognises. A screen that changes beyond its rules makes the session `unrecognised` rather than a wrong action.
- A CLI that redraws constantly has slower states. Snapshots settle after 50 ms of quiet, or every 300 ms at most.
- Items come from a transcript when the CLI writes one. Without a transcript, a turn reports states and requests but little content.

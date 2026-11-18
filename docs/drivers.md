# Writing a driver

A driver connects Parrot to one agent CLI. It is a program that speaks the driver protocol on
standard input and output, packed in a Hatchery egg with the `parrot` target. Parrot builds in
only two general cores, for ACP agents and for agents driven through their terminal. Every other
driver is a program in an egg, built and released on its own.

`examples/driver-echo/` is a complete driver in one file. It builds outside the Parrot workspace,
the way a third-party driver would.

## The protocol

Messages are JSON-RPC 2.0, one per line. Spec section 30 has every message type. A driver must answer
these:

| Method | Answer |
| --- | --- |
| `initialize` | `{ protocol: 1, peer: { kind: "driver", name, version }, features }` |
| `agent/list` | `{ agents: [Agent] }`, one agent with the id the runtime gave it |
| `binding/start` | `{ isolation, level, network, options, actions }` once the CLI is ready |
| `binding/prompt` | `{ disposition: "started" }`, then updates as the turn runs |
| `binding/cancel` | `{ interrupted }` |
| `binding/answer` | `{}` |
| `binding/stop` | `{}`, and the CLI process ends |

A method the driver doesn't know is answered with error `-32601`. Optional methods, such as
`binding/options`, `binding/fork` and `history/read`, need the features that announce them.

The driver sends `update` notifications with `{ binding, update }`. A turn usually sends these
updates in order:

1. `{ type: "state", state: "running" }`
2. `{ type: "turn", turn: { id, status: "running", input } }`
3. `{ type: "item", item: { id, kind: "assistant", turn, set: { text } } }`
4. `{ type: "turn", turn: { id, status: "completed", ended } }`
5. `{ type: "state", state: "idle" }`

Ids in updates are the driver's own keys. The runtime maps them to Parrot ids.

## The environment

The runtime starts the driver with these variables:

| Variable | Holds |
| --- | --- |
| `PARROT_DRIVER_DATA` | The installed egg's `parrot` folder, with `driver.kdl` and the other data files |
| `PARROT_DRIVER_AGENT` | The egg's package id, to report as the agent id in `agent/list` |
| `PARROT_DRIVER_AGENT_NAME` | The egg's `about { name }`, to report as the display name |

A driver reports the id and name it was given, so one program can serve eggs of different names.
The runtime runs one driver process for each binding, plus short ones for `agent/list`. A driver
starts its CLI so that the CLI stops when the driver stops: a Rust driver can call
`parrot_driver::bin::main`, which also answers `<exe> guard -- <cmd>`.

On Windows a bare command name may resolve to the `.ps1` shim a package manager installed. Parrot
runs such a script through `pwsh` or Windows PowerShell, because the OS cannot start it directly.

## CLI versions

The egg's `cli "name" versions="…"` says which CLI versions its data covers. When a driver reports
a version, the runtime compares it with that range and marks the agent unverified when it falls
outside.

A driver reads the version where the CLI reports one:

| Agent | Where the version comes from |
| --- | --- |
| ACP | `agentInfo.version` or `info.version` in the `initialize` answer |
| T3 | `serverInfo.version` in the MCP `initialize` answer |
| pi | the first version `pi --version` prints |
| opencode | the first version `opencode --version` prints |

The ranges in `eggs/` state the release line the data was checked against, such as
`>=1.5.0, <1.6.0` for pi. A newer CLI runs with the newest data and unverified.

## Prompts and attachments

The prompt holds text, context entries and attachments. The driver sends the parts its CLI takes,
and for each part it drops it sends a `notice` update with the code `attachment_unsupported` and
text that says which attachment and why.

## The egg

A package folder contains the manifest and a `parrot` folder:

```text
echo/
  package.kdl
  parrot/
    driver.kdl
    variants/linux-x86_64/parrot-driver-echo
    variants/windows-x86_64/parrot-driver-echo.exe
```

`package.kdl` lists one Hatchery variant per platform the driver is built for:

```kdl
targets {
  parrot {
    requires {
      parrot "^0.1"
    }
    variants {
      variant os=linux arch=x86_64
      variant os=windows arch=x86_64
    }
  }
}
```

`driver.kdl` names the program. In `run`, `{os}`, `{arch}` and `{exe}` are filled in for the
machine it runs on:

```kdl
driver "command" {
  run "variants/{os}-{arch}/parrot-driver-echo{exe}"
}
data-version "1"
```

Put each platform's binary in its variant folder, then pack and publish:

```bash
parrot eggs check ./echo
parrot eggs pack ./echo --publish ./registry
parrot eggs install example/echo --registry ./registry
```

`pack` packs the variants whose folders exist, so a machine with one platform's binary packs that
variant and the base egg. A release copies every platform's binary in first. The registry folder
is a static Hatchery registry: serve it over HTTPS and install with `--registry https://…`.

## An egg without a program

An agent that speaks ACP, or one that Parrot can drive through its terminal, does not need a program of
its own. Its egg points at a built-in core and contains only data:

```kdl
// ACP: the command that starts the agent
driver "builtin" name="acp"
command "gemini" "--experimental-acp"
data-version "1"
```

```kdl
// TUI: profile.kdl beside this file describes the screens
driver "builtin" name="tui"
command "example-cli"
controls "tui"
data-version "1"
transcript "jsonl" dir="~/.example-cli/transcripts"
mcp "mcp-config"
```

Either shows up under the egg's own id, such as `gemini`.

The TUI core reads items from the CLI's own transcript when `transcript` names one:

| Node | The TUI core |
| --- | --- |
| `transcript "claude"` | Reads Claude Code's project transcript for the working directory |
| `transcript "codex"` | Reads the Codex rollout under `sessions/` |
| `transcript "jsonl" dir=…` | Reads the newest `.jsonl` in the folder, in Claude Code's message format |
| `mcp "mcp-config"` | Passes the `parrot` MCP server as `--mcp-config` JSON |
| `mcp "config-override"` | Passes it as `-c mcp_servers.parrot.*` overrides |

## Rookery target

An egg whose CLI or server runs on a Rookery host can also declare a `rookery` target in
`package.kdl` alongside `parrot`:

```kdl
targets {
  parrot {
    requires {
      parrot "^0.1"
    }
  }
  rookery {
    requires {
      rookery ">=0.6"
    }
  }
}
```

Its `rookery/` folder holds `rookery.json` and Ansible tasks under `tasks/` that install the CLI or
server on the host and define its status, sign-in and update actions. Each feature reads its own
`rookery_<egg>_<feature>_version` variable (such as `rookery_claude_cli_version` or
`rookery_t3_server_version`, plus `rookery_t3_server_sha256` for non-default T3 releases) so it
never collides with Rookery's built-in role defaults. Keep its `| default('…')` inside
`parrot/driver.kdl`'s `cli` `versions` range so a freshly provisioned host matches the driver data.
Run `parrot eggs check <dir>` and `rookery plugin check <dir>` to verify both targets locally.

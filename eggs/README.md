# Driver eggs

Each folder here is a Hatchery package for one agent. The runtime installs the `parrot` target of each package. The `parrot` binary contains no eggs. An agent exists only when its egg is installed. A release publishes these eggs, with their driver binaries, to the registry at `https://aiviarium.github.io/parrot/eggs`.

`docs/drivers.md` explains how to write a driver and pack it as an egg.

## Layout

| Path | Holds |
| --- | --- |
| `<agent>/package.kdl` | The Hatchery manifest: `group aiviarium`, the agent's id, its version, and `targets { parrot { requires { parrot "^0.1" } } }` |
| `<agent>/parrot/driver.kdl` | How the driver runs, which CLI versions it covers, and its data version |
| `<agent>/parrot/options.kdl` | Option maps: roles, choices with semantic tags, `dependsOn` rules |
| `<agent>/parrot/levels.kdl` | Permission levels and the tools each level allows (E4) |
| `<agent>/parrot/limits.kdl` | Numbers the CLI imposes: `tool-timeout` is how many seconds it waits for one tool call. Calls into Parrot and approvals return before it |
| `<agent>/parrot/correlate.kdl` | How items from several sources match, and which source wins |
| `<agent>/parrot/profile.kdl` | The TUI profile, for agents with TUI control. This is the live copy |
| `<agent>/parrot/shims/<dir>/` | Files that replace the ones above for a range of CLI versions |
| `<agent>/parrot/variants/<os>-<arch>/` | The driver binary for one platform. Built, never committed |
| `<agent>/parrot/probes/*.kdl` | Probe scenarios. They run against a real CLI to record transcripts, and never in CI |

## driver.kdl

A driver is a built-in core of the runtime or a program in the egg. The Claude, Codex, pi, opencode and T3 eggs contain a program per platform. The ACP and TUI cores serve data-only eggs.

```kdl
driver "command" {
  run "variants/{os}-{arch}/parrot-driver-claude{exe}"
}
cli "claude" versions=">=2.1.0, <2.2.0"
data-version "1"
shim ">=2.1.325" dir="shims/2.1.325"
```

```kdl
driver "builtin" name="acp"
command "gemini" "--experimental-acp"
data-version "1"
```

The runtime marks a session `unverified` when the CLI reports a version outside `versions`.

| Node | Meaning |
| --- | --- |
| `driver "builtin" name=…` | A core built into the runtime: `acp` runs `parrot driver acp`, and `tui` runs the TUI core in the runtime |
| `driver "command" { … }` | The runtime runs a program from the egg's `parrot` folder |
| `run "path" args…` | The program when no variant matches, with its arguments. `{os}`, `{arch}` and `{exe}` are filled in |
| `variant os=… arch=… run=…` | The program for one platform. The first match wins. Either key may be left out |
| `command "prog" "arg"…` | The CLI command, for the ACP and TUI cores |
| `transcript "claude"` or `transcript "<dir>"` | Where the TUI core reads the transcript |
| `cli "name" versions=…` | The agent CLI and the semver range this data covers |
| `data-version "1"` | The data version, reported as `driver.data` on the agent |
| `controls "native" "tui"` | How the agent can be driven. The first is the default. `native` when left out |
| `bindings 1` | How many bindings one driver process serves. Any number when left out |
| `shim "range" dir=…` | A version shim, described below |

A program is stored in a Hatchery variant folder, such as `variants/linux-x86_64/`. Each variant is listed in `package.kdl`. Only the variant for this machine is installed, and the runtime ignores nodes it does not know.

## Version shims

Driver data can differ between CLI versions. A `shim` line names a semver range and a folder. When the driver detects a CLI version in the range, each file in the folder replaces the egg's file of the same name. List the lines from the oldest range to the newest: the last range that matches the version applies. A file the shim folder lacks comes from the egg. The Claude egg uses one for the `manual` permission mode of Claude Code 2.1.325 and later.

## Carrier eggs

A carrier egg gives links a channel of a platform's own, for machines that SSH, a command or TCP can't get to. Its `parrot` folder holds `carrier.kdl` and the program it runs, and no `driver.kdl`. The runtime loads installed carrier eggs at start, and the `carrier.kind` of a `link/add` selects one.

```kdl
carrier "relay" title="Relay" max-message=16384 containment="restricted"
run "bin/carrier"
target "{\"type\":\"object\"}"
```

| Node | Meaning |
| --- | --- |
| `carrier "kind"` | The kind that a link's carrier selects. `title`, `max-message`, `containment` and `listens` are optional |
| `max-message=…` | The largest message on the channel, at least 4096 bytes |
| `containment=…` | `restricted` or `shell` |
| `run "path" args…` | The program, relative to the `parrot` folder |
| `target "json"` | The JSON Schema of the carrier's target |

The runtime starts `<program> dial` for each connection and writes the target as JSON on its first input line. The program answers `{"ready":true}` on its first output line, and from then on its standard input and output are the channel. A carrier egg that leaves out `containment` counts as `shell`.

## Probe scenarios

A probe is a list of steps. `parrot probe <agent> <scenario>` runs `probes/<scenario>.kdl` through the agent's driver against the real CLI. It writes `transcript.jsonl`, `frames.jsonl` and `probe.json` into `--out`.

```kdl
description "One short turn."
control "native"
timeout-secs 180
option "model" "default"
prompt "Reply with the single word: hello."
wait "idle" timeout-secs=120
```

| Step | Does |
| --- | --- |
| `prompt "text"` | Sends a prompt |
| `wait "state"` | Waits for a state, such as `idle`, or for `request` |
| `answer "choice"` | Answers the newest request |
| `cancel` | Cancels the turn |
| `keys "Enter" text="…"` | Types and presses keys, under TUI control |
| `sleep-ms 500` | Waits |

## Installing

The runtime installs eggs into `<state_dir>/eggs`. The runtime refuses an egg whose `requires` does not hold, and says why. With no egg installed, `agent/list` is empty and its `hint` says how to install one.

| Command | Does |
| --- | --- |
| `parrot eggs install aiviarium/claude` | Installs from the release registry, or from `PARROT_EGGS_REGISTRY` when it is set |
| `parrot eggs install aiviarium/claude --registry <url or dir>` | Installs from a static registry over HTTPS, or from a folder |
| `parrot eggs install ./eggs/acp` | Installs one package folder |
| `parrot eggs list` | Lists each installed agent's egg |
| `parrot eggs check [dir…]` | Checks package folders, or what the runtime would load |
| `parrot eggs pack <dir…> --out DIR` | Writes `.egg` files. `--publish DIR` publishes them into a static registry instead |
| `parrot eggs remove aiviarium/claude` | Removes an installed package |

`pack` leaves out a variant whose folder does not exist. `scripts/pack-eggs.sh` builds this machine's driver binaries, copies them into their variant folders and packs every egg into `target/eggs-registry`.

The schemas of `driver.kdl` and the probes are in `spec/schema/driver-data/`, with examples in `spec/examples/driver-data/`.

## How a driver finds its data

The runtime starts every driver with these variables:

| Variable | Holds |
| --- | --- |
| `PARROT_DRIVER_DATA` | The installed egg's `parrot` folder |
| `PARROT_DRIVER_AGENT` | The egg's package id, which the driver reports as the agent id |
| `PARROT_DRIVER_AGENT_NAME` | The egg's `about { name }`, which the driver reports as the name |

A driver reads its files from `PARROT_DRIVER_DATA`. A driver binary may contain a copy of its own egg's data for when the variable is unset, since both are released together. The variable takes precedence.

## The data files

These shapes are the minimum. Owners may add fields, but not rename or remove these.

```kdl
// options.kdl
option "model" role="model" label="Model" applies="now" default="default" {
  choice "default" label="Default"
}
option "mode" role="mode" label="Mode" applies="next-turn" {
  choice "plan" label="Plan" tags="plan"
  choice "auto" label="Auto" tags="bypass"
}
option "effort" role="effort" label="Effort" {
  depends-on "model" {
    when "fast-model" { choice "low"; choice "high"; }
  }
}
```

```kdl
// levels.kdl: the tools each permission level allows
level "read" { tool "Read"; tool "Grep"; }
level "write" { tool "Edit"; tool "Write"; }
level "host" { tool "Bash"; }
```

```kdl
// correlate.kdl: how items from two sources match, and which wins
kind "tool" {
  match "ref"
  prefer "native"
}
```

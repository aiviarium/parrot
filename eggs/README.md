# Driver eggs

Each folder here is a Hatchery package for one agent. The runtime installs the `parrot` target of each package. This folder is also the registry the built-in drivers ship from.

## Layout

| Path | Holds |
| --- | --- |
| `<agent>/package.kdl` | The Hatchery manifest: `group aiviarium`, the agent's id, its version, and `targets { parrot { requires { parrot "^0.1" } } }` |
| `<agent>/parrot/driver.kdl` | How the driver runs, which CLI versions it covers, and its data version |
| `<agent>/parrot/options.kdl` | Option maps: roles, choices with semantic tags, `dependsOn` rules |
| `<agent>/parrot/levels.kdl` | Permission levels and the tools each level allows (E4) |
| `<agent>/parrot/correlate.kdl` | How items from several sources match, and which source wins |
| `<agent>/parrot/profile.kdl` | The TUI profile, for agents with TUI control |
| `<agent>/parrot/probes/*.kdl` | Probe scenarios. They run against a real CLI to record transcripts, and never in CI |

## driver.kdl

A built-in driver names itself. A driver that runs as a command names the program, with a variant per platform when it is a binary.

```kdl
driver "builtin" name="claude"
cli "claude" versions=">=2.0"
data-version "1"
```

```kdl
driver "command" {
  run "bin/driver"
  variant os="linux" arch="x86_64" run="bin/linux-x86_64/driver"
}
cli "example" versions=">=1.0"
data-version "1"
```

The runtime marks a session `unverified` when the CLI reports a version outside `versions`.

| Node | Meaning |
| --- | --- |
| `driver "builtin" name=…` | The runtime runs `parrot driver <name>` |
| `driver "command" { … }` | The runtime runs a program from the egg's `parrot` folder |
| `run "path" args…` | The program when no variant matches, with its arguments |
| `variant os=… arch=… run=…` | The program for one platform. The first match wins. Either key may be left out |
| `cli "name" versions=…` | The harness CLI and the semver range this data covers |
| `data-version "1"` | The data version, reported as `driver.data` on the agent |
| `controls "native" "tui"` | How the agent can be driven. The first is the default. `native` when left out |
| `bindings 1` | How many bindings one driver process serves. Any number when left out |

A program can live in a Hatchery variant folder, such as `variants/linux/driver`. Only the variant for this machine is installed. The runtime ignores nodes it does not know.

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

The runtime installs eggs into `<state_dir>/eggs`. An agent with no installed egg uses the copy of this folder compiled into the runtime. The runtime refuses an egg whose `requires` does not hold, and says why.

| Command | Does |
| --- | --- |
| `parrot eggs install aiviarium/claude --registry eggs` | Installs from a registry folder. This folder works as one |
| `parrot eggs install ./eggs/claude` | Installs one package folder |
| `parrot eggs list` | Lists each agent's egg and where it came from |
| `parrot eggs check [dir…]` | Checks package folders, or what the runtime would load |
| `parrot eggs pack <dir…> --out DIR` | Writes `.egg` files. `--publish DIR` publishes them into a static registry instead |
| `parrot eggs remove aiviarium/claude` | Removes an installed package |

The schemas of `driver.kdl` and the probes are in `spec/schema/driver-data/`, with examples in `spec/examples/driver-data/`.

## How a driver finds its data

The runtime starts every driver with `PARROT_DRIVER_DATA` set to the installed egg's `parrot` folder. A built-in driver reads its files from there. Without the variable, it uses the copy of this folder compiled into it, so a fresh install works offline.

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

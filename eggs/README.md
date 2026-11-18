# Driver eggs

Each folder here is a Hatchery package for one agent. The runtime installs the `parrot` target of each package. The `parrot` binary contains no eggs. An agent exists only when its egg is installed. A release publishes these eggs, with their driver binaries, to the registry at `https://aiviarium.github.io/parrot/eggs`.

`docs/drivers.md` explains how to write a driver and pack it as an egg.

## Layout

| Path | Holds |
| --- | --- |
| `<agent>/package.kdl` | The Hatchery manifest: `group aiviarium`, the agent's id, its version, and `targets { parrot { requires { parrot "^0.1" } } }` |
| `<agent>/parrot/driver.kdl` | How the driver runs, which CLI versions it covers, and its data version |
| `<agent>/parrot/options.kdl` | Option maps with roles, choices with semantic tags, and `dependsOn` rules |
| `<agent>/parrot/levels.kdl` | Permission levels and the tools each level allows (E4) |
| `<agent>/parrot/limits.kdl` | Numbers the CLI imposes: `tool-timeout` is how many seconds it waits for one tool call. Calls into Parrot and approvals return before it |
| `<agent>/parrot/correlate.kdl` | How items from several sources match, and which source is preferred |
| `<agent>/parrot/profile.kdl` | The TUI profile, for agents with TUI control. This is the live copy |
| `<agent>/parrot/shims/<dir>/` | Files that replace the ones above for a range of CLI versions |
| `<agent>/parrot/variants/<os>-<arch>/` | The driver binary for one platform. Built, never committed |
| `<agent>/parrot/probes/*.kdl` | Probe scenarios. They run against a real CLI to record transcripts, and never in CI |

## The driver file

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
cli "gemini" versions=">=0.20.0, <0.21.0"
data-version "1"
```

The runtime marks a session `unverified` when the CLI reports a version outside `versions`. Each
egg states the release line its data was checked against, such as the pi egg's
`>=1.5.0, <1.6.0`. The runtime compares only a version the CLI reports.

| Node | Meaning |
| --- | --- |
| `driver "builtin" name=…` | A core built into the runtime: `acp` runs `parrot driver acp`, and `tui` runs the TUI core in the runtime |
| `driver "command" { … }` | The runtime runs a program from the egg's `parrot` folder |
| `run "path" args…` | The program when no variant matches, with its arguments. `{os}`, `{arch}` and `{exe}` are filled in |
| `variant os=… arch=… run=…` | The program for one platform. The first match is used. Either key may be left out |
| `command "prog" "arg"…` | The CLI command, for the ACP and TUI cores |
| `transcript "claude"`, `"codex"` or `"jsonl" dir=…` | The transcript the TUI core reads, by format. `dir=` replaces the format's own folder |
| `mcp "mcp-config"` or `mcp "config-override"` | The form in which the TUI core passes the `parrot` MCP server to the CLI, as `--mcp-config` JSON or `-c` overrides |
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
| `listens=#true` | The runtime runs `<program> listen` beside itself for inbound channels |
| `max-message=…` | The largest message on the channel, at least 4096 bytes |
| `containment=…` | `restricted` or `shell` |
| `run "path" args…` | The program, relative to the `parrot` folder |
| `target "json"` | The JSON Schema of the carrier's target |

The runtime starts `<program> dial` for each connection and writes the target as JSON on its first input line. The program answers `{"ready":true}` on its first output line, and from then on its standard input and output are the channel. A carrier egg that leaves out `containment` counts as `shell`.

On the channel, each Noise message has its length as two bytes in front. A carrier whose platform sends messages rather than a byte stream sends each Noise message as one message of its own. Neither side's Noise messages exceed `max-message`: the dialling runtime tells the other runtime the limit in its handshake.

A carrier egg with `listens=#true` takes inbound channels as well. The runtime starts `<program> listen` beside itself, under `parrot guard`, and starts it again when it ends. For each inbound channel the program does one of these:

| Way | What the program does |
| --- | --- |
| Hand over a descriptor | Runs `$PARROT_BIN connect --link --socket $PARROT_SOCKET --fd <n>` with the channel's descriptor `n` inherited |
| Relay | Connects to `$PARROT_LINK_SOCKET` and copies bytes both ways |

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
| `parrot eggs pack <dir…> --out DIR` | Writes `.egg` files. `--publish DIR` publishes them into a static registry instead, and `--sign-key-env VAR` signs them |
| `parrot eggs remove aiviarium/claude` | Removes an installed package |

`pack` leaves out a variant whose folder does not exist. `parrot eggs pack eggs --build --publish target/eggs-registry` builds this machine's driver binaries, copies them into their variant folders and packs every egg into `target/eggs-registry`. On another platform, add `--target <triple>`. A release builds once per platform and packs every variant together.

## A signed registry

A static registry can be signed, so an egg cannot change without the key. The default registry is not signed: it is served from this repository's GitHub Pages. If the repository has a `PARROT_EGG_SIGNING_KEY` secret, the release workflow signs the registry with it and prints the public key, which goes in the release notes.

```bash
hatchery keygen > keys.txt           # line 1 is the signing key, line 2 the public key
PARROT_EGG_SIGNING_KEY="$(sed -n 1p keys.txt)" \
  parrot eggs pack eggs --publish site/eggs --sign-key-env PARROT_EGG_SIGNING_KEY
```

The key is only ever in the environment. The command takes the variable's name, never the key. To check eggs against it, give the public key to the registry line in `<state_dir>/hatchery.kdl`:

```kdl
registries {
  http "https://aiviarium.github.io/parrot/eggs" key="ed25519:…"
}
```

A registry with a key refuses an egg whose signature is missing or wrong. Without a key, eggs install on the checksum in the index.

The schemas of `driver.kdl` and the probes are in `spec/schema/driver-data/`, with examples in `spec/examples/driver-data/`.

## A driver's data folder

The runtime starts every driver with these variables:

| Variable | Holds |
| --- | --- |
| `PARROT_DRIVER_DATA` | The installed egg's `parrot` folder |
| `PARROT_DRIVER_AGENT` | The egg's package id, which the driver reports as the agent id |
| `PARROT_DRIVER_AGENT_NAME` | The egg's `about { name }`, which the driver reports as the name |

A driver reads its files from `PARROT_DRIVER_DATA`. A driver binary may contain a copy of its own egg's data for when the variable is unset, since both are released together. The variable takes precedence.

## The data files

These fields are the minimum. Owners may add fields, but not rename or remove these.

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

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

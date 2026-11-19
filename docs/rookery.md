# Behind Rookery

Rookery provisions and manages machines for coding agents. Parrot drives the agents. The two meet in two places: the `rookery` target of a Parrot egg, and a Parrot runtime running on a host that Rookery set up.

## One package, two targets

A driver egg has a `parrot` target, which installs it into a Parrot runtime. Add a `rookery` target and the same package also installs the agent CLI on a Rookery host:

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

The `rookery/` folder holds `rookery.json` and Ansible tasks:

| Path | Holds |
| --- | --- |
| `rookery/rookery.json` | The features the plugin offers, their labels, commands and actions |
| `rookery/tasks/*.yml` | The task lists that install the CLI or server and define its actions |

The `claude`, `codex`, `pi`, `opencode` and `t3` eggs each define one feature. Installing the CLI is only part of it. The feature also adds a sign-in action, a status action and an update action to Rookery, so a host's overview can say whether the agent is signed in and can start the sign-in flow.

Each task list reads its own version variable with a default, such as `{{ rookery_claude_cli_version | default('2.1.327') }}`. The variable is scoped to the plugin, so it can't collide with Rookery's own role defaults. The default stays inside the egg's `cli "<name>" versions="…"` range in `parrot/driver.kdl`, so a freshly provisioned host gets a release that the driver data covers.

| Egg | Version variable |
| --- | --- |
| `claude` | `rookery_claude_cli_version` |
| `codex` | `rookery_codex_cli_version` |
| `pi` | `rookery_pi_cli_version` |
| `opencode` | `rookery_opencode_cli_version` |
| `t3` | `rookery_t3_server_version`, and `rookery_t3_server_sha256` for a custom build |

Check both targets before opening a pull request:

```bash
parrot eggs check ./eggs/claude
rookery plugin check ./eggs/claude
```

The first checks the `parrot` target and its data files. The second applies Rookery's plugin checks. The repository's Rookery workflow also runs the tasks in Ubuntu 26.04 and checks that every command a feature declares exists and answers `--version`.

## Parrot on a Rookery host

A Rookery host has a desktop and the agent CLIs that its plugins installed. A `parrot` runtime there finds the CLIs on `PATH` and drives them like any other:

```bash
parrot serve
parrot eggs install aiviarium/claude
parrot new claude --dir /home/agent/src/notes
```

Use a state directory on the host, or pass `--state-dir`. The runtime's own directory is private to the user who runs it ([Configuration](reference/configuration.md)).

When `box` is on `PATH`, the runtime adds the desktop functions of the host:

| Function | Does |
| --- | --- |
| `parrot.desktop.screenshot` | Takes a screenshot and returns its file by URI |
| `parrot.desktop.pointer` | Moves the pointer, clicks, double-clicks or scrolls |
| `parrot.desktop.keys` | Types text or presses keys held together |

`box` comes from Rookery. On another machine the functions are absent and the `profile.desktop` feature is not advertised. Set `PARROT_BOX` to name a `box` program at another path.

Rookery doesn't start Parrot, install eggs or sign the CLIs in to Parrot. Provisioning and the agent CLI are Rookery's work; the runtime and its sessions are Parrot's.

## Where the pieces sit

| Piece | Installed by | Runs on |
| --- | --- | --- |
| Agent CLI, such as `claude` | Rookery's `rookery` target, or the user | The Rookery host |
| Driver program, such as `parrot-driver-claude` | The egg's `parrot` target | The machine that runs `parrot serve` |
| Parrot runtime | The user, from a release or `cargo install` | Any machine that reaches the CLIs |
| Workspace | Parrot | A folder on the runtime's machine |

The driver and the CLI can be on different machines only when a link connects the two runtimes. Today a binding runs on the runtime's own machine, next to the CLI it drives.

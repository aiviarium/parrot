# Parrot documentation

Parrot runs agent CLIs and gives them one model to talk through. It starts a CLI in a terminal and reads its screen, or it speaks the CLI's own wire protocol. Both ways produce the same items, states and requests, so a client can drive Claude Code, Codex, opencode, pi, an ACP agent or a TUI-only program without knowing which one it is.

Parrot is the agent part of the Aiviarium stack: drivers, terminals and protocol translators, and nothing above them. It has no user interface. A runtime serves the [Parrot protocol](reference/protocol.md) on a local socket and, when asked, on WebSocket. Any client can then list agents, create sessions, send prompts and answer requests.

It is for people building a client or an orchestrator over agent CLIs, and for people adding a driver for another CLI. A client can be a desktop app, a command line, a phone or another service.

| Page | What it covers |
| --- | --- |
| [Install](install.md) | Building the command and installing the agent eggs |
| [Getting started](quick-start.md) | A first runtime, agent and session |
| [Concepts](concepts.md) | Runtime, sessions, bindings, drivers, eggs, workspaces and links |
| [Items and state](items.md) | Text, thinking, tool calls, requests, plans and the state machine |
| [Workspaces](workspaces.md) | Folders, git worktrees, merges and checkpoints |
| [Links](links.md) | Pairing two runtimes and the carriers between them |
| [Behind Rookery](rookery.md) | Agents on a provisioned host, and the eggs' `rookery` target |
| [Drivers](drivers/claude.md) | One page for each agent: Claude Code, Codex, opencode, pi, ACP, T3 and the TUI core |
| [Writing a driver](writing-drivers.md) | The driver protocol and the egg that carries it |
| [Command line](reference/cli.md) | Every command and flag |
| [Protocol](reference/protocol.md) | Transports, scopes, methods, updates and errors |
| [TypeScript client](reference/client.md) | `@aiviarium/parrot-client` |
| [Protocol translators](reference/agents.md) | `@aiviarium/parrot-agents` |
| [Configuration](reference/configuration.md) | `config.json`, the state directory and environment variables |
| [Rust crates](reference/crates.md) | What each crate does |

## Where to start

- To try Parrot, [install](install.md) it and run the [quick start](quick-start.md).
- To add an agent, read its page under [Drivers](drivers/claude.md). Each page says which CLI version the driver data covers.
- To write a client, read the [protocol](reference/protocol.md) and the [TypeScript client](reference/client.md).
- To write a driver, read the [driver protocol](writing-drivers.md).

## How it fits together

| Project | Is |
| --- | --- |
| Parrot | The agent layer: drivers for agent CLIs, terminal control, protocol translation, sessions and model API clients |
| [Rookery](https://rookery.helight.dev/) | Provisioning and management for machines. Its plugins install agent CLIs, and each Parrot egg ships a `rookery` target that does the same ([Behind Rookery](rookery.md)) |
| [Crow](https://github.com/aiviarium/crow) | The product built on Rookery: its shell, default plugins and desktop app. It doesn't depend on Parrot |
| [Hatchery](https://hatchery.helight.dev/) | Packages and registries. A Parrot driver is a Hatchery egg with the `parrot` target |
| [Nocturne](https://nocturne.helight.dev/) | Design tokens and components for Crow and plugins |

Parrot is early. Its protocol is version 1, and the runtime advertises which [implementation profiles](reference/protocol.md#implementation-profiles) it answers. Methods outside them fail with an `unsupported` error rather than a different shape.

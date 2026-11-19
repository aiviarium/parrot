<h1><a href="https://github.com/aiviarium/parrot"><picture><source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/aiviarium/.github/main/brand/lockups/lockup-parrot.svg"><img src="https://raw.githubusercontent.com/aiviarium/.github/main/brand/lockups/lockup-parrot-light.svg" alt="Parrot" height="56"></picture></a><br><a href="https://github.com/aiviarium/parrot/actions/workflows/ci.yml"><img src="https://aiviarium-badges.helight.workers.dev/parrot/github/actions/workflow/status/aiviarium/parrot/ci.yml?branch=main&label=CI" alt="CI"></a> <a href="#license"><img src="https://aiviarium-badges.helight.workers.dev/parrot/github/license/aiviarium/parrot" alt="License"></a></h1>

Agent CLIs in one runtime: drivers, terminals and protocol translators.

Parrot runs a coding agent's CLI and gives it one model to talk through. It speaks the CLI's own protocol where there is one, and otherwise drives its terminal and reads the screen. Both ways produce the same items, states and requests, so a client can drive Claude Code, Codex, opencode, pi, an ACP agent or a TUI-only program without knowing which one it is.

Parrot is the agent part of the Aiviarium stack. It has no user interface. A runtime serves the Parrot protocol on a local socket, and on WebSocket when asked. It doesn't provision machines, install agent CLIs on a host or sign them in; [Rookery](https://rookery.helight.dev/) does that, and an egg can also carry a Rookery target for it.

Parrot is early. The protocol is version 1, and its APIs can change before a first release.

## Install

Parrot needs Rust 1.95 or later:

```bash
cargo install --locked --git https://github.com/aiviarium/parrot parrot-cli
```

The runtime has no agents until an egg is installed. A tagged release publishes the eggs at `https://aiviarium.github.io/parrot/eggs`:

```bash
parrot eggs install aiviarium/claude
```

Until a release is published, pack the repository's eggs with this machine's driver binaries and install from the folder:

```bash
parrot eggs pack eggs --build --publish target/eggs-registry
parrot eggs install aiviarium/claude --registry target/eggs-registry
```

## Quick start

```bash
parrot serve   # leave this running; the rest runs in another terminal
parrot new claude --dir ~/src/notes --title "Notes"
parrot prompt ses_01J8Q4T7N3K2M9X8C6V5B4A3S2 "Summarise the README."
parrot watch ses_01J8Q4T7N3K2M9X8C6V5B4A3S2 --until idle
```

`parrot new` starts the session, and `parrot watch` prints the items and state as they change. Requests appear with `parrot requests`, and `parrot answer` sends a choice. The [getting started guide](https://parrot.helight.dev/docs/quick-start) continues from there.

## Documentation

The documentation is at [parrot.helight.dev](https://parrot.helight.dev/). The pages are the Markdown files in [docs](docs/README.md), which also read here.

- [Concepts](docs/concepts.md) covers the runtime, sessions, bindings, drivers and eggs.
- [Items and state](docs/items.md) covers text, thinking, tool calls, approvals, questions, plans and the state machine.
- [Drivers](docs/drivers/claude.md) has a page for each agent, and [writing a driver](docs/writing-drivers.md) the protocol and egg format.
- [Command line](docs/reference/cli.md) lists every command and flag.
- [Protocol](docs/reference/protocol.md) has the transports, scopes, methods, updates and errors.
- [Configuration](docs/reference/configuration.md) has `config.json`, the state directory and the environment variables.

## What is in it

| Part | Holds |
| --- | --- |
| `crates/parrot-core`, `parrot-serve`, `parrot-store`, `parrot-proto` | The runtime, its transports, storage and wire types |
| `crates/parrot-driver-*` | Drivers for Claude Code, Codex, opencode, pi, ACP and T3, and the TUI core |
| `crates/parrot-term`, `parrot-tui` | Terminal emulation and the labelled tree that profiles act on |
| `crates/parrot-eggs`, `parrot-workspace`, `parrot-mcp`, `parrot-link`, `parrot-models` | Eggs, workspaces, the injected MCP server, links and model API clients |
| `crates/parrot-cli` | The `parrot` command |
| `packages/parrot-client` | `@aiviarium/parrot-client`, generated protocol types and a JSON-RPC client |
| `packages/parrot-agents` | `@aiviarium/parrot-agents`, the CLI protocol translators and fake CLIs |

Each crate and package can be used alone. [Rust crates](docs/reference/crates.md) says what each one does.

## Build and test

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo nextest run --locked --workspace
cargo test --locked --workspace --doc

bun install
bun run check
bun run typecheck
bun run test
```

The Rust crates need Rust 1.95 or later. The TypeScript packages need Bun 1.3 or later. The documentation site builds on Node 22 or later:

```bash
cd site
npm ci
npm run build
```

## Contributing

Read [AGENTS.md](AGENTS.md) for the layout, the checks and the conventions. Open an issue before a pull request.

## License

Licensed under either of the [Apache License, Version 2.0](LICENSE-APACHE) or the [MIT license](LICENSE-MIT), at your option. Contributions are licensed the same way.

The recorded casts show Claude Code, Codex and other programs. Those programs are separate products under their vendors' terms, and Parrot doesn't include them.

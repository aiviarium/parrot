# Rust crates

The workspace has 22 crates. Each can be used alone in another program, and the runtime uses them together.

| Crate | Does |
| --- | --- |
| `parrot-cli` | The `parrot` command: serve, connect, driver, guard, conform, mcp, link, eggs, probe, acp and the client commands |
| `parrot-core` | The runtime as a state machine without I/O: calls and driver updates go in, effects come out |
| `parrot-serve` | The transports (local socket, named pipe, WebSocket, stdio), dispatch and the backend that carries out effects |
| `parrot-proto` | The wire types of the protocol, and the JSON Schema generated from them |
| `parrot-store` | SQLite storage: the update log, projections, intent log and numbered migrations |
| `parrot-driver` | The driver protocol: a host for process drivers and the trait for built-in ones |
| `parrot-driver-claude` | The Claude Code driver over stream-json, with a fake Claude Code for tests |
| `parrot-driver-codex` | The Codex driver over the app server, with a fake Codex for tests |
| `parrot-driver-opencode` | The opencode driver over its HTTP server, with a fake opencode for tests |
| `parrot-driver-pi` | The pi driver over pi RPC, with a fake pi for tests |
| `parrot-driver-t3` | The T3 Code driver, through its MCP tools, with a fake server |
| `parrot-driver-acp` | The ACP driver, and the pieces `parrot acp` uses to export a session as an ACP agent |
| `parrot-driver-tui` | TUI control: a CLI on a PTY, its labelled tree, states, dialogs and actions |
| `parrot-eggs` | The `parrot` Hatchery target, installed eggs, their data files and packing |
| `parrot-guard` | Starts a command so that it dies with its parent: process groups, job objects and shims |
| `parrot-link` | Links: keys, Noise sessions, framing, pairing and the SSH, command and TCP carriers |
| `parrot-mcp` | The MCP server injected into agent CLIs, and the function catalog it lists |
| `parrot-models` | OpenAI-compatible model clients: chat, streaming, structured output, transcription and model lists |
| `parrot-term` | Terminal emulation, snapshots, input encoding, PTYs and asciinema casts |
| `parrot-tui` | Labelled terminal trees, profiles, locators, waits, actions and profile measurement |
| `parrot-workspace` | Folders, git worktrees, merges, transfers and checkpoints |
| `parrot-conform` | Golden fixtures of the protocol: parser, placeholder matcher and runner |

Two TypeScript packages sit beside them:

| Package | Does |
| --- | --- |
| `@aiviarium/parrot-client` | Types generated from the schema, and a JSON-RPC client over WebSocket or stdio |
| `@aiviarium/parrot-agents` | Translators from the CLIs' protocols, transcript readers and fake CLIs |

## The layers

A client talks to `parrot-serve`, which feeds `parrot-core`. The core decides what to do and returns effects. `parrot-store` keeps the log and projections. `parrot-driver` starts the driver process or calls a built-in core, and `parrot-eggs` says which driver an agent uses.

The terminal side is separate: `parrot-term` owns the screen, and `parrot-tui` turns a screen into a tree a profile can name. `parrot-driver-tui` joins them to the driver protocol.

`parrot-models` does not use the runtime. It needs a `Transport` you write over your HTTP client, so it has no network stack of its own and its tests replay fixtures.

## Build

The crates need Rust 1.95 or later.

```bash
cargo build --workspace
cargo nextest run --locked --workspace
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --workspace --doc
```

The workspace uses edition 2024. The `parrot` binary is `parrot-cli`'s, and the driver binaries are `parrot-driver-<agent>`. The ACP and TUI cores are built into `parrot` and served by `parrot driver acp` and `parrot driver tui`.

A driver outside this repository builds against the protocol alone. [`examples/driver-echo`](https://github.com/aiviarium/parrot/tree/main/examples/driver-echo) is one file and its own Cargo workspace.

## License

Every crate and package is licensed under either the Apache License, Version 2.0 or the MIT license, at your option. The recorded casts show other products, which keep their vendors' terms.

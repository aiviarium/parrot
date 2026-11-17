# Parrot

Parrot has the parts of driving coding agents that depend on the agent and the model, and on nothing
else. It runs agent CLIs in a terminal and reads their screens. It speaks the CLIs' wire protocols, and
it calls model APIs.

Parrot is early, and its APIs change without notice until a first release.

## What is in it

| Path | What it is |
| --- | --- |
| [`crates/parrot-term`](crates/parrot-term) | Terminal emulation. A screen model over `alacritty_terminal`, a local PTY, input encoding with the kitty keyboard protocol, and settled snapshots. It records and replays asciinema casts |
| [`crates/parrot-tui`](crates/parrot-tui) | A labelled tree over a terminal screen, in effect an accessibility tree. TOML profiles define a program's states and actions, and locators, waits and actions drive it. It also scores a profile against labelled casts, and has the `parrot-term-tool` command |
| [`crates/parrot-models`](crates/parrot-models) | OpenAI-compatible model clients for chat, structured output and audio transcription. They list models too. HTTP goes through a `Transport` trait you write for your HTTP client |
| [`packages/parrot-agents`](packages/parrot-agents) | `@aiviarium/parrot-agents`, in TypeScript. It translates the protocols of Claude Code, Codex, the Agent Client Protocol and Pi into one item and state model. It has the CLIs' transcript readers, TUI profiles and fake CLIs for tests |

Each crate and the package can be used alone, in any program. The TypeScript package has no runtime
dependencies.

## Quick start

Replay a recorded Claude Code session and print the screen at a marker:

```bash
cargo run -p parrot-tui --bin parrot-term-tool -- markers crates/parrot-tui/fixtures/casts/claude-permission.cast
cargo run -p parrot-tui --bin parrot-term-tool -- replay crates/parrot-tui/fixtures/casts/claude-permission.cast --at 10.363
```

Print the labelled tree and the profile's state at that moment:

```bash
cargo run -p parrot-tui --bin parrot-term-tool -- tree crates/parrot-tui/fixtures/casts/claude-permission.cast \
  --at 10.363 --profile packages/parrot-agents/tui/claude-code.toml
```

Run a program with its screen beside its tree, and the profile reloaded when you save it:

```bash
cargo run -p parrot-tui --bin parrot-term-tool -- inspect --profile packages/parrot-agents/tui/codex.toml -- codex
```

## Build and test

```bash
cargo test --workspace
cargo fmt --check
cargo clippy --all-targets -- -D warnings

bun install
bun run typecheck
bun run test
bun run check
```

The Rust crates need Rust 1.88 or later. The TypeScript package needs Bun 1.3 or later.

## License

Licensed under either of the [Apache License, Version 2.0](LICENSE-APACHE) or
the [MIT license](LICENSE-MIT), at your option. Contributions are licensed the same way.

The recorded casts show Claude Code, Codex and other programs. Those programs are separate products
under their vendors' terms, and Parrot doesn't include them.

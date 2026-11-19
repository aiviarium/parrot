# Working on Parrot

Parrot runs agent CLIs in a terminal and reads their screens. It also translates the CLIs' wire
protocols and calls model APIs. This file is for coding agents working in this repository.

## Layout

| Path | What it is |
| --- | --- |
| `crates/parrot-term/` | Terminal sessions. `emulator.rs` wraps `alacritty_terminal`, and `screen.rs` is the immutable screen it hands out. `input.rs` encodes keys, text, paste and mouse for the current modes. `pty.rs` runs a command on a local PTY. `session.rs` ties a byte source to the emulator, `settle.rs` decides when a snapshot is due, and `cast.rs` records and replays asciinema v2 casts |
| `crates/parrot-tui/` | The labelled tree and automation. `tree/` segments a screen into nodes and gives a node the same id in every frame. `profile/` reads TOML profiles and evaluates them. `locator.rs` finds text and boxes, `automate.rs` has waits and actions, and `measure.rs` scores a profile against labelled casts. `src/bin/parrot-term-tool/` is the command line |
| `crates/parrot-models/` | OpenAI-compatible model clients behind the `Transport` trait, all in `src/lib.rs` |
| `packages/parrot-agents/` | `@aiviarium/parrot-agents`. `src/` has the protocol translators and `test/` their tests. `fake/` has the fake CLIs |
| `site/` | The documentation site, following aiviarium/docs-base. `docs/` holds the pages as Markdown |
| `docs/` | The documentation pages, which read on GitHub and build the site |

Fixtures sit beside the crate that uses them, in `fixtures/`. `crates/parrot-tui/fixtures/casts/README.md` says how each cast was recorded and what it shows.

## Build and check

These are what CI runs:

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo nextest run --locked --workspace
cargo test --locked --workspace --doc
bun install --frozen-lockfile
bun run check
bun run typecheck
bun run test
vale --minAlertLevel=warning README.md AGENTS.md crates/*/README.md packages/*/README.md docs

cd site
npm ci
npm run check   # typecheck, lint, link check and build
```

Run the tests with [cargo-nextest](https://nexte.st), which runs every test binary at once. `cargo test --workspace` still works but runs the binaries one after another. A test that needs a workspace binary, such as a fake CLI, calls `workspace_bin` in `crates/parrot-cli/tests/common/mod.rs`. Do not start `cargo build -p` from a test: a narrower build resolves features differently and rebuilds the dependency graph.

For a fast local loop on one crate, filter inside `--workspace` instead of passing `-p`:

```bash
cargo nextest run --locked --workspace -E 'package(parrot-core)'
TMPDIR=~/.tmp cargo nextest run --locked --workspace -E 'package(parrot-cli)'
```

Passing `-p` resolves features differently from `--workspace` and triggers a dependency rebuild, which `workspace_bin` then reverses. Keeping `--workspace` avoids that rebuild. Setting `TMPDIR` to a short directory on disk, such as `~/.tmp` (`mkdir -p ~/.tmp`), avoids the 108-byte `SUN_LEN` Unix socket limit when `$TMPDIR` is deep in a workspace directory. Don't use `/tmp`: on some machines it is in memory, and test runs fill it.

Measured on an 8-core Linux laptop with a warm `target/` directory:

| Check | Time |
| --- | --- |
| Single library crate (`-E 'package(parrot-core)'`) | ~1.1s |
| Full `parrot-cli` suite (`-E 'package(parrot-cli)'`) | ~25s |
| Full Rust nextest suite (`--workspace`) | ~26s |
| Incremental `cargo clippy --all-targets` | ~2.1s |
| Doctests (`--workspace --doc`) | ~2.6s |
| TypeScript (`bun install`, `check`, `typecheck`, `test`) | ~1.5s |

CI runs the Linux Rust jobs for a change to `crates/`, `Cargo.toml`, `Cargo.lock`, `rustfmt.toml`, `.config/nextest.toml` or `.github/workflows/ci.yml`. It builds the documentation site for a change to `docs/` or `site/`. Pull requests run the macOS and Windows `platform` matrix only for changes to the platform crates listed in `.github/workflows/ci.yml` or workspace build files. Pushes to `main`, merge queues and nightly scheduled runs test every Rust change on macOS and Windows. CI runs the TypeScript job for a change to `packages/` or the Bun and Biome files at the root. A change to Markdown runs only Vale. Started by hand, CI runs everything.

## Drivers and eggs

- `parrot` builds in only the ACP and TUI cores. The Claude, Codex, pi, opencode and T3 drivers are binaries, `parrot-driver-<agent>`, packed into the eggs in `eggs/`. `eggs/README.md` describes the eggs and `docs/writing-drivers.md` the driver protocol for authors.
- `parrot eggs pack eggs --build --publish target/eggs-registry` builds this machine's driver binaries, copies them into `eggs/<agent>/parrot/variants/<os>-<arch>/` and packs every egg into a static registry in `target/eggs-registry`. Install from it with `parrot eggs install aiviarium/claude --registry target/eggs-registry`.
- The CLI tests install the in-repo eggs, with freshly built driver binaries, into each test's state directory. Start `parrot serve` through `common::Serve`, or pass the state directory through `common::with_eggs`.

## Profiles and casts

- The profiles in `eggs/<agent>/parrot/profile.kdl` are the live ones. `crates/parrot-tui/fixtures/profiles/` holds frozen copies that the truth files and tree tests use. Change both together, then run `cargo test -p parrot-tui` and `parrot-term-tool measure crates/parrot-tui/fixtures/casts`.
- The development casts are in both `crates/parrot-term/fixtures/casts/` and `crates/parrot-tui/fixtures/casts/`. Refresh both copies when you record one again.
- `cargo run -p parrot-term --example record_casts -- --list` lists the recording scenarios. Record into a scratch directory, never into the repository, and check the screens for paths, account names and process lists before you commit a cast.
- The holdout casts in `crates/parrot-tui/fixtures/casts/holdout/` were labelled after the profiles were frozen. Do not tune a profile against them.

## Conventions

- Write a short, specific commit subject, and add a body only when it explains something the diff does not.
- A commit written with AI help says so with one `Assisted-by: <tool>:<model id>` trailer per model, such as `Assisted-by: Claude:claude-opus-5-5`. Do not add `Co-Authored-By` trailers or links to sessions.
- A comment gives the reason for the code in a sentence. Neither code nor docs refer to plans or decisions made elsewhere.
- Documentation is written for people, in short sentences and with tables for lookups. It has to pass vale.
- Tests and fixtures use made-up names, keys and addresses, never a real account or machine.
- Secrets never go into argv, logs or the repository.

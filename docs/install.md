# Install

Parrot is a Rust workspace and a Bun workspace. The `parrot` command starts a runtime; the agent side comes from eggs, which are Hatchery packages installed into the runtime's state directory.

## Build the command

Parrot needs Rust 1.95 or later.

```bash
cargo install --locked --git https://github.com/aiviarium/parrot parrot-cli
```

From a checkout, build the binary into `target/release/`:

```bash
cargo build --locked --release --bin parrot
```

A tagged release publishes archives for Linux x86_64 and aarch64, macOS aarch64 and Windows x86_64. Each archive holds `parrot`, with a `.sha256` file beside the archive. The release also builds the five driver binaries that the eggs carry. Until a release exists, use the checkout.

## Install an agent egg

An egg is one agent: its driver program or its data, the options it offers and the CLI versions its data covers. Parrot ships no agent in the binary. With no egg installed, `parrot agents` shows an empty table, and the protocol's `agent/list` returns a hint that says how to install one.

The release registry is `https://aiviarium.github.io/parrot/eggs`. It is the default, so this is enough once a release is published:

```bash
parrot eggs install aiviarium/claude
parrot eggs list
```

The repository's `eggs/` folder is also a registry, but it has no driver binaries committed. Build this machine's binaries, pack every egg and publish the result into a folder:

```bash
parrot eggs pack eggs --build --publish target/eggs-registry
parrot eggs install aiviarium/claude --registry target/eggs-registry
```

`--build` runs cargo for each command egg and copies the program into its variant folder. On another platform, add `--target <triple>`. A release builds once per platform and packs every variant together.

The ACP egg contains no program. It runs on the ACP core built into `parrot`, so it installs without a build step:

```bash
parrot eggs install ./eggs/acp
```

The generic egg has no `command` of its own, so the ACP core starts `acp` on `PATH`, then `PARROT_ACP_BIN`, or the `command` option. An egg for a named ACP agent puts its own command in `driver.kdl`.

| Agent | Egg | Needs a driver program |
| --- | --- | --- |
| Claude Code | `aiviarium/claude` | Yes |
| Codex | `aiviarium/codex` | Yes |
| opencode | `aiviarium/opencode` | Yes |
| pi | `aiviarium/pi` | Yes |
| T3 Code | `aiviarium/t3` | Yes |
| Any ACP agent | `aiviarium/acp` | No, the built-in ACP core starts the agent |
| A CLI driven by its screen | an egg with `controls "tui"` | No, the built-in TUI core drives it |

Installing a package does not install the agent CLI itself. Each [driver page](drivers/claude.md) names the command it expects and the versions its data covers.

## Registries

`--registry` takes the HTTPS URL of a static registry, an `index.json` folder, or a folder of package folders such as `eggs/`. `PARROT_EGGS_REGISTRY` replaces the default registry when no `--registry` is given. `hatchery.kdl` in the state directory adds registries that every install searches ([Configuration](reference/configuration.md#hatcherykdl)). Installing with a `git` registry needs `git` 2.25 or later.

## TypeScript packages

The packages need Bun 1.3 or later. From a checkout:

```bash
bun install
bun run typecheck
bun run test
```

`@aiviarium/parrot-client` and `@aiviarium/parrot-agents` are TypeScript sources with no build step and no runtime dependencies. See the [client](reference/client.md) and [translators](reference/agents.md).

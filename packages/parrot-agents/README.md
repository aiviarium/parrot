# parrot-agents

`@aiviarium/parrot-agents` has translators between the wire protocols of agent CLIs and one item and
state model. Each translator reads the CLI's messages and turns them into items, state changes and usage.
Items are text, thinking, tool calls, approvals, questions and plans. The translator reports them through
a `Reporter` object you supply. It does not start processes or own any I/O, so you can feed it recorded
lines in a test.

The package has no runtime dependencies. The modules are TypeScript sources, and there is no build step.

## Modules

| Module | CLI | Protocol | Checked against |
| --- | --- | --- | --- |
| `./claude` | Claude Code | stream-json over stdio (`claude -p --input-format stream-json ...`) | CLI 2.1.325 |
| `./codex` | Codex | app-server, JSON-RPC 2.0 without the `jsonrpc` field | CLI 0.167.1 |
| `./acp` | Any ACP agent | Agent Client Protocol, version 1 | schema 2.6.0, `claude-agent-acp` 0.107.0 |
| `./pi` | Pi | RPC mode (`pi --mode rpc`) | Pi 1.5.1 docs |
| `./claude-transcript` | Claude Code | session transcript, JSONL | CLI 2.1.325 |
| `./codex-transcript` | Codex | rollout file, JSONL | CLI 0.167.1 |
| `./types` | | Shared types (`Capabilities`, `Usage`, `Answer`, `DriverInput`, ...) | |

Each module header describes the messages it sends and reads.

## Transcript readers

Claude Code and Codex write a transcript file for every session. `ClaudeTranscript` and `CodexTranscript`
read it one line at a time (`line(text)`) and pass the content to the same translator that reads the live
protocol, so items look the same whichever source they come from. A line returns a `LineResult` when it
says something the reader cannot report as an item. That is a prompt, the end of a turn or the CLI's
session id.
`threadItem` converts a Codex rollout item to the app-server's shape.

## Fake CLIs

`fake/` holds scripted stand-ins for the CLIs, so tests are deterministic and don't need a model. They are plain
Node scripts for Node 18 or later, and they don't import any package.

| File | Replaces |
| --- | --- |
| `fake/claude/fake-claude.mjs` | Claude Code, with stream-json for `-p` and a terminal UI without it |
| `fake/codex/fake-codex.mjs` | Codex: `app-server`, or a terminal UI and rollout file |
| `fake/fake-acp.mjs` | An ACP agent |
| `fake/fake-pi.mjs` | Pi in RPC mode |

A fake does what the message it receives says. The message holds directives in double brackets, and text
outside them is ignored:

```
[[text:Hello there]]  [[tool:notes_search {"query":"x"}]]  [[bash:touch a.txt]]  [[sleep:2000]]
```

Without a directive the answer is `You said: <message>`. The file headers list every directive.

## TUI profiles

`tui/claude-code.toml` and `tui/codex.toml` describe what the terminal UIs of Claude Code and Codex look
like: screen states, rules to recognise them and the actions that answer their dialogs. The Rust crate
`crates/parrot-tui` reads them. The fake terminal UIs draw screens these profiles recognise.

## Development

```
bun install
bunx tsc -p .     # typecheck, from this directory
bun test
```

From the repository root, `bun run typecheck`, `bun run test` and `bun run check` (Biome) run for every
package.

## License

MIT OR Apache-2.0

# Protocol translators

`@aiviarium/parrot-agents` translates the wire protocols of agent CLIs into one item and state model. A translator reads the CLI's messages and reports items, state changes, requests and usage to a `Reporter` object you supply. It starts no processes and owns no I/O, so a test can feed it recorded lines.

The package has no runtime dependencies. The modules are TypeScript sources with no build step.

## Modules

| Module | CLI | Protocol | Checked against |
| --- | --- | --- | --- |
| `@aiviarium/parrot-agents/claude` | Claude Code | `stream-json` over stdio | CLI 2.1.325 |
| `@aiviarium/parrot-agents/codex` | Codex | app server, JSON-RPC 2.0 without the `jsonrpc` field | CLI 0.167.1 |
| `@aiviarium/parrot-agents/acp` | Any ACP agent | Agent Client Protocol, version 1 | schema 2.6.0, `claude-agent-acp` 0.107.0 |
| `@aiviarium/parrot-agents/pi` | Pi | RPC mode, `pi --mode rpc` | Pi 1.5.1 |
| `@aiviarium/parrot-agents/claude-transcript` | Claude Code | Session transcript, JSONL | CLI 2.1.325 |
| `@aiviarium/parrot-agents/codex-transcript` | Codex | Rollout file under `sessions/` | CLI 0.167.1 |
| `@aiviarium/parrot-agents/types` | | Shared types | |

Each module's header describes the messages it sends and reads.

## The reporter

Every translator calls the same six methods:

| Method | Reports |
| --- | --- |
| `item(item)` | A new item, such as text, a tool call or a plan |
| `delta(id, text, field?)` | More text for an item's field, `text` by default |
| `patch(id, patch)` | A change to an item |
| `state(state, detail?)` | `idle`, `working`, `awaitingApproval` or `awaitingInput` |
| `meta(meta)` | The CLI session id, model, mode, capabilities and usage |
| `turnEnd(error?)` | The end of a turn, with an error when it failed |

The models, modes, efforts, commands and skills of the agent travel in `meta` as `Capabilities`. The host turns all of this into the Parrot protocol's items, states and requests. The translator's ids are local to the module; the host maps them.

## Types

`@aiviarium/parrot-agents/types` holds the shapes the modules share:

| Type | Is |
| --- | --- |
| `Capabilities` | Models, efforts and modes, what can change live, steer, images, files, commands, skills, resume, usage, interrupt |
| `AgentOption` | A choice for a model, mode or effort, with its label and description |
| `HarnessCommand` | A slash command or skill, with its argument hint |
| `Usage` | Token counts, optional cost, context tokens and context window |
| `Answer` | An approval action or question answers, with an optional message |
| `Attachment` | A blob id, name, content type and size |
| `ContextText` | Text from an `@` mention, with its URI and title |
| `DriverInput` | A prompt's text, context and attachments |
| `LineResult` | What a transcript line said: a prompt, a turn end or a session id |
| `TranscriptReader` | A reader with one `line(text)` method |

## Transcript readers

Claude Code and Codex write a transcript for each session. `ClaudeTranscript` and `CodexTranscript` read one line at a time and pass the content to the same translator that reads the live protocol, so items look the same from either source. A line returns a `LineResult` when it says something that isn't an item: a prompt, the end of a turn or the CLI's session id.

Parrot's TUI core uses the same readers on the eggs' profiles, so a TUI session gets its items from the transcript while the screen gives states and dialogs.

## Fake CLIs

`fake/` holds scripted stand-ins, so tests need no model:

| File | Replaces |
| --- | --- |
| `fake/claude/fake-claude.mjs` | Claude Code, with `stream-json` for `-p` and a terminal UI without it |
| `fake/codex/fake-codex.mjs` | Codex, with `app-server` or a terminal UI and rollout file |
| `fake/fake-acp.mjs` | An ACP agent |
| `fake/fake-pi.mjs` | Pi in RPC mode |

A fake does what the message says. Directives are in double brackets, and text outside them is ignored:

```text
[[text:Hello there]]  [[tool:notes_search {"query":"x"}]]  [[bash:touch a.txt]]  [[sleep:2000]]
```

Without a directive the answer is `You said: <message>`. Each file's header lists every directive. They are plain Node scripts for Node 18 or later and import nothing from the package.

## Development

```bash
cd packages/parrot-agents
bunx tsc -p .
bun test
```

From the repository root, `bun run typecheck`, `bun run test` and `bun run check` cover every package.

The package is in [`packages/parrot-agents`](https://github.com/aiviarium/parrot/tree/main/packages/parrot-agents).

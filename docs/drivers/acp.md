# ACP agents

The ACP driver speaks the Agent Client Protocol, version 1. Any agent that implements ACP can be an agent in Parrot without a driver program of its own: the core is built into `parrot`, and the egg supplies the command and the data.

| Part | Value |
| --- | --- |
| Agent id | The egg's package id, such as `gemini` |
| Egg | `aiviarium/acp` for the generic egg, or one egg per agent |
| Driver | The ACP core, `parrot driver acp` |
| Protocol versions | ACP schema `>=0.4.0, <0.5.0` in the generic egg |
| Controls | `native` |

The generic egg's `versions` range names the ACP schema its data was checked against. An egg for one agent sets its own `cli` name and range.

## Install

```bash
parrot eggs install ./eggs/acp
```

The generic egg has no command of its own. The core finds the agent's command in this order:

1. The binding's `command` or `agent.command` option.
2. The egg's `driver.kdl` `command` node.
3. `PARROT_ACP_BIN`, with extra arguments in `PARROT_ACP_ARGS`.
4. `acp` on `PATH`.

An egg for a named agent usually stops at step 2:

```kdl
driver "builtin" name="acp"
command "gemini" "--experimental-acp"
cli "gemini" versions=">=0.20.0, <0.21.0"
data-version "1"
```

Its agent id is the package id, and its display name is the egg's `about { name }`. The same core can serve several such eggs.

## What the driver reports

| Item | Source |
| --- | --- |
| Assistant text | `agent_message_chunk` updates |
| Thinking | `agent_thought_chunk` updates |
| Tool calls | Tool call and tool call update messages |
| Plans | Plan updates |
| Requests | `session/request_permission` messages |
| Usage | Token and cost fields of a prompt result |

Features: `session.stream`, `session.resume`, `attachments` for images and files, and `usage` with cost and context.

## Options and levels

ACP agents report their modes and configuration options themselves. The egg's `options.kdl` supplies a model, a mode and a reasoning level for an agent that reports none, and renames between ACP versions.

The driver maps the current mode to a Parrot level. A mode that maps to nothing counts as `host` with network.

## Exporting a Parrot session

`parrot acp` is the other direction: it serves a Parrot session as an ACP agent on standard input and output. An ACP client such as Zed or a T3 ACP provider can then work with any Parrot session.

```bash
parrot acp --agent claude --dir ~/src/notes
```

| Flag | Does |
| --- | --- |
| `--socket <PATH>` | The runtime's socket instead of the default |
| `--session <ID>` | Serves this session instead of creating one on `session/new` |
| `--agent <ID>` | The agent `session/new` starts; the first usable agent by default |
| `--dir <PATH>` | The folder to use when the ACP client sends no `cwd` |

Parrot items become ACP updates, and Parrot requests become ACP permission calls. An answer goes back to `request/answer`.

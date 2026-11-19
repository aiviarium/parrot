# Claude Code

The Claude Code driver speaks Claude Code's `stream-json` protocol. It also supplies a TUI profile, so the same egg can drive the CLI through its terminal when a client asks for that.

| Part | Value |
| --- | --- |
| Agent id | `claude` |
| Egg | `aiviarium/claude` |
| Driver | `parrot-driver-claude`, a program in the egg |
| CLI versions | `>=2.1.0, <2.2.0` |
| Controls | `native` by default, and `tui` |
| Version shim | `shims/2.1.325` for Claude Code 2.1.325 and later |

## Install

```bash
parrot eggs install aiviarium/claude
claude --version
```

The driver starts `claude` on `PATH`. Set `PARROT_CLAUDE_BIN` to use a binary elsewhere, for example in a test. The CLI has to be signed in for the agent account that runs Parrot, and its model access is the CLI's own.

## What the driver reports

| Item | Source |
| --- | --- |
| Assistant text | The `stream-json` message stream |
| Thinking | Reasoning blocks, with their summaries |
| Tool calls | Tool use and tool result blocks, including the edits and writes a tool reports |
| Plans | Plan items from Claude Code's plan messages |
| Requests | Permission prompts, questions and plan approval |
| Subagents | Task tool runs, as embedded sessions |
| Usage | Tokens, cost and context use |

Features: `session.stream`, `session.resume`, `session.steer` at the next step, `session.fork` at a turn, `session.history`, `session.subagents` with cancel, `attachments`, `usage`, `functions.mcp`.

## Options

| Option | Role | Applies | Values |
| --- | --- | --- | --- |
| `model` | `model` | `now` | The models Claude Code reports, with `default`, `opus`, `sonnet` and `haiku` before it answers |
| `permissionMode` | `mode` | `now` | `default`, `acceptEdits`, `plan`, `bypassPermissions`, `dontAsk` |
| `effort` | `effort` | `next_turn` | `low`, `medium`, `high`, `max`; `low` only for Haiku |

The permission mode maps to a Parrot level: `default`, `plan` and `dontAsk` are `read`, `acceptEdits` is `write`, and `bypassPermissions` is `host` with network. Under a ceiling, the driver starts Claude Code with `--setting-sources ""`, so user and project allow-rules can't raise the level unseen.

## TUI control

The egg's `profile.kdl` names the screens and actions of Claude Code's terminal. A client can create a session with `control: "tui"`. The profile also reads the project transcript, so items come from the transcript while the screen supplies states and dialogs.

The driver was checked against Claude Code 2.1.325. The shim replaces data for that version and later. A newer CLI inside the range is verified; one outside the range runs with the newest data and the agent reports `harness.verified: false`.

## Limits

- The driver needs a signed-in Claude Code. A signed-out CLI lists with `auth: "signed_out"`, and the first prompt fails with the CLI's own message. A missing CLI lists with `installed: false` and `usable: false`.
- Steering reaches the turn at the next step. A prompt sent while a tool runs waits for that tool.
- Subagents are reported, but the driver can't prompt or cancel them from Parrot.
- Images, PDFs, text, JSON and remote files are attachments. An attachment the CLI refuses is dropped with a `notice` update that names it.

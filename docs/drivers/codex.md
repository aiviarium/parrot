# Codex

The Codex driver speaks the Codex app server, a JSON-RPC protocol that omits the `jsonrpc` field. It also supplies a TUI profile and reads Codex's rollout files.

| Part | Value |
| --- | --- |
| Agent id | `codex` |
| Egg | `aiviarium/codex` |
| Driver | `parrot-driver-codex`, a program in the egg |
| CLI versions | `>=0.167.0, <0.168.0` |
| Controls | `native` by default, and `tui` |
| Transcript | `transcript "codex"`, rollouts under `sessions/` |

## Install

```bash
parrot eggs install aiviarium/codex
codex --version
codex login
```

The driver starts `codex` on `PATH`. Set `PARROT_CODEX_BIN` to use another binary. The CLI signs in itself and keeps its credentials where it normally does.

## What the driver reports

| Item | Source |
| --- | --- |
| Assistant text | The app server's turn items |
| Thinking | Reasoning items and their summaries |
| Tool calls | MCP and other tool calls the app server reports |
| Commands | Command executions, with their output and exit code |
| File changes | The patches Codex applies |
| Plans | Plan items from the app server |
| Requests | Approval, sign-in, input and question requests from the app server |
| Subagents | Collab agent threads, as embedded sessions |
| Usage | Tokens, context use and rate limits |

Features: `session.stream`, `session.resume`, `session.steer` at the next step, `session.fork` at a turn, `session.history`, `session.subagents` with prompt and cancel, `attachments` for images, audio and files, and `usage` with context and rate limits. Codex does not report a cost, so `usage.cost` is absent.

## Options

| Option | Role | Applies | Values |
| --- | --- | --- | --- |
| `model` | `model` | `next_turn` | The models the app server lists |
| `effort` | `effort` | `next_turn` | `low`, `medium`, `high` |
| `approvalPolicy` | `mode` | `next_turn` | `untrusted`, `on-request`, `never` |
| `sandbox` | `sandbox` | `next_turn` | `read-only`, `workspace-write`, `danger-full-access` |

The level follows the sandbox and approval policy together:

| Sandbox | Approval | Level | Network |
| --- | --- | --- | --- |
| `read-only` | any | `read` | No |
| `workspace-write` | `untrusted` | `write` | No |
| `workspace-write` | `on-request` or `never` | `sandboxed` | No |
| `danger-full-access` | any | `host` | Yes |

Under a ceiling, the driver passes overrides that keep the user's `config.toml` from loosening what Parrot set: approvals go to the person, the workspace sandbox has no network, and web search is disabled.

## TUI control

Codex can be driven through its terminal with the egg's `profile.kdl`. The profile reads Codex's rollout files for items, so the screen supplies states, dialogs and actions. The TUI core passes the Parrot MCP server as `-c mcp_servers.parrot.*` overrides.

## Limits

- The app server is a private Codex interface. The driver covers one minor release line at a time, and the egg's `versions` range says which.
- Steering reaches the turn at the next step.
- The driver reports a cost only when Codex does, which it currently does not.

# Pi

The pi driver speaks pi's RPC mode: JSON messages over the CLI's standard input and output.

| Part | Value |
| --- | --- |
| Agent id | `pi` |
| Egg | `aiviarium/pi` |
| Driver | `parrot-driver-pi`, a program in the egg |
| CLI versions | `>=1.5.0, <1.6.0` |
| Controls | `native` |

## Install

```bash
parrot eggs install aiviarium/pi
pi --version
```

The driver runs `pi --mode rpc` on `PATH`. Set `PARROT_PI_BIN` for another binary. The CLI's own sign-in and provider configuration apply.

## What the driver reports

| Item | Source |
| --- | --- |
| Assistant text | RPC response events |
| Thinking | Reasoning text and its summaries |
| Tool calls | Tool start and end events, including edits and bash runs |
| Requests | Approval, input and question requests |
| Usage | Tokens and cost |

Features: `session.stream`, `session.resume`, `session.steer` at the next step, `session.fork` at a turn, `session.history`, `attachments` for images, and `usage` with cost and context. pi reports no subagent feature.

## Options

| Option | Role | Applies | Values |
| --- | --- | --- | --- |
| `model` | `model` | `now` | The models pi reports, `default` before it answers |
| `effort` | `effort` | `now` | `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` |

pi reports its models and thinking levels at runtime. The egg's choices are the fallback, and the levels the current model offers replace them.

## Permission levels

pi has no permission system, so it lists one level, `host`. Every tool a pi session runs is outside a ceiling's control, and the session reports `advisory: true`: the driver offers only `none` isolation.

## MCP

The driver writes an `mcp.json` for the binding and gives it to pi in the CLI's own agent folder. The Parrot MCP server reaches pi that way, with the binding's token in the environment.

## Limits

- The driver covers one pi release line at a time; the egg's `versions` range says which.
- A pi session can't be steered from Parrot at an arbitrary point. Steering reaches it at the next step.
- Images are the only attachments the driver sends. Other attachments are dropped with a notice.

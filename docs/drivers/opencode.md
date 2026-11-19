# opencode

The opencode driver starts `opencode serve` on a loopback port and speaks its HTTP API and event stream. The server lives only while a binding runs.

| Part | Value |
| --- | --- |
| Agent id | `opencode` |
| Egg | `aiviarium/opencode` |
| Driver | `parrot-driver-opencode`, a program in the egg |
| CLI versions | `>=1.19.0, <1.20.0` |
| Controls | `native` |

## Install

```bash
parrot eggs install aiviarium/opencode
opencode --version
```

The driver runs `opencode serve --hostname 127.0.0.1 --port <free port>` on `PATH`, with a password it generates for that server. Set `PARROT_OPENCODE_BIN` to name another binary. Sign in to opencode itself before a session starts.

## What the driver reports

| Item | Source |
| --- | --- |
| Assistant text | The session and message events |
| Thinking | Reasoning parts of a message |
| Tool calls | Tool parts, with input and output, including edits and shell runs |
| Requests | Permission requests |
| Subagents | Session children, reported but not controllable |
| Usage | Tokens and cost |

Features: `session.stream`, `session.resume`, `session.steer` at the next step, `session.fork` at a turn, `session.history`, `session.subagents`, `attachments` for images and files, `functions.mcp`, and `usage` with cost and context.

## Options

| Option | Role | Applies | Values |
| --- | --- | --- | --- |
| `model` | `model` | `now` | The models opencode reports, `default` before it answers |
| `provider` | `model` | `now` | The providers opencode reports; choosing one selects its first enabled model |

opencode reports its models and providers at runtime, so the choices in the egg are only a fallback. Two options share the `model` role: a client can show the provider picker and the model picker as one control.

## Limits

- The server is private to the binding. Parrot does not connect to an `opencode serve` that someone else started.
- A subagent is reported as an embedded session, but the driver cannot prompt or cancel it.
- The HTTP client speaks plain HTTP on loopback. It uses the server's password as basic auth, and the password never goes in a log or an argument.

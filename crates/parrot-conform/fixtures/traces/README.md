# Recorded harness traces

Real exchanges recorded on 2026-11-18 from signed-in CLIs, for checking wire types and writing fake scenarios. Each line is `{ "dir": "in" | "out", "at": <unix seconds>, "msg": <the JSON message> }`. Paths, accounts, host names, MCP servers and user skills are replaced with made-up values, and a file the model read is replaced with a short stand-in.

| File | CLI | What happens |
| --- | --- | --- |
| `claude-2.1.327.jsonl` | Claude Code 2.1.327, `claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages --permission-prompt-tool stdio --model claude-haiku-5-5` | The `initialize` control request (commands, models, account), a prompt with a base64 PNG image, `can_use_tool` permission requests answered with `allow`, tool calls and results, a second prompt interrupted mid-stream with the `interrupt` control request |
| `codex-0.167.1.jsonl` | Codex 0.167.1, `codex app-server` | `initialize`, `model/list`, `thread/start` with `approvalPolicy: untrusted`, a turn with text and a data-URL image, two `item/commandExecution/requestApproval` server requests answered with `accept`, token usage and rate limits, a second turn interrupted with `turn/interrupt` |
| `pi-1.5.1.jsonl` | pi 1.5.1, `pi --mode rpc --model local/local-chat` | Startup state, models, thinking levels and commands; a text turn with a `read` tool call; an image prompt; session stats and fork messages; a manual `compact`; `set_session_name`; `clear_queue`; an aborted turn; `fork` |

The pi exchange ran against a local OpenAI-compatible endpoint, because this machine has no
provider signed in. The endpoint streamed a reply and one `read` tool call; pi's own RPC records,
session file and tool execution are unchanged by that. Its paths, session and entry ids are
made up, and the system prompt in its system message is a short stand-in.

# Recorded harness traces

Real exchanges recorded on 2026-11-18 from signed-in CLIs, for checking wire types and writing fake scenarios. Each line is `{ "dir": "in" | "out", "at": <unix seconds>, "msg": <the JSON message> }`. Paths, accounts, host names, MCP servers and user skills are replaced with made-up values, and a file the model read is replaced with a short stand-in.

| File | CLI | What happens |
| --- | --- | --- |
| `claude-2.1.327.jsonl` | Claude Code 2.1.327, `claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages --permission-prompt-tool stdio --model claude-haiku-5-5` | The `initialize` control request (commands, models, account), a prompt with a base64 PNG image, `can_use_tool` permission requests answered with `allow`, tool calls and results, a second prompt interrupted mid-stream with the `interrupt` control request |
| `codex-0.167.1.jsonl` | Codex 0.167.1, `codex app-server` | `initialize`, `model/list`, `thread/start` with `approvalPolicy: untrusted`, a turn with text and a data-URL image, two `item/commandExecution/requestApproval` server requests answered with `accept`, token usage and rate limits, a second turn interrupted with `turn/interrupt` |

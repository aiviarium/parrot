# parrot-mcp

The MCP server that Parrot adds to each agent session, and the catalog of functions it lists.
An agent CLI starts it as `parrot mcp --binding <id>` and talks to it on standard input and output.
It connects to the runtime as the binding's session and maps MCP tools onto Parrot functions.

| Module | What it does |
| --- | --- |
| `server` | MCP over newline-delimited JSON-RPC: `initialize`, `ping`, `tools/list` with cursors, `tools/call`, and `notifications/tools/list_changed` |
| `catalog` | The tool list, sorted by name. It holds the session functions and pinned functions, plus search, describe and call tools that reach every other function |
| `session_functions` | `parrot.sessions.*`, `parrot.tasks.*`, `parrot.task.finish` and `parrot.task.fail` as projections of protocol methods, with input and output schemas |
| `rules` | JSON text results, fixed error codes, idempotency keys, the 256 KiB cut, and the call time limit |
| `upstream` | The JSON-RPC client to the runtime |
| `ws` | The client over WebSocket, for a runtime endpoint |

## Running it

| Option | Default | Meaning |
| --- | --- | --- |
| `--binding` | `PARROT_BINDING` | The binding the server serves |
| `--socket` | `PARROT_SOCKET`, then the platform path | The runtime's local socket |
| `--endpoint` | none | A runtime WebSocket endpoint, such as `ws://127.0.0.1:7420/parrot` |
| `--token-file` | `PARROT_MCP_TOKEN` | Where the binding's credential is read from |
| `--tool-timeout` | `PARROT_TOOL_TIMEOUT` | The agent CLI's tool-call timeout, in seconds |

The credential never goes on the command line. The server sends it in `initialize` as
`binding.token`, and over WebSocket also as the `parrot.token.` subprotocol.

## Calls that wait

The server ends each call before the agent CLI's tool timeout, with a margin, and within 3 minutes.
Every `function/call` it sends has a key. The server makes one when the caller gives none.
A call that runs out of time returns the error `pending_approval` with that key.
Calling again with the key returns the result once a person answers the request.

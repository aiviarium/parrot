# TypeScript client

`@aiviarium/parrot-client` is a JSON-RPC client for a Parrot runtime, with types generated from `spec/schema/parrot.schema.json`. It has no runtime dependencies, and its tests run under Bun.

The package is in [`packages/parrot-client`](https://github.com/aiviarium/parrot/tree/main/packages/parrot-client). Add it from the repository or import it inside the workspace.

## Use

```ts
import { Client } from "@aiviarium/parrot-client"

const client = Client.overWebSocket("ws://127.0.0.1:7420/parrot", { token: process.env.PARROT_TOKEN })
await client.initialize()

const subscription = await client.subscribe({ session: "ses_01J8Q4T7N3K2M9X8C6V5B4A3S2" })
const stop = client.on("update", ({ update }) => {
  if (update.type === "state") console.log(update.state)
})
await client.prompt("ses_01J8Q4T7N3K2M9X8C6V5B4A3S2", "Summarise the README.")
```

A WebSocket connection needs a client token. Make one over a local connection, where the caller is the owner, with `token/create` (`{ name, scopes, sessions?, expires? }`). The secret is returned once.

For a program that speaks the protocol on standard input and output, import the stdio entry. The root entry doesn't import child-process code, and it builds for the browser:

```ts
import { stdioClient } from "@aiviarium/parrot-client/stdio"

const local = stdioClient("parrot", ["connect"])
await local.initialize()
```

## API

| Member | Does |
| --- | --- |
| `Client.overWebSocket(url, options)` | A client of `ws://` or `wss://`, with a client `token` and client options |
| `stdioClient(command, args, options)` | From `@aiviarium/parrot-client/stdio`: a client of a child process's standard input and output |
| `initialize(options?)` | Sends the handshake and returns its result |
| `subscribe(target, options?)` | Subscribes to a session, a tree or the index |
| `unsubscribe(subscription)` | Ends a subscription |
| `prompt(session, text, options?)` | Sends a prompt with an optional delivery, context and attachments |
| `call(method, params)` | Calls any protocol method, typed from the schema |
| `request(method, params)` | Calls a method without the generated type check |
| `notify(method, params)` | Sends a notification |
| `on(method, handler)` | Listens for a notification and returns a function that stops it |
| `provide(method, handler)` | Answers a request the runtime sends, for a function this client provides |
| `close()` | Closes the transport |
| `initialized` | The handshake result, once `initialize` succeeded |

`call` names a method from the schema and checks its params and result. `provide` does the same for the requests a runtime sends to a client, such as `function/call`.

## Types

`src/schema.ts` is generated. It has every type the schema defines, and three maps:

| Map | Holds |
| --- | --- |
| `Methods` | Each method's params and result |
| `ClientMethods` | Each request the runtime sends to a client |
| `Notifications` | Each notification's params |

After a change to the schema, run `bun run generate` in the package. `bun run test` regenerates in check mode and runs the tests, so a stale file fails.

## Transports

| Transport | Entry point |
| --- | --- |
| WebSocket | `Client.overWebSocket(url, { token })`, from the root entry |
| stdio | `stdioClient(command, args)`, from `@aiviarium/parrot-client/stdio` |

A test walks the root entry's imports and fails if any module reaches `node:`. `stdioTransport` sits beside `stdioClient` in the stdio entry, for a client built another way.

Each message is one WebSocket text frame or one line. Messages sent before a WebSocket opens wait for it. `parrot connect` gives a stdio client the local socket without opening it directly.

## Tests

```bash
cd packages/parrot-client
bun test
bunx tsc -p .
```

The tests cover the transports, the request and response ids, subscriptions and the generated types.

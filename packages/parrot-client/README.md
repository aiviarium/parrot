# @aiviarium/parrot-client

The TypeScript client of the Parrot runtime. It has the protocol types generated from
`spec/schema/parrot.schema.json`, and a JSON-RPC client over a WebSocket or a child process's stdio.

The root entry is browser-safe. The stdio transport starts a child process, so it lives in
`@aiviarium/parrot-client/stdio`.

```ts
import { Client } from "@aiviarium/parrot-client"

const client = Client.overWebSocket("ws://127.0.0.1:7878/parrot")
await client.initialize()
const subscription = await client.subscribe({ tree: "ses_1" })
client.on("update", (update) => console.log(update))
await client.prompt("ses_1", "hello")
```

## Types

`src/schema.ts` is generated. Do not edit it. Run `bun run generate` in this package after a change to
the schema. `bun run test` fails when the file is stale.

It has every type the schema defines, and three maps:

| Map | Holds | Used by |
| --- | --- | --- |
| `Methods` | Each method's params and result | `Client.call` |
| `ClientMethods` | Each request the runtime sends to a client, such as `function/call` | `Client.provide` |
| `Notifications` | Each notification's params | `Client.on` |

The compiler checks a method, its params and its result. `Client.initialize`, `subscribe`,
`unsubscribe` and `prompt` stay as shortcuts.

```ts
const result = await client.call("session/get", { session: "ses_1" })
client.on("update", (update) => {
  if (update.update.type === "state") console.log(update.update.state)
})
client.provide("function/call", (call) => ({ output: { echoed: call.input } }))
```

## Transports

| Transport | Entry point | Use |
| --- | --- | --- |
| WebSocket | `Client.overWebSocket(url, { token })` | A runtime at `/parrot`, with a client token |
| stdio | `stdioClient(command, args)` from `@aiviarium/parrot-client/stdio` | A process that speaks JSON-RPC lines, such as `parrot connect` |

`stdioTransport` is exported beside `stdioClient` for a client that is built another way.

Each message is one WebSocket text frame or one line, as spec 3.1 says. Messages sent before a
WebSocket opens wait for it.

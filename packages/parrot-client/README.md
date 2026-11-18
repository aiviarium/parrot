# @aiviarium/parrot-client

The TypeScript client of the Parrot runtime. It has the protocol types generated from
`spec/schema/parrot.schema.json`, and a JSON-RPC client over a WebSocket or a child process's stdio.

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

It holds every type the schema defines, and two maps: `Methods` with each method's params and result,
and `Notifications` with each notification's params. `Client.call` and `Client.on` use them, so a
method, its params and its result are checked by the compiler. `Client.initialize`, `subscribe`,
`unsubscribe` and `prompt` stay as shortcuts.

```ts
const result = await client.call("session/get", { session: "ses_1" })
client.on("update", (update) => {
  if (update.update.type === "state") console.log(update.update.state)
})
```

## Transports

| Transport | Entry point | Use |
| --- | --- | --- |
| WebSocket | `Client.overWebSocket(url, { token })` | A runtime at `/parrot`, with a client token |
| stdio | `Client.overStdio(command, args)` | A process that speaks JSON-RPC lines, such as a driver |

Each message is one WebSocket text frame or one line, as spec 3.1 says. Messages sent before a
WebSocket opens wait for it.

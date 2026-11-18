import { afterEach, describe, expect, test } from "bun:test"
import { join } from "node:path"
import { Client, type Notifications, RpcError } from "../src"
import { answer, answers } from "./fake"

let server: ReturnType<typeof Bun.serve> | undefined

afterEach(() => {
  server?.stop(true)
  server = undefined
})

function startWebSocket(): string {
  server = Bun.serve({
    port: 0,
    fetch(request, runtime) {
      if (runtime.upgrade(request, { data: {} })) return
      return new Response("websocket only", { status: 400 })
    },
    websocket: {
      message(socket, message) {
        for (const line of answer(String(message))) socket.send(line)
      },
    },
  })
  return `ws://127.0.0.1:${server.port}/parrot`
}

describe("Client", () => {
  test("initializes, subscribes and prompts over a WebSocket", async () => {
    const client = Client.overWebSocket(startWebSocket())
    try {
      const opened = await client.initialize({ features: { "client.notify": {} } })
      expect(opened.protocol).toBe(1)
      expect(opened.runtime.env).toBe("made-up")
      expect(client.initialized).toEqual(opened)

      const updates: unknown[] = []
      const stop = client.on("update", (params) => updates.push(params))
      const subscription = await client.subscribe({ session: "ses_made-up" }, { items: 10 })
      expect(subscription.head).toBe("1")

      const prompted = await client.prompt("ses_made-up", "hello", { delivery: "queue" })
      expect(prompted).toMatchObject({ disposition: "started" })
      expect(updates).toHaveLength(1)
      stop()
    } finally {
      client.close()
    }
  })

  test("types a call and an update handler from the schema", async () => {
    const client = Client.overWebSocket(startWebSocket())
    try {
      await client.initialize()
      const seen: Notifications["update"][] = []
      const stop = client.on("update", (params) => {
        seen.push(params)
        if (params.update.type === "state") expect(params.update.state).toBe("idle")
      })

      const subscription = await client.call("subscribe", { target: { tree: "ses_made-up" } })
      expect(subscription.subscription).toBe("sub_made-up")
      const prompted = await client.call("session/prompt", { session: "ses_made-up", text: "hello" })
      expect(prompted.disposition).toBe("started")
      expect(seen).toHaveLength(1)
      stop()

      // @ts-expect-error a method outside the table is a type error
      await client.call("missing/method", {}).catch(() => undefined)
    } finally {
      client.close()
    }
  })

  test("answers a function call the runtime sends, typed from the schema", async () => {
    const client = Client.overWebSocket(startWebSocket())
    try {
      await client.initialize()
      const stop = client.provide("function/call", (params) => {
        expect(params.function).toBe("acme.echo")
        return { output: params.input }
      })
      await client.call("function/provide", {
        functions: [
          {
            name: "acme.echo",
            description: "Echoes its input",
            input: { type: "object" },
            effects: ["read"],
            level: "read",
          },
        ],
      })
      for (let i = 0; i < 50 && !answers.has(900); i++) await Bun.sleep(10)
      expect(answers.get(900)).toEqual({ output: { text: "hi" } })
      stop()
    } finally {
      client.close()
    }
  })

  test("rejects a method the runtime does not have", async () => {
    const client = Client.overWebSocket(startWebSocket())
    try {
      await client.initialize()
      const error = await client.request("missing/method").catch((caught: unknown) => caught)
      expect(error).toBeInstanceOf(RpcError)
      expect((error as RpcError).code).toBe(-32601)
    } finally {
      client.close()
    }
  })

  test("initializes, subscribes and prompts over stdio", async () => {
    const script = join(import.meta.dir, "fake-stdio.ts")
    const client = Client.overStdio(process.execPath, [script])
    try {
      const opened = await client.initialize()
      expect(opened.connection.principal).toBe("owner")

      const updates: unknown[] = []
      client.on("update", (params) => updates.push(params))
      await client.subscribe({ tree: "ses_made-up" })
      const prompted = await client.prompt("ses_made-up", "hello")
      expect(prompted.message).toBe("msg_made-up")
      expect(updates).toHaveLength(1)
    } finally {
      client.close()
    }
  })

  test("fails pending calls when the other side closes", async () => {
    const client = Client.overStdio(process.execPath, ["-e", "process.stdin.resume()"])
    try {
      const call = client.request("session/list")
      client.close()
      const error = await call.catch((caught: unknown) => caught)
      expect(error).toBeInstanceOf(RpcError)
    } finally {
      client.close()
    }
  })
})

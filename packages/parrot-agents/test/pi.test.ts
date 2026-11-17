// Pi's RPC mode into session items, on records shaped as Pi 1.5.1's docs show them (docs/json.md,
// rpc-commands.md, rpc-extension-ui.md) and as the installed CLI answered state commands.
import { describe, expect, test } from "bun:test"
import { PiProtocol } from "../src/pi"

function setup() {
  const log: any[] = []
  const written: any[] = []
  const out = {
    item: (i: any) => log.push(["item", i]),
    delta: (id: string, text: string) => log.push(["delta", id, text]),
    patch: (id: string, p: any) => log.push(["patch", id, p]),
    state: (s: string) => log.push(["state", s]),
    meta: (m: any) => log.push(["meta", m]),
    turnEnd: (e?: string) => log.push(["turnEnd", e]),
  }
  const p = new PiProtocol(out as any, (l) => written.push(JSON.parse(l)), {} as any)
  return { p, log, written }
}

describe("PiProtocol", () => {
  test("a refused prompt fails the send (Pi without a provider: No API key found)", async () => {
    const { p, written } = setup()
    const sent = p.prompt({ text: "say pong" })
    p.event({
      id: written[0].id,
      type: "response",
      command: "prompt",
      success: false,
      error: "No API key found for the selected model.",
    })
    await expect(sent).rejects.toThrow("No API key found")
  })

  test("a run: text deltas, a tool, the authoritative message end, settled", async () => {
    const { p, log, written } = setup()
    const sent = p.prompt({ text: "list files" })
    p.event({
      id: written[0].id,
      type: "response",
      command: "prompt",
      success: true,
      data: { disposition: "started" },
    })
    await sent
    const ev = (e: any) => p.event(e)
    ev({ type: "agent_start" })
    ev({ type: "message_start", message: { role: "assistant", content: [], stopReason: "pending" } })
    ev({ type: "message_update", usage: {}, assistantMessageEvent: { type: "text_start", contentIndex: 0 } })
    ev({
      type: "message_update",
      usage: {},
      assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta: "Hello " },
    })
    ev({
      type: "message_update",
      usage: {},
      assistantMessageEvent: { type: "text_end", contentIndex: 0, content: "Hello there" },
    })
    ev({
      type: "tool_execution_start",
      toolCallId: "call_abc123",
      toolName: "bash",
      args: { command: "ls -la" },
    })
    ev({
      type: "tool_execution_end",
      toolCallId: "call_abc123",
      toolName: "bash",
      result: { content: [{ type: "text", text: "complete output" }], details: {} },
      isError: false,
    })
    ev({
      type: "message_end",
      message: {
        role: "assistant",
        content: [],
        stopReason: "stop",
        usage: {
          input: 100,
          output: 1,
          cacheRead: 0,
          cacheWrite: 0,
          totalTokens: 101,
          cost: { total: 0.01 },
        },
      },
    })
    ev({ type: "agent_end", messages: [], willRetry: false })
    ev({ type: "agent_settled", aborted: false })
    expect(log).toContainEqual(["state", "working"])
    expect(log).toContainEqual(["item", { id: "pi-1-0", kind: "assistant", text: "", streaming: true }])
    expect(log).toContainEqual(["delta", "pi-1-0", "Hello "])
    expect(log).toContainEqual(["patch", "pi-1-0", { text: "Hello there", streaming: false }])
    expect(log).toContainEqual([
      "item",
      {
        id: "call_abc123",
        kind: "tool",
        name: "bash",
        title: "ls -la",
        input: { command: "ls -la" },
        status: "running",
        source: "harness",
      },
    ])
    expect(log).toContainEqual(["patch", "call_abc123", { status: "ok", output: "complete output" }])
    expect(log.findLast((x) => x[0] === "meta")![1].usage).toMatchObject({
      inputTokens: 100,
      outputTokens: 1,
      costUsd: 0.01,
    })
    expect(log.at(-1)).toEqual(["turnEnd", undefined])
  })

  test("an extension's confirm is an approval, its select a question", () => {
    const { p, log, written } = setup()
    p.event({
      type: "extension_ui_request",
      id: "uuid-2",
      method: "confirm",
      title: "Clear session?",
      message: "All messages will be lost.",
    })
    expect(log.at(-1)![1]).toMatchObject({ id: "pi-ui-uuid-2", kind: "approval", summary: "Clear session?" })
    p.answer("pi-ui-uuid-2", { action: "allow" })
    expect(written.at(-1)).toEqual({ type: "extension_ui_response", id: "uuid-2", confirmed: true })
    p.event({
      type: "extension_ui_request",
      id: "uuid-1",
      method: "select",
      title: "Allow dangerous command?",
      options: ["Allow", "Block"],
    })
    p.answer("pi-ui-uuid-1", { answers: { "Allow dangerous command?": "Block" } })
    expect(written.at(-1)).toEqual({ type: "extension_ui_response", id: "uuid-1", value: "Block" })
  })
})

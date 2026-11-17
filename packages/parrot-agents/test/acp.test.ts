// The Agent Client Protocol into session items, on messages shaped as `claude-agent-acp` 0.107.0 sent them
// (recorded with a probe client) and as the published schema (v1) describes the rest.
import { describe, expect, test } from "bun:test"
import { AcpProtocol } from "../src/acp"

function setup() {
  const log: any[] = []
  const written: any[] = []
  const out = {
    item: (i: any) => log.push(["item", i]),
    delta: (id: string, text: string) => log.push(["delta", id, text]),
    patch: (id: string, p: any) => log.push(["patch", id, p]),
    meta: (m: any) => log.push(["meta", m]),
    turnEnd: (e?: string) => log.push(["turnEnd", e]),
  }
  const p = new AcpProtocol(out as any, (l) => written.push(JSON.parse(l)), {} as any)
  const answer = (id: number, result: any) => p.message({ jsonrpc: "2.0", id, result })
  return { p, log, written, answer }
}

describe("AcpProtocol", () => {
  test("initialize and a new session: capabilities, modes, models from config options, MCP over HTTP", async () => {
    const { p, written, answer } = setup()
    const opened = p.open({ cwd: "/w", mcp: { name: "parrot", url: "http://k/mcp", token: "parrot_tok_x" } })
    await Promise.resolve()
    expect(written[0]).toMatchObject({
      method: "initialize",
      params: { protocolVersion: 1, clientCapabilities: { terminal: false } },
    })
    answer(1, {
      protocolVersion: 1,
      agentCapabilities: {
        promptCapabilities: { image: true, embeddedContext: true },
        mcpCapabilities: { http: true, sse: true },
        loadSession: true,
        sessionCapabilities: { resume: {} },
      },
      agentInfo: { name: "@agentclientprotocol/claude-agent-acp", version: "0.107.0" },
      authMethods: [],
    })
    await new Promise((r) => setTimeout(r, 0))
    expect(written[1]).toEqual({
      jsonrpc: "2.0",
      id: 2,
      method: "session/new",
      params: {
        cwd: "/w",
        mcpServers: [
          {
            type: "http",
            name: "parrot",
            url: "http://k/mcp",
            headers: [{ name: "Authorization", value: "Bearer parrot_tok_x" }],
          },
        ],
      },
    })
    answer(2, {
      sessionId: "6ef06461",
      modes: {
        currentModeId: "auto",
        availableModes: [
          { id: "default", name: "Manual", description: "Always ask" },
          { id: "auto", name: "Auto" },
        ],
      },
      configOptions: [
        {
          id: "model",
          name: "Model",
          category: "model",
          type: "select",
          currentValue: "default",
          options: [
            { value: "default", name: "Default" },
            { value: "haiku", name: "Haiku" },
          ],
        },
      ],
    })
    expect(await opened).toBe("6ef06461")
    expect(p.capabilities.models?.map((m) => m.id)).toEqual(["default", "haiku"])
    expect(p.capabilities.modes?.map((m) => m.id)).toEqual(["default", "auto"])
    expect(p.capabilities.resume).toBe(true)
    expect(p.mode).toBe("auto")
  })

  test("a prompt: message chunks by message id, a tool call with a permission request, the stop reason", async () => {
    const { p, log, written, answer } = setup()
    p.sessionId = "s"
    await p.send({ text: "go" })
    const promptId = written[0].id
    const up = (u: any) =>
      p.message({ jsonrpc: "2.0", method: "session/update", params: { sessionId: "s", update: u } })
    up({ sessionUpdate: "usage_update", used: 25619, size: 1000000 })
    up({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "po" }, messageId: "msg_1" })
    up({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "ng" }, messageId: "msg_1" })
    up({
      sessionUpdate: "tool_call",
      toolCallId: "t1",
      title: "`touch a`",
      kind: "execute",
      status: "pending",
      rawInput: { command: "touch a" },
    })
    p.message({
      jsonrpc: "2.0",
      id: 7,
      method: "session/request_permission",
      params: {
        sessionId: "s",
        toolCall: { toolCallId: "t1", title: "`touch a`", kind: "execute", rawInput: { command: "touch a" } },
        options: [
          { optionId: "allow_always", name: "Always Allow", kind: "allow_always" },
          { optionId: "allow", name: "Allow", kind: "allow_once" },
          { optionId: "reject", name: "Reject", kind: "reject_once" },
        ],
      },
    })
    const a = log.find((x) => x[0] === "item" && x[1].kind === "approval")![1]
    expect(a).toMatchObject({ id: "acp-perm-7", tool: "execute", items: ["touch a"], canAllowSession: true })
    p.answer("acp-perm-7", { action: "allowSession" })
    expect(written.at(-1)).toEqual({
      jsonrpc: "2.0",
      id: 7,
      result: { outcome: { outcome: "selected", optionId: "allow_always" } },
    })
    up({
      sessionUpdate: "tool_call_update",
      toolCallId: "t1",
      status: "completed",
      content: [{ type: "content", content: { type: "text", text: "done" } }],
    })
    answer(promptId, {
      stopReason: "end_turn",
      usage: {
        inputTokens: 2,
        outputTokens: 71,
        cachedReadTokens: 11372,
        cachedWriteTokens: 14240,
        totalTokens: 25685,
      },
    })
    await new Promise((r) => setTimeout(r, 0))
    const items = log.filter((x) => x[0] === "item").map((x) => x[1])
    expect(items[0]).toMatchObject({ id: "msg_1:t", kind: "assistant", text: "po" })
    expect(log).toContainEqual(["delta", "msg_1:t", "ng"])
    expect(log).toContainEqual(["patch", "t1", { output: "done", status: "ok" }])
    expect(log.at(-1)).toEqual(["turnEnd", undefined])
    expect(log.findLast((x) => x[0] === "meta")![1].usage).toMatchObject({
      outputTokens: 71,
      contextTokens: 25619,
      contextWindow: 1000000,
    })
  })

  test("cancel answers open permission requests with cancelled; a refusal ends the turn with an error", async () => {
    const { p, log, written, answer } = setup()
    p.sessionId = "s"
    await p.send({ text: "go" })
    const promptId = written[0].id
    p.message({
      jsonrpc: "2.0",
      id: 9,
      method: "session/request_permission",
      params: { sessionId: "s", toolCall: { toolCallId: "t" }, options: [] },
    })
    p.cancel()
    expect(written).toContainEqual({ jsonrpc: "2.0", method: "session/cancel", params: { sessionId: "s" } })
    expect(written).toContainEqual({ jsonrpc: "2.0", id: 9, result: { outcome: { outcome: "cancelled" } } })
    answer(promptId, { stopReason: "refusal" })
    await new Promise((r) => setTimeout(r, 0))
    expect(log.at(-1)).toEqual(["turnEnd", "The agent refused to go on."])
  })
})

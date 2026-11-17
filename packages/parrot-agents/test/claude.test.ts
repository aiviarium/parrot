// The stream-json translation, fed lines shaped like those Claude Code 2.1.325 sent in a real run (a
// turn with a streamed answer, a Bash permission prompt, AskUserQuestion, a result with usage).
import { describe, expect, test } from "bun:test"
import { ClaudeProtocol, type Reporter } from "../src/claude"

function record() {
  const events: Array<[string, ...unknown[]]> = []
  const written: any[] = []
  const r: Reporter = {
    item: (i) => events.push(["item", i]),
    delta: (id, text, field) => events.push(["delta", id, text, field]),
    patch: (id, p) => events.push(["patch", id, p]),
    state: (s) => events.push(["state", s]),
    meta: (m) => events.push(["meta", m]),
    turnEnd: (e) => events.push(["turnEnd", e]),
  }
  const p = new ClaudeProtocol(r, (l) => written.push(JSON.parse(l)), { resume: true, steer: true } as any)
  return { p, events, written }
}

const sid = "05fd0190-777c-45a1-811f-5f46a4acfa5d"
const lines = (p: ClaudeProtocol, ...ms: object[]) => {
  for (const m of ms) p.line(JSON.stringify(m))
}

describe("ClaudeProtocol", () => {
  test("a thinking block without text (a transcript keeps only its signature) shows nothing", () => {
    const { p, events } = record()
    lines(p, {
      type: "assistant",
      message: {
        id: "msg_t",
        role: "assistant",
        content: [{ type: "thinking", thinking: "", signature: "abc" }],
      },
      parent_tool_use_id: null,
    })
    lines(p, {
      type: "assistant",
      message: { id: "msg_t", role: "assistant", content: [{ type: "text", text: "Heron." }] },
      parent_tool_use_id: null,
    })
    const items = events.filter((e) => e[0] === "item").map((e) => e[1] as { kind: string; text?: string })
    expect(items.map((i) => i.kind)).toEqual(["assistant"])
    expect(items[0]?.text).toBe("Heron.")
  })

  test("a streamed answer becomes one assistant item, replaced by the whole block", () => {
    const { p, events } = record()
    lines(
      p,
      {
        type: "system",
        subtype: "init",
        session_id: sid,
        model: "claude-haiku-5-5",
        permissionMode: "default",
        skills: ["research"],
        slash_commands: ["review", "research"],
      },
      {
        type: "stream_event",
        event: { type: "message_start", message: { id: "msg_1", content: [] } },
        parent_tool_use_id: null,
      },
      {
        type: "stream_event",
        event: { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
        parent_tool_use_id: null,
      },
      {
        type: "stream_event",
        event: { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "p" } },
        parent_tool_use_id: null,
      },
      {
        type: "stream_event",
        event: { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "ong" } },
        parent_tool_use_id: null,
      },
      {
        type: "assistant",
        message: {
          id: "msg_1",
          content: [{ type: "text", text: "pong" }],
          usage: { input_tokens: 2, output_tokens: 4 },
        },
        parent_tool_use_id: null,
      },
      {
        type: "result",
        subtype: "success",
        is_error: false,
        total_cost_usd: 0.003,
        usage: {
          input_tokens: 2,
          output_tokens: 4,
          cache_read_input_tokens: 25990,
          cache_creation_input_tokens: 14735,
        },
        modelUsage: { "claude-haiku-5-5": { contextWindow: 1000000 } },
      },
    )
    const meta = events.find((e) => e[0] === "meta")![1] as any
    expect(meta.cliSession).toBe(sid)
    expect(meta.model).toBe("claude-haiku-5-5")
    expect(meta.capabilities.skills.map((s: any) => s.name)).toEqual(["research"])
    expect(meta.capabilities.commands.map((s: any) => s.name)).toEqual(["review"])
    const items = events.filter((e) => e[0] === "item").map((e) => e[1] as any)
    expect(items.map((i) => [i.id, i.kind, i.text, i.streaming])).toEqual([
      ["msg_1:t1", "assistant", "", true],
      ["msg_1:t1", "assistant", "pong", false],
    ])
    expect(events.filter((e) => e[0] === "delta").map((e) => e[2])).toEqual(["p", "ong"])
    const usage = events.filter((e) => e[0] === "meta").pop()![1] as any
    expect(usage.usage.costUsd).toBe(0.003)
    expect(usage.usage.contextWindow).toBe(1000000)
    expect(events[events.length - 1]).toEqual(["turnEnd", undefined])
  })

  test("a permission prompt is an approval; the answer goes back as a control response", () => {
    const { p, events, written } = record()
    lines(
      p,
      {
        type: "assistant",
        message: {
          id: "msg_2",
          content: [
            {
              type: "tool_use",
              id: "toolu_1",
              name: "Bash",
              input: { command: "touch hello.txt", description: "Create hello.txt" },
            },
          ],
        },
        parent_tool_use_id: null,
      },
      {
        type: "control_request",
        request_id: "req-1",
        request: {
          subtype: "can_use_tool",
          tool_name: "Bash",
          input: { command: "touch hello.txt", description: "Create hello.txt" },
          description: "Create hello.txt",
          permission_suggestions: [{ type: "setMode", mode: "acceptEdits", destination: "session" }],
          tool_use_id: "toolu_1",
        },
      },
    )
    const approval = events.find((e) => e[0] === "item" && (e[1] as any).kind === "approval")![1] as any
    expect(approval).toMatchObject({
      id: "req-1",
      tool: "Bash",
      status: "pending",
      items: ["touch hello.txt"],
      canAllowSession: true,
    })
    expect(approval.summary).toContain("Create hello.txt")
    expect(p.answer("req-1", { action: "allow" })).toBe(true)
    expect(written.pop()).toEqual({
      type: "control_response",
      response: {
        subtype: "success",
        request_id: "req-1",
        response: {
          behavior: "allow",
          updatedInput: { command: "touch hello.txt", description: "Create hello.txt" },
        },
      },
    })
    expect(p.answer("req-1", { action: "allow" })).toBe(false)
    lines(p, {
      type: "user",
      message: {
        role: "user",
        content: [
          {
            tool_use_id: "toolu_1",
            type: "tool_result",
            content: "(Bash completed with no output)",
            is_error: false,
          },
        ],
      },
      parent_tool_use_id: null,
    })
    expect(events.pop()).toEqual([
      "patch",
      "toolu_1",
      { status: "ok", output: "(Bash completed with no output)" },
    ])
  })

  test("a denial carries a message; AskUserQuestion answers carry the answers", () => {
    const { p, events, written } = record()
    lines(p, {
      type: "control_request",
      request_id: "q1",
      request: {
        subtype: "can_use_tool",
        tool_name: "AskUserQuestion",
        input: {
          questions: [
            {
              question: "Tea or coffee?",
              header: "Drink",
              options: [{ label: "Tea" }, { label: "Coffee" }],
              multiSelect: false,
            },
          ],
        },
        tool_use_id: "toolu_q",
        requires_user_interaction: true,
      },
    })
    const q = events.find((e) => e[0] === "item")![1] as any
    expect(q.kind).toBe("question")
    expect(q.questions[0].options.map((o: any) => o.label)).toEqual(["Tea", "Coffee"])
    p.answer("q1", { answers: { "Tea or coffee?": "Tea" } })
    expect(written.pop().response.response).toEqual({
      behavior: "allow",
      updatedInput: {
        questions: q.questions.map((x: any) => ({
          ...x,
          options: x.options.map((o: any) => ({ label: o.label })),
        })),
        answers: { "Tea or coffee?": "Tea" },
      },
    })
    lines(p, {
      type: "control_request",
      request_id: "w1",
      request: {
        subtype: "can_use_tool",
        tool_name: "Write",
        input: { file_path: "/x/a.txt", content: "hi" },
      },
    })
    p.answer("w1", { action: "deny", message: "Not that file" })
    expect(written.pop().response.response).toEqual({ behavior: "deny", message: "Not that file" })
  })

  test("ExitPlanMode is a plan with an approval; an unknown control request is refused", () => {
    const { p, events, written } = record()
    lines(p, {
      type: "control_request",
      request_id: "pl",
      request: { subtype: "can_use_tool", tool_name: "ExitPlanMode", input: { plan: "1. Read\n2. Write" } },
    })
    const kinds = events.filter((e) => e[0] === "item").map((e) => (e[1] as any).kind)
    expect(kinds).toEqual(["plan", "approval"])
    p.answer("pl", { action: "allow" })
    expect(
      events.some((e) => e[0] === "patch" && e[1] === "plan-pl" && (e[2] as any).status === "approved"),
    ).toBe(true)
    lines(p, { type: "control_request", request_id: "h", request: { subtype: "hook_callback" } })
    expect(written.pop().response.subtype).toBe("error")
  })

  test("a user message puts mentioned entities first", () => {
    const { p } = record()
    p.sessionId = sid
    const m = JSON.parse(
      p.userMessage({ text: "Summarise it", context: [{ uri: "note://1", title: "Plan", text: "Ship it" }] }),
    )
    expect(m.message.content).toEqual([
      { type: "text", text: '<context uri="note://1" title="Plan">\nShip it\n</context>' },
      { type: "text", text: "Summarise it" },
    ])
    expect(m.session_id).toBe(sid)
  })
})

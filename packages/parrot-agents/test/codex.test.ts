// Codex's app-server protocol into session items, on lines shaped as CLI 0.167.1 sent them (recorded with
// a probe client: a turn with a command that needed approval), and its rollout file for the TUI driver.
import { describe, expect, test } from "bun:test"
import { CodexProtocol, shortCommand } from "../src/codex"
import { CodexTranscript, threadItem } from "../src/codex-transcript"

function reporter() {
  const log: any[] = []
  return {
    log,
    out: {
      item: (i: any) => log.push(["item", i]),
      delta: (id: string, text: string, field?: string) => log.push(["delta", id, text, field]),
      patch: (id: string, p: any) => log.push(["patch", id, p]),
      state: (s: string) => log.push(["state", s]),
      meta: (m: any) => log.push(["meta", m]),
      turnEnd: (e?: string) => log.push(["turnEnd", e]),
    },
  }
}

const T = "01a1de18-b7f0-78a0-aa1a-9082ae1a8fe6"
const U = "01a1de18-b809-7723-900b-c585bea1163d"
const lines = [
  { method: "turn/started", params: { threadId: T, turn: { id: U, items: [], status: "inProgress" } } },
  {
    method: "item/started",
    params: { item: { type: "userMessage", id: "u1", content: [] }, threadId: T, turnId: U },
  },
  {
    method: "item/started",
    params: {
      item: { type: "agentMessage", id: "msg_1", text: "", phase: "commentary" },
      threadId: T,
      turnId: U,
    },
  },
  { method: "item/agentMessage/delta", params: { threadId: T, turnId: U, itemId: "msg_1", delta: "I’ll" } },
  {
    method: "item/agentMessage/delta",
    params: { threadId: T, turnId: U, itemId: "msg_1", delta: " write it." },
  },
  {
    method: "item/completed",
    params: {
      item: { type: "agentMessage", id: "msg_1", text: "I’ll write it.", phase: "commentary" },
      threadId: T,
      turnId: U,
    },
  },
  {
    method: "item/started",
    params: { item: { type: "reasoning", id: "rs_1", summary: [], content: [] }, threadId: T, turnId: U },
  },
  {
    method: "item/completed",
    params: { item: { type: "reasoning", id: "rs_1", summary: [], content: [] }, threadId: T, turnId: U },
  },
  {
    method: "thread/status/changed",
    params: { threadId: T, status: { type: "active", activeFlags: ["waitingOnApproval"] } },
  },
  {
    method: "item/started",
    params: {
      item: {
        type: "commandExecution",
        id: "exec-5ff3",
        command: "/bin/bash -lc \"printf 'live' > live.txt\"",
        cwd: "/w",
        processId: null,
        source: "agent",
        status: "inProgress",
        commandActions: [],
        aggregatedOutput: null,
        exitCode: null,
      },
      threadId: T,
      turnId: U,
    },
  },
  {
    method: "item/commandExecution/requestApproval",
    id: 0,
    params: {
      kind: "command",
      threadId: T,
      turnId: U,
      itemId: "exec-5ff3",
      startedAtMs: 1,
      environmentId: "local",
      reason: "Do you want to allow writing live.txt in the workspace outside the sandbox?",
      command: "/bin/bash -lc \"printf 'live' > live.txt\"",
      cwd: "/w",
      availableDecisions: [
        "accept",
        { acceptWithExecpolicyAmendment: { execpolicy_amendment: [] } },
        "cancel",
      ],
    },
  },
]

describe("CodexProtocol", () => {
  test("a turn: streamed text, an empty reasoning item dropped, a command approval", () => {
    const r = reporter()
    const written: string[] = []
    const p = new CodexProtocol(r.out as any, (l) => written.push(l), {} as any, "/w")
    for (const l of lines) p.message(l)
    const items = r.log.filter((x) => x[0] === "item").map((x) => x[1])
    expect(items.map((i) => [i.kind, i.id])).toEqual([
      ["assistant", "msg_1"],
      ["assistant", "msg_1"],
      ["tool", "exec-5ff3"],
      ["approval", "codex-0-exec-5ff3"],
    ])
    expect(
      r.log
        .filter((x) => x[0] === "delta")
        .map((x) => x[2])
        .join(""),
    ).toBe("I’ll write it.")
    const a = items[3]
    expect(a.tool).toBe("Bash")
    expect(a.items).toEqual(["printf 'live' > live.txt"])
    expect(a.canAllowSession).toBe(false) // acceptForSession was not offered
    expect(p.answer("codex-0-exec-5ff3", { action: "allow" })).toBe(true)
    expect(JSON.parse(written.at(-1)!)).toEqual({ id: 0, result: { decision: "accept" } })
    p.message({
      method: "item/completed",
      params: {
        item: {
          type: "commandExecution",
          id: "exec-5ff3",
          command: "x",
          status: "completed",
          aggregatedOutput: "",
          exitCode: 0,
        },
        threadId: T,
        turnId: U,
      },
    })
    p.message({
      method: "thread/tokenUsage/updated",
      params: {
        threadId: T,
        turnId: U,
        tokenUsage: {
          total: { inputTokens: 53660, cachedInputTokens: 51200, outputTokens: 223 },
          last: { inputTokens: 18305, outputTokens: 100 },
          modelContextWindow: 258400,
        },
      },
    })
    p.message({
      method: "turn/completed",
      params: { threadId: T, turn: { id: U, status: "completed", error: null } },
    })
    expect(r.log.find((x) => x[0] === "patch" && x[1] === "exec-5ff3")![2]).toEqual({
      status: "ok",
      output: "",
    })
    expect(r.log.findLast((x) => x[0] === "meta")![1].usage).toMatchObject({
      inputTokens: 53660,
      contextWindow: 258400,
      contextTokens: 18405,
    })
    expect(r.log.at(-1)).toEqual(["turnEnd", undefined])
  })

  test("a denial, and a question from request_user_input", () => {
    const r = reporter()
    const written: string[] = []
    const p = new CodexProtocol(r.out as any, (l) => written.push(l), {} as any, "/w")
    p.message({
      method: "item/fileChange/requestApproval",
      id: 4,
      params: { threadId: T, turnId: U, itemId: "patch-1", startedAtMs: 1 },
    })
    p.answer("codex-4-patch-1", { action: "deny" })
    expect(JSON.parse(written.at(-1)!).result).toEqual({ decision: "decline" })
    p.message({
      method: "item/tool/requestUserInput",
      id: 5,
      params: {
        threadId: T,
        turnId: U,
        itemId: "q",
        questions: [
          {
            id: "q1",
            header: "H",
            question: "Tea?",
            isOther: true,
            isSecret: false,
            options: [{ label: "Tea", description: "" }],
          },
        ],
        isBlocking: true,
      },
    })
    p.answer("codex-5-q", { answers: { "Tea?": "Tea" } })
    expect(JSON.parse(written.at(-1)!)).toEqual({ id: 5, result: { answers: { q1: { answers: ["Tea"] } } } })
  })

  test("a failed turn ends with its error; open requests are withdrawn", () => {
    const r = reporter()
    const p = new CodexProtocol(r.out as any, () => {}, {} as any, "/w")
    p.message({
      method: "item/commandExecution/requestApproval",
      id: 1,
      params: { threadId: T, turnId: U, itemId: "e", command: "ls" },
    })
    p.message({
      method: "turn/completed",
      params: { threadId: T, turn: { id: U, status: "failed", error: { message: "Overloaded" } } },
    })
    expect(r.log).toContainEqual(["turnEnd", "Overloaded"])
    expect(r.log).toContainEqual(["patch", "codex-1-e", { status: "cancelled" }])
  })

  test("shell wrappers are taken off commands", () => {
    expect(shortCommand("/bin/bash -lc 'echo hi'")).toBe("echo hi")
    expect(shortCommand(`/bin/bash -lc "printf 'a' > b"`)).toBe("printf 'a' > b")
    expect(shortCommand("ls -la")).toBe("ls -la")
  })
})

describe("the rollout, for the TUI driver", () => {
  test("core items become app-server items; a prompt and the end of a turn are reported", () => {
    const r = reporter()
    const t = new CodexTranscript(r.out as any, {} as any, "/w")
    const ev = (payload: any) => JSON.stringify({ timestamp: "t", type: "event_msg", payload })
    expect(t.line(JSON.stringify({ type: "session_meta", payload: { id: T, cwd: "/w" } }))).toEqual({
      cliSession: T,
    })
    expect(
      t.line(
        ev({
          type: "item_completed",
          item: { type: "UserMessage", id: "u", content: [{ type: "text", text: "hi there" }] },
        }),
      ),
    ).toEqual({ user: "hi there" })
    t.line(
      ev({
        type: "item_completed",
        item: {
          type: "AgentMessage",
          id: "m",
          content: [{ type: "Text", text: "Hello" }],
          phase: "final_answer",
        },
      }),
    )
    t.line(
      ev({
        type: "item_completed",
        item: {
          type: "CommandExecution",
          id: "x",
          command: ["/bin/bash", "-lc", "ls"],
          status: "completed",
          aggregated_output: "a\n",
          exit_code: 0,
        },
      }),
    )
    expect(t.line(ev({ type: "task_complete", turn_id: U }))).toEqual({ turnDone: true })
    const items = r.log.filter((x) => x[0] === "item").map((x) => x[1])
    expect(items).toEqual([
      { id: "m", kind: "assistant", text: "Hello", streaming: false },
      {
        id: "x",
        kind: "tool",
        source: "harness",
        name: "Bash",
        title: "ls",
        input: { command: "ls", cwd: undefined },
        status: "ok",
        output: "a\n",
      },
    ])
    expect(
      threadItem({ type: "FileChange", id: "f", changes: { "a.txt": { type: "add", unified_diff: "+x" } } })
        .changes,
    ).toEqual([{ path: "a.txt", diff: "+x" }])
  })
})

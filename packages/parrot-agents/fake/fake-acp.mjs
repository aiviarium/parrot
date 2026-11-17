#!/usr/bin/env node
// A fake ACP agent for tests: it speaks the Agent Client Protocol v1 over stdio in the shapes of the
// published schema (@agentclientprotocol/sdk 2.6.0) and of what `claude-agent-acp` 0.107.0 sends, and
// does what the message says. Plain Node (18+, no dependencies).
//
//   [[text:Hello]]  [[slow:40]]  [[think:...]]  [[tool:notes_search {"query":"x"}]] (a tool of the MCP server the session was given)
//   [[bash:touch a.txt]] (asks with session/request_permission)  [[plan:One|Two]]  [[sleep:2000]]
//   [[recall]]  [[crash]]  [[refuse]]
import { execSync } from "node:child_process"
import { randomUUID } from "node:crypto"
import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

const home = process.env.FAKE_ACP_HOME ?? join(tmpdir(), "fake-acp")
const out = (m) => process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", ...m })}\n`)
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
let session = null // { id, cwd, said, mcp, mode, model }
let turn = null
let reqId = 0
const waiting = new Map()
const update = (u) => out({ method: "session/update", params: { sessionId: session.id, update: u } })
const ask = (method, params) => {
  const id = `agent-${++reqId}`
  out({ id, method, params })
  return new Promise((r) => waiting.set(id, r))
}
const save = () => {
  mkdirSync(home, { recursive: true })
  writeFileSync(join(home, `${session.id}.json`), JSON.stringify({ said: session.said, cwd: session.cwd }))
}
const MODES = [
  { id: "default", name: "Manual", description: "Always ask before making changes" },
  { id: "bypassPermissions", name: "Bypass permissions", description: "Accepts all permissions" },
]
const config = () => [
  {
    id: "model",
    name: "Model",
    category: "model",
    type: "select",
    currentValue: session.model,
    options: [
      { value: "fake-small", name: "Fake small", description: "Quick" },
      { value: "fake-large", name: "Fake large" },
    ],
  },
]

async function stream(text, delay) {
  const messageId = `msg_${randomUUID().slice(0, 8)}`
  for (const w of text.split(/(?<= )/)) {
    if (turn?.cancelled) break
    update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: w }, messageId })
    if (delay) await sleep(delay)
  }
  return text
}

async function mcpTool(name, args) {
  const s = session.mcp.find((m) => m.type === "http")
  if (!s) throw new Error("no MCP server")
  const headers = { "content-type": "application/json", accept: "application/json, text/event-stream" }
  for (const h of s.headers ?? []) headers[h.name.toLowerCase()] = h.value
  const post = async (id, method, params) => {
    const r = await fetch(s.url, {
      method: "POST",
      headers,
      body: JSON.stringify({ jsonrpc: "2.0", id, method, params }),
    })
    const b = await r.json()
    if (b.error) throw new Error(b.error.message)
    return b.result
  }
  await post(1, "initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "fake-acp", version: "1" },
  })
  return post(2, "tools/call", { name, arguments: args })
}

async function prompt(id, params) {
  const text = (params.prompt ?? [])
    .map((b) => (b.type === "text" ? b.text : b.type === "resource" ? `[${b.resource?.uri}]` : `[${b.type}]`))
    .join("\n")
  turn = { cancelled: false }
  session.said.push(text)
  save()
  let delay = 0
  let stop = "end_turn"
  const ds = [...text.matchAll(/\[\[(\w+)(?::([\s\S]*?))?\]\]/g)]
  if (!ds.length) await stream(`You said: ${text}`, delay)
  for (const [, op, a = ""] of ds) {
    if (turn.cancelled) break
    switch (op) {
      case "text":
        await stream(a, delay)
        break
      case "slow":
        delay = Number(a) || 0
        break
      case "think":
        update({ sessionUpdate: "agent_thought_chunk", content: { type: "text", text: a } })
        break
      case "tool": {
        const sp = a.indexOf(" ")
        const name = sp < 0 ? a : a.slice(0, sp)
        const args = sp < 0 ? {} : JSON.parse(a.slice(sp + 1))
        const toolCallId = `toolu_${randomUUID().slice(0, 8)}`
        update({
          sessionUpdate: "tool_call",
          toolCallId,
          title: `parrot: ${name}`,
          kind: "other",
          status: "in_progress",
          rawInput: args,
        })
        try {
          const r = await mcpTool(name, args)
          const t = (r.content ?? []).map((c) => c.text ?? "").join("\n")
          update({
            sessionUpdate: "tool_call_update",
            toolCallId,
            status: r.isError ? "failed" : "completed",
            content: [{ type: "content", content: { type: "text", text: t } }],
          })
          await stream(`The tool said: ${t.slice(0, 300)}`, delay)
        } catch (e) {
          update({
            sessionUpdate: "tool_call_update",
            toolCallId,
            status: "failed",
            content: [{ type: "content", content: { type: "text", text: String(e.message) } }],
          })
        }
        break
      }
      case "bash": {
        const toolCallId = `toolu_${randomUUID().slice(0, 8)}`
        const call = {
          toolCallId,
          title: `\`${a}\``,
          kind: "execute",
          status: "pending",
          rawInput: { command: a },
        }
        update({ sessionUpdate: "tool_call", ...call })
        let allowed = session.mode === "bypassPermissions" || session.always.has(a)
        if (!allowed) {
          const r = await ask("session/request_permission", {
            sessionId: session.id,
            toolCall: call,
            options: [
              { optionId: "allow_always", name: "Always Allow", kind: "allow_always" },
              { optionId: "allow", name: "Allow", kind: "allow_once" },
              { optionId: "reject", name: "Reject", kind: "reject_once" },
            ],
          })
          if (r?.outcome?.outcome === "cancelled") break
          allowed = r?.outcome?.optionId !== "reject"
          if (r?.outcome?.optionId === "allow_always") session.always.add(a)
        }
        if (allowed) {
          let output = ""
          try {
            output = execSync(a, { cwd: session.cwd, encoding: "utf8" })
          } catch (e) {
            output = String(e.message)
          }
          update({
            sessionUpdate: "tool_call_update",
            toolCallId,
            status: "completed",
            content: [{ type: "content", content: { type: "text", text: output || "(no output)" } }],
          })
          await stream("Ran it.", delay)
        } else {
          update({
            sessionUpdate: "tool_call_update",
            toolCallId,
            status: "failed",
            content: [{ type: "content", content: { type: "text", text: "The user rejected it." } }],
          })
          await stream("I did not run it.", delay)
        }
        break
      }
      case "plan":
        update({
          sessionUpdate: "plan",
          entries: a
            .split("|")
            .map((content, i) => ({ content, priority: "medium", status: i ? "pending" : "in_progress" })),
        })
        break
      case "sleep": {
        const until = Date.now() + (Number(a) || 1000)
        while (Date.now() < until && !turn.cancelled) await sleep(25)
        break
      }
      case "recall":
        await stream(`Before this you said: ${session.said.slice(0, -1).join(" / ") || "nothing"}`, delay)
        break
      case "crash":
        process.stderr.write("fake-acp: crashing as asked\n")
        process.exit(3)
        break
      case "refuse":
        stop = "refusal"
        break
    }
  }
  update({
    sessionUpdate: "usage_update",
    used: 1234,
    size: 200000,
    cost: { amount: 0.001, currency: "USD" },
  })
  const cancelled = turn.cancelled
  turn = null
  out({
    id,
    result: {
      stopReason: cancelled ? "cancelled" : stop,
      usage: {
        inputTokens: 10,
        outputTokens: 20,
        cachedReadTokens: 5,
        cachedWriteTokens: 0,
        totalTokens: 35,
      },
    },
  })
}

const methods = {
  initialize: () => ({
    protocolVersion: 1,
    agentCapabilities: {
      loadSession: true,
      promptCapabilities: { image: true, embeddedContext: true },
      mcpCapabilities: { http: true, sse: false },
      sessionCapabilities: { resume: {} },
    },
    agentInfo: { name: "fake-acp", title: "Fake ACP agent", version: "0.0.0" },
    authMethods: [],
  }),
  "session/new": (p) => {
    session = {
      id: randomUUID(),
      cwd: p.cwd,
      said: [],
      mcp: p.mcpServers ?? [],
      mode: "default",
      model: "fake-small",
      always: new Set(),
    }
    save()
    setTimeout(
      () =>
        update({
          sessionUpdate: "available_commands_update",
          availableCommands: [{ name: "review", description: "Review the changes", input: null }],
        }),
      5,
    )
    return {
      sessionId: session.id,
      modes: { currentModeId: session.mode, availableModes: MODES },
      configOptions: config(),
    }
  },
  "session/resume": (p) => {
    let saved
    try {
      saved = JSON.parse(readFileSync(join(home, `${p.sessionId}.json`), "utf8"))
    } catch {
      throw { code: -32002, message: `no session ${p.sessionId}` }
    }
    session = {
      id: p.sessionId,
      cwd: p.cwd,
      said: saved.said,
      mcp: p.mcpServers ?? [],
      mode: "default",
      model: "fake-small",
      always: new Set(),
    }
    return { modes: { currentModeId: session.mode, availableModes: MODES }, configOptions: config() }
  },
  "session/set_mode": (p) => {
    session.mode = p.modeId
    return {}
  },
  "session/set_config_option": (p) => {
    if (p.configId === "model") session.model = p.value
    return { configOptions: config() }
  },
}

let buf = ""
process.stdin.on("data", (d) => {
  buf += d
  for (let i = buf.indexOf("\n"); i >= 0; i = buf.indexOf("\n")) {
    const line = buf.slice(0, i)
    buf = buf.slice(i + 1)
    if (!line.trim()) continue
    let m
    try {
      m = JSON.parse(line)
    } catch {
      continue
    }
    if (m.method === "session/prompt") {
      if (turn) out({ id: m.id, error: { code: -32600, message: "a prompt is running" } })
      else prompt(m.id, m.params ?? {})
    } else if (m.method === "session/cancel") {
      if (turn) turn.cancelled = true
    } else if (m.method && m.id != null) {
      const f = methods[m.method]
      if (!f) out({ id: m.id, error: { code: -32601, message: `Method not found: ${m.method}` } })
      else {
        try {
          out({ id: m.id, result: f(m.params ?? {}) })
        } catch (e) {
          out({ id: m.id, error: { code: e.code ?? -32603, message: e.message ?? String(e) } })
        }
      }
    } else if (m.id != null && !m.method) {
      const w = waiting.get(m.id)
      if (w) {
        waiting.delete(m.id)
        w(m.result)
      }
    }
  }
})
process.stdin.on("end", () => process.exit(0))

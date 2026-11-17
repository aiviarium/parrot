#!/usr/bin/env node
// A fake Pi in RPC mode for tests, in the shapes of Pi 1.5.1's docs (docs/rpc.md, rpc-commands.md,
// json.md, rpc-extension-ui.md): `fake-pi.mjs --mode rpc --session-id <id>`. It reads the MCP server
// from `$PI_CODING_AGENT_DIR/mcp.json` as Pi does. Plain Node (18+, no dependencies).
//
//   [[text:Hello]]  [[slow:40]]  [[think:...]]  [[tool:notes_search {"query":"x"}]]  [[bash:echo hi]]
//   [[confirm:Run the migration?]] (an extension's confirm dialog)  [[select:Pick one|A|B]]
//   [[sleep:2000]]  [[recall]]  [[crash]]  [[fail:Overloaded]]
import { execSync } from "node:child_process"
import { randomUUID } from "node:crypto"
import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

const args = process.argv.slice(2)
const arg = (n) => {
  const i = args.indexOf(n)
  return i >= 0 ? args[i + 1] : undefined
}
const sessionId = arg("--session-id") ?? randomUUID()
let model = arg("--model") ?? "fake/fake-1"
let thinking = arg("--thinking") ?? "off"
const home = process.env.FAKE_PI_HOME ?? join(tmpdir(), "fake-pi")
const file = join(home, `${sessionId}.json`)
let said = []
try {
  said = JSON.parse(readFileSync(file, "utf8"))
} catch {
  process.stderr.write(
    `Warning: No project session found with id '${sessionId}'; creating a new session with that id.\n`,
  )
}
const save = () => {
  mkdirSync(home, { recursive: true })
  writeFileSync(file, JSON.stringify(said))
}
const out = (m) => process.stdout.write(`${JSON.stringify(m)}\n`)
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
let run = null // { aborted }
const dialogs = new Map()

function mcpServer() {
  try {
    const cfg = JSON.parse(readFileSync(join(process.env.PI_CODING_AGENT_DIR ?? "", "mcp.json"), "utf8"))
    const [name, s] = Object.entries(cfg.mcpServers ?? {}).find(([, v]) => v.url) ?? []
    if (!s) return null
    const headers = {}
    for (const [k, v] of Object.entries(s.headers ?? {}))
      headers[k] = String(v).replace(/\$\{(\w+)\}/g, (_, n) => process.env[n] ?? "")
    return { name, url: s.url, headers }
  } catch {
    return null
  }
}

async function assistantText(text, delay) {
  out({ type: "message_start", message: { role: "assistant", content: [], stopReason: "pending" } })
  out({ type: "message_update", usage: {}, assistantMessageEvent: { type: "text_start", contentIndex: 0 } })
  for (const w of text.split(/(?<= )/)) {
    if (run?.aborted) break
    out({
      type: "message_update",
      usage: {},
      assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta: w },
    })
    if (delay) await sleep(delay)
  }
  out({
    type: "message_update",
    usage: {},
    assistantMessageEvent: { type: "text_end", contentIndex: 0, content: text },
  })
  out({
    type: "message_end",
    message: {
      role: "assistant",
      content: [{ type: "text", text }],
      stopReason: "stop",
      usage: { input: 10, output: 5, cacheRead: 0, cacheWrite: 0, totalTokens: 15, cost: { total: 0.0001 } },
    },
  })
}

function dialog(method, fields) {
  const id = randomUUID()
  out({ type: "extension_ui_request", id, method, ...fields })
  return new Promise((r) => dialogs.set(id, r))
}

async function tool(name, args, exec) {
  const toolCallId = `call_${randomUUID().slice(0, 8)}`
  out({ type: "tool_execution_start", toolCallId, toolName: name, args })
  let result
  let isError = false
  try {
    result = { content: [{ type: "text", text: await exec() }], details: {} }
  } catch (e) {
    result = { content: [{ type: "text", text: String(e.message ?? e) }], details: {} }
    isError = true
  }
  out({ type: "tool_execution_end", toolCallId, toolName: name, result, isError, durationMs: 1 })
  return { text: result.content[0].text, isError }
}

async function prompt(text) {
  run = { aborted: false }
  said.push(text)
  save()
  out({ type: "agent_start" })
  out({ type: "turn_start" })
  out({ type: "message_start", message: { role: "user", content: text } })
  out({ type: "message_end", message: { role: "user", content: text } })
  let delay = 0
  let error = null
  const ds = [...text.matchAll(/\[\[(\w+)(?::([\s\S]*?))?\]\]/g)]
  if (!ds.length) await assistantText(`You said: ${text}`, delay)
  for (const [, op, a = ""] of ds) {
    if (run.aborted || error) break
    switch (op) {
      case "text":
        await assistantText(a, delay)
        break
      case "slow":
        delay = Number(a) || 0
        break
      case "think":
        out({
          type: "message_update",
          usage: {},
          assistantMessageEvent: { type: "thinking_start", contentIndex: 0 },
        })
        out({
          type: "message_update",
          usage: {},
          assistantMessageEvent: { type: "thinking_end", contentIndex: 0, content: a },
        })
        break
      case "tool": {
        const sp = a.indexOf(" ")
        const name = sp < 0 ? a : a.slice(0, sp)
        const input = sp < 0 ? {} : JSON.parse(a.slice(sp + 1))
        const s = mcpServer()
        const r = await tool(`mcp__${s?.name ?? "parrot"}__${name}`, input, async () => {
          if (!s) throw new Error("no MCP server in mcp.json")
          const headers = {
            "content-type": "application/json",
            accept: "application/json, text/event-stream",
            ...s.headers,
          }
          const post = async (id, method, params) => {
            const res = await fetch(s.url, {
              method: "POST",
              headers,
              body: JSON.stringify({ jsonrpc: "2.0", id, method, params }),
            })
            const b = await res.json()
            if (b.error) throw new Error(b.error.message)
            return b.result
          }
          await post(1, "initialize", {
            protocolVersion: "2025-06-18",
            capabilities: {},
            clientInfo: { name: "fake-pi", version: "1" },
          })
          const res = await post(2, "tools/call", { name, arguments: input })
          return (res.content ?? []).map((c) => c.text ?? "").join("\n")
        })
        await assistantText(
          r.isError ? `The tool failed: ${r.text}` : `The tool said: ${r.text.slice(0, 300)}`,
          delay,
        )
        break
      }
      case "bash":
        await tool("bash", { command: a }, async () => execSync(a, { cwd: process.cwd(), encoding: "utf8" }))
        await assistantText("Ran it.", delay)
        break
      case "confirm": {
        const r = await dialog("confirm", { title: a, message: "An extension asks first" })
        if (run.aborted) break
        await assistantText(r?.confirmed ? "Confirmed." : "Not confirmed.", delay)
        break
      }
      case "select": {
        const [title, ...options] = a.split("|")
        const r = await dialog("select", { title, options })
        if (run.aborted) break
        await assistantText(r?.cancelled ? "Nothing picked." : `You picked ${r?.value}.`, delay)
        break
      }
      case "sleep": {
        const until = Date.now() + (Number(a) || 1000)
        while (Date.now() < until && !run.aborted) await sleep(25)
        break
      }
      case "recall":
        await assistantText(`Before this you said: ${said.slice(0, -1).join(" / ") || "nothing"}`, delay)
        break
      case "crash":
        process.stderr.write("fake-pi: crashing as asked\n")
        process.exit(3)
        break
      case "fail":
        error = a || "failed"
        out({
          type: "message_end",
          message: { role: "assistant", content: [], stopReason: "error", errorMessage: error },
        })
        break
    }
  }
  out({ type: "agent_end", messages: [], willRetry: false })
  const aborted = run.aborted
  run = null
  out({ type: "agent_settled", aborted })
}

const handlers = {
  get_state: () => ({
    model: { id: model.split("/")[1], name: "Fake 1", provider: model.split("/")[0] },
    thinkingLevel: thinking,
    isStreaming: !!run,
    sessionId,
    sessionFile: file,
    messageCount: said.length,
  }),
  get_available_models: () => ({
    models: [
      {
        id: "fake-1",
        name: "Fake 1",
        provider: "fake",
        reasoning: true,
        input: ["text", "image"],
        contextWindow: 100000,
      },
      {
        id: "fake-2",
        name: "Fake 2",
        provider: "fake",
        reasoning: false,
        input: ["text"],
        contextWindow: 50000,
      },
    ],
  }),
  get_available_thinking_levels: () => ({ levels: ["off", "low", "medium", "high"] }),
  get_commands: () => ({
    commands: [
      { name: "review", description: "Review the changes", source: "prompt" },
      { name: "skill:fake-skill", description: "A skill the fake offers", source: "skill" },
    ],
  }),
  set_model: (m) => {
    model = `${m.provider}/${m.modelId}`
    return { id: m.modelId, provider: m.provider, name: m.modelId }
  },
  set_thinking_level: (m) => {
    thinking = m.level
    return undefined
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
      out({ type: "response", command: "parse", success: false, error: "Failed to parse command" })
      continue
    }
    const reply = (fields) =>
      out({ ...(m.id ? { id: m.id } : {}), type: "response", command: m.type, ...fields })
    if (m.type === "extension_ui_response") {
      const r = dialogs.get(m.id)
      if (r) {
        dialogs.delete(m.id)
        r(m)
      }
    } else if (m.type === "prompt") {
      if (run && !m.streamingBehavior)
        reply({ success: false, error: "The agent is streaming; set streamingBehavior" })
      else {
        reply({ success: true, data: { disposition: run ? "queued" : "started" } })
        if (!run) prompt(m.message)
      }
    } else if (m.type === "steer") {
      reply({ success: true, data: { disposition: "queued" } })
      if (run) said.push(m.message)
    } else if (m.type === "abort") {
      if (run) run.aborted = true
      for (const [id, r] of dialogs) {
        dialogs.delete(id)
        r({ cancelled: true })
      }
      const wait = async () => {
        while (run) await sleep(10)
        reply({ success: true })
      }
      wait()
    } else if (handlers[m.type]) {
      reply({ success: true, data: handlers[m.type](m) })
    } else reply({ success: false, error: `Unknown command: ${m.type}` })
  }
})
process.stdin.on("end", async () => {
  while (run) await sleep(10)
  process.exit(0)
})

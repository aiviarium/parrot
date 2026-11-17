// The stream-json half of the fake Claude Code (see ./fake-claude.mjs): it speaks the CLI's stream-json
// protocol (the shapes the real CLI 2.1.325 sends, see ../../src/claude.ts) and does what the message tells
// it, so tests are deterministic and need no model. Plain Node (18+, no dependencies).
//
// A message is read as directives in double brackets; text outside them is ignored. Without any, the
// answer is "You said: <message>".
//
//   [[text:Hello there]]           streams the text, word by word
//   [[slow:40]]                    waits 40 ms between streamed words from here on
//   [[think:Considering…]]         a thinking block
//   [[tool:notes_search {"query":"x"}]]  calls a tool of the MCP server in --mcp-config
//   [[bash:touch a.txt]]           runs a command (asks first unless the mode allows it)
//   [[write:path/file.txt:content]] writes a file (asks first in the default and plan modes)
//   [[question:Tea or coffee?|Tea|Coffee]]  asks with AskUserQuestion, then says what was picked
//   [[plan:1. Do this\n2. Then that]]  proposes a plan with ExitPlanMode
//   [[sleep:2000]]                 waits (an interrupt ends the turn)
//   [[recall]]                     says what was said before in this CLI session (to test resume)
//   [[crash]]                      exits with status 3 in the middle of the turn
//   [[fail:Overloaded]]            ends the turn with an error result
import { execSync } from "node:child_process"
import { randomUUID } from "node:crypto"
import { appendFileSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { homedir, tmpdir } from "node:os"
import { dirname, join, resolve } from "node:path"
import { createInterface } from "node:readline"

const args = process.argv.slice(2)
const arg = (name) => {
  const i = args.indexOf(name)
  return i >= 0 ? args[i + 1] : undefined
}
const resumed = arg("--resume")
const sessionId = resumed ?? arg("--session-id") ?? randomUUID()
let mode = arg("--permission-mode") ?? "default"
let model = arg("--model") ?? "fake-default"
const mcpConfig = (() => {
  try {
    return JSON.parse(arg("--mcp-config") ?? "{}")
  } catch {
    return {}
  }
})()

const home = process.env.FAKE_CLAUDE_HOME ?? join(tmpdir(), "fake-claude")
const transcriptFile = join(home, `${sessionId}.json`)
let transcript = []
if (resumed) {
  try {
    transcript = JSON.parse(readFileSync(transcriptFile, "utf8"))
  } catch {
    process.stderr.write(`No conversation found with session ID: ${resumed}\n`)
    process.exit(1)
  }
}
function saveTranscript() {
  mkdirSync(home, { recursive: true })
  writeFileSync(transcriptFile, JSON.stringify(transcript))
}

// The JSONL transcript the real CLI writes in either mode (the terminal half's reader follows it there).
const jsonl = join(
  process.env.CLAUDE_CONFIG_DIR ?? join(homedir(), ".claude"),
  "projects",
  process.cwd().replace(/[^a-zA-Z0-9]/g, "-"),
  `${sessionId}.jsonl`,
)
let parentUuid = null
function record(entry) {
  const uuid = randomUUID()
  // Best effort: a host where the config folder cannot be written still runs turns.
  try {
    mkdirSync(dirname(jsonl), { recursive: true })
    appendFileSync(
      jsonl,
      `${JSON.stringify({ parentUuid, isSidechain: false, sessionId, cwd: process.cwd(), uuid, timestamp: new Date().toISOString(), ...entry })}\n`,
    )
    parentUuid = uuid
  } catch {}
}

const out = (m) => process.stdout.write(`${JSON.stringify(m)}\n`)
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
let turn = null // { interrupted }
const waiting = new Map() // our control requests to the host
const queue = []
let busy = false
let msgCounter = 0

const commands = [
  { name: "review", description: "Review the changes on this branch", argumentHint: "" },
  { name: "fake-skill", description: "A skill the fake harness offers", argumentHint: "" },
]

function init() {
  out({
    type: "system",
    subtype: "init",
    cwd: process.cwd(),
    session_id: sessionId,
    tools: ["Bash", "Write", "AskUserQuestion", "ExitPlanMode"],
    mcp_servers: Object.keys(mcpConfig.mcpServers ?? {}).map((name) => ({ name, status: "connected" })),
    model,
    permissionMode: mode,
    slash_commands: commands.map((c) => c.name),
    skills: ["fake-skill"],
    claude_code_version: "fake",
  })
}

async function ask(tool, withUse) {
  const { __toolUse, ...input } = withUse
  const id = randomUUID()
  out({
    type: "control_request",
    request_id: id,
    request: {
      subtype: "can_use_tool",
      tool_name: tool,
      display_name: tool,
      input,
      tool_use_id: __toolUse,
      permission_suggestions: [{ type: "setMode", mode: "acceptEdits", destination: "session" }],
    },
  })
  return new Promise((r) => waiting.set(id, r))
}

function newMessage() {
  const id = `msg_fake_${++msgCounter}_${Date.now()}`
  out({
    type: "stream_event",
    event: { type: "message_start", message: { id, role: "assistant", content: [] } },
    session_id: sessionId,
    parent_tool_use_id: null,
  })
  return id
}

async function streamText(text, delay) {
  const id = newMessage()
  out({
    type: "stream_event",
    event: { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
    session_id: sessionId,
    parent_tool_use_id: null,
  })
  const words = text.split(/(?<= )/)
  for (const w of words) {
    if (turn?.interrupted) break
    out({
      type: "stream_event",
      event: { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: w } },
      session_id: sessionId,
      parent_tool_use_id: null,
    })
    if (delay) await sleep(delay)
  }
  out({
    type: "assistant",
    message: {
      id,
      role: "assistant",
      content: [{ type: "text", text }],
      usage: { input_tokens: 10, output_tokens: words.length },
    },
    session_id: sessionId,
    parent_tool_use_id: null,
  })
  record({
    type: "assistant",
    message: { id, role: "assistant", content: [{ type: "text", text }], stop_reason: null },
  })
  out({
    type: "stream_event",
    event: { type: "content_block_stop", index: 0 },
    session_id: sessionId,
    parent_tool_use_id: null,
  })
  return text
}

function toolUse(name, input) {
  const id = `toolu_fake_${randomUUID().slice(0, 8)}`
  const msg = newMessage()
  out({
    type: "stream_event",
    event: {
      type: "content_block_start",
      index: 0,
      content_block: { type: "tool_use", id, name, input: {} },
    },
    session_id: sessionId,
    parent_tool_use_id: null,
  })
  out({
    type: "assistant",
    message: { id: msg, role: "assistant", content: [{ type: "tool_use", id, name, input }] },
    session_id: sessionId,
    parent_tool_use_id: null,
  })
  return id
}

function toolResult(id, content, isError = false) {
  out({
    type: "user",
    message: {
      role: "user",
      content: [{ type: "tool_result", tool_use_id: id, content, is_error: isError }],
    },
    session_id: sessionId,
    parent_tool_use_id: null,
  })
}

const mcpSession = { initialized: false, id: 0 }
async function mcp(method, params) {
  const [name, server] = Object.entries(mcpConfig.mcpServers ?? {})[0] ?? []
  if (!server) throw new Error("no MCP server configured")
  const headers = { "content-type": "application/json", accept: "application/json, text/event-stream" }
  for (const [k, v] of Object.entries(server.headers ?? {})) {
    headers[k] = String(v).replace(/\$\{(\w+)\}/g, (_, n) => process.env[n] ?? "")
  }
  const res = await fetch(server.url, {
    method: "POST",
    headers,
    body: JSON.stringify({ jsonrpc: "2.0", id: ++mcpSession.id, method, params }),
  })
  if (!res.ok) throw new Error(`${name}: HTTP ${res.status} ${await res.text()}`)
  const body = await res.json()
  if (body.error) throw new Error(`${name}: ${body.error.message}`)
  return body.result
}

async function callTool(name, input) {
  const useId = toolUse(`mcp__parrot__${name}`, input)
  try {
    if (!mcpSession.initialized) {
      await mcp("initialize", {
        protocolVersion: "2025-06-18",
        capabilities: {},
        clientInfo: { name: "fake-claude", version: "1" },
      })
      mcpSession.initialized = true
    }
    const r = await mcp("tools/call", { name, arguments: input })
    const text = (r.content ?? []).map((c) => c.text ?? "").join("\n")
    toolResult(useId, text, !!r.isError)
    return { text, isError: !!r.isError }
  } catch (e) {
    toolResult(useId, String(e.message ?? e), true)
    return { text: String(e.message ?? e), isError: true }
  }
}

function needsAsk(kind) {
  if (mode === "bypassPermissions") return false
  if (mode === "acceptEdits" && kind === "edit") return false
  return true
}

async function runTurn(text) {
  turn = { interrupted: false }
  init()
  transcript.push(text)
  saveTranscript()
  record({ type: "user", message: { role: "user", content: text } })
  const said = []
  let delay = 0
  const directives = [...text.matchAll(/\[\[(\w+)(?::([\s\S]*?))?\]\]/g)]
  if (!directives.length) said.push(await streamText(`You said: ${text}`, delay))
  for (const [, op, rawArg] of directives) {
    if (turn.interrupted) break
    const a = (rawArg ?? "").replace(/\\n/g, "\n")
    switch (op) {
      case "text":
        said.push(await streamText(a, delay))
        break
      case "slow":
        delay = Number(a) || 0
        break
      case "think": {
        const id = newMessage()
        out({
          type: "stream_event",
          event: { type: "content_block_start", index: 0, content_block: { type: "thinking", thinking: "" } },
          session_id: sessionId,
          parent_tool_use_id: null,
        })
        out({
          type: "stream_event",
          event: { type: "content_block_delta", index: 0, delta: { type: "thinking_delta", thinking: a } },
          session_id: sessionId,
          parent_tool_use_id: null,
        })
        out({
          type: "assistant",
          message: { id, role: "assistant", content: [{ type: "thinking", thinking: a }] },
          session_id: sessionId,
          parent_tool_use_id: null,
        })
        break
      }
      case "tool": {
        const sp = a.indexOf(" ")
        const name = sp < 0 ? a : a.slice(0, sp)
        const input = sp < 0 ? {} : JSON.parse(a.slice(sp + 1))
        const r = await callTool(name, input)
        said.push(
          await streamText(
            r.isError ? `The tool failed: ${r.text}` : `The tool said: ${r.text.slice(0, 400)}`,
            delay,
          ),
        )
        break
      }
      case "bash": {
        const input = { command: a, description: `Run ${a}` }
        const id = toolUse("Bash", input)
        const answer = needsAsk("bash")
          ? await ask("Bash", { ...input, __toolUse: id })
          : { behavior: "allow" }
        if (turn.interrupted) break
        if (answer.behavior === "allow") {
          let output = ""
          try {
            output = execSync(a, { cwd: process.cwd(), encoding: "utf8" })
          } catch (e) {
            output = String(e.message)
          }
          toolResult(id, output || "(no output)")
          said.push(await streamText("Ran it.", delay))
        } else {
          toolResult(id, `Permission denied: ${answer.message ?? ""}`, true)
          said.push(await streamText("I did not run it.", delay))
        }
        break
      }
      case "write": {
        const i = a.indexOf(":")
        const path = a.slice(0, i)
        const content = a.slice(i + 1)
        const input = { file_path: resolve(process.cwd(), path), content }
        const id = toolUse("Write", input)
        const answer = needsAsk("edit")
          ? await ask("Write", { ...input, __toolUse: id })
          : { behavior: "allow" }
        if (turn.interrupted) break
        if (answer.behavior === "allow") {
          mkdirSync(dirname(input.file_path), { recursive: true })
          writeFileSync(input.file_path, content.endsWith("\n") ? content : `${content}\n`)
          toolResult(id, `Wrote ${path}`)
          said.push(await streamText(`Wrote ${path}.`, delay))
        } else {
          toolResult(id, "Permission denied", true)
          said.push(await streamText(`I did not write ${path}.`, delay))
        }
        break
      }
      case "question": {
        const [q, ...options] = a.split("|")
        const input = {
          questions: [
            {
              question: q,
              header: "Question",
              multiSelect: false,
              options: options.map((label) => ({ label, description: `Pick ${label}` })),
            },
          ],
        }
        const id = toolUse("AskUserQuestion", input)
        const answer = await ask("AskUserQuestion", { ...input, __toolUse: id })
        if (turn.interrupted) break
        const picked = answer.updatedInput?.answers?.[q] ?? "nothing"
        toolResult(id, `"${q}"="${picked}"`)
        said.push(await streamText(`You picked ${picked}.`, delay))
        break
      }
      case "plan": {
        const id = toolUse("ExitPlanMode", { plan: a })
        const answer = await ask("ExitPlanMode", { plan: a, __toolUse: id })
        if (turn.interrupted) break
        if (answer.behavior === "allow") {
          mode = "default"
          toolResult(id, "The user approved the plan")
          said.push(await streamText("Starting on the plan.", delay))
        } else {
          toolResult(id, "The user wants to keep planning", true)
          said.push(await streamText("I will keep planning.", delay))
        }
        break
      }
      case "sleep": {
        const until = Date.now() + (Number(a) || 1000)
        while (Date.now() < until && !turn.interrupted) await sleep(25)
        break
      }
      case "recall":
        said.push(
          await streamText(
            `Before this you said: ${transcript.slice(0, -1).join(" / ") || "nothing"}`,
            delay,
          ),
        )
        break
      case "crash":
        process.stderr.write("fake-claude: crashing as asked\n")
        process.exit(3)
        break
      case "fail":
        out({
          type: "result",
          subtype: "error_during_execution",
          is_error: true,
          result: a || "failed",
          session_id: sessionId,
          usage: { input_tokens: 1, output_tokens: 1 },
          total_cost_usd: 0,
        })
        turn = null
        return
    }
  }
  const interrupted = turn.interrupted
  out({
    type: "result",
    subtype: interrupted ? "error_during_execution" : "success",
    is_error: interrupted,
    terminal_reason: interrupted ? "interrupted" : "completed",
    result: said.join(" "),
    session_id: sessionId,
    num_turns: 1,
    usage: {
      input_tokens: 12,
      output_tokens: 34,
      cache_read_input_tokens: 100,
      cache_creation_input_tokens: 5,
    },
    total_cost_usd: 0.0012,
    modelUsage: { [model]: { contextWindow: 200000 } },
  })
  turn = null
}

async function pump() {
  if (busy) return
  busy = true
  while (queue.length) await runTurn(queue.shift())
  busy = false
}

const rl = createInterface({ input: process.stdin })
rl.on("line", (line) => {
  if (!line.trim()) return
  let m
  try {
    m = JSON.parse(line)
  } catch {
    return
  }
  if (m.type === "control_request") {
    const r = m.request ?? {}
    const reply = (response) =>
      out({ type: "control_response", response: { subtype: "success", request_id: m.request_id, response } })
    if (r.subtype === "initialize") {
      reply({
        commands,
        models: [
          {
            value: "default",
            displayName: "Default",
            description: "The fake default",
            supportedEffortLevels: ["low", "high"],
          },
          { value: "fake-fast", displayName: "Fake fast", description: "Quicker" },
        ],
        current_permission_mode: mode,
        claude_code_version: "fake",
      })
    } else if (r.subtype === "interrupt") {
      if (turn) turn.interrupted = true
      for (const [id, resolve] of waiting) {
        out({ type: "control_cancel_request", request_id: id })
        resolve({ behavior: "deny", message: "interrupted" })
      }
      waiting.clear()
      reply({})
    } else if (r.subtype === "set_permission_mode") {
      mode = r.mode
      reply({ mode })
    } else if (r.subtype === "set_model") {
      model = r.model ?? "fake-default"
      reply({})
    } else {
      out({
        type: "control_response",
        response: { subtype: "error", request_id: m.request_id, error: `unknown ${r.subtype}` },
      })
    }
    return
  }
  if (m.type === "control_response") {
    const r = m.response ?? {}
    const resolve = waiting.get(r.request_id)
    if (resolve) {
      waiting.delete(r.request_id)
      resolve(r.response ?? {})
    }
    return
  }
  if (m.type === "user") {
    const content = m.message?.content
    const text =
      typeof content === "string"
        ? content
        : (content ?? [])
            .filter((c) => c.type === "text")
            .map((c) => c.text)
            .join("\n")
    queue.push(text)
    pump()
  }
})
rl.on("close", async () => {
  while (busy) await sleep(20)
  process.exit(0)
})

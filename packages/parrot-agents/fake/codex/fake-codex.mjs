#!/usr/bin/env node
// A fake Codex for tests. `fake-codex.mjs app-server` speaks the app-server protocol over stdio (the shapes
// CLI 0.167.1 sends, see ../../src/codex.ts); without `app-server` it draws a terminal UI close to the real one
// (see ./tui.mjs) that the `codex` TUI profile reads, and writes a rollout file a terminal driver can follow.
// It does what the message says, so tests are deterministic and need no model.
// Plain Node (18+, no dependencies).
//
// A message is read as directives in double brackets; text outside them is ignored. Without any, the
// answer is "You said: <message>".
//
//   [[text:Hello there]]           streams the text, word by word
//   [[slow:40]]                    waits 40 ms between streamed words from here on
//   [[think:Considering…]]         a reasoning summary
//   [[tool:notes_search {"query":"x"}]]  calls a tool of the MCP server in the thread's config
//   [[bash:touch a.txt]]           runs a command, asking first unless the approval policy is "never"
//   [[write:path/file.txt:content]] writes a file, asking first unless the approval policy is "never"
//   [[question:Tea or coffee?|Tea|Coffee]]  asks with request_user_input, then says what was picked
//   [[plan:Step one|Step two]]     updates the turn's plan
//   [[sleep:2000]]                 waits (an interrupt ends the turn)
//   [[recall]]                     says what was said before in this thread (to test resume)
//   [[crash]]                      exits with status 3 in the middle of the turn
//   [[fail:Overloaded]]            ends the turn as failed
//   [[dialog:Pick a theme|Dark|Light]]  (terminal UI only) a dialog no profile rule knows
//   [[garble:3000]]                (terminal UI only) a screen no profile state matches, for 3 s
import { execSync } from "node:child_process"
import { randomUUID } from "node:crypto"
import { appendFileSync, existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs"
import { homedir, tmpdir } from "node:os"
import { dirname, join, resolve } from "node:path"

export const home = process.env.FAKE_CODEX_HOME ?? join(tmpdir(), "fake-codex")
export const codexHome = process.env.CODEX_HOME ?? join(homedir(), ".codex")
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

// ── threads: what was said, for resume and recall; and the rollout file a terminal driver reads ──

export function loadThread(id) {
  try {
    return JSON.parse(readFileSync(join(home, `${id}.json`), "utf8"))
  } catch {
    return null
  }
}
export function saveThread(t) {
  mkdirSync(home, { recursive: true })
  writeFileSync(join(home, `${t.id}.json`), JSON.stringify(t))
}

/** The rollout of a thread: found by id, or a new one under sessions/YYYY/MM/DD. */
export function rolloutPath(id, cwd, create) {
  const root = join(codexHome, "sessions")
  const find = (dir) => {
    if (!existsSync(dir)) return null
    for (const e of readdirSync(dir, { withFileTypes: true })) {
      const p = join(dir, e.name)
      if (e.isDirectory()) {
        const f = find(p)
        if (f) return f
      } else if (e.name.endsWith(`-${id}.jsonl`)) return p
    }
    return null
  }
  const found = find(root)
  if (found || !create) return found
  const d = new Date()
  const pad = (n) => String(n).padStart(2, "0")
  const dir = join(root, String(d.getFullYear()), pad(d.getMonth() + 1), pad(d.getDate()))
  mkdirSync(dir, { recursive: true })
  const stamp = d.toISOString().slice(0, 19).replace(/:/g, "-")
  const p = join(dir, `rollout-${stamp}-${id}.jsonl`)
  appendFileSync(
    p,
    `${JSON.stringify({ timestamp: d.toISOString(), type: "session_meta", payload: { id, cwd, cli_version: "fake", source: "cli" } })}\n`,
  )
  return p
}

export function rollout(path, payload) {
  if (!path) return
  appendFileSync(
    path,
    `${JSON.stringify({ timestamp: new Date().toISOString(), type: "event_msg", payload })}\n`,
  )
}

// ── the MCP server from the thread's config ──

export function mcpClient(servers) {
  const session = { initialized: false, id: 0 }
  async function call(method, params) {
    const [name, server] = Object.entries(servers ?? {})[0] ?? []
    if (!server) throw new Error("no MCP server configured")
    const headers = { "content-type": "application/json", accept: "application/json, text/event-stream" }
    const token = server.bearer_token_env_var ? process.env[server.bearer_token_env_var] : server.bearer_token
    if (token) headers.authorization = `Bearer ${token}`
    const res = await fetch(server.url, {
      method: "POST",
      headers,
      body: JSON.stringify({ jsonrpc: "2.0", id: ++session.id, method, params }),
    })
    if (!res.ok) throw new Error(`${name}: HTTP ${res.status} ${await res.text()}`)
    const body = await res.json()
    if (body.error) throw new Error(`${name}: ${body.error.message}`)
    return body.result
  }
  return {
    name: Object.keys(servers ?? {})[0] ?? "parrot",
    async tool(name, args) {
      if (!session.initialized) {
        await call("initialize", {
          protocolVersion: "2025-06-18",
          capabilities: {},
          clientInfo: { name: "fake-codex", version: "1" },
        })
        session.initialized = true
      }
      return call("tools/call", { name, arguments: args })
    },
  }
}

export function directives(text) {
  const ds = [...text.matchAll(/\[\[(\w+)(?::([\s\S]*?))?\]\]/g)].map(([, op, a]) => [
    op,
    (a ?? "").replace(/\\n/g, "\n"),
  ])
  // The update check's plain prompts, so a script that answers them runs against the fake as against the CLI.
  const file = text.match(/create a file (\S+) containing the word (\w+)/)
  if (!ds.length && file) ds.push(["bash", `printf '${file[2]}' > ${file[1]}`])
  return ds
}

export function run(command, cwd) {
  try {
    return {
      output: execSync(command, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }),
      code: 0,
    }
  } catch (e) {
    return { output: String(e.stdout ?? "") + String(e.stderr ?? e.message), code: e.status ?? 1 }
  }
}

export function writeFile(cwd, path, content) {
  const p = resolve(cwd, path)
  mkdirSync(dirname(p), { recursive: true })
  writeFileSync(p, content.endsWith("\n") ? content : `${content}\n`)
  return p
}

// ── app-server ──

async function appServer() {
  const out = (m) => process.stdout.write(`${JSON.stringify(m)}\n`)
  const notify = (method, params) => out({ method, params, emittedAtMs: Date.now() })
  let thread = null // { id, cwd, said: [], mcp, approval, sandbox, model }
  let turn = null // { id, interrupted, steer: [] }
  let reqCounter = 0
  const waiting = new Map()
  const ask = (method, params) => {
    const id = reqCounter++
    out({ method, id, params })
    return new Promise((r) => waiting.set(id, { resolve: r, method }))
  }
  const asks = () => thread.approval !== "never"

  function threadInfo() {
    return {
      id: thread.id,
      sessionId: thread.id,
      forkedFromId: null,
      parentThreadId: null,
      preview: thread.said[0] ?? "",
      ephemeral: false,
      modelProvider: "fake",
      model: thread.model,
      reasoningEffort: thread.effort ?? null,
      createdAt: Math.floor(Date.now() / 1000),
      updatedAt: Math.floor(Date.now() / 1000),
      status: { type: turn ? "active" : "idle" },
      path: rolloutPath(thread.id, thread.cwd, false),
      cwd: thread.cwd,
      cliVersion: "fake",
      source: "vscode",
      name: null,
      turns: [],
    }
  }

  async function streamText(turnId, text, delay, phase = "final_answer") {
    const id = `msg_fake_${randomUUID().slice(0, 12)}`
    notify("item/started", {
      item: { type: "agentMessage", id, text: "", phase },
      threadId: thread.id,
      turnId,
    })
    for (const w of text.split(/(?<= )/)) {
      if (turn?.interrupted) break
      notify("item/agentMessage/delta", { threadId: thread.id, turnId, itemId: id, delta: w })
      if (delay) await sleep(delay)
    }
    notify("item/completed", { item: { type: "agentMessage", id, text, phase }, threadId: thread.id, turnId })
    return text
  }

  async function runTurn(turnId, text) {
    turn = { id: turnId, interrupted: false, steer: [] }
    notify("turn/started", { threadId: thread.id, turn: { id: turnId, items: [], status: "inProgress" } })
    thread.said.push(text)
    saveThread(thread)
    const said = []
    let delay = 0
    let failed = null
    const ds = directives(text)
    if (!ds.length) said.push(await streamText(turnId, `You said: ${text}`, delay))
    for (const [op, a] of ds) {
      if (turn.interrupted) break
      // A message that steered the turn is taken at the next step.
      while (turn.steer.length) said.push(await streamText(turnId, `Also: ${turn.steer.shift()}`, delay))
      switch (op) {
        case "text":
          said.push(await streamText(turnId, a, delay))
          break
        case "slow":
          delay = Number(a) || 0
          break
        case "think": {
          const id = `rs_fake_${randomUUID().slice(0, 8)}`
          notify("item/started", {
            item: { type: "reasoning", id, summary: [], content: [] },
            threadId: thread.id,
            turnId,
          })
          notify("item/reasoning/summaryTextDelta", {
            threadId: thread.id,
            turnId,
            itemId: id,
            delta: a,
            summaryIndex: 0,
          })
          notify("item/completed", {
            item: { type: "reasoning", id, summary: [a], content: [] },
            threadId: thread.id,
            turnId,
          })
          break
        }
        case "tool": {
          const sp = a.indexOf(" ")
          const name = sp < 0 ? a : a.slice(0, sp)
          const args = sp < 0 ? {} : JSON.parse(a.slice(sp + 1))
          const id = `call_fake_${randomUUID().slice(0, 8)}`
          const mcp = mcpClient(thread.mcp)
          const base = { type: "mcpToolCall", id, server: mcp.name, tool: name, arguments: args }
          notify("item/started", {
            item: { ...base, status: "inProgress", result: null, error: null },
            threadId: thread.id,
            turnId,
          })
          let r
          try {
            const res = await mcp.tool(name, args)
            r = { ok: !res.isError, text: (res.content ?? []).map((c) => c.text ?? "").join("\n"), res }
          } catch (e) {
            r = { ok: false, text: String(e.message ?? e) }
          }
          notify("item/completed", {
            item: {
              ...base,
              status: r.ok ? "completed" : "failed",
              result: r.res ? { content: r.res.content ?? [], structuredContent: null, _meta: null } : null,
              error: r.ok ? null : { message: r.text },
            },
            threadId: thread.id,
            turnId,
          })
          said.push(
            await streamText(
              turnId,
              r.ok ? `The tool said: ${r.text.slice(0, 400)}` : `The tool failed: ${r.text}`,
              delay,
            ),
          )
          break
        }
        case "bash": {
          const id = `exec-${randomUUID()}`
          const command = `/bin/bash -lc '${a.replace(/'/g, "'\\''")}'`
          const base = {
            type: "commandExecution",
            id,
            command,
            cwd: thread.cwd,
            commandActions: [{ type: "unknown", command: a }],
          }
          notify("item/started", {
            item: { ...base, status: "inProgress", aggregatedOutput: null, exitCode: null },
            threadId: thread.id,
            turnId,
          })
          let decision = "accept"
          if (asks()) {
            const r = await ask("item/commandExecution/requestApproval", {
              kind: "command",
              threadId: thread.id,
              turnId,
              itemId: id,
              startedAtMs: Date.now(),
              environmentId: "local",
              reason: "The fake asks before every command",
              command,
              cwd: thread.cwd,
              availableDecisions: ["accept", "acceptForSession", "decline", "cancel"],
            })
            decision = r?.decision ?? "decline"
          }
          if (turn.interrupted) break
          if (decision === "accept" || decision === "acceptForSession") {
            const r = run(a, thread.cwd)
            notify("item/completed", {
              item: {
                ...base,
                status: r.code === 0 ? "completed" : "failed",
                aggregatedOutput: r.output,
                exitCode: r.code,
              },
              threadId: thread.id,
              turnId,
            })
            said.push(await streamText(turnId, "Ran it.", delay))
          } else {
            notify("item/completed", {
              item: { ...base, status: "declined", aggregatedOutput: null, exitCode: null },
              threadId: thread.id,
              turnId,
            })
            said.push(await streamText(turnId, "I did not run it.", delay))
          }
          break
        }
        case "write": {
          const i = a.indexOf(":")
          const path = a.slice(0, i)
          const content = a.slice(i + 1)
          const id = `patch-${randomUUID()}`
          const full = resolve(thread.cwd, path)
          const changes = [{ path: full, kind: { type: "add" }, diff: content }]
          notify("item/started", {
            item: { type: "fileChange", id, changes, status: "inProgress" },
            threadId: thread.id,
            turnId,
          })
          let decision = "accept"
          if (asks()) {
            const r = await ask("item/fileChange/requestApproval", {
              threadId: thread.id,
              turnId,
              itemId: id,
              startedAtMs: Date.now(),
              reason: null,
            })
            decision = r?.decision ?? "decline"
          }
          if (turn.interrupted) break
          if (decision === "accept" || decision === "acceptForSession") {
            writeFile(thread.cwd, path, content)
            notify("item/completed", {
              item: { type: "fileChange", id, changes, status: "completed" },
              threadId: thread.id,
              turnId,
            })
            said.push(await streamText(turnId, `Wrote ${path}.`, delay))
          } else {
            notify("item/completed", {
              item: { type: "fileChange", id, changes, status: "declined" },
              threadId: thread.id,
              turnId,
            })
            said.push(await streamText(turnId, `I did not write ${path}.`, delay))
          }
          break
        }
        case "question": {
          const [q, ...options] = a.split("|")
          const r = await ask("item/tool/requestUserInput", {
            threadId: thread.id,
            turnId,
            itemId: `q-${randomUUID().slice(0, 8)}`,
            questions: [
              {
                id: "q1",
                header: "Question",
                question: q,
                isOther: true,
                isSecret: false,
                options: options.map((label) => ({ label, description: `Pick ${label}` })),
              },
            ],
            isBlocking: true,
            autoResolutionMs: null,
          })
          if (turn.interrupted) break
          const picked = r?.answers?.q1?.answers?.join(", ") || "nothing"
          said.push(await streamText(turnId, `You picked ${picked}.`, delay))
          break
        }
        case "plan":
          notify("turn/plan/updated", {
            threadId: thread.id,
            turnId,
            explanation: null,
            plan: a.split("|").map((step, i) => ({ step, status: i === 0 ? "inProgress" : "pending" })),
          })
          break
        case "sleep": {
          const until = Date.now() + (Number(a) || 1000)
          while (Date.now() < until && !turn.interrupted) await sleep(25)
          break
        }
        case "recall":
          said.push(
            await streamText(
              turnId,
              `Before this you said: ${thread.said.slice(0, -1).join(" / ") || "nothing"}`,
              delay,
            ),
          )
          break
        case "crash":
          process.stderr.write("fake-codex: crashing as asked\n")
          process.exit(3)
          break
        case "fail":
          failed = a || "failed"
          break
      }
      if (failed) break
    }
    while (turn.steer.length && !turn.interrupted)
      said.push(await streamText(turnId, `Also: ${turn.steer.shift()}`, delay))
    notify("thread/tokenUsage/updated", {
      threadId: thread.id,
      turnId,
      tokenUsage: {
        total: {
          totalTokens: 150,
          inputTokens: 100,
          cachedInputTokens: 40,
          cacheWriteInputTokens: 0,
          outputTokens: 50,
          reasoningOutputTokens: 0,
        },
        last: {
          totalTokens: 150,
          inputTokens: 100,
          cachedInputTokens: 40,
          cacheWriteInputTokens: 0,
          outputTokens: 50,
          reasoningOutputTokens: 0,
        },
        modelContextWindow: 200000,
      },
    })
    const status = turn.interrupted ? "interrupted" : failed ? "failed" : "completed"
    notify("turn/completed", {
      threadId: thread.id,
      turn: {
        id: turnId,
        items: [],
        status,
        error: failed ? { message: failed, codexErrorInfo: null, additionalDetails: null } : null,
      },
    })
    turn = null
  }

  const methods = {
    initialize: () => ({
      userAgent: "fake-codex/0.0.0",
      codexHome,
      platformFamily: "unix",
      platformOs: "linux",
    }),
    "model/list": () => ({
      data: [
        {
          id: "fake-mini",
          model: "fake-mini",
          displayName: "Fake Mini",
          description: "The fake's quick model",
          hidden: false,
          supportedReasoningEfforts: [
            { reasoningEffort: "low", description: "Fast" },
            { reasoningEffort: "medium", description: "Balanced" },
            { reasoningEffort: "high", description: "Deep" },
          ],
          defaultReasoningEffort: "low",
          inputModalities: ["text", "image"],
          isDefault: true,
        },
        {
          id: "fake-big",
          model: "fake-big",
          displayName: "Fake Big",
          description: "Slower",
          hidden: false,
          supportedReasoningEfforts: [],
          defaultReasoningEffort: "medium",
          inputModalities: ["text"],
          isDefault: false,
        },
      ],
      nextCursor: null,
    }),
    "skills/list": (p) => ({
      data: [
        {
          cwd: p.cwds?.[0] ?? process.cwd(),
          skills: [
            {
              name: "fake-skill",
              description: "A skill the fake harness offers",
              path: "/fake/SKILL.md",
              scope: "user",
              enabled: true,
            },
          ],
          errors: [],
        },
      ],
    }),
    "thread/start": (p) => {
      thread = {
        id: randomUUID(),
        cwd: p.cwd ?? process.cwd(),
        said: [],
        mcp: p.config?.mcp_servers ?? {},
        approval: p.approvalPolicy ?? "on-request",
        sandbox: p.sandbox ?? "workspace-write",
        model: p.model ?? "fake-mini",
      }
      saveThread(thread)
      notify("thread/started", { thread: threadInfo() })
      return {
        thread: threadInfo(),
        model: thread.model,
        modelProvider: "fake",
        cwd: thread.cwd,
        approvalPolicy: thread.approval,
        sandbox: { type: "workspaceWrite" },
        reasoningEffort: null,
      }
    },
    "thread/resume": (p) => {
      const t = loadThread(p.threadId)
      if (!t) throw { code: -32600, message: `no rollout found for thread id ${p.threadId}` }
      thread = {
        ...t,
        mcp: p.config?.mcp_servers ?? t.mcp,
        approval: p.approvalPolicy ?? t.approval,
        sandbox: p.sandbox ?? t.sandbox,
        model: p.model ?? t.model,
      }
      return {
        thread: threadInfo(),
        model: thread.model,
        modelProvider: "fake",
        cwd: thread.cwd,
        approvalPolicy: thread.approval,
        sandbox: { type: "workspaceWrite" },
        reasoningEffort: null,
      }
    },
    "turn/start": (p) => {
      if (!thread || p.threadId !== thread.id) throw { code: -32600, message: "no such thread" }
      if (turn) throw { code: -32600, message: "a turn is running" }
      if (p.approvalPolicy) thread.approval = p.approvalPolicy
      if (p.model) thread.model = p.model
      if (p.effort) thread.effort = p.effort
      const text = (p.input ?? [])
        .filter((i) => i.type === "text")
        .map((i) => i.text)
        .join("\n")
      const images = (p.input ?? []).filter((i) => i.type === "image" || i.type === "localImage").length
      const id = randomUUID()
      setTimeout(
        () =>
          runTurn(id, images ? `${text} [[text:I see ${images} image${images === 1 ? "" : "s"}.]]` : text),
        0,
      )
      return { turn: { id, items: [], status: "inProgress", error: null } }
    },
    "turn/steer": (p) => {
      if (!turn || p.expectedTurnId !== turn.id)
        throw { code: -32600, message: "no active turn with that id" }
      turn.steer.push(
        (p.input ?? [])
          .filter((i) => i.type === "text")
          .map((i) => i.text)
          .join("\n"),
      )
      return { turnId: turn.id }
    },
    "turn/interrupt": (p) => {
      if (turn && p.turnId === turn.id) {
        turn.interrupted = true
        for (const [id, w] of waiting) {
          notify("serverRequest/resolved", { threadId: thread.id, requestId: id })
          w.resolve(w.method.endsWith("requestUserInput") ? { answers: {} } : { decision: "cancel" })
        }
        waiting.clear()
      }
      return {}
    },
    "thread/compact/start": () => {
      setTimeout(() => notify("thread/compacted", { threadId: thread.id, turnId: null }), 10)
      return {}
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
      if (m.method && m.id != null) {
        const f = methods[m.method]
        if (!f) {
          out({ id: m.id, error: { code: -32601, message: `method not found: ${m.method}` } })
          continue
        }
        try {
          out({ id: m.id, result: f(m.params ?? {}) })
        } catch (e) {
          out({ id: m.id, error: { code: e.code ?? -32603, message: e.message ?? String(e) } })
        }
      } else if (m.id != null && !m.method) {
        const w = waiting.get(m.id)
        if (w) {
          waiting.delete(m.id)
          w.resolve(m.result ?? null)
        }
      }
    }
  })
  process.stdin.on("end", async () => {
    while (turn) await sleep(20)
    process.exit(0)
  })
}

const args = process.argv.slice(2)
if (args.includes("--version")) process.stdout.write("codex-cli 0.167.1 (fake)\n")
else if (args[0] === "app-server") appServer()
else import("./tui.mjs").then((m) => m.tui(args))

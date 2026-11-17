// The terminal half of the fake Claude Code (see ./fake-claude.mjs): a screen laid out like the real
// CLI 2.1.325's (crates/parrot-tui/fixtures/casts), so the `claude-code` TUI profile reads its states, and a
// JSONL transcript in the real one's place and shape (`$CLAUDE_CONFIG_DIR/projects/<folder>/<id>.jsonl`)
// for a terminal driver to follow. Keys arrive as a terminal sends them: text, bracketed paste, Enter, Esc,
// arrows, Backspace, Ctrl+U, Ctrl+C.
//
// The directives of stream-json.mjs, plus two that only a screen has:
//   [[dialog:Pick a theme|Dark|Light]]  a boxed dialog no profile rule knows (a generic surface)
//   [[garble:3000]]                     a screen no profile state matches, for 3 s
import { execSync } from "node:child_process"
import { randomUUID } from "node:crypto"
import { appendFileSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { homedir, tmpdir } from "node:os"
import { dirname, join, resolve } from "node:path"

// Key sequences as a terminal sends them (built from ESC: biome allows no control characters in regexes).
const ESC = String.fromCharCode(27)
const PASTE = new RegExp(`^${ESC}\\[200~([\\s\\S]*?)${ESC}\\[201~`)
const SEQ = new RegExp(`^${ESC}(\\[[0-9;?]*[A-Za-z~]|O[A-Z])`)

const args = process.argv.slice(2)
const arg = (name) => {
  const i = args.indexOf(name)
  return i >= 0 ? args[i + 1] : undefined
}
const resumed = arg("--resume")
const sessionId = resumed ?? arg("--session-id") ?? randomUUID()
let mode = arg("--permission-mode") ?? "default"
const model = arg("--model") ?? "fake-default"
const mcpConfig = (() => {
  try {
    return JSON.parse(arg("--mcp-config") ?? "{}")
  } catch {
    return {}
  }
})()
const cwd = process.cwd()

// What was said, shared with the stream-json half so either can resume the other's session.
const home = process.env.FAKE_CLAUDE_HOME ?? join(tmpdir(), "fake-claude")
const saidFile = join(home, `${sessionId}.json`)
let said = []
if (resumed) {
  try {
    said = JSON.parse(readFileSync(saidFile, "utf8"))
  } catch {
    process.stderr.write(`No conversation found with session ID: ${resumed}\n`)
    process.exit(1)
  }
}
const saveSaid = () => {
  mkdirSync(home, { recursive: true })
  writeFileSync(saidFile, JSON.stringify(said))
}
saveSaid()

// The JSONL transcript, where the real CLI writes it.
const config = process.env.CLAUDE_CONFIG_DIR ?? join(homedir(), ".claude")
const jsonl = join(config, "projects", cwd.replace(/[^a-zA-Z0-9]/g, "-"), `${sessionId}.jsonl`)
mkdirSync(dirname(jsonl), { recursive: true })
let parent = null
function record(entry) {
  const uuid = randomUUID()
  appendFileSync(
    jsonl,
    `${JSON.stringify({ parentUuid: parent, isSidechain: false, sessionId, cwd, uuid, timestamp: new Date().toISOString(), ...entry })}\n`,
  )
  parent = uuid
}
const assistantBlock = (block, stop = null) =>
  record({
    type: "assistant",
    message: {
      id: `msg_fake_${randomUUID().slice(0, 12)}`,
      role: "assistant",
      model,
      content: [block],
      stop_reason: stop,
      usage: { input_tokens: 10, output_tokens: 5 },
    },
  })
const toolResultLine = (id, content, isError = false) =>
  record({
    type: "user",
    message: {
      role: "user",
      content: [{ type: "tool_result", tool_use_id: id, content, is_error: isError }],
    },
  })

// ── the screen ──

const out = (s) => process.stdout.write(s)
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
const size = () => ({ rows: process.stdout.rows || 30, cols: process.stdout.columns || 100 })
const DIM = "\x1b[38;2;153;153;153m"
const RESET = "\x1b[0m"
const BOLD = "\x1b[1m"

const st = {
  phase: "idle", // idle | working | dialog | garble
  input: "",
  convo: [],
  spinner: "",
  dialog: null, // { lines, options: [{label, desc?}], selected, resolve, boxed, footer }
  turn: null, // { interrupted }
  allowed: new Set(),
}

function wrap(text, width) {
  const out = []
  for (const raw of String(text).split("\n")) {
    let line = raw
    while (line.length > width) {
      out.push(line.slice(0, width))
      line = line.slice(width)
    }
    out.push(line)
  }
  return out
}

function render() {
  const { rows, cols } = size()
  const rule = (c = "─") => `${DIM}${c.repeat(cols)}${RESET}`
  const top = [
    "",
    ` ${BOLD}▐▛███▛█   Claude Code v2.1.325${RESET}`,
    `${DIM} ▝▜██████▀  Fake ${model} · test double${RESET}`,
    `${DIM}  ▝▝   ▝▝   ${cwd.slice(-60)}${RESET}`,
    "",
  ]
  let bottom = []
  let cursor = null
  if (st.phase === "garble") {
    const noise = []
    for (let i = 0; i < rows; i++)
      noise.push(
        Array.from({ length: Math.min(cols, 70) }, (_, j) => "░▒▓█▚▞"[(i * 7 + j * 3 + Date.now()) % 6]).join(
          "",
        ),
      )
    paint(noise, null)
    return
  }
  if (st.phase === "dialog" && st.dialog) {
    const d = st.dialog
    const body = []
    if (d.boxed) {
      const w = Math.min(cols - 2, 48)
      body.push(`╭${"─".repeat(w)}╮`)
      for (const l of d.lines) body.push(`│ ${l.padEnd(w - 1)}│`)
      body.push(`│${" ".repeat(w)}│`)
      d.options.forEach((o, i) => {
        const t = `${i === d.selected ? "❯" : " "} ${i + 1}. ${o.label}`
        body.push(`│ ${t.padEnd(w - 1)}│`)
      })
      body.push(`╰${"─".repeat(w)}╯`)
      body.push(` ${DIM}↑/↓ to move · Enter to pick${RESET}`)
    } else {
      body.push(rule())
      for (const l of d.lines) body.push(l)
      d.options.forEach((o, i) => {
        body.push(`${i === d.selected ? "❯" : " "} ${i + 1}. ${o.label}`)
        if (o.desc) body.push(`     ${DIM}${o.desc}${RESET}`)
        if (o.ruleAfter) body.push(rule())
      })
      body.push("")
      body.push(`${DIM}${d.footer ?? "Esc to cancel"}${RESET}`)
    }
    bottom = body
  } else {
    const working = st.phase === "working"
    // The placeholder is dim (SGR 2), which is how the tree tells it from typed text.
    const prompt = st.input ? `❯\u00a0${st.input}` : `❯ \x1b[2mTry "fix the failing test"${RESET}`
    const hint = working ? "⏸ default mode · esc to interrupt" : "⏸ default mode · ? for shortcuts"
    bottom = [
      working ? `✢ ${st.spinner || "Thinking…"} ${DIM}(1s · ↓ 20 tokens)${RESET}` : "",
      rule(),
      prompt,
      rule(),
      `  ${DIM}${hint}${RESET}`,
    ]
    cursor = { row: rows - 2, col: 3 + st.input.length }
  }
  const room = rows - top.length - bottom.length
  const convo = st.convo.flatMap((l) => wrap(l, cols)).slice(-Math.max(0, room))
  const lines = [...top, ...convo]
  while (lines.length < rows - bottom.length) lines.push("")
  lines.push(...bottom)
  paint(lines.slice(0, rows), cursor)
}

function paint(lines, cursor) {
  let s = "\x1b[?25l\x1b[H"
  for (let i = 0; i < lines.length; i++) s += `\x1b[${i + 1};1H${lines[i]}\x1b[K`
  s += "\x1b[J"
  if (cursor) s += `\x1b[${cursor.row};${cursor.col}H\x1b[?25h`
  const title = st.phase === "working" ? "◐ Claude Code" : "✳ Claude Code"
  s += `\x1b]0;${title}\x07`
  out(s)
}

// ── dialogs ──

function dialog(d) {
  return new Promise((resolve) => {
    st.dialog = { selected: 0, ...d, resolve }
    st.phase = "dialog"
    render()
  })
}

function closeDialog(answer) {
  const d = st.dialog
  st.dialog = null
  st.phase = st.turn ? "working" : "idle"
  render()
  d?.resolve(answer)
}

// ── a turn ──

async function stream(text, delay) {
  const words = text.split(/(?<= )/)
  st.convo.push("● ")
  for (const w of words) {
    if (st.turn?.interrupted) break
    st.convo[st.convo.length - 1] += w
    if (delay) {
      render()
      await sleep(delay)
    }
  }
  render()
  return text
}

async function callTool(name, input) {
  const [server, cfg] = Object.entries(mcpConfig.mcpServers ?? {})[0] ?? []
  const id = `toolu_fake_${randomUUID().slice(0, 8)}`
  assistantBlock({ type: "tool_use", id, name: `mcp__${server}__${name}`, input })
  st.convo.push(`● ${server} - ${name} (MCP)`)
  try {
    if (!cfg) throw new Error("no MCP server configured")
    const headers = { "content-type": "application/json", accept: "application/json, text/event-stream" }
    for (const [k, v] of Object.entries(cfg.headers ?? {}))
      headers[k] = String(v).replace(/\$\{(\w+)\}/g, (_, n) => process.env[n] ?? "")
    const post = async (rid, method, params) => {
      const r = await fetch(cfg.url, {
        method: "POST",
        headers,
        body: JSON.stringify({ jsonrpc: "2.0", id: rid, method, params }),
      })
      const b = await r.json()
      if (b.error) throw new Error(b.error.message)
      return b.result
    }
    await post(1, "initialize", {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "fake-claude-tui", version: "1" },
    })
    const r = await post(2, "tools/call", { name, arguments: input })
    const text = (r.content ?? []).map((c) => c.text ?? "").join("\n")
    toolResultLine(id, text, !!r.isError)
    st.convo.push(`  ⎿  ${text.split("\n")[0].slice(0, 80)}`)
    return { text, isError: !!r.isError }
  } catch (e) {
    toolResultLine(id, String(e.message ?? e), true)
    return { text: String(e.message ?? e), isError: true }
  }
}

async function permission(heading, body, question, sessionLabel) {
  if (mode === "bypassPermissions") return "allow"
  return dialog({
    lines: [
      ` ${BOLD}${heading}${RESET}`,
      `${DIM} Tip: a fake dialog for tests${RESET}`,
      `${DIM}${"╌".repeat(size().cols)}${RESET}`,
      ...body.map((l) => ` ${l}`),
      `${DIM}${"╌".repeat(size().cols)}${RESET}`,
      ` ${question}`,
    ],
    options: [{ label: "Yes" }, { label: sessionLabel }, { label: "No" }],
  })
}

async function runTurn(text) {
  st.turn = { interrupted: false }
  st.phase = "working"
  said.push(text)
  saveSaid()
  render()
  const parts = []
  let delay = 0
  const ds = [...text.matchAll(/\[\[(\w+)(?::([\s\S]*?))?\]\]/g)]
  // The update check's plain prompt, so a script that answers it runs against the fake as against the CLI.
  const exact = text.match(/run exactly this command: (.+)$/)
  if (!ds.length && exact) ds.push([`[[bash:${exact[1]}]]`, "bash", exact[1]])
  const say = async (t) => {
    parts.push(await stream(t, delay))
  }
  if (!ds.length) await say(`You said: ${text}`)
  for (const [, op, raw] of ds) {
    if (st.turn.interrupted) break
    const a = (raw ?? "").replace(/\\n/g, "\n")
    switch (op) {
      case "text":
        await say(a)
        break
      case "slow":
        delay = Number(a) || 0
        break
      case "think":
        assistantBlock({ type: "thinking", thinking: a })
        st.convo.push(`✻ ${a}`)
        break
      case "tool": {
        const sp = a.indexOf(" ")
        const r = await callTool(sp < 0 ? a : a.slice(0, sp), sp < 0 ? {} : JSON.parse(a.slice(sp + 1)))
        await say(r.isError ? `The tool failed: ${r.text}` : `The tool said: ${r.text.slice(0, 400)}`)
        break
      }
      case "bash": {
        const id = `toolu_fake_${randomUUID().slice(0, 8)}`
        assistantBlock({ type: "tool_use", id, name: "Bash", input: { command: a, description: `Run ${a}` } })
        st.convo.push(`● Bash(${a})`)
        const answer = st.allowed.has(a)
          ? 0
          : await permission(
              "Bash command",
              [a, `${DIM}Run ${a}${RESET}`],
              "Do you want to proceed?",
              `Yes, and don't ask again for ${a.split(" ")[0]} commands in ${cwd}`,
            )
        if (st.turn.interrupted || answer === "esc" || answer === 2) {
          toolResultLine(id, "The user doesn't want to proceed with this tool use.", true)
          if (answer === 2) await say("I did not run it.")
          else st.turn.interrupted = true
          break
        }
        if (answer === 1) st.allowed.add(a)
        let output = ""
        try {
          output = execSync(a, { cwd, encoding: "utf8" })
        } catch (e) {
          output = String(e.message)
        }
        toolResultLine(id, output || "(no output)")
        st.convo.push(`  ⎿  ${(output || "(no output)").split("\n")[0]}`)
        await say("Ran it.")
        break
      }
      case "write": {
        const i = a.indexOf(":")
        const path = a.slice(0, i)
        const content = a.slice(i + 1)
        const file = resolve(cwd, path)
        const id = `toolu_fake_${randomUUID().slice(0, 8)}`
        assistantBlock({ type: "tool_use", id, name: "Write", input: { file_path: file, content } })
        st.convo.push(`● Write(${path})`)
        const answer =
          mode === "acceptEdits"
            ? 0
            : await permission(
                "Create file",
                [path, content],
                `Do you want to create ${path}?`,
                "Yes, allow all edits during this session (shift+tab)",
              )
        if (st.turn.interrupted || answer === "esc" || answer === 2) {
          toolResultLine(id, "The user doesn't want to proceed with this tool use.", true)
          if (answer === 2) await say(`I did not write ${path}.`)
          else st.turn.interrupted = true
          break
        }
        if (answer === 1) mode = "acceptEdits"
        mkdirSync(dirname(file), { recursive: true })
        writeFileSync(file, content.endsWith("\n") ? content : `${content}\n`)
        toolResultLine(id, `File created successfully at: ${file}`)
        await say(`Wrote ${path}.`)
        break
      }
      case "question": {
        const [q, ...options] = a.split("|")
        const id = `toolu_fake_${randomUUID().slice(0, 8)}`
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
        assistantBlock({ type: "tool_use", id, name: "AskUserQuestion", input })
        const opts = options.map((label) => ({ label, desc: `Pick ${label}` }))
        opts.push({ label: "Type something.", ruleAfter: true }, { label: "Chat about this" })
        const answer = await dialog({
          lines: [` \x1b[30;48;2;177;185;249m☐ Question${RESET}`, "", `${BOLD}${q}${RESET}`, ""],
          options: opts,
          footer: "Enter to select · ↑/↓ to navigate · Esc to cancel",
        })
        if (st.turn.interrupted || answer === "esc") {
          toolResultLine(id, "The user declined to answer.", true)
          st.turn.interrupted = true
          break
        }
        const picked = opts[answer]?.label ?? "nothing"
        toolResultLine(id, `User has answered your questions: "${q}"="${picked}".`)
        await say(`You picked ${picked}.`)
        break
      }
      case "dialog": {
        const [title, ...options] = a.split("|")
        const answer = await dialog({
          boxed: true,
          lines: [title],
          options: options.map((label) => ({ label })),
        })
        if (answer === "esc") break
        await say(`${title}: ${options[answer]}`)
        break
      }
      case "garble": {
        st.phase = "garble"
        const until = Date.now() + (Number(a) || 3000)
        while (Date.now() < until && !st.turn.interrupted) {
          render()
          await sleep(200)
        }
        st.phase = "working"
        render()
        break
      }
      case "sleep": {
        const until = Date.now() + (Number(a) || 1000)
        while (Date.now() < until && !st.turn.interrupted) await sleep(25)
        break
      }
      case "recall":
        await say(`Before this you said: ${said.slice(0, -1).join(" / ") || "nothing"}`)
        break
      case "crash":
        out("\x1b[?1049l")
        process.stderr.write("fake-claude: crashing as asked\n")
        process.exit(3)
        break
      case "fail":
        st.convo.push(`  ⎿  API Error: ${a || "failed"}`)
        break
    }
  }
  const interrupted = st.turn.interrupted
  if (interrupted) {
    st.convo.push("  ⎿  Interrupted · What should Claude do instead?")
    record({
      type: "user",
      message: { role: "user", content: [{ type: "text", text: "[Request interrupted by user]" }] },
    })
    // Like the real CLI, the interrupted prompt goes back into the input.
    st.input = text
  } else {
    assistantBlock({ type: "text", text: parts.join(" ") || "Done." }, "end_turn")
  }
  record({ type: "system", subtype: "turn_duration", durationMs: 1000 })
  st.convo.push(`${DIM}✻ Worked for 1s${RESET}`)
  st.turn = null
  st.phase = "idle"
  render()
}

// ── keys ──

function submit() {
  const text = st.input
  st.input = ""
  if (!text.trim()) return render()
  if (text.trim() === "/exit") {
    out("\x1b[?2004l\x1b[?1049l")
    process.exit(0)
  }
  st.convo.push(`❯ ${text}`)
  record({ type: "user", message: { role: "user", content: text } })
  runTurn(text)
}

function key(k) {
  if (k.startsWith("text:") && k.length === 6 && st.phase === "dialog") k = k.slice(5)
  if (st.phase === "dialog" && st.dialog) {
    const d = st.dialog
    if (k === "up") d.selected = Math.max(0, d.selected - 1)
    else if (k === "down") d.selected = Math.min(d.options.length - 1, d.selected + 1)
    else if (k === "enter") return closeDialog(d.selected)
    else if (k === "esc") return closeDialog("esc")
    else if (/^[1-9]$/.test(k) && Number(k) <= d.options.length) return closeDialog(Number(k) - 1)
    return render()
  }
  if (st.phase === "working" || st.phase === "garble") {
    if (k === "esc" && st.turn) {
      st.turn.interrupted = true
    }
    return
  }
  if (k === "enter") return submit()
  if (k === "backspace") st.input = st.input.slice(0, -1)
  else if (k === "ctrl+u") st.input = ""
  else if (k === "ctrl+c") {
    if (!st.input) {
      out("\x1b[?2004l\x1b[?1049l")
      process.exit(0)
    }
    st.input = ""
  } else if (k.length === 1 || k.startsWith("text:")) st.input += k.startsWith("text:") ? k.slice(5) : k
  render()
}

function feed(data) {
  let s = data.toString("utf8")
  while (s.length) {
    const paste = s.match(PASTE)
    if (paste) {
      if (st.phase === "idle") st.input += paste[1].replace(/\r\n?/g, "\n")
      s = s.slice(paste[0].length)
      render()
      continue
    }
    const seq = s.match(SEQ)
    if (seq) {
      const code = seq[1]
      if (/A$/.test(code)) key("up")
      else if (/B$/.test(code)) key("down")
      else if (code === "[13u") key("enter")
      else if (code === "[27u") key("esc")
      s = s.slice(seq[0].length)
      continue
    }
    const c = s[0]
    s = s.slice(1)
    if (c === ESC) key("esc")
    else if (c === "\r" || c === "\n") key("enter")
    else if (c === "\x7f" || c === "\b") key("backspace")
    else if (c === "\x15") key("ctrl+u")
    else if (c === "\x03") key("ctrl+c")
    else if (c >= " ") key(`text:${c}`)
  }
}

if (process.stdin.isTTY) process.stdin.setRawMode(true)
process.stdin.on("data", feed)
process.stdout.on("resize", render)
out("\x1b[?1049h\x1b[?2004h\x1b[2J")
// The real CLI redraws a moment after it starts; keys before that are lost there too.
setTimeout(render, 150)

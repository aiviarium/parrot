// The terminal half of the fake Codex (see ./fake-codex.mjs): a screen laid out like CLI 0.167.1's
// (crates/parrot-tui/fixtures/casts), so the `codex` TUI profile reads its states, and a rollout file in the
// real one's place and shape (`$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<time>-<thread id>.jsonl`, with
// `event_msg` lines of `item_completed`, `token_count` and `task_complete`) for a terminal driver to follow.
// `fake-codex.mjs [resume <thread id>] [-c key=value]... [-a policy] [-s sandbox] [-m model]`.
import { randomUUID } from "node:crypto"
import {
  directives,
  loadThread,
  mcpClient,
  rollout,
  rolloutPath,
  run,
  saveThread,
  sleep,
  writeFile,
} from "./fake-codex.mjs"

// Key sequences as a terminal sends them (built from ESC: biome allows no control characters in regexes).
const ESC = String.fromCharCode(27)
const PASTE = new RegExp(`^${ESC}\\[200~([\\s\\S]*?)${ESC}\\[201~`)
const SEQ = new RegExp(`^${ESC}(\\[[0-9;?]*[A-Za-z~]|O[A-Z])`)
const CSI = new RegExp(`${ESC}\\[[0-9;?]*[A-Za-z]`, "g")

const out = (s) => process.stdout.write(s)
const size = () => ({ rows: process.stdout.rows || 30, cols: process.stdout.columns || 100 })
const FILL = "\x1b[48;2;49;49;53m"
const USER = "\x1b[48;2;59;59;62m"
const SEL = "\x1b[1;38;2;0;0;46;48;2;99;168;248m"
const DIM = "\x1b[2m"
const GREEN = "\x1b[38;2;171;223;167m"
const RESET = "\x1b[0m"

export function tui(args) {
  const opt = (flag) => {
    const i = args.indexOf(flag)
    return i >= 0 ? args[i + 1] : undefined
  }
  const config = {}
  args.forEach((a, i) => {
    if (a === "-c" && args[i + 1]) {
      const kv = args[i + 1]
      const eq = kv.indexOf("=")
      let v = kv.slice(eq + 1)
      try {
        v = JSON.parse(v)
      } catch {}
      config[kv.slice(0, eq)] = v
    }
  })
  const servers = {}
  for (const [k, v] of Object.entries(config)) {
    const m = k.match(/^mcp_servers\.([^.]+)\.(.+)$/)
    if (!m) continue
    servers[m[1]] = servers[m[1]] ?? {}
    servers[m[1]][m[2]] = v
  }
  const cwd = process.cwd()
  const resumeId = args[0] === "resume" ? args[1] : undefined
  let thread
  if (resumeId) {
    thread = loadThread(resumeId)
    if (!thread) {
      process.stderr.write(`No saved session found with ID ${resumeId}\n`)
      process.exit(1)
    }
  } else {
    thread = {
      id: randomUUID(),
      cwd,
      said: [],
      approval: "on-request",
      sandbox: "workspace-write",
      model: "fake-mini",
    }
  }
  thread.approval = opt("-a") ?? thread.approval
  thread.sandbox = opt("-s") ?? thread.sandbox
  thread.model = opt("-m") ?? thread.model
  thread.mcp = Object.keys(servers).length ? servers : thread.mcp
  saveThread(thread)
  const path = rolloutPath(thread.id, cwd, true)

  const st = { phase: "idle", input: "", convo: [], dialog: null, turn: null, started: 0 }

  const item = (it) =>
    rollout(path, { type: "item_completed", thread_id: thread.id, turn_id: st.turn?.id, item: it })

  function render() {
    const { rows, cols } = size()
    const pad = (s, w = cols) => s + " ".repeat(Math.max(0, w - visible(s).length))
    const top = [
      "",
      `  \x1b[1m>_ OpenAI Codex (v0.167.1)${RESET}`,
      `     ${DIM}${cwd.slice(-70)}${RESET}`,
      "",
    ]
    let bottom = []
    let cursor = null
    if (st.phase === "garble") {
      const noise = []
      for (let i = 0; i < rows; i++)
        noise.push(Array.from({ length: 60 }, (_, j) => "▖▗▘▙▚▛"[(i * 5 + j + Date.now()) % 6]).join(""))
      return paint(noise, null, "codex")
    }
    if (st.phase === "dialog" && st.dialog) {
      const d = st.dialog
      if (d.boxed) {
        const w = 46
        bottom.push(`╭${"─".repeat(w)}╮`)
        for (const l of d.lines) bottom.push(`│ ${l.padEnd(w - 1)}│`)
        bottom.push(`│${" ".repeat(w)}│`)
        for (const [i, o] of d.options.entries())
          bottom.push(`│ ${`${i === d.selected ? "›" : " "} ${i + 1}. ${o}`.padEnd(w - 1)}│`)
        bottom.push(`╰${"─".repeat(w)}╯`)
        bottom.push(`  ${DIM}↑/↓ to move · enter to pick${RESET}`)
      } else {
        // As the real one: the dialog on a fill from a blank row above it to one below the options, the
        // selected option on a highlight the width of the screen, the key hint under the fill.
        const body = ["", ...d.lines.map((l) => `  ${l}`), "", ""]
        bottom = body.map((l) => `${FILL}${pad(l)}${RESET}`)
        for (const [i, o] of d.options.entries())
          bottom.push(
            i === d.selected
              ? `${SEL}${pad(`› ${i + 1}. ${o}`)}${RESET}`
              : `${FILL}${pad(`  ${i + 1}. ${o}`)}${RESET}`,
          )
        bottom.push(`${FILL}${pad("")}${RESET}`)
        bottom.push(`  ${DIM}Press enter to confirm or esc to cancel${RESET}`)
      }
    } else {
      const working = st.phase === "working"
      // A long prompt wraps inside the fill, the cursor at its end, as in the real CLI.
      const width = cols - 2
      const chunks = []
      for (let i = 0; i < st.input.length; i += width) chunks.push(st.input.slice(i, i + width))
      const promptRows = st.input
        ? chunks.map((c, i) => `${FILL}${pad(`${i === 0 ? "›" : " "} ${c}`)}${RESET}`)
        : [`${FILL}${pad(`› ${DIM}Ask Codex to do anything${RESET}${FILL}`)}${RESET}`]
      bottom = [
        working
          ? `${DIM}• Working (${Math.round((Date.now() - st.started) / 1000)}s • esc to interrupt)${RESET}`
          : "",
        "",
        `${FILL}${pad("")}${RESET}`,
        ...promptRows,
        `${FILL}${pad("")}${RESET}`,
        `  ${GREEN}Fake Mini default · ${cwd.slice(-50)}${RESET}`,
        `  ${DIM}? for shortcuts${RESET}`,
      ]
      const last = chunks.at(-1) ?? ""
      cursor = { row: rows - 3, col: 3 + (st.input.length && last.length === width ? width : last.length) }
    }
    const room = rows - top.length - bottom.length
    // A prompt the user sent sits on a fill of its own, a blank row above and below, as in the real CLI.
    const convo = st.convo
      .flatMap((l) =>
        typeof l === "string"
          ? [l]
          : [
              `${USER}${pad("")}${RESET}`,
              `${USER}${pad(`› ${l.user}`)}${RESET}`,
              `${USER}${pad("")}${RESET}`,
              "",
            ],
      )
      .slice(-Math.max(0, room))
    const lines = [...top, ...convo]
    while (lines.length < rows - bottom.length) lines.push("")
    lines.push(...bottom)
    const spin = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"[Math.floor(Date.now() / 100) % 10]
    const title =
      st.phase === "dialog" && !st.dialog?.boxed
        ? "[ . ] Action Required | codex"
        : st.phase === "working"
          ? `${spin} codex`
          : "codex"
    paint(lines.slice(0, rows), cursor, title)
  }

  function paint(lines, cursor, title) {
    let s = "\x1b[?2026h\x1b[?25l"
    for (let i = 0; i < lines.length; i++) s += `\x1b[${i + 1};1H${lines[i]}\x1b[K`
    if (cursor) s += `\x1b[${cursor.row};${cursor.col}H\x1b[?25h`
    s += `\x1b]0;${title}\x07\x1b[?2026l`
    out(s)
  }

  const dialog = (d) =>
    new Promise((resolve) => {
      st.dialog = { selected: 0, ...d, resolve }
      st.phase = "dialog"
      render()
    })
  const close = (answer) => {
    const d = st.dialog
    st.dialog = null
    st.phase = st.turn ? "working" : "idle"
    render()
    d?.resolve(answer)
  }

  async function say(text, delay) {
    const idx = st.convo.push("• ") - 1
    for (const w of text.split(/(?<= )/)) {
      if (st.turn?.interrupted) break
      st.convo[idx] += w
      if (delay) {
        render()
        await sleep(delay)
      }
    }
    item({
      type: "AgentMessage",
      id: `msg_fake_${randomUUID().slice(0, 12)}`,
      content: [{ type: "Text", text }],
      phase: "final_answer",
    })
    render()
    return text
  }

  const asks = () => thread.approval !== "never"

  async function runTurn(text) {
    st.turn = { id: randomUUID(), interrupted: false }
    st.phase = "working"
    st.started = Date.now()
    thread.said.push(text)
    saveThread(thread)
    rollout(path, { type: "task_started", turn_id: st.turn.id })
    const ticker = setInterval(render, 300)
    render()
    let delay = 0
    const ds = directives(text)
    if (!ds.length) await say(`You said: ${text}`, delay)
    for (const [op, a] of ds) {
      if (st.turn.interrupted) break
      switch (op) {
        case "text":
          await say(a, delay)
          break
        case "slow":
          delay = Number(a) || 0
          break
        case "think":
          item({
            type: "Reasoning",
            id: `rs_fake_${randomUUID().slice(0, 8)}`,
            summary_text: [a],
            raw_content: [],
          })
          st.convo.push(`${DIM}• ${a}${RESET}`)
          break
        case "tool": {
          const sp = a.indexOf(" ")
          const name = sp < 0 ? a : a.slice(0, sp)
          const input = sp < 0 ? {} : JSON.parse(a.slice(sp + 1))
          const mcp = mcpClient(thread.mcp)
          st.convo.push(`• Called ${mcp.name}.${name}`)
          let r
          try {
            const res = await mcp.tool(name, input)
            r = { ok: !res.isError, text: (res.content ?? []).map((c) => c.text ?? "").join("\n"), res }
          } catch (e) {
            r = { ok: false, text: String(e.message ?? e) }
          }
          item({
            type: "McpToolCall",
            id: `call_fake_${randomUUID().slice(0, 8)}`,
            server: mcp.name,
            tool: name,
            arguments: input,
            status: r.ok ? "completed" : "failed",
            result: r.res ? { content: r.res.content ?? [] } : null,
            error: r.ok ? null : { message: r.text },
          })
          await say(r.ok ? `The tool said: ${r.text.slice(0, 400)}` : `The tool failed: ${r.text}`, delay)
          break
        }
        case "bash": {
          const id = `exec-${randomUUID()}`
          st.convo.push(`• Running ${a}`)
          let answer = 0
          if (asks())
            answer = await dialog({
              lines: [
                "\x1b[1mWould you like to run the following command?\x1b[22m",
                "",
                "Environment: local",
                "",
                "Reason: the fake asks before every command",
                "",
                `$ ${a}`,
              ],
              options: [
                "Yes, proceed (y)",
                `Yes, and don't ask again for commands that start with \`${a.split(" ")[0]}\` (p)`,
                "No, and tell Codex what to do differently (esc)",
              ],
            })
          if (st.turn.interrupted) break
          if (answer === "esc" || answer === 2) {
            item({
              type: "CommandExecution",
              id,
              command: ["/bin/bash", "-lc", a],
              status: "declined",
              aggregated_output: "",
              exit_code: null,
            })
            st.convo.push("  ✗ You canceled the request to run the command")
            if (answer === "esc") st.turn.interrupted = true
            else await say("I did not run it.", delay)
            break
          }
          if (answer === 1) thread.approval = "never"
          const r = run(a, cwd)
          item({
            type: "CommandExecution",
            id,
            command: ["/bin/bash", "-lc", a],
            status: r.code === 0 ? "completed" : "failed",
            aggregated_output: r.output,
            exit_code: r.code,
          })
          await say("Ran it.", delay)
          break
        }
        case "write": {
          const i = a.indexOf(":")
          const file = a.slice(0, i)
          const content = a.slice(i + 1)
          let answer = 0
          if (asks())
            answer = await dialog({
              lines: [
                "\x1b[1mWould you like to make the following edits?\x1b[22m",
                "",
                `${file} (+1 -0)`,
                `    1 +${content}`,
              ],
              options: [
                "Yes, proceed (y)",
                "Yes, and don't ask again for these files (a)",
                "No, and tell Codex what to do differently (esc)",
              ],
            })
          if (st.turn.interrupted) break
          const id = `patch-${randomUUID()}`
          if (answer === "esc" || answer === 2) {
            item({
              type: "FileChange",
              id,
              changes: { [file]: { type: "add", content } },
              status: "declined",
            })
            if (answer === "esc") st.turn.interrupted = true
            else await say(`I did not write ${file}.`, delay)
            break
          }
          writeFile(cwd, file, content)
          item({
            type: "FileChange",
            id,
            changes: { [file]: { type: "add", content, unified_diff: `+${content}` } },
            status: "completed",
          })
          await say(`Wrote ${file}.`, delay)
          break
        }
        case "question":
        case "dialog": {
          const [q, ...options] = a.split("|")
          const answer = await dialog({ boxed: true, lines: [q], options })
          if (st.turn.interrupted || answer === "esc") break
          await say(op === "question" ? `You picked ${options[answer]}.` : `${q}: ${options[answer]}`, delay)
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
          break
        }
        case "sleep": {
          const until = Date.now() + (Number(a) || 1000)
          while (Date.now() < until && !st.turn.interrupted) await sleep(25)
          break
        }
        case "recall":
          await say(`Before this you said: ${thread.said.slice(0, -1).join(" / ") || "nothing"}`, delay)
          break
        case "crash":
          out("\x1b[?1049l")
          process.stderr.write("fake-codex: crashing as asked\n")
          process.exit(3)
          break
        case "fail":
          st.convo.push(`■ ${a || "failed"}`)
          break
      }
    }
    clearInterval(ticker)
    rollout(path, {
      type: "token_count",
      info: {
        total_token_usage: { input_tokens: 100, cached_input_tokens: 40, output_tokens: 50 },
        last_token_usage: { input_tokens: 100, output_tokens: 50 },
        model_context_window: 200000,
      },
    })
    if (st.turn.interrupted) {
      st.convo.push("■ Conversation interrupted - tell the model what to do differently.")
      rollout(path, { type: "turn_aborted", turn_id: st.turn.id, reason: "interrupted" })
    } else {
      st.convo.push(`${DIM}─ Worked for 1s ─${RESET}`)
      rollout(path, { type: "task_complete", turn_id: st.turn.id, last_agent_message: null })
    }
    st.turn = null
    st.phase = "idle"
    render()
  }

  function submit() {
    const text = st.input
    st.input = ""
    if (!text.trim()) return render()
    st.convo.push({ user: text })
    rollout(path, {
      type: "item_completed",
      thread_id: thread.id,
      item: { type: "UserMessage", id: randomUUID(), content: [{ type: "text", text }] },
    })
    runTurn(text)
  }

  function key(k) {
    if (st.phase === "dialog" && st.dialog) {
      const d = st.dialog
      if (k === "up") d.selected = Math.max(0, d.selected - 1)
      else if (k === "down") d.selected = Math.min(d.options.length - 1, d.selected + 1)
      else if (k === "enter") return close(d.selected)
      else if (k === "esc") return close("esc")
      else if (/^[1-9]$/.test(k) && Number(k) <= d.options.length) return close(Number(k) - 1)
      return render()
    }
    if (st.phase !== "idle") {
      if (k === "esc" && st.turn) st.turn.interrupted = true
      return
    }
    if (k === "enter") return submit()
    if (k === "backspace") st.input = st.input.slice(0, -1)
    else if (k === "ctrl+u") st.input = ""
    else if (k === "ctrl+c") {
      out("\x1b[?2004l\x1b[?1049l")
      process.stdout.write(`To continue this session, run codex resume ${thread.id}\n`)
      process.exit(0)
    } else if (k.length === 1) st.input += k
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
      else if (c >= " ") key(c)
    }
  }

  if (process.stdin.isTTY) process.stdin.setRawMode(true)
  process.stdin.on("data", feed)
  process.stdout.on("resize", render)
  out("\x1b[?1049h\x1b[?2004h\x1b[2J")
  setTimeout(render, 150)
}

/** Text without escape sequences, for padding. */
function visible(s) {
  return s.replace(CSI, "")
}

// Claude Code's stream-json protocol (`claude -p --input-format stream-json --output-format stream-json
// --verbose --include-partial-messages --permission-prompt-tool stdio`), checked against CLI 2.1.325:
//
// - In: one JSON object per line. `{"type":"user","message":{"role":"user","content":[...]}}` sends a
//   message (also while a turn runs: the CLI takes it at its next step, which is how steering works).
//   `{"type":"control_request","request_id","request":{"subtype":...}}` asks the CLI something:
//   `initialize` (answers with its commands, models and permission mode), `interrupt`,
//   `set_permission_mode {mode}`, `set_model {model}`.
// - Out: `system` (`init` at the start of each query, with the session id, model, mode, tools, skills
//   and slash commands), `stream_event` (the API's streaming events: text and thinking deltas),
//   `assistant` (whole content blocks; several messages share one message id, one block each), `user`
//   (tool results), `result` (the end of a turn, with usage and cost), `control_response` (answers to
//   ours) and `control_request` (`can_use_tool`: a permission prompt, AskUserQuestion's questions and
//   ExitPlanMode's plan come this way; we answer with a `control_response` whose `response` is
//   `{behavior:"allow",updatedInput}` or `{behavior:"deny",message}`), and `control_cancel_request`.
//
// `ClaudeProtocol` turns those into session items. It knows nothing of processes, so the tests feed it
// recorded lines.
import type { Answer, Capabilities, DriverInput, HarnessCommand, Usage } from "./types"

/** The part of ctx.session the protocol reports to. */
export interface Reporter {
  item(item: { id: string; kind: string; [k: string]: unknown }): void
  delta(id: string, text: string, field?: string): void
  patch(id: string, patch: Record<string, unknown>): void
  state(state: "idle" | "working" | "awaitingApproval" | "awaitingInput", detail?: string): void
  meta(meta: {
    cliSession?: string
    model?: string
    mode?: string
    capabilities?: Capabilities
    usage?: Usage
  }): void
  turnEnd(error?: string): void
}

type Json = Record<string, any>

interface Pending {
  kind: "approval" | "question" | "plan"
  tool: string
  input: Json
  suggestions?: unknown[]
}

/**
 * Mode ids as sessions use them and as Claude Code names them. Since 2.1.3xx the CLI has no `default`
 * mode: its own default is `auto` (a classifier decides what to ask about), and asking before edits and
 * commands is `manual`. The session id for "ask first" stays `default`, so stored sessions and settings
 * keep their meaning. `cliMode` maps a session id to the CLI's, `sessionMode` the other way.
 */
export function cliMode(mode: string | undefined | null): string | undefined {
  if (!mode) return undefined
  return mode === "default" ? "manual" : mode
}

export function sessionMode(mode: string | undefined | null): string | undefined {
  if (!mode) return undefined
  return mode === "manual" ? "default" : mode
}

/**
 * What a CLI started by a host must not inherit: the markers of a Claude Code the host itself may run in
 * (a host started from a Claude Code terminal). With them the CLI thinks it is a child session and keeps
 * no transcript, so there is nothing to read and nothing can resume.
 */
export const INHERITED = [
  "CLAUDECODE",
  "CLAUDE_CODE_CHILD_SESSION",
  "CLAUDE_CODE_SESSION_ID",
  "CLAUDE_CODE_ENTRYPOINT",
  "CLAUDE_CODE_SESSION_ATTENDED",
  "CLAUDE_CODE_MESSAGING_SOCKET",
  "CLAUDE_CODE_MESSAGING_TOKEN",
  "CLAUDE_CODE_EXECPATH",
  "CLAUDE_AGENT_SDK_VERSION",
  "CLAUDE_PID",
  "CLAUDE_EFFORT",
]

/** The command line run through `env -u …`, so it starts without those. */
export function clean(command: string[]): string[] {
  return ["env", ...INHERITED.flatMap((v) => ["-u", v]), ...command]
}

/** The longest tool output kept in an item. */
const OUTPUT_MAX = 20_000

export class ClaudeProtocol {
  /** Requests the CLI asked us, by request id. */
  readonly pending = new Map<string, Pending>()
  /** Our requests to the CLI, by request id. */
  private readonly waiting = new Map<string, { resolve: (v: Json) => void; reject: (e: Error) => void }>()
  private counter = 0
  /** Per API message: how many text and thinking blocks streamed, and how many arrived whole. */
  private readonly blocks = new Map<
    string,
    { streamed: Record<string, number>; whole: Record<string, number> }
  >()
  /** Stream index → item id, per message, for deltas. */
  private readonly streaming = new Map<string, string>()
  private currentMessage = ""
  private tools = new Map<string, string>()
  capabilities: Capabilities
  sessionId: string | undefined
  mode: string | undefined
  private skills = new Set<string>()
  /** The CLI reports a resolved model id (`claude-haiku-5-5`); the picker's ids are aliases (`haiku`). */
  private resolved = new Map<string, string>()
  private commands: HarnessCommand[] = []

  constructor(
    private readonly out: Reporter,
    private readonly write: (line: string) => void,
    base: Capabilities,
  ) {
    this.capabilities = { ...base }
  }

  // ── requests to the CLI ──

  request(subtype: string, fields: Json = {}, timeoutMs = 60_000): Promise<Json> {
    const id = `parrot-${++this.counter}`
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.waiting.delete(id)
        reject(new Error(`the CLI did not answer ${subtype} in ${timeoutMs} ms`))
      }, timeoutMs)
      this.waiting.set(id, {
        resolve: (v) => {
          clearTimeout(timer)
          resolve(v)
        },
        reject: (e) => {
          clearTimeout(timer)
          reject(e)
        },
      })
      this.write(JSON.stringify({ type: "control_request", request_id: id, request: { subtype, ...fields } }))
    })
  }

  /** The CLI went away: requests in flight fail. */
  closed(why: string) {
    for (const w of this.waiting.values()) w.reject(new Error(why))
    this.waiting.clear()
  }

  async initialize(): Promise<Json> {
    const r = await this.request("initialize")
    this.applyInit(r)
    return r
  }

  /** The user message for a turn: the context first, then what was written, then images and files. */
  userMessage(input: DriverInput, files: Array<{ name: string; contentType: string; base64: string }> = []) {
    const content: Json[] = []
    for (const c of input.context ?? []) {
      content.push({
        type: "text",
        text: `<context uri="${c.uri}"${c.title ? ` title="${escapeAttr(c.title)}"` : ""}>\n${c.text}\n</context>`,
      })
    }
    for (const f of files) {
      if (f.contentType.startsWith("image/")) {
        content.push({ type: "image", source: { type: "base64", media_type: f.contentType, data: f.base64 } })
      } else if (f.contentType === "application/pdf") {
        content.push({
          type: "document",
          source: { type: "base64", media_type: f.contentType, data: f.base64 },
        })
      } else {
        content.push({
          type: "document",
          title: f.name,
          source: { type: "text", media_type: "text/plain", data: decodeBase64(f.base64) },
        })
      }
    }
    content.push({ type: "text", text: input.text })
    return JSON.stringify({
      type: "user",
      message: { role: "user", content },
      parent_tool_use_id: null,
      session_id: this.sessionId ?? "",
    })
  }

  /** The answer to a permission prompt or question. */
  answer(requestId: string, answer: Answer): boolean {
    const p = this.pending.get(requestId)
    if (!p) return false
    this.pending.delete(requestId)
    let response: Json
    if (p.kind === "question") {
      const answers: Record<string, string> = { ...(answer.answers ?? {}) }
      const first = p.input.questions?.[0]?.question
      if (!Object.keys(answers).length && answer.message && first) answers[first] = answer.message
      response = { behavior: "allow", updatedInput: { ...p.input, answers } }
    } else if (answer.action === "allow" || answer.action === "allowSession") {
      response = { behavior: "allow", updatedInput: p.input }
      if (answer.action === "allowSession" && p.suggestions?.length)
        response.updatedPermissions = p.suggestions
      if (p.kind === "plan") {
        this.out.patch(`plan-${requestId}`, { status: "approved" })
        this.mode = "default"
        this.out.meta({ mode: "default" })
      }
    } else {
      response = {
        behavior: "deny",
        message:
          answer.message?.trim() || "The user denied this. Do not try it again; ask what to do instead.",
      }
      if (p.kind === "plan") this.out.patch(`plan-${requestId}`, { status: "rejected" })
    }
    this.write(
      JSON.stringify({
        type: "control_response",
        response: { subtype: "success", request_id: requestId, response },
      }),
    )
    return true
  }

  // ── what the CLI says ──

  line(text: string) {
    const t = text.trim()
    if (!t) return
    let m: Json
    try {
      m = JSON.parse(t)
    } catch {
      return
    }
    this.message(m)
  }

  message(m: Json) {
    switch (m.type) {
      case "control_response": {
        const r = m.response ?? {}
        const w = this.waiting.get(r.request_id)
        if (!w) return
        this.waiting.delete(r.request_id)
        if (r.subtype === "error") w.reject(new Error(r.error ?? "the CLI refused the request"))
        else w.resolve(r.response ?? {})
        return
      }
      case "control_request":
        return this.controlRequest(m.request_id, m.request ?? {})
      case "control_cancel_request": {
        const id = m.request_id
        if (this.pending.delete(id)) this.out.patch(id, { status: "cancelled" })
        return
      }
      case "system":
        return this.system(m)
      case "stream_event":
        if (m.parent_tool_use_id) return // a subagent's: shown as its Task step
        return this.streamEvent(m.event ?? {})
      case "assistant":
        return this.assistant(m)
      case "user":
        return this.toolResults(m)
      case "result":
        return this.result(m)
    }
  }

  private applyInit(r: Json) {
    if (Array.isArray(r.models)) {
      for (const x of r.models as Json[]) {
        if (x.resolvedModel && !this.resolved.has(String(x.resolvedModel)) && x.value !== "default") {
          this.resolved.set(String(x.resolvedModel), String(x.value))
        }
      }
      this.capabilities.models = r.models.map((x: Json) => ({
        id: String(x.value),
        label: String(x.displayName ?? x.value),
        ...(x.description ? { description: String(x.description) } : {}),
        ...(Array.isArray(x.supportedEffortLevels) ? { efforts: x.supportedEffortLevels.map(String) } : {}),
      }))
    }
    if (Array.isArray(r.commands)) {
      this.commands = r.commands.map((c: Json) => ({
        name: String(c.name),
        ...(c.description ? { description: String(c.description) } : {}),
        ...(c.argumentHint ? { argumentHint: String(c.argumentHint) } : {}),
      }))
      this.splitCommands()
    }
    if (r.current_permission_mode) this.mode = sessionMode(String(r.current_permission_mode))
    this.out.meta({ capabilities: this.capabilities, ...(this.mode ? { mode: this.mode } : {}) })
  }

  private splitCommands() {
    const all = this.commands
    this.capabilities.skills = all.filter((c) => this.skills.has(c.name))
    this.capabilities.commands = all.filter((c) => !this.skills.has(c.name))
  }

  private system(m: Json) {
    if (m.subtype === "init") {
      if (m.session_id) this.sessionId = String(m.session_id)
      if (Array.isArray(m.skills)) {
        this.skills = new Set(
          m.skills.map((s: unknown) => (typeof s === "string" ? s : String((s as Json)?.name))),
        )
        if (this.commands.length) this.splitCommands()
        else {
          this.capabilities.skills = [...this.skills].map((name) => ({ name }))
          this.capabilities.commands = (m.slash_commands ?? [])
            .filter((c: string) => !this.skills.has(c))
            .map((name: string) => ({ name }))
        }
      }
      if (m.permissionMode) this.mode = sessionMode(String(m.permissionMode))
      this.out.meta({
        cliSession: this.sessionId,
        ...(m.model ? { model: this.resolved.get(String(m.model)) ?? String(m.model) } : {}),
        ...(this.mode ? { mode: this.mode } : {}),
        capabilities: this.capabilities,
      })
    } else if (m.subtype === "compact_boundary") {
      this.out.item({
        id: m.uuid ?? `compact-${Date.now()}`,
        kind: "notice",
        text: "The conversation was compacted.",
      })
    }
  }

  private counts(msg: string) {
    let c = this.blocks.get(msg)
    if (!c) {
      c = { streamed: {}, whole: {} }
      this.blocks.set(msg, c)
      // Only the last few messages matter.
      if (this.blocks.size > 50) this.blocks.delete(this.blocks.keys().next().value!)
    }
    return c
  }

  private blockId(msg: string, kind: "text" | "thinking", which: "streamed" | "whole") {
    const c = this.counts(msg)[which]
    c[kind] = (c[kind] ?? 0) + 1
    return `${msg}:${kind === "text" ? "t" : "r"}${c[kind]}`
  }

  private streamEvent(e: Json) {
    switch (e.type) {
      case "message_start":
        this.currentMessage = String(e.message?.id ?? `m${Date.now()}`)
        return
      case "content_block_start": {
        const b = e.content_block ?? {}
        const msg = this.currentMessage
        if (b.type === "text") {
          const id = this.blockId(msg, "text", "streamed")
          this.streaming.set(`${msg}#${e.index}`, id)
          this.out.item({ id, kind: "assistant", text: b.text ?? "", streaming: true })
        } else if (b.type === "thinking") {
          const id = this.blockId(msg, "thinking", "streamed")
          this.streaming.set(`${msg}#${e.index}`, id)
          this.out.item({ id, kind: "reasoning", text: b.thinking ?? "", streaming: true })
        } else if (b.type === "tool_use" && b.id) {
          // Show the step as soon as it starts; its input comes with the whole block.
          this.tools.set(String(b.id), String(b.name))
          this.out.item({
            id: String(b.id),
            kind: "tool",
            name: String(b.name),
            input: {},
            status: "running",
            source: "harness",
          })
        }
        return
      }
      case "content_block_delta": {
        const id = this.streaming.get(`${this.currentMessage}#${e.index}`)
        if (!id) return
        const d = e.delta ?? {}
        if (d.type === "text_delta" && d.text) this.out.delta(id, String(d.text))
        else if (d.type === "thinking_delta" && d.thinking) this.out.delta(id, String(d.thinking))
        return
      }
    }
  }

  private assistant(m: Json) {
    const msg = String(m.message?.id ?? m.uuid ?? `m${Date.now()}`)
    const sub = m.parent_tool_use_id ? String(m.parent_tool_use_id) : null
    for (const b of m.message?.content ?? []) {
      if (sub) {
        // A subagent's steps: only its tool calls, under the Task step.
        if (b.type === "tool_use") {
          this.tools.set(String(b.id), String(b.name))
          this.out.item({
            id: String(b.id),
            kind: "tool",
            name: String(b.name),
            title: describe(b.name, b.input),
            input: b.input ?? {},
            status: "running",
            source: "harness",
            parent: sub,
          })
        }
        continue
      }
      if (b.type === "text") {
        this.out.item({
          id: this.blockId(msg, "text", "whole"),
          kind: "assistant",
          text: String(b.text ?? ""),
          streaming: false,
        })
      } else if (b.type === "thinking") {
        // Counted either way; a block without text (the transcript keeps only its signature) shows nothing.
        const id = this.blockId(msg, "thinking", "whole")
        if (String(b.thinking ?? "").trim())
          this.out.item({ id, kind: "reasoning", text: String(b.thinking), streaming: false })
      } else if (b.type === "tool_use") {
        this.tools.set(String(b.id), String(b.name))
        this.out.item({
          id: String(b.id),
          kind: "tool",
          name: String(b.name),
          title: describe(b.name, b.input),
          input: b.input ?? {},
          status: "running",
          source: "harness",
        })
        if (b.name === "TodoWrite" && Array.isArray(b.input?.todos)) {
          this.out.item({
            id: `todos-${b.id}`,
            kind: "plan",
            text: b.input.todos
              .map(
                (t: Json) => `- [${t.status === "completed" ? "x" : " "}] ${t.content ?? t.activeForm ?? ""}`,
              )
              .join("\n"),
            todos: b.input.todos,
          })
        }
      }
    }
    const usage = m.message?.usage
    if (usage && !sub) this.out.meta({ usage: usageOf(usage) })
  }

  private toolResults(m: Json) {
    const content = m.message?.content
    if (!Array.isArray(content)) return
    for (const b of content) {
      if (b?.type !== "tool_result" || !b.tool_use_id) continue
      const text = resultText(b.content)
      this.out.patch(String(b.tool_use_id), {
        status: b.is_error ? "error" : "ok",
        output: text.length > OUTPUT_MAX ? `${text.slice(0, OUTPUT_MAX)}\n…` : text,
      })
    }
  }

  private result(m: Json) {
    const usage = usageOf(m.usage ?? {})
    if (typeof m.total_cost_usd === "number") usage.costUsd = m.total_cost_usd
    const models = Object.values(m.modelUsage ?? {}) as Json[]
    const window = models.find((x) => typeof x.contextWindow === "number")?.contextWindow
    if (window) usage.contextWindow = window
    this.out.meta({ usage })
    const failed = m.is_error || (m.subtype && m.subtype !== "success")
    const interrupted = m.terminal_reason === "interrupted" || m.subtype === "error_during_execution"
    if (failed && !interrupted) {
      this.out.turnEnd(
        typeof m.result === "string" && m.result ? m.result : `The turn ended with ${m.subtype}`,
      )
    } else {
      this.out.turnEnd()
    }
    // Requests left open belong to the turn that ended.
    for (const id of this.pending.keys()) this.out.patch(id, { status: "cancelled" })
    this.pending.clear()
  }

  private controlRequest(id: string, r: Json) {
    if (r.subtype !== "can_use_tool") {
      this.write(
        JSON.stringify({
          type: "control_response",
          response: { subtype: "error", request_id: id, error: `Parrot does not answer ${r.subtype}` },
        }),
      )
      return
    }
    const tool = String(r.tool_name ?? "")
    const input: Json = r.input ?? {}
    if (tool === "AskUserQuestion") {
      this.pending.set(id, { kind: "question", tool, input })
      this.out.item({
        id,
        kind: "question",
        questions: (input.questions ?? []).map((q: Json) => ({
          question: String(q.question ?? ""),
          ...(q.header ? { header: String(q.header) } : {}),
          options: (q.options ?? []).map((o: Json) => ({
            label: String(o.label ?? ""),
            ...(o.description ? { description: String(o.description) } : {}),
          })),
          multiSelect: !!q.multiSelect,
        })),
        status: "pending",
        toolUseId: r.tool_use_id,
      })
      return
    }
    if (tool === "ExitPlanMode") {
      this.pending.set(id, { kind: "plan", tool, input })
      this.out.item({ id: `plan-${id}`, kind: "plan", text: String(input.plan ?? ""), status: "proposed" })
      this.out.item({
        id,
        kind: "approval",
        tool,
        summary: "Leave plan mode and start on the plan",
        status: "pending",
        allowLabel: "Start",
        denyLabel: "Keep planning",
        toolUseId: r.tool_use_id,
      })
      return
    }
    this.pending.set(id, { kind: "approval", tool, input, suggestions: r.permission_suggestions })
    this.out.item({
      id,
      kind: "approval",
      tool,
      summary: summarize(tool, r.description),
      items: affected(tool, input, r.blocked_path),
      input,
      status: "pending",
      toolUseId: r.tool_use_id,
      canAllowSession: Array.isArray(r.permission_suggestions) && r.permission_suggestions.length > 0,
    })
  }
}

function escapeAttr(s: string) {
  return s.replace(/&/g, "&amp;").replace(/"/g, "&quot;")
}

function decodeBase64(b64: string): string {
  const bin = atob(b64)
  const bytes = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
  return new TextDecoder().decode(bytes)
}

function usageOf(u: Json): Usage {
  return {
    inputTokens: Number(u.input_tokens ?? 0),
    outputTokens: Number(u.output_tokens ?? 0),
    cacheReadTokens: Number(u.cache_read_input_tokens ?? 0),
    cacheWriteTokens: Number(u.cache_creation_input_tokens ?? 0),
    contextTokens:
      Number(u.input_tokens ?? 0) +
      Number(u.cache_read_input_tokens ?? 0) +
      Number(u.cache_creation_input_tokens ?? 0),
  }
}

function resultText(c: unknown): string {
  if (typeof c === "string") return c
  if (Array.isArray(c)) {
    return c
      .map((x: Json) => (x?.type === "text" ? String(x.text ?? "") : x?.type === "image" ? "[image]" : ""))
      .filter(Boolean)
      .join("\n")
  }
  return c == null ? "" : JSON.stringify(c)
}

/** A short line for a tool step: what it does to what. */
export function describe(tool: string, input: Json = {}): string {
  const path = input.file_path ?? input.path ?? input.notebook_path
  switch (tool) {
    case "Bash":
      return String(input.command ?? "")
    case "Read":
    case "Write":
    case "Edit":
    case "MultiEdit":
    case "NotebookEdit":
      return String(path ?? "")
    case "Glob":
    case "Grep":
      return String(input.pattern ?? "")
    case "WebFetch":
      return String(input.url ?? "")
    case "WebSearch":
      return String(input.query ?? "")
    case "Task":
    case "Agent":
      return String(input.description ?? input.prompt ?? "")
    default:
      if (tool.startsWith("mcp__")) {
        const [, server, name] = tool.split("__")
        return `${server}: ${name ?? ""}`
      }
      return path ? String(path) : ""
  }
}

function summarize(tool: string, description?: string): string {
  switch (tool) {
    case "Bash":
      return description ? `Run a command: ${description}` : "Run a command"
    case "Write":
      return "Create or overwrite a file"
    case "Edit":
    case "MultiEdit":
      return "Edit a file"
    case "NotebookEdit":
      return "Edit a notebook"
    case "WebFetch":
      return "Fetch a web page"
    default:
      if (tool.startsWith("mcp__")) {
        const [, server, name] = tool.split("__")
        return `Use ${name ?? tool} from ${server}`
      }
      return description ? `${tool}: ${description}` : `Use ${tool}`
  }
}

function affected(tool: string, input: Json, blocked?: string): string[] {
  const out: string[] = []
  if (tool === "Bash" && input.command) out.push(String(input.command))
  const path = input.file_path ?? input.path ?? input.notebook_path
  if (path) out.push(String(path))
  if (tool === "WebFetch" && input.url) out.push(String(input.url))
  if (!out.length && blocked) out.push(String(blocked))
  return out
}

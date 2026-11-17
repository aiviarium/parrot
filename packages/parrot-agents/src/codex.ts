// Codex's app-server protocol (`codex app-server`, JSON-RPC 2.0 without the "jsonrpc" field, one JSON
// object per line over stdio), checked against CLI 0.167.1 by generating its schema
// (`codex app-server generate-ts --experimental`) and by running it:
//
// - We ask: `initialize` (then the `initialized` notification), `model/list`, `skills/list`,
//   `thread/start` or `thread/resume` (with per-thread `config`, which is where an MCP server goes, so
//   nothing is written to ~/.codex/config.toml), `turn/start` (with per-turn overrides of model, effort,
//   approval policy, sandbox policy and collaboration mode), `turn/steer` (needs the running turn's id),
//   `turn/interrupt`, `thread/compact/start` and `review/start`.
// - It tells: `thread/started`, `turn/started`, `item/started` and `item/completed` (a `ThreadItem`:
//   userMessage, agentMessage, reasoning, commandExecution, fileChange, mcpToolCall, dynamicToolCall,
//   webSearch, plan, imageView, contextCompaction, collabAgentToolCall …), streamed deltas
//   (`item/agentMessage/delta`, `item/reasoning/summaryTextDelta`, `item/reasoning/textDelta`,
//   `item/commandExecution/outputDelta`, `item/fileChange/outputDelta`), `turn/plan/updated`,
//   `thread/tokenUsage/updated`, `turn/completed` (status completed, interrupted or failed), `error`, and
//   `serverRequest/resolved` when a request of its own was settled elsewhere.
// - It asks (requests with an id, which may be the number 0): `item/commandExecution/requestApproval`
//   and `item/fileChange/requestApproval` (decisions accept, acceptForSession, decline, cancel),
//   `item/permissions/requestApproval`, `item/tool/requestUserInput` (questions with options),
//   `mcpServer/elicitation/request`, and the older `execCommandApproval` / `applyPatchApproval`.
//
// `CodexProtocol` turns that into session items. It knows nothing of processes, so the tests
// feed it recorded lines.
import type { Answer, Capabilities, DriverInput, HarnessCommand, Usage } from "./types"

export interface Reporter {
  item(item: { id: string; kind: string; [k: string]: unknown }): void
  delta(id: string, text: string, field?: string): void
  patch(id: string, patch: Record<string, unknown>): void
  state(state: "idle" | "working" | "awaitingApproval" | "awaitingInput", detail?: string): void
  meta(meta: {
    cliSession?: string
    model?: string
    effort?: string
    mode?: string
    capabilities?: Capabilities
    usage?: Usage
  }): void
  turnEnd(error?: string): void
}

type Json = Record<string, any>

/** How a mode id maps to Codex's approval policy, sandbox and collaboration mode. */
export const MODES: Record<string, { approval: string; sandbox: string; plan?: boolean }> = {
  default: { approval: "on-request", sandbox: "workspace-write" },
  "read-only": { approval: "on-request", sandbox: "read-only" },
  untrusted: { approval: "untrusted", sandbox: "workspace-write" },
  plan: { approval: "on-request", sandbox: "read-only", plan: true },
  full: { approval: "never", sandbox: "danger-full-access" },
}

/** The sandbox as `turn/start` wants it (a policy object, where `thread/start` takes the mode's name). */
export function sandboxPolicy(mode: string, cwd: string): Json {
  switch (mode) {
    case "read-only":
      return { type: "readOnly", networkAccess: false }
    case "danger-full-access":
      return { type: "dangerFullAccess" }
    default:
      return {
        type: "workspaceWrite",
        writableRoots: [cwd],
        networkAccess: false,
        excludeTmpdirEnvVar: false,
        excludeSlashTmp: false,
      }
  }
}

interface Pending {
  /** The request id as Codex sent it (a number or a string). */
  raw: string | number
  method: string
  params: Json
  questions?: Array<{ id: string; question: string }>
}

const OUTPUT_MAX = 20_000

/** Commands the protocol offers as methods rather than as text. */
export const COMMANDS: HarnessCommand[] = [
  { name: "compact", description: "Summarise the conversation to free context" },
  { name: "review", description: "Review the uncommitted changes", argumentHint: "[instructions]" },
]

export class CodexProtocol {
  /** Its requests to us, by item id (`codex-<request id>`). */
  readonly pending = new Map<string, Pending>()
  private readonly waiting = new Map<string, { resolve: (v: Json) => void; reject: (e: Error) => void }>()
  private counter = 0
  capabilities: Capabilities
  threadId: string | undefined
  turnId: string | undefined
  model: string | undefined
  effort: string | undefined
  mode = "default"
  /** Items it started, by id: what kind we made them, for deltas and completion. */
  private readonly items = new Map<string, { kind: string; text?: string }>()
  /** File changes by item id, so an approval can say which files. */
  private readonly changes = new Map<string, string[]>()
  private running = false

  constructor(
    private readonly out: Reporter,
    private readonly write: (line: string) => void,
    base: Capabilities,
    private readonly cwd: string,
  ) {
    this.capabilities = { ...base, commands: [...COMMANDS] }
  }

  // ── requests to Codex ──

  request(method: string, params: Json = {}, timeoutMs = 60_000): Promise<Json> {
    const id = `parrot-${++this.counter}`
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.waiting.delete(id)
        reject(new Error(`Codex did not answer ${method} in ${timeoutMs} ms`))
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
      this.write(JSON.stringify({ id, method, params }))
    })
  }

  notify(method: string, params: Json = {}) {
    this.write(JSON.stringify({ method, params }))
  }

  closed(why: string) {
    for (const w of this.waiting.values()) w.reject(new Error(why))
    this.waiting.clear()
  }

  get busy() {
    return this.running
  }

  /** initialize, the models and skills, then a new thread or the old one. */
  async open(opts: {
    version: string
    resume?: string
    model?: string
    effort?: string
    mode?: string
    mcp?: { name: string; url: string; tokenVar: string }
  }): Promise<string> {
    await this.request("initialize", {
      clientInfo: { name: "parrot", title: "Parrot", version: opts.version },
      capabilities: { experimentalApi: true, requestAttestation: false },
    })
    this.notify("initialized")
    try {
      const models = await this.request("model/list", { limit: 100 })
      this.applyModels(models.data ?? [])
    } catch {
      // An older Codex without model/list: the manifest's defaults stay.
    }
    try {
      const sk = await this.request("skills/list", { cwds: [this.cwd] })
      const skills: HarnessCommand[] = []
      for (const e of sk.data ?? [])
        for (const s of e.skills ?? [])
          if (s.enabled !== false)
            skills.push({
              name: String(s.name),
              ...(s.shortDescription || s.description
                ? { description: String(s.shortDescription || s.description) }
                : {}),
            })
      this.capabilities.skills = skills
      this.skillPaths = new Map(
        (sk.data ?? []).flatMap((e: Json) => (e.skills ?? []).map((s: Json) => [String(s.name), s.path])),
      )
    } catch {}
    this.mode = opts.mode && MODES[opts.mode] ? opts.mode : "default"
    const m = MODES[this.mode]!
    const config: Json = {}
    if (opts.mcp) {
      config.mcp_servers = {
        [opts.mcp.name]: {
          url: opts.mcp.url,
          bearer_token_env_var: opts.mcp.tokenVar,
          // A `confirm` tool waits for a person.
          tool_timeout_sec: 24 * 3600,
          startup_timeout_sec: 30,
        },
      }
    }
    const common: Json = {
      cwd: this.cwd,
      approvalPolicy: m.approval,
      sandbox: m.sandbox,
      config,
      ...(opts.model && opts.model !== "default" ? { model: opts.model } : {}),
    }
    const r = opts.resume
      ? await this.request("thread/resume", { threadId: opts.resume, excludeTurns: true, ...common })
      : await this.request("thread/start", { ...common, serviceName: "parrot" })
    this.threadId = String(r.thread?.id)
    this.model = r.model ?? r.thread?.model ?? undefined
    this.effort = opts.effort ?? r.reasoningEffort ?? r.thread?.reasoningEffort ?? undefined
    this.out.meta({
      cliSession: this.threadId,
      ...(this.model ? { model: this.model } : {}),
      ...(this.effort ? { effort: this.effort } : {}),
      mode: this.mode,
      capabilities: this.capabilities,
    })
    return this.threadId
  }

  private skillPaths = new Map<string, string>()

  private applyModels(list: Json[]) {
    const visible = list.filter((x) => !x.hidden)
    if (!visible.length) return
    this.capabilities.models = visible.map((x) => ({
      id: String(x.id ?? x.model),
      label: String(x.displayName ?? x.id),
      ...(x.description ? { description: String(x.description) } : {}),
      efforts: (x.supportedReasoningEfforts ?? []).map((e: Json) => String(e.reasoningEffort)),
    }))
    const efforts = new Map<string, string>()
    for (const x of visible)
      for (const e of x.supportedReasoningEfforts ?? [])
        if (!efforts.has(String(e.reasoningEffort)))
          efforts.set(String(e.reasoningEffort), String(e.description ?? ""))
    if (efforts.size)
      this.capabilities.effort = [...efforts].map(([id, description]) => ({
        id,
        label: id === "xhigh" ? "Extra high" : id[0]!.toUpperCase() + id.slice(1),
        ...(description ? { description } : {}),
      }))
  }

  /** The input of a turn: the context first, then what was written; images as data URLs. */
  input(
    input: DriverInput,
    files: Array<{ name: string; contentType: string; base64: string }> = [],
  ): Json[] {
    const out: Json[] = []
    const texts: string[] = []
    for (const c of input.context ?? [])
      texts.push(
        `<context uri="${c.uri}"${c.title ? ` title="${escapeAttr(c.title)}"` : ""}>\n${c.text}\n</context>`,
      )
    for (const f of files) {
      if (f.contentType.startsWith("image/")) {
        out.push({ type: "image", url: `data:${f.contentType};base64,${f.base64}` })
      } else {
        texts.push(`<file name="${escapeAttr(f.name)}">\n${decodeBase64(f.base64)}\n</file>`)
      }
    }
    // `$skill` chips: the protocol's skill input, so Codex loads it.
    for (const m of input.text.matchAll(/(?:^|\s)\$([\w.-]+)/g)) {
      const path = this.skillPaths.get(m[1]!)
      if (path) out.push({ type: "skill", name: m[1], path })
    }
    texts.push(input.text)
    out.unshift({ type: "text", text: texts.join("\n\n"), text_elements: [] })
    return out
  }

  /** Starts a turn. `/compact` and `/review` are protocol methods, not text. */
  async turn(input: DriverInput, files: Array<{ name: string; contentType: string; base64: string }> = []) {
    const t = input.text.trim()
    this.running = true
    try {
      if (t === "/compact") {
        await this.request("thread/compact/start", { threadId: this.threadId })
        return
      }
      if (t === "/review" || t.startsWith("/review ")) {
        const instructions = t.slice(7).trim()
        await this.request("review/start", {
          threadId: this.threadId,
          target: instructions ? { type: "custom", instructions } : { type: "uncommittedChanges" },
        })
        return
      }
      const m = MODES[this.mode] ?? MODES.default!
      const r = await this.request("turn/start", {
        threadId: this.threadId,
        input: this.input(input, files),
        approvalPolicy: m.approval,
        sandboxPolicy: sandboxPolicy(m.sandbox, this.cwd),
        ...(this.model ? { model: this.model } : {}),
        ...(this.effort ? { effort: this.effort } : {}),
        ...(this.model
          ? {
              collaborationMode: {
                mode: m.plan ? "plan" : "default",
                settings: {
                  model: this.model,
                  reasoning_effort: this.effort ?? null,
                  developer_instructions: null,
                },
              },
            }
          : {}),
      })
      if (r.turn?.id) this.turnId = String(r.turn.id)
    } catch (e) {
      this.running = false
      throw e
    }
  }

  async steer(input: DriverInput, files: Array<{ name: string; contentType: string; base64: string }> = []) {
    if (!this.turnId || !this.running) return this.turn(input, files)
    await this.request("turn/steer", {
      threadId: this.threadId,
      input: this.input(input, files),
      expectedTurnId: this.turnId,
    })
  }

  async interrupt() {
    if (!this.turnId) return
    try {
      await this.request("turn/interrupt", { threadId: this.threadId, turnId: this.turnId }, 15_000)
    } catch (e) {
      // The turn ended while the interrupt was on its way: nothing is left to stop.
      if (!/no active turn/i.test(String((e as Error).message))) throw e
    }
  }

  /** The answer to one of its requests. */
  answer(itemId: string, answer: Answer): boolean {
    const p = this.pending.get(itemId)
    if (!p) return false
    this.pending.delete(itemId)
    const allow = answer.action === "allow" || answer.action === "allowSession"
    const session = answer.action === "allowSession"
    let result: Json
    switch (p.method) {
      case "item/commandExecution/requestApproval":
      case "item/fileChange/requestApproval":
        result = { decision: allow ? (session ? "acceptForSession" : "accept") : "decline" }
        break
      case "execCommandApproval":
      case "applyPatchApproval":
        result = { decision: allow ? (session ? "approved_for_session" : "approved") : "denied" }
        break
      case "item/permissions/requestApproval":
        result = allow
          ? { permissions: p.params.permissions ?? {}, scope: session ? "session" : "turn" }
          : { permissions: {}, scope: "turn" }
        break
      case "mcpServer/elicitation/request":
        result = { action: allow ? "accept" : "decline", content: allow ? {} : null, _meta: null }
        break
      case "item/tool/requestUserInput": {
        const answers: Json = {}
        for (const q of p.questions ?? []) {
          const a = answer.answers?.[q.question] ?? (p.questions!.length === 1 ? answer.message : undefined)
          if (a != null) answers[q.id] = { answers: String(a).split(", ").filter(Boolean) }
        }
        result = { answers }
        break
      }
      default:
        result = {}
    }
    this.write(JSON.stringify({ id: p.raw, result }))
    return true
  }

  // ── what Codex says ──

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
    if (m.id != null && m.method == null) {
      const w = this.waiting.get(String(m.id))
      if (!w) return
      this.waiting.delete(String(m.id))
      if (m.error) w.reject(new Error(String(m.error.message ?? "Codex refused the request")))
      else w.resolve(m.result ?? {})
      return
    }
    if (m.id != null && m.method) return this.serverRequest(m.id, String(m.method), m.params ?? {})
    if (m.method) return this.notification(String(m.method), m.params ?? {})
  }

  private notification(method: string, p: Json) {
    switch (method) {
      case "turn/started":
        if (p.turn?.id) this.turnId = String(p.turn.id)
        this.running = true
        return
      case "item/started":
        return this.started(p.item ?? {})
      case "item/completed":
        return this.completed(p.item ?? {})
      case "item/agentMessage/delta":
        return this.delta(String(p.itemId), String(p.delta ?? ""))
      case "item/reasoning/summaryTextDelta":
      case "item/reasoning/textDelta":
        return this.reasoningDelta(String(p.itemId), String(p.delta ?? ""))
      case "item/reasoning/summaryPartAdded":
        return this.reasoningDelta(String(p.itemId), "\n\n")
      case "item/commandExecution/outputDelta":
      case "item/fileChange/outputDelta":
        if (this.items.has(String(p.itemId)))
          this.out.delta(String(p.itemId), String(p.delta ?? ""), "output")
        return
      case "item/plan/delta":
        return this.delta(String(p.itemId), String(p.delta ?? ""))
      case "turn/plan/updated":
        return this.out.item({
          id: `todos-${p.turnId}`,
          kind: "plan",
          text: [
            ...(p.explanation ? [String(p.explanation), ""] : []),
            ...(p.plan ?? []).map(
              (s: Json) => `- [${s.status === "completed" ? "x" : " "}] ${String(s.step ?? "")}`,
            ),
          ].join("\n"),
          todos: (p.plan ?? []).map((s: Json) => ({ content: s.step, status: s.status })),
        })
      case "thread/tokenUsage/updated":
        return this.out.meta({ usage: usageOf(p.tokenUsage ?? {}) })
      case "turn/completed": {
        const turn = p.turn ?? {}
        this.running = false
        const failed = turn.status === "failed"
        this.out.turnEnd(failed ? String(turn.error?.message ?? "The turn failed.") : undefined)
        // Requests left open belong to the turn that ended.
        for (const id of this.pending.keys()) this.out.patch(id, { status: "cancelled" })
        this.pending.clear()
        return
      }
      case "error":
        if (!p.willRetry && p.error?.message)
          this.out.item({ id: `err-${Date.now()}`, kind: "error", message: String(p.error.message) })
        return
      case "serverRequest/resolved": {
        for (const [id, q] of this.pending)
          if (String(q.raw) === String(p.requestId)) {
            this.pending.delete(id)
            this.out.patch(id, { status: "cancelled" })
          }
        return
      }
      case "thread/compacted":
        return this.out.item({
          id: `compact-${Date.now()}`,
          kind: "notice",
          text: "The conversation was compacted.",
        })
      case "model/rerouted":
        if (p.toModel) this.out.meta({ model: String(p.toModel) })
        return
    }
  }

  private delta(id: string, text: string) {
    if (!text) return
    const it = this.items.get(id)
    if (!it) {
      this.items.set(id, { kind: "assistant", text })
      this.out.item({ id, kind: "assistant", text, streaming: true })
      return
    }
    it.text = (it.text ?? "") + text
    this.out.delta(id, text)
  }

  private reasoningDelta(id: string, text: string) {
    if (!text) return
    const it = this.items.get(id)
    if (!it?.text) {
      if (!text.trim()) return
      this.items.set(id, { kind: "reasoning", text })
      this.out.item({ id, kind: "reasoning", text, streaming: true })
      return
    }
    it.text += text
    this.out.delta(id, text)
  }

  private started(it: Json) {
    const id = String(it.id ?? "")
    if (!id) return
    switch (it.type) {
      case "agentMessage":
        this.items.set(id, { kind: "assistant", text: String(it.text ?? "") })
        this.out.item({ id, kind: "assistant", text: String(it.text ?? ""), streaming: true })
        return
      case "reasoning":
        // Shown once it has text; Codex often sends reasoning items with nothing in them.
        this.items.set(id, { kind: "reasoning", text: "" })
        return
      case "plan":
        this.items.set(id, { kind: "plan", text: String(it.text ?? "") })
        return
      default: {
        const tool = toolOf(it)
        if (!tool) return
        this.items.set(id, { kind: "tool" })
        if (it.type === "fileChange")
          this.changes.set(
            id,
            (it.changes ?? []).map((c: Json) => String(c.path)),
          )
        this.out.item({ id, kind: "tool", status: "running", source: "harness", ...tool })
      }
    }
  }

  private completed(it: Json) {
    const id = String(it.id ?? "")
    if (!id) return
    switch (it.type) {
      case "userMessage":
        return
      case "agentMessage":
        this.items.set(id, { kind: "assistant", text: String(it.text ?? "") })
        return this.out.item({ id, kind: "assistant", text: String(it.text ?? ""), streaming: false })
      case "reasoning": {
        const text = [...(it.summary ?? []), ...(it.summary?.length ? [] : (it.content ?? []))]
          .map(String)
          .join("\n\n")
        const had = this.items.get(id)?.text
        if (!text.trim() && !had) return
        return this.out.item({ id, kind: "reasoning", text: text || had || "", streaming: false })
      }
      case "plan":
        return this.out.item({ id, kind: "plan", text: String(it.text ?? ""), status: "proposed" })
      case "contextCompaction":
        return this.out.item({ id, kind: "notice", text: "The conversation was compacted." })
      case "enteredReviewMode":
        return this.out.item({ id, kind: "notice", text: "Reviewing the changes." })
      case "exitedReviewMode":
        return this.out.item({ id, kind: "assistant", text: String(it.review ?? ""), streaming: false })
      default: {
        const tool = toolOf(it)
        if (!tool) return
        const status = statusOf(it)
        const output = outputOf(it)
        const patch: Json = { status }
        if (output != null)
          patch.output = output.length > OUTPUT_MAX ? `${output.slice(0, OUTPUT_MAX)}\n…` : output
        if (!this.items.has(id)) {
          this.items.set(id, { kind: "tool" })
          this.out.item({ id, kind: "tool", source: "harness", ...tool, ...patch })
        } else this.out.patch(id, patch)
      }
    }
  }

  private serverRequest(raw: string | number, method: string, p: Json) {
    const id = `codex-${raw}-${p.itemId ?? ""}`
    switch (method) {
      case "item/commandExecution/requestApproval":
      case "execCommandApproval": {
        const command = shortCommand(String(p.command ?? (p.command_array ?? []).join(" ") ?? ""))
        this.pending.set(id, { raw, method, params: p })
        const decisions = (p.availableDecisions ?? []) as unknown[]
        this.out.item({
          id,
          kind: "approval",
          tool: "Bash",
          summary: p.reason ? `Run a command: ${String(p.reason)}` : "Run a command",
          items: [command],
          input: { command, cwd: p.cwd },
          status: "pending",
          toolUseId: p.itemId,
          canAllowSession: decisions.length ? decisions.includes("acceptForSession") : true,
        })
        return
      }
      case "item/fileChange/requestApproval":
      case "applyPatchApproval": {
        const files = this.changes.get(String(p.itemId)) ?? Object.keys(p.fileChanges ?? {})
        this.pending.set(id, { raw, method, params: p })
        this.out.item({
          id,
          kind: "approval",
          tool: "Edit",
          summary: p.reason
            ? `Edit files: ${String(p.reason)}`
            : files.length === 1
              ? "Edit a file"
              : "Edit files",
          items: files,
          status: "pending",
          toolUseId: p.itemId,
          canAllowSession: true,
        })
        return
      }
      case "item/permissions/requestApproval": {
        this.pending.set(id, { raw, method, params: p })
        this.out.item({
          id,
          kind: "approval",
          tool: "Permissions",
          summary: p.reason ? `More access: ${String(p.reason)}` : "More access",
          items: describePermissions(p.permissions ?? {}),
          input: p.permissions ?? {},
          status: "pending",
          canAllowSession: true,
        })
        return
      }
      case "mcpServer/elicitation/request": {
        this.pending.set(id, { raw, method, params: p })
        this.out.item({
          id,
          kind: "approval",
          tool: `mcp__${p.serverName}`,
          summary: String(p.message ?? `${p.serverName} asks to go ahead`),
          ...(p.url ? { items: [String(p.url)] } : {}),
          status: "pending",
        })
        return
      }
      case "item/tool/requestUserInput": {
        const qs = (p.questions ?? []) as Json[]
        this.pending.set(id, {
          raw,
          method,
          params: p,
          questions: qs.map((q) => ({ id: String(q.id), question: String(q.question ?? "") })),
        })
        this.out.item({
          id,
          kind: "question",
          questions: qs.map((q) => ({
            question: String(q.question ?? ""),
            ...(q.header ? { header: String(q.header) } : {}),
            options: (q.options ?? []).map((o: Json) => ({
              label: String(o.label ?? ""),
              ...(o.description ? { description: String(o.description) } : {}),
            })),
            multiSelect: false,
          })),
          status: "pending",
        })
        return
      }
      default:
        this.write(
          JSON.stringify({ id: raw, error: { code: -32601, message: `Parrot does not answer ${method}` } }),
        )
    }
  }
}

/** `/bin/bash -lc 'echo hi'` → `echo hi`. */
export function shortCommand(c: string): string {
  const m = c.match(/^\/bin\/(?:ba|z)?sh -lc (['"])([\s\S]*)\1$/)
  return m ? m[2]!.replace(/'\\''/g, "'") : c
}

function toolOf(it: Json): Json | null {
  switch (it.type) {
    case "commandExecution": {
      const command = shortCommand(String(it.command ?? ""))
      return { name: "Bash", title: command, input: { command, cwd: it.cwd } }
    }
    case "fileChange": {
      const paths = (it.changes ?? []).map((c: Json) => String(c.path))
      return { name: "Edit", title: paths.join(", "), input: { changes: it.changes ?? [] } }
    }
    case "mcpToolCall":
      return {
        name: `mcp__${it.server}__${it.tool}`,
        title: `${it.server}: ${it.tool}`,
        input: it.arguments ?? {},
      }
    case "dynamicToolCall":
      return { name: String(it.tool), title: String(it.tool), input: it.arguments ?? {} }
    case "webSearch":
      return { name: "WebSearch", title: String(it.query ?? ""), input: { query: it.query } }
    case "imageView":
      return { name: "Read", title: String(it.path ?? ""), input: { path: it.path } }
    case "collabAgentToolCall":
      return { name: "Agent", title: String(it.prompt ?? it.tool ?? ""), input: { prompt: it.prompt } }
    default:
      return null
  }
}

function statusOf(it: Json): "ok" | "error" {
  if (it.status === "failed" || it.status === "declined") return "error"
  if (it.type === "commandExecution" && typeof it.exitCode === "number" && it.exitCode !== 0) return "error"
  if (it.type === "mcpToolCall" && it.error) return "error"
  if (it.type === "dynamicToolCall" && it.success === false) return "error"
  return "ok"
}

function outputOf(it: Json): string | null {
  switch (it.type) {
    case "commandExecution":
      return it.aggregatedOutput == null
        ? it.status === "declined"
          ? "Declined."
          : null
        : String(it.aggregatedOutput)
    case "fileChange":
      return (it.changes ?? []).map((c: Json) => String(c.diff ?? "")).join("\n") || null
    case "mcpToolCall":
      if (it.error) return String(it.error.message ?? it.error)
      return (it.result?.content ?? [])
        .map((c: Json) => (c?.type === "text" ? String(c.text ?? "") : c?.type === "image" ? "[image]" : ""))
        .filter(Boolean)
        .join("\n")
    case "dynamicToolCall":
      return (it.contentItems ?? []).map((c: Json) => String(c.text ?? "")).join("\n") || null
    default:
      return null
  }
}

function describePermissions(p: Json): string[] {
  const out: string[] = []
  if (p.network) out.push("Network access")
  for (const path of p.fileSystem?.write ?? p.file_system?.write ?? []) out.push(`Write ${path}`)
  for (const path of p.fileSystem?.read ?? p.file_system?.read ?? []) out.push(`Read ${path}`)
  return out.length ? out : [JSON.stringify(p)]
}

function usageOf(u: Json): Usage {
  const total = u.total ?? {}
  const last = u.last ?? {}
  return {
    inputTokens: Number(total.inputTokens ?? 0),
    outputTokens: Number(total.outputTokens ?? 0),
    cacheReadTokens: Number(total.cachedInputTokens ?? 0),
    cacheWriteTokens: Number(total.cacheWriteInputTokens ?? 0),
    contextTokens: Number(last.inputTokens ?? 0) + Number(last.outputTokens ?? 0),
    ...(u.modelContextWindow ? { contextWindow: Number(u.modelContextWindow) } : {}),
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

// The Agent Client Protocol (https://agentclientprotocol.com, protocol version 1), as a client of any
// ACP agent (OpenCode, Claude Agent ACP, Gemini CLI and others): JSON-RPC 2.0, one message per line over
// the agent's stdio. Checked against the published schema (`@agentclientprotocol/sdk` 2.6.0,
// schema/v1/schema.json) and by running `@agentclientprotocol/claude-agent-acp` 0.107.0.
//
// - We ask: `initialize` (we offer no file system and no terminal: the agent runs on the same machine and
//   uses its own), `session/new` (with an MCP server over HTTP and a token in a header),
//   `session/resume` (no replay) or `session/load` (replays the history as updates, which are dropped
//   until it answers), `session/prompt` (answers when the turn ends, with a stop reason and usage),
//   `session/set_mode`, `session/set_config_option` (model, reasoning level), and the `session/cancel`
//   notification.
// - It tells: `session/update` with agent message and thought chunks, tool calls and their updates, plans,
//   available commands, mode and config option changes, usage.
// - It asks: `session/request_permission` (options allow_once, allow_always, reject_once, reject_always).
//   File system and terminal requests are refused: we did not offer them.
import type { AgentOption, Answer, Capabilities, DriverInput, HarnessCommand, Usage } from "./types"

export interface Reporter {
  item(item: { id: string; kind: string; [k: string]: unknown }): void
  delta(id: string, text: string, field?: string): void
  patch(id: string, patch: Record<string, unknown>): void
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

const OUTPUT_MAX = 20_000

interface Pending {
  raw: string | number
  options: Array<{ optionId: string; kind: string; name: string }>
}

export class AcpProtocol {
  readonly pending = new Map<string, Pending>()
  private readonly waiting = new Map<string, { resolve: (v: Json) => void; reject: (e: Error) => void }>()
  private counter = 0
  capabilities: Capabilities
  sessionId: string | undefined
  agent: Json = {}
  /** While `session/load` replays the history: its updates are in the log already. */
  private replaying = false
  /** The item streaming now, by kind, so chunks without a message id go on in it. */
  private current: { assistant?: string; reasoning?: string } = {}
  private turn = 0
  private chunk = 0
  private configIds: { model?: string; effort?: string } = {}
  usage: Usage = { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0 }

  constructor(
    private readonly out: Reporter,
    private readonly write: (line: string) => void,
    base: Capabilities,
  ) {
    this.capabilities = { ...base }
  }

  request(method: string, params: Json = {}, timeoutMs = 120_000): Promise<Json> {
    const id = ++this.counter
    return new Promise((resolve, reject) => {
      const timer =
        timeoutMs > 0
          ? setTimeout(() => {
              this.waiting.delete(String(id))
              reject(new Error(`the agent did not answer ${method} in ${timeoutMs} ms`))
            }, timeoutMs)
          : undefined
      this.waiting.set(String(id), {
        resolve: (v) => {
          clearTimeout(timer)
          resolve(v)
        },
        reject: (e) => {
          clearTimeout(timer)
          reject(e)
        },
      })
      this.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }))
    })
  }

  notify(method: string, params: Json) {
    this.write(JSON.stringify({ jsonrpc: "2.0", method, params }))
  }

  closed(why: string) {
    for (const w of this.waiting.values()) w.reject(new Error(why))
    this.waiting.clear()
  }

  /** initialize, then a new session or the old one. */
  async open(opts: {
    cwd: string
    resume?: string
    mcp?: { name: string; url: string; token: string }
    model?: string
    mode?: string
  }): Promise<string> {
    const init = await this.request("initialize", {
      protocolVersion: 1,
      clientCapabilities: { fs: { readTextFile: false, writeTextFile: false }, terminal: false },
      clientInfo: { name: "parrot", title: "Parrot", version: "0.1.0" },
    })
    this.agent = init
    const caps = init.agentCapabilities ?? {}
    this.capabilities.images = !!caps.promptCapabilities?.image
    this.capabilities.files = !!caps.promptCapabilities?.embeddedContext
    const canResume = !!caps.sessionCapabilities?.resume
    const canLoad = !!caps.loadSession
    this.capabilities.resume = canResume || canLoad
    const mcpServers =
      opts.mcp && caps.mcpCapabilities?.http
        ? [
            {
              type: "http",
              name: opts.mcp.name,
              url: opts.mcp.url,
              headers: [{ name: "Authorization", value: `Bearer ${opts.mcp.token}` }],
            },
          ]
        : []
    let r: Json
    if (opts.resume && canResume) {
      r = await this.request("session/resume", { sessionId: opts.resume, cwd: opts.cwd, mcpServers })
      this.sessionId = opts.resume
    } else if (opts.resume && canLoad) {
      this.replaying = true
      try {
        r = await this.request("session/load", { sessionId: opts.resume, cwd: opts.cwd, mcpServers }, 300_000)
      } finally {
        this.replaying = false
      }
      this.sessionId = opts.resume
    } else {
      if (opts.resume) throw new Error("the agent can neither resume nor load a session")
      r = await this.request("session/new", { cwd: opts.cwd, mcpServers })
      this.sessionId = String(r.sessionId)
    }
    this.applySession(r ?? {})
    if (opts.mode && opts.mode !== this.mode) await this.setMode(opts.mode).catch(() => {})
    if (opts.model && opts.model !== this.model) await this.setModel(opts.model).catch(() => {})
    this.out.meta({
      cliSession: this.sessionId,
      capabilities: this.capabilities,
      ...(this.model ? { model: this.model } : {}),
      ...(this.mode ? { mode: this.mode } : {}),
    })
    return this.sessionId!
  }

  mode: string | undefined
  model: string | undefined

  private applySession(r: Json) {
    if (r.modes?.availableModes) {
      this.capabilities.modes = (r.modes.availableModes as Json[]).map((m) => ({
        id: String(m.id),
        label: String(m.name ?? m.id),
        ...(m.description ? { description: String(m.description) } : {}),
      }))
      this.capabilities.modeLive = true
    }
    if (r.modes?.currentModeId) this.mode = String(r.modes.currentModeId)
    if (r.models?.availableModels) {
      // The older, unstable shape.
      this.capabilities.models = (r.models.availableModels as Json[]).map((m) => ({
        id: String(m.modelId),
        label: String(m.name ?? m.modelId),
        ...(m.description ? { description: String(m.description) } : {}),
      }))
      this.capabilities.modelLive = true
      if (r.models.currentModelId) this.model = String(r.models.currentModelId)
    }
    this.applyConfig(r.configOptions)
  }

  private applyConfig(list: Json[] | undefined | null) {
    for (const o of list ?? []) {
      if (o.type && o.type !== "select") continue
      const options: AgentOption[] = flattenOptions(o.options).map((x) => ({
        id: String(x.value),
        label: String(x.name ?? x.value),
        ...(x.description ? { description: String(x.description) } : {}),
      }))
      if (o.category === "model" || o.id === "model") {
        this.configIds.model = String(o.id)
        this.capabilities.models = options
        this.capabilities.modelLive = true
        if (o.currentValue) this.model = String(o.currentValue)
      } else if (o.category === "thought_level" || o.id === "effort" || o.id === "thought_level") {
        this.configIds.effort = String(o.id)
        this.capabilities.effort = options
        this.capabilities.effortLive = true
      }
    }
  }

  async setMode(mode: string) {
    await this.request("session/set_mode", { sessionId: this.sessionId, modeId: mode }, 30_000)
    this.mode = mode
  }

  async setModel(model: string) {
    if (this.configIds.model)
      await this.request(
        "session/set_config_option",
        { sessionId: this.sessionId, configId: this.configIds.model, value: model },
        30_000,
      )
    else await this.request("session/set_model", { sessionId: this.sessionId, modelId: model }, 30_000)
    this.model = model
  }

  async setEffort(effort: string) {
    if (!this.configIds.effort) throw new Error("the agent has no reasoning level")
    await this.request(
      "session/set_config_option",
      { sessionId: this.sessionId, configId: this.configIds.effort, value: effort },
      30_000,
    )
  }

  /** The prompt's content blocks: the context as embedded resources where the agent takes them. */
  prompt(
    input: DriverInput,
    files: Array<{ name: string; contentType: string; base64: string }> = [],
  ): Json[] {
    const out: Json[] = []
    for (const c of input.context ?? []) {
      out.push(
        this.capabilities.files
          ? { type: "resource", resource: { uri: c.uri, text: c.text, mimeType: "text/markdown" } }
          : { type: "text", text: `<context uri="${c.uri}">\n${c.text}\n</context>` },
      )
    }
    for (const f of files) {
      if (f.contentType.startsWith("image/") && this.capabilities.images)
        out.push({ type: "image", data: f.base64, mimeType: f.contentType })
      else
        out.push({
          type: "resource",
          resource: {
            uri: `file:///${encodeURIComponent(f.name)}`,
            text: decodeBase64(f.base64),
            mimeType: f.contentType,
          },
        })
    }
    out.push({ type: "text", text: input.text })
    return out
  }

  /** A turn: answers when the agent's turn ends. */
  async send(input: DriverInput, files: Array<{ name: string; contentType: string; base64: string }> = []) {
    this.turn++
    this.current = {}
    const id = this.sessionId
    this.request("session/prompt", { sessionId: id, prompt: this.prompt(input, files) }, 0).then(
      (r) => this.ended(r),
      (e) => {
        this.cancelPending()
        this.out.turnEnd(String((e as Error).message ?? e))
      },
    )
  }

  cancel() {
    this.notify("session/cancel", { sessionId: this.sessionId })
    // The protocol: the client answers every open permission request with `cancelled`.
    for (const [id, p] of this.pending) {
      this.write(JSON.stringify({ jsonrpc: "2.0", id: p.raw, result: { outcome: { outcome: "cancelled" } } }))
      this.out.patch(id, { status: "cancelled" })
    }
    this.pending.clear()
  }

  private cancelPending() {
    for (const id of this.pending.keys()) this.out.patch(id, { status: "cancelled" })
    this.pending.clear()
  }

  private ended(r: Json) {
    if (r.usage) {
      this.usage = {
        inputTokens: Number(r.usage.inputTokens ?? 0),
        outputTokens: Number(r.usage.outputTokens ?? 0),
        cacheReadTokens: Number(r.usage.cachedReadTokens ?? 0),
        cacheWriteTokens: Number(r.usage.cachedWriteTokens ?? 0),
        ...(this.usage.contextTokens != null ? { contextTokens: this.usage.contextTokens } : {}),
        ...(this.usage.contextWindow != null ? { contextWindow: this.usage.contextWindow } : {}),
        ...(this.usage.costUsd != null ? { costUsd: this.usage.costUsd } : {}),
      }
      this.out.meta({ usage: this.usage })
    }
    this.cancelPending()
    for (const kind of ["assistant", "reasoning"] as const) {
      const cur = this.current[kind]
      if (cur) this.out.patch(cur, { streaming: false })
    }
    const stop = String(r.stopReason ?? "end_turn")
    this.out.turnEnd(
      stop === "refusal"
        ? "The agent refused to go on."
        : stop === "max_tokens"
          ? "The agent reached its token limit."
          : stop === "max_turn_requests"
            ? "The agent reached its limit of requests for one turn."
            : undefined,
    )
  }

  answer(itemId: string, answer: Answer): boolean {
    const p = this.pending.get(itemId)
    if (!p) return false
    this.pending.delete(itemId)
    const want =
      answer.action === "allowSession"
        ? ["allow_always", "allow_once"]
        : answer.action === "allow"
          ? ["allow_once", "allow_always"]
          : ["reject_once", "reject_always"]
    const option = want.map((k) => p.options.find((o) => o.kind === k)).find(Boolean)
    this.write(
      JSON.stringify({
        jsonrpc: "2.0",
        id: p.raw,
        result: {
          outcome: option ? { outcome: "selected", optionId: option.optionId } : { outcome: "cancelled" },
        },
      }),
    )
    return true
  }

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
      if (m.error) w.reject(new Error(String(m.error.message ?? "the agent refused the request")))
      else w.resolve(m.result ?? {})
      return
    }
    if (m.id != null && m.method) return this.agentRequest(m.id, String(m.method), m.params ?? {})
    if (m.method === "session/update") return this.update(m.params?.update ?? {})
  }

  private agentRequest(raw: string | number, method: string, p: Json) {
    if (method !== "session/request_permission") {
      this.write(
        JSON.stringify({
          jsonrpc: "2.0",
          id: raw,
          error: { code: -32601, message: `Parrot does not offer ${method}` },
        }),
      )
      return
    }
    const call = p.toolCall ?? {}
    const id = `acp-perm-${raw}`
    const options = ((p.options ?? []) as Json[]).map((o) => ({
      optionId: String(o.optionId),
      kind: String(o.kind ?? ""),
      name: String(o.name ?? ""),
    }))
    this.pending.set(id, { raw, options })
    const input = call.rawInput ?? {}
    this.out.item({
      id,
      kind: "approval",
      tool: String(call.kind ?? call.name ?? "tool"),
      summary: String(call.title ?? "The agent asks to go ahead"),
      items: affected(input, call.locations),
      input,
      status: "pending",
      toolUseId: call.toolCallId,
      canAllowSession: options.some((o) => o.kind === "allow_always"),
    })
  }

  private update(u: Json) {
    if (this.replaying) return
    switch (u.sessionUpdate) {
      case "agent_message_chunk":
        return this.text("assistant", u)
      case "agent_thought_chunk":
        return this.text("reasoning", u)
      case "tool_call": {
        this.current = {}
        const id = String(u.toolCallId)
        this.out.item({
          id,
          kind: "tool",
          name: String(u.name ?? u.kind ?? "tool"),
          title: String(u.title ?? ""),
          input: u.rawInput ?? {},
          status: status(u.status),
          source: "harness",
          ...outputOf(u),
        })
        return
      }
      case "tool_call_update": {
        const patch: Json = { ...outputOf(u) }
        if (u.status) patch.status = status(u.status)
        if (u.title) patch.title = String(u.title)
        if (u.rawInput) patch.input = u.rawInput
        this.out.patch(String(u.toolCallId), patch)
        return
      }
      case "plan":
        return this.out.item({
          id: `acp-plan-${this.turn}`,
          kind: "plan",
          text: ((u.entries ?? []) as Json[])
            .map((e) => `- [${e.status === "completed" ? "x" : " "}] ${String(e.content ?? "")}`)
            .join("\n"),
          todos: (u.entries ?? []).map((e: Json) => ({ content: e.content, status: e.status })),
        })
      case "available_commands_update":
        this.capabilities.commands = ((u.availableCommands ?? []) as Json[]).map(
          (c): HarnessCommand => ({
            name: String(c.name),
            ...(c.description ? { description: String(c.description) } : {}),
            ...(c.input?.hint ? { argumentHint: String(c.input.hint) } : {}),
          }),
        )
        return this.out.meta({ capabilities: this.capabilities })
      case "current_mode_update":
        if (u.currentModeId) {
          this.mode = String(u.currentModeId)
          this.out.meta({ mode: this.mode })
        }
        return
      case "config_option_update":
        this.applyConfig(u.configOptions)
        return this.out.meta({
          capabilities: this.capabilities,
          ...(this.model ? { model: this.model } : {}),
        })
      case "usage_update":
        this.usage = {
          ...this.usage,
          contextTokens: Number(u.used ?? 0),
          contextWindow: Number(u.size ?? 0),
          ...(u.cost?.amount != null && u.cost.currency === "USD" ? { costUsd: Number(u.cost.amount) } : {}),
        }
        return this.out.meta({ usage: this.usage })
      case "compaction_update":
      case "compaction_summary_chunk":
        return
      case "notice":
        if (u.message || u.text)
          this.out.item({
            id: `acp-notice-${++this.chunk}`,
            kind: "notice",
            text: String(u.message ?? u.text),
          })
        return
    }
  }

  private text(kind: "assistant" | "reasoning", u: Json) {
    const c = u.content ?? {}
    if (c.type !== "text" || !c.text) return
    const id = u.messageId
      ? `${u.messageId}:${kind === "assistant" ? "t" : "r"}`
      : (this.current[kind] ?? `acp-${kind}-${this.turn}-${++this.chunk}`)
    if (this.current[kind] === id) {
      this.out.delta(id, String(c.text))
      return
    }
    // Another kind or another message: the one before is done.
    const prev = this.current[kind]
    if (prev) this.out.patch(prev, { streaming: false })
    this.current[kind] = id
    this.out.item({ id, kind, text: String(c.text), streaming: true })
  }
}

function flattenOptions(options: unknown): Json[] {
  const out: Json[] = []
  for (const o of (options as Json[]) ?? []) {
    if (Array.isArray(o.options)) out.push(...o.options)
    else out.push(o)
  }
  return out
}

function status(s: unknown): "running" | "ok" | "error" {
  return s === "completed" ? "ok" : s === "failed" ? "error" : "running"
}

function outputOf(u: Json): Json {
  const parts: string[] = []
  for (const c of (u.content ?? []) as Json[]) {
    if (c.type === "content" && c.content?.type === "text") parts.push(String(c.content.text ?? ""))
    else if (c.type === "diff")
      parts.push(`${c.path}\n${c.oldText == null ? "+ " : ""}${String(c.newText ?? "")}`)
  }
  if (!parts.length && typeof u.rawOutput === "string") parts.push(u.rawOutput)
  if (
    !parts.length &&
    u.rawOutput &&
    typeof u.rawOutput === "object" &&
    typeof u.rawOutput.output === "string"
  )
    parts.push(u.rawOutput.output)
  if (!parts.length) return {}
  const text = parts.join("\n")
  return { output: text.length > OUTPUT_MAX ? `${text.slice(0, OUTPUT_MAX)}\n…` : text }
}

function affected(input: Json, locations?: Json[]): string[] {
  const out: string[] = []
  if (input.command) out.push(String(input.command))
  for (const k of ["file_path", "path", "filePath", "url"]) if (input[k]) out.push(String(input[k]))
  for (const l of locations ?? []) if (l?.path && !out.includes(String(l.path))) out.push(String(l.path))
  return out
}

function decodeBase64(b64: string): string {
  const bin = atob(b64)
  const bytes = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
  return new TextDecoder().decode(bytes)
}

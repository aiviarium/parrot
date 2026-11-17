// Pi's RPC mode (`pi --mode rpc`), checked against Pi 1.5.1's docs (docs/rpc.md, rpc-commands.md, json.md,
// rpc-extension-ui.md) and by running it. Without a signed-in provider Pi refuses a prompt with "No API key
// found", so turns are tested against the fake, which follows the docs.
//
// - Commands, one JSON object per line on stdin with an `id`, each answered by a `response` record:
//   `prompt` (accepted, queued or handled: the run goes on as events), `steer`, `abort`, `get_state`,
//   `get_available_models`, `set_model`, `get_available_thinking_levels`, `set_thinking_level`,
//   `get_commands`.
// - Events, as they happen: `agent_start`, `message_update` (text, thinking and tool-call deltas by
//   content index), `message_end` (the authoritative message, with usage, a stop reason and an error),
//   `tool_execution_start` / `_update` / `_end`, `agent_settled` (Pi will not go on by itself: the turn is
//   over), and `extension_ui_request` for an extension's dialogs (`confirm`, `select`, `input`, `editor`),
//   answered with `extension_ui_response`. Pi itself never asks before running a tool; extensions can.
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
    capabilities?: Capabilities
    usage?: Usage
  }): void
  turnEnd(error?: string): void
}

type Json = Record<string, any>

const OUTPUT_MAX = 20_000

export class PiProtocol {
  private readonly waiting = new Map<string, { resolve: (v: Json) => void; reject: (e: Error) => void }>()
  private counter = 0
  capabilities: Capabilities
  /** Open extension dialogs, by item id. */
  readonly pending = new Map<string, { raw: string; method: string; options?: string[] }>()
  private message = 0
  /** Content index → item id, for the assistant message streaming now. */
  private blocks = new Map<number, string>()
  private usage: Usage = { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0 }
  private running = false
  private lastError: string | undefined
  model: string | undefined

  constructor(
    private readonly out: Reporter,
    private readonly write: (line: string) => void,
    base: Capabilities,
  ) {
    this.capabilities = { ...base }
  }

  request(type: string, fields: Json = {}, timeoutMs = 60_000): Promise<Json> {
    const id = `parrot-${++this.counter}`
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.waiting.delete(id)
        reject(new Error(`Pi did not answer ${type} in ${timeoutMs} ms`))
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
      this.write(JSON.stringify({ id, type, ...fields }))
    })
  }

  closed(why: string) {
    for (const w of this.waiting.values()) w.reject(new Error(why))
    this.waiting.clear()
  }

  get busy() {
    return this.running
  }

  /** What the session is: its id, model, models, thinking levels, commands and skills. */
  async open(): Promise<string | undefined> {
    const st = await this.request("get_state")
    const models = await this.request("get_available_models").catch(() => ({ models: [] }))
    this.capabilities.models = ((models.models ?? []) as Json[]).map((m) => ({
      id: `${m.provider}/${m.id}`,
      label: String(m.name ?? m.id),
      description: String(m.provider ?? ""),
    }))
    this.capabilities.modelLive = true
    const levels = await this.request("get_available_thinking_levels").catch(() => ({ levels: [] }))
    this.capabilities.effort = ((levels.levels ?? []) as string[])
      .filter((l) => l !== "off")
      .map((l) => ({ id: l, label: l[0]!.toUpperCase() + l.slice(1) }))
    this.capabilities.effortLive = true
    const cmds = await this.request("get_commands").catch(() => ({ commands: [] }))
    const all: Array<HarnessCommand & { source?: string }> = ((cmds.commands ?? []) as Json[]).map((c) => ({
      name: String(c.name),
      ...(c.description ? { description: String(c.description) } : {}),
      source: String(c.source ?? ""),
    }))
    this.capabilities.commands = all.filter((c) => c.source !== "skill").map(({ source: _, ...c }) => c)
    this.capabilities.skills = all.filter((c) => c.source === "skill").map(({ source: _, ...c }) => c)
    const m = st.model
    this.model = m && m.provider !== "unknown" ? `${m.provider}/${m.id}` : undefined
    this.out.meta({
      ...(st.sessionId ? { cliSession: String(st.sessionId) } : {}),
      ...(this.model ? { model: this.model } : {}),
      ...(st.thinkingLevel && st.thinkingLevel !== "off" ? { effort: String(st.thinkingLevel) } : {}),
      capabilities: this.capabilities,
    })
    return st.sessionId
  }

  private text(input: DriverInput) {
    const parts = (input.context ?? []).map((c) => `<context uri="${c.uri}">\n${c.text}\n</context>`)
    parts.push(input.text)
    return parts.join("\n\n")
  }

  private images(files: Array<{ contentType: string; base64: string }>) {
    return files
      .filter((f) => f.contentType.startsWith("image/"))
      .map((f) => ({ type: "image", data: f.base64, mimeType: f.contentType }))
  }

  async prompt(input: DriverInput, files: Array<{ name: string; contentType: string; base64: string }> = []) {
    this.running = true
    this.lastError = undefined
    // `$skill` chips: Pi runs a skill as the command `/skill:<name>`.
    const text = this.text(input).replace(/(^|\s)\$([\w.-]+)/g, (all, pre, name) =>
      this.capabilities.skills?.some((s) => s.name === `skill:${name}`) ? `${pre}/skill:${name}` : all,
    )
    const images = this.images(files)
    try {
      const r = await this.request("prompt", { message: text, ...(images.length ? { images } : {}) })
      if (r.disposition === "handled") {
        // An extension command took it; no run follows.
        this.running = false
        this.out.turnEnd()
      }
    } catch (e) {
      this.running = false
      throw e
    }
  }

  async steer(input: DriverInput, files: Array<{ name: string; contentType: string; base64: string }> = []) {
    if (!this.running) return this.prompt(input, files)
    const images = this.images(files)
    await this.request("steer", { message: this.text(input), ...(images.length ? { images } : {}) })
  }

  async abort() {
    await this.request("abort", {}, 30_000)
  }

  async setModel(model: string) {
    const i = model.indexOf("/")
    const data = await this.request("set_model", { provider: model.slice(0, i), modelId: model.slice(i + 1) })
    this.model = data?.provider ? `${data.provider}/${data.id}` : model
  }

  async setThinking(level: string) {
    await this.request("set_thinking_level", { level })
  }

  answer(itemId: string, answer: Answer): boolean {
    const p = this.pending.get(itemId)
    if (!p) return false
    this.pending.delete(itemId)
    const value = Object.values(answer.answers ?? {})[0] ?? answer.message
    let r: Json
    if (p.method === "confirm")
      r = { confirmed: answer.action === "allow" || answer.action === "allowSession" }
    else if (value == null || answer.action === "deny") r = { cancelled: true }
    else r = { value: String(value) }
    this.write(JSON.stringify({ type: "extension_ui_response", id: p.raw, ...r }))
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
    this.event(m)
  }

  event(m: Json) {
    switch (m.type) {
      case "response": {
        const w = m.id != null ? this.waiting.get(String(m.id)) : undefined
        if (!w) return
        this.waiting.delete(String(m.id))
        if (m.success === false) w.reject(new Error(String(m.error ?? `Pi refused ${m.command}`)))
        else w.resolve(m.data ?? {})
        return
      }
      case "agent_start":
        this.running = true
        return this.out.state("working")
      case "message_start":
        if (m.message?.role === "assistant") {
          this.message++
          this.blocks.clear()
        }
        return
      case "message_update":
        return this.update(m.assistantMessageEvent ?? {})
      case "message_end":
        return this.messageEnd(m.message ?? {})
      case "tool_execution_start":
        return this.out.item({
          id: String(m.toolCallId),
          kind: "tool",
          name: String(m.toolName ?? "tool"),
          title: describe(String(m.toolName ?? ""), m.args ?? {}),
          input: m.args ?? {},
          status: "running",
          source: "harness",
        })
      case "tool_execution_update": {
        const text = resultText(m.partialResult)
        if (text) this.out.patch(String(m.toolCallId), { output: clip(text) })
        return
      }
      case "tool_execution_end":
        return this.out.patch(String(m.toolCallId), {
          status: m.isError ? "error" : "ok",
          output: clip(resultText(m.result)),
        })
      case "auto_retry_end":
        if (m.success === false && m.finalError) this.lastError = String(m.finalError)
        return
      case "compaction_end":
        if (m.result)
          this.out.item({
            id: `pi-compact-${Date.now()}`,
            kind: "notice",
            text: "The conversation was compacted.",
          })
        return
      case "agent_settled": {
        this.running = false
        for (const id of this.pending.keys()) this.out.patch(id, { status: "cancelled" })
        this.pending.clear()
        const err = m.aborted ? undefined : this.lastError
        this.lastError = undefined
        return this.out.turnEnd(err)
      }
      case "extension_ui_request":
        return this.dialog(m)
    }
  }

  private update(e: Json) {
    const idx = Number(e.contentIndex ?? 0)
    const id = `pi-${this.message}-${idx}`
    switch (e.type) {
      case "text_start":
      case "thinking_start":
        this.blocks.set(idx, id)
        return this.out.item({
          id,
          kind: e.type === "text_start" ? "assistant" : "reasoning",
          text: "",
          streaming: true,
        })
      case "text_delta":
      case "thinking_delta":
        if (!this.blocks.has(idx)) {
          this.blocks.set(idx, id)
          return this.out.item({
            id,
            kind: e.type === "text_delta" ? "assistant" : "reasoning",
            text: String(e.delta ?? ""),
            streaming: true,
          })
        }
        return this.out.delta(id, String(e.delta ?? ""))
      case "text_end":
      case "thinking_end":
        return this.out.patch(id, {
          text: typeof e.content === "string" ? e.content : contentText(e.content),
          streaming: false,
        })
    }
  }

  private messageEnd(msg: Json) {
    if (msg.role !== "assistant") return
    const u = msg.usage
    if (u) {
      this.usage = {
        inputTokens: this.usage.inputTokens + Number(u.input ?? 0),
        outputTokens: this.usage.outputTokens + Number(u.output ?? 0),
        cacheReadTokens: this.usage.cacheReadTokens + Number(u.cacheRead ?? 0),
        cacheWriteTokens: this.usage.cacheWriteTokens + Number(u.cacheWrite ?? 0),
        contextTokens: Number(u.totalTokens ?? 0),
        costUsd: (this.usage.costUsd ?? 0) + Number(u.cost?.total ?? 0),
      }
      this.out.meta({ usage: this.usage })
    }
    if (msg.stopReason === "error" && msg.errorMessage) this.lastError = String(msg.errorMessage)
  }

  private dialog(m: Json) {
    const id = `pi-ui-${m.id}`
    const method = String(m.method)
    if (!["confirm", "select", "input", "editor"].includes(method)) return // fire-and-forget: nothing to answer
    this.pending.set(id, { raw: String(m.id), method, options: m.options })
    if (method === "confirm") {
      this.out.item({
        id,
        kind: "approval",
        tool: "Pi extension",
        summary: String(m.title ?? "An extension asks to go ahead"),
        ...(m.message ? { items: [String(m.message)] } : {}),
        status: "pending",
      })
      return
    }
    this.out.item({
      id,
      kind: "question",
      questions: [
        {
          question: String(m.title ?? m.message ?? "An extension asks"),
          options: ((m.options ?? []) as string[]).map((label) => ({ label: String(label) })),
          multiSelect: false,
        },
      ],
      status: "pending",
    })
  }
}

function contentText(c: unknown): string {
  if (typeof c === "string") return c
  if (Array.isArray(c)) return c.map((x: Json) => (x?.type === "text" ? String(x.text ?? "") : "")).join("")
  return ""
}

function resultText(r: Json | undefined): string {
  if (!r) return ""
  if (typeof r === "string") return r
  return contentText(r.content)
}

function clip(s: string) {
  return s.length > OUTPUT_MAX ? `${s.slice(0, OUTPUT_MAX)}\n…` : s
}

function describe(tool: string, args: Json): string {
  if (tool === "bash") return String(args.command ?? "")
  return String(args.path ?? args.file_path ?? args.pattern ?? "")
}

// Reads a Codex rollout, `~/.codex/sessions/YYYY/MM/DD/rollout-<time>-<thread id>.jsonl`, one line at a
// time. `event_msg` lines of type `item_completed` hold each finished item (the core's shape, with
// PascalCase types and snake_case fields, converted here to the app-server's and read by `CodexProtocol`),
// `token_count` holds the usage and `task_complete` the end of a turn. `session_meta` carries the thread id.
import { CodexProtocol, type Reporter } from "./codex"
import type { Capabilities, TranscriptReader } from "./types"

type Json = Record<string, any>

const camel = (k: string) => k.replace(/_([a-z])/g, (_, c: string) => c.toUpperCase())

/** A rollout's `item_completed` item (core shape) as the app-server's `ThreadItem`. */
export function threadItem(it: Json): Json {
  const type = String(it.type ?? "")
  const out: Json = {}
  for (const [k, v] of Object.entries(it)) out[camel(k)] = v
  out.type = type.charAt(0).toLowerCase() + type.slice(1)
  switch (type) {
    case "AgentMessage":
      out.text = (it.content ?? [])
        .filter((c: Json) => c.type === "Text" || c.type === "text" || c.type === "output_text")
        .map((c: Json) => String(c.text ?? ""))
        .join("")
      break
    case "Reasoning":
      out.summary = it.summary_text ?? it.summary ?? []
      out.content = it.raw_content ?? it.content ?? []
      break
    case "CommandExecution": {
      const c = it.command
      out.command = Array.isArray(c)
        ? c.length === 3 && /sh$/.test(String(c[0])) && c[1] === "-lc"
          ? String(c[2])
          : c.map(String).join(" ")
        : String(c ?? "")
      out.aggregatedOutput = it.aggregated_output ?? null
      out.exitCode = it.exit_code ?? null
      out.status = it.status === "completed" ? "completed" : it.status
      break
    }
    case "FileChange":
    case "PatchApply": {
      out.type = "fileChange"
      const ch = it.changes ?? {}
      out.changes = Array.isArray(ch)
        ? ch
        : Object.entries(ch).map(([path, v]) => ({
            path,
            diff: (v as Json)?.unified_diff ?? (v as Json)?.diff ?? "",
          }))
      break
    }
    case "McpToolCall":
      out.arguments = it.arguments ?? it.invocation?.arguments ?? {}
      out.server = it.server ?? it.invocation?.server
      out.tool = it.tool ?? it.invocation?.tool
      break
  }
  return out
}

function userText(it: Json): string {
  return (it.content ?? [])
    .filter((c: Json) => c.type === "text" || c.type === "Text")
    .map((c: Json) => String(c.text ?? ""))
    .join("\n")
}

/** Reads a Codex rollout into items through the app-server translator. */
export class CodexTranscript implements TranscriptReader {
  private readonly protocol: CodexProtocol
  constructor(
    private readonly out: Reporter,
    base: Capabilities,
    cwd: string,
  ) {
    this.protocol = new CodexProtocol(out, () => {}, base, cwd)
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
    const p = m.payload ?? {}
    if (m.type === "session_meta") return p.id ? { cliSession: String(p.id) } : undefined
    if (m.type !== "event_msg") return
    switch (p.type) {
      case "item_completed": {
        const it = p.item ?? {}
        if (it.type === "UserMessage") return { user: userText(it) }
        this.protocol.message({ method: "item/completed", params: { item: threadItem(it) } })
        return
      }
      case "token_count": {
        const info = p.info
        if (!info) return
        const total = info.total_token_usage ?? {}
        const last = info.last_token_usage ?? {}
        this.out.meta({
          usage: {
            inputTokens: Number(total.input_tokens ?? 0),
            outputTokens: Number(total.output_tokens ?? 0),
            cacheReadTokens: Number(total.cached_input_tokens ?? 0),
            cacheWriteTokens: 0,
            contextTokens: Number(last.input_tokens ?? 0) + Number(last.output_tokens ?? 0),
            ...(info.model_context_window ? { contextWindow: Number(info.model_context_window) } : {}),
          },
        })
        return
      }
      case "task_complete":
      case "turn_aborted":
        return { turnDone: true }
    }
    return
  }
}

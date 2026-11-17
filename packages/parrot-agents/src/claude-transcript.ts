// Reads Claude Code's session transcript, `~/.claude/projects/<folder>/<session id>.jsonl` (under
// `$CLAUDE_CONFIG_DIR` when that is set), one line at a time. Each line is one message: `assistant` lines
// carry one content block each (text, thinking, tool_use) and `user` lines the prompt (a string) or tool
// results. `ClaudeProtocol` reads them as it reads stream-json, so items look the same on both paths. The
// end of a turn is an assistant line whose `stop_reason` is `end_turn` (or `stop_sequence`), or the
// `system` line Claude Code writes after one.
import { ClaudeProtocol, type Reporter } from "./claude"
import type { Capabilities, TranscriptReader } from "./types"

/** Reads Claude Code's JSONL transcript into items through the stream-json translator. */
export class ClaudeTranscript implements TranscriptReader {
  private readonly protocol: ClaudeProtocol
  constructor(out: Reporter, base: Capabilities) {
    this.protocol = new ClaudeProtocol(out, () => {}, base)
  }
  line(text: string) {
    const t = text.trim()
    if (!t) return
    let m: Record<string, any>
    try {
      m = JSON.parse(t)
    } catch {
      return
    }
    if (m.isSidechain || m.isMeta) return
    if (m.type === "assistant") {
      this.protocol.message(m)
      const stop = m.message?.stop_reason
      return { turnDone: stop === "end_turn" || stop === "stop_sequence", cliSession: m.sessionId }
    }
    if (m.type === "user") {
      const c = m.message?.content
      if (typeof c === "string") return userText(c)
      if (Array.isArray(c)) {
        if (c.some((b: any) => b?.type === "tool_result")) {
          this.protocol.message(m)
          return
        }
        const text = c
          .filter((b: any) => b?.type === "text")
          .map((b: any) => String(b.text ?? ""))
          .join("\n")
        return userText(text)
      }
      return
    }
    if (m.type === "system" && (m.subtype === "turn_duration" || m.subtype === "stop_hook_summary"))
      return { turnDone: true }
    return
  }
}

function userText(text: string) {
  // Interruptions, slash command echoes and hook output are the CLI's, not the person's.
  if (!text.trim() || text.startsWith("[Request interrupted") || /^\s*</.test(text)) return
  return { user: text }
}

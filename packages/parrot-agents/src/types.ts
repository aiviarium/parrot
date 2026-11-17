// The types the protocol translators share with their host.

/** A choice a UI offers: a model, a mode, an effort level. */
export interface AgentOption {
  id: string
  label: string
  description?: string
  /** For a model: the effort levels it accepts (none: no effort setting). */
  efforts?: string[]
}

/** A slash command or a skill the agent offers. */
export interface HarnessCommand {
  name: string
  description?: string
  argumentHint?: string
}

/** What the agent can do. A UI should offer only that. */
export interface Capabilities {
  models: AgentOption[]
  /** The model can change during a session. */
  modelLive: boolean
  effort: AgentOption[]
  effortLive: boolean
  /** Permission modes, plan mode among them. */
  modes: AgentOption[]
  modeLive: boolean
  /** A message sent during a turn steers it. */
  steer: boolean
  images: boolean
  files: boolean
  commands: HarnessCommand[]
  skills: HarnessCommand[]
  resume: boolean
  usage: boolean
  interrupt: boolean
}

/** Token counts for a session, and cost and context size where the agent reports them. */
export interface Usage {
  inputTokens: number
  outputTokens: number
  cacheReadTokens: number
  cacheWriteTokens: number
  costUsd?: number
  /** Tokens in the context window now. */
  contextTokens?: number
  /** Size of the context window. */
  contextWindow?: number
}

/**
 * An answer to an approval (`action`: `allow`, `deny`, `allowSession`) or a question (`answers`, by
 * question text, several labels joined with ", ").
 */
export interface Answer {
  action?: string
  answers?: Record<string, string>
  /** A message to the agent with a denial, or a free answer. */
  message?: string
}

/** A file or image sent with a message. */
export interface Attachment {
  /** An identifier the host can use to fetch the bytes. */
  blob: string
  name: string
  contentType: string
  size: number
}

/** Text from an `@` mention, sent along with a message. */
export interface ContextText {
  uri: string
  title?: string
  text: string
}

/** A message for the agent. */
export interface DriverInput {
  /** What the person wrote. */
  text: string
  /** The `@` mentions as text, to send with it. */
  context?: ContextText[]
  attachments?: Attachment[]
}

/** What reading one transcript line found. */
export interface LineResult {
  /** A prompt the CLI got. */
  user?: string
  /** The CLI finished a turn. */
  turnDone?: boolean
  /** The CLI's session id, where the transcript says it. */
  cliSession?: string
}

/** Reads a session transcript one line at a time. */
export interface TranscriptReader {
  line(text: string): LineResult | undefined
}

// Generated from spec/schema/parrot.schema.json by scripts/generate.ts. Do not edit.

export interface Action {
  id: string
  /** The harness's name, such as `/compact`. */
  name?: string | null
  label: string
  description?: string | null
  input?: unknown
  hint?: string | null
  scope: ActionScope
  when: ActionWhen[]
  effects: Effect[]
  requires?: Requirement[] | null
  source: OpenActionSource
  _meta?: Record<string, unknown> | null
}

export interface ActionInvokeParams {
  session?: string | null
  agent?: string | null
  env?: string | null
  action: string
  input?: unknown
  args?: string | null
  delivery?: Delivery | null
  key?: string | null
}

export interface ActionInvokeResult {
  status: ActionStatus
  message?: string | null
  request?: string | null
  result?: unknown
}

/** `action/list` (Appendix E8): a session's actions, or an agent's. */
export interface ActionListParams {
  session?: string | null
  agent?: string | null
  env?: string | null
}

export interface ActionListResult {
  actions: Action[]
}

export type ActionScope = "agent" | "session"

export type ActionSource = "harness" | "driver" | "skill" | "extension" | "runtime"

export type ActionStatus = "done" | "started" | "queued" | "requested"

export type ActionWhen = "idle" | "running"

export interface Agent {
  id: string
  /** Absent when a driver reports it; the runtime sets it. */
  env?: string
  name: string
  driver: AgentDriver
  harness?: HarnessInfo | null
  installed: boolean
  auth: AuthStatus
  usable: boolean
  detail?: string | null
  controls: Control[]
  features: Record<string, unknown>
  levels: PermissionLevel[]
  isolation: Isolation[]
  options: ConfigOption[]
  actions: Action[]
  _meta?: Record<string, unknown> | null
}

export interface AgentDriver {
  name: string
  version: string
  kind: DriverKind
  /** The driver data version. */
  data: string
}

export interface AgentListParams {
  env?: string | null
}

export interface AgentListResult {
  agents: Agent[]
  /** How to install a driver egg, when the runtime has none. */
  hint?: string | null
}

export interface AgentRefreshParams {
  agent: string
  env?: string | null
}

export interface AgentRefreshResult {
  agent: Agent
}

export interface Answer {
  choice?: string | null
  value?: unknown
  input?: unknown
  message?: string | null
}

/** Who can still answer once things go away. `process` was removed (Appendix E8). */
export type Answerable = "connection" | "message"

export type Applies = "start" | "now" | "next_turn"

export interface Attachment {
  name: string
  /** A media type. */
  type: string
  /** Base64 content. */
  data?: string | null
  uri?: string | null
}

export type AuthStatus = "signed_in" | "signed_out" | "unknown"

export interface Background {
  id: string
  kind: OpenBackgroundKind
  title: string
  item?: string | null
}

export type BackgroundKind = "task" | "subagent" | "command"

export interface Binding {
  id: string
  session: string
  agent: string
  env: string
  control: Control
  ref?: Ref | null
  isolation: Isolation
  level?: PermissionLevel | null
  network?: boolean | null
  status: BindingStatus
  via: BindingVia
  pending?: BindingPending | null
  started: string
  stopped?: string | null
}

export interface BindingActionParams {
  binding?: string | null
  agent?: string | null
  action: string
  input?: unknown
  args?: string | null
  /** The user item id for turn actions (Appendix E4). */
  message?: string | null
}

export interface BindingActionResult {
  status: ActionStatus
  request?: string | null
  result?: unknown
}

export interface BindingAnswerParams {
  binding: string
  /** The driver key of the request. */
  request: string
  answer: Answer
}

export interface BindingCancelParams {
  binding: string
  subagent?: string | null
}

export interface BindingCancelResult {
  interrupted: boolean
}

export type BindingDisposition = "started" | "steered" | "handled"

export interface BindingForkParams {
  binding: string
  /** A turn key. */
  at?: string | null
}

export interface BindingForkResult {
  ref: Ref
}

export interface BindingKeysParams {
  binding: string
  text?: string | null
  press?: string[] | null
}

export interface BindingOptionsParams {
  binding: string
  values: Record<string, unknown>
}

export interface BindingOptionsResult {
  options: ConfigOption[]
  level: PermissionLevel
  network: boolean
}

export type BindingPending = "bundle"

export interface BindingPromptParams {
  binding: string
  subagent?: string | null
  /** The id the user item gets. */
  message: string
  text: string
  context?: ContextEntry[] | null
  attachments?: Attachment[] | null
  steer: boolean
}

export interface BindingPromptResult {
  disposition: BindingDisposition
}

/** Params of `binding/stop` and `binding/screen`. */
export interface BindingRef {
  binding: string
}

export interface BindingScreenResult {
  text: string
  cursor: ScreenPos
  size: ScreenSize
}

export interface BindingStart {
  session: string
  binding: string
  agent: string
  /** The workspace's path. */
  workspace: string
  cwd: string
  options: Record<string, unknown>
  control: Control
  resume?: Ref | null
  pin?: Pin | null
  isolation?: Isolation | null
  /** Paths the harness must not reach. */
  deny: string[]
  mcp?: McpInjection | null
  _meta?: Record<string, unknown> | null
}

export interface BindingStartResult {
  ref?: Ref | null
  isolation: Isolation
  level: PermissionLevel
  network: boolean
  options: ConfigOption[]
  actions: Action[]
}

export type BindingStatus = "starting" | "running" | "stopped" | "lost" | "failed"

export type BindingVia = "new" | "resume" | "adopt" | "fork" | "bundle"

/** One chunk of a transfer between runtimes, at most 1 MiB (spec 24.2, E9). */
export interface BlobPutParams {
  transfer: string
  index: number
  data: string
  last?: boolean | null
}

export interface Budget {
  /** Input (without cache reads) plus output. */
  tokens?: number | null
  cost?: Cost | null
  seconds?: number | null
  turns?: number | null
  sessions?: number | null
}

export interface BudgetState {
  limit: Budget
  used: Budget
}

export type Caller = "model" | "script"

export interface CarrierInfo {
  kind: string
  title: string
  /** The JSON Schema of the carrier's target. */
  target: unknown
  listens: boolean
  containment: Containment
  maxMessage: number
}

export type CarrierListParams = Record<string, unknown>

export interface CarrierListResult {
  carriers: CarrierInfo[]
}

/** How a runtime reaches a peer. */
export interface CarrierRef {
  /** `ssh`, `command`, `tcp`, or a carrier egg's name. */
  kind: string
  /** Validated against the carrier's target schema. */
  target: Record<string, unknown>
  /** Tried in order when this one fails. */
  fallback?: CarrierRef[] | null
}

export type Carry = "commits" | "staged" | "unstaged" | "untracked"

export interface Choice {
  id: string
  label: string
  intent?: ChoiceIntent | null
  scope?: ChoiceScope | null
  amends?: unknown
  description?: string | null
}

/** `choice "<value>" label= tags=`, or a bare `choice "<value>"`. */
export type ChoiceData = string | ChoiceFull

export interface ChoiceFull {
  name: string
  label?: string | null
  description?: string | null
  /** Semantic tags, separated by spaces: `plan`, `bypass`, `ask`, `read-only` and so on. */
  tags?: string | null
  /** The harness's own value, when it differs from `name`. */
  harness?: string | null
}

export type ChoiceIntent = "allow" | "deny" | "neutral"

export type ChoiceScope = "once" | "session" | "always"

/** Fields every item kind has. */
export interface CommandItem {
  id: string
  turn?: string | null
  binding?: string | null
  parent?: string | null
  source?: ItemSource | null
  ref?: Ref | null
  origin?: Origin | null
  _meta?: Record<string, unknown> | null
  command: string
  cwd: string
  status: RunStatus
  output?: string | null
  exit?: number | null
}

/** Fields every item kind has. */
export interface CompactionItem {
  id: string
  turn?: string | null
  binding?: string | null
  parent?: string | null
  source?: ItemSource | null
  ref?: Ref | null
  origin?: Origin | null
  _meta?: Record<string, unknown> | null
  trigger: CompactionTrigger
  summary?: string | null
}

export type CompactionTrigger = "manual" | "auto"

export interface CompletionValue {
  const: unknown
  title: string
}

export interface ConfigOption {
  id: string
  role?: OpenOptionRole | null
  label: string
  description?: string | null
  schema: unknown
  value: unknown
  default?: unknown
  applies: Applies
  dependsOn?: DependsOn | null
  complete?: boolean | null
  _meta?: Record<string, unknown> | null
}

/** The `connection` of `InitializeResult`: who the caller is and what it may do. */
export interface ConnectionGrant {
  id: string
  principal: string
  scopes: Scope[]
  sessions?: string[] | null
}

export interface ConnectionInfo {
  id: string
  name: string
  principal: string
  device?: string | null
}

/** Whether the carrier's credentials reach more of the peer than its link endpoint (spec 24.4). */
export type Containment = "restricted" | "shell" | "unknown"

export interface ContextEntry {
  uri: string
  title?: string | null
  text: string
}

export interface ContextUsage {
  used: number
  window: number
}

export type Control = "native" | "tui"

/**
 * `correlate.kdl`: per item kind, how items of two sources match and which source wins
 * (spec 32.1).
 */
export interface CorrelateFile {
  kind?: CorrelateKind[]
}

/**
 * `kind "tool" { match "ref" "position"; prefer "native"; history "transcript"; }`. `kind "*"`
 * covers the kinds not named.
 */
export interface CorrelateKind {
  name: string
  /** The keys tried in order: `ref` (the harness id), `position` (turn and order), `text`. */
  match?: unknown[]
  /** The source that wins while the binding runs. */
  prefer?: string[]
  /** The source that wins when history is read again. */
  history?: string[]
  /** Fields compared in shadow mode; all of `set` when empty. */
  compare?: unknown[]
}

export interface Cost {
  amount: number
  currency: string
}

export type DeletedKind = "session" | "workspace"

export type Delivery = "auto" | "queue" | "steer" | "interrupt"

export interface DependsOn {
  option: string
  schemas: Record<string, unknown>
}

/** `depends-on "<option>" { when "<value>" { choice ... } }`; `when "*"` matches any other value. */
export interface DependsOnData {
  name: string
  when?: WhenChoices[]
}

/** `data` of `-32020` and `-32040`. */
export interface DetailData {
  detail: string
}

/** A harness session no Parrot session has. */
export interface DiscoveredSession {
  ref: Ref
  title?: string | null
  cwd: string
  updated: string
}

export type DriverKind = "builtin" | "process"

/** Params of the driver's `update` notification. */
export interface DriverUpdate {
  /** Empty in `history/read` results. */
  binding: string
  /** A subagent key (30.3). */
  subagent?: string | null
  update: OpenDriverUpdateKind
}

/** Ids inside are driver keys. */
export type DriverUpdateKind =
  | {
      type: "state"
      state: State
      background: Background[]
      detail?: string | null
    }
  | {
      type: "item"
      item: ItemPatch
    }
  | {
      type: "turn"
      turn: Turn
    }
  | {
      type: "request"
      request: Request
    }
  | {
      type: "options"
      options: ConfigOption[]
      level: PermissionLevel
      network: boolean
    }
  | {
      type: "actions"
      actions: Action[]
    }
  | {
      type: "usage"
      usage: Usage
    }
  | {
      type: "ref"
      ref: Ref
    }
  | {
      type: "mcp"
      servers: McpServer[]
    }
  | {
      type: "notice"
      level: NoticeLevel
      text: string
      code?: string | null
    }
  | {
      type: "frame"
      source: string
      data: unknown
    }

export type Effect = "read" | "write" | "turn" | "destructive"

/** The `{}` result of methods that return nothing; unknown fields are kept. */
export type Empty = Record<string, unknown>

export interface EnvListResult {
  envs: Environment[]
}

export type EnvStatus = "online" | "offline"

export interface EnvelopeNotification {
  jsonrpc: string
  method: string
  params?: unknown
}

export interface EnvelopeRequest {
  jsonrpc: string
  id: unknown
  method: string
  params?: unknown
}

export interface EnvelopeResponse {
  jsonrpc: string
  id: unknown
  result?: unknown
  error?: RpcError | null
}

export interface Environment {
  id: string
  runtime: string
  name: string
  /** Absent for the local environment. */
  link?: string | null
  os: Os
  arch: string
  status: EnvStatus
  roots: string[]
  features: Record<string, unknown>
  _meta?: Record<string, unknown> | null
}

/** Fields every item kind has. */
export interface ErrorItem {
  id: string
  turn?: string | null
  binding?: string | null
  parent?: string | null
  source?: ItemSource | null
  ref?: Ref | null
  origin?: Origin | null
  _meta?: Record<string, unknown> | null
  message: string
  code?: string | null
}

export interface FileChange {
  path: string
  op: FileOp
  from?: string | null
  diff?: string | null
}

/** Fields every item kind has. */
export interface FileChangeItem {
  id: string
  turn?: string | null
  binding?: string | null
  parent?: string | null
  source?: ItemSource | null
  ref?: Ref | null
  origin?: Origin | null
  _meta?: Record<string, unknown> | null
  changes: FileChange[]
  status: RunStatus
}

export type FileOp = "add" | "modify" | "delete" | "move"

/** `data` of `-32003`. */
export interface ForbiddenData {
  reason: string
}

/** Fields every item kind has. */
export interface HandoffItem {
  id: string
  turn?: string | null
  binding?: string | null
  parent?: string | null
  source?: ItemSource | null
  ref?: Ref | null
  origin?: Origin | null
  _meta?: Record<string, unknown> | null
  from: string
  to: string
  via: BindingVia
}

export interface HarnessInfo {
  version: string
  verified: boolean
}

export interface HistoryDiscoverResult {
  found: DiscoveredSession[]
  next?: string | null
}

export interface HistoryReadParams {
  agent: string
  ref: Ref
  cursor?: string | null
}

/** The stored history as updates; each has `binding: ""` (Appendix E4). */
export interface HistoryReadResult {
  updates: DriverUpdate[]
  next?: string | null
}

/** Fields every item kind has. */
export interface HookItem {
  id: string
  turn?: string | null
  binding?: string | null
  parent?: string | null
  source?: ItemSource | null
  ref?: Ref | null
  origin?: Origin | null
  _meta?: Record<string, unknown> | null
  event: string
  name: string
  outcome: HookOutcome
  output?: string | null
}

export type HookOutcome = "ok" | "blocked" | "error"

export interface IndexSnapshot {
  sessions: SessionSummary[]
  next?: string | null
  agents: Agent[]
  envs: Environment[]
}

export interface InitializeParams {
  protocol: number[]
  peer: PeerInfo
  features: Record<string, unknown>
  /** `FunctionSpec`s this client offers (25.2). */
  provides?: unknown[] | null
  _meta?: Record<string, unknown> | null
}

export interface InitializeResult {
  protocol: number
  peer: PeerInfo
  runtime: RuntimeInfo
  features: Record<string, unknown>
  connection: ConnectionGrant
  _meta?: Record<string, unknown> | null
}

export type Isolation = "none" | "sandbox" | "user" | "container"

/** `data` of `-32051`. */
export interface IsolationData {
  required: string
  available?: string | null
}

export type ItemBody =
  | UserItem
  | TextItem
  | ReasoningItem
  | ToolItem
  | ScriptItem
  | CommandItem
  | FileChangeItem
  | PlanItem
  | SubagentItem
  | WebItem
  | ReviewItem
  | CompactionItem
  | HookItem
  | HandoffItem
  | ErrorItem

/** Fields every item kind has. */
export interface ItemCommon {
  id: string
  turn?: string | null
  binding?: string | null
  parent?: string | null
  source?: ItemSource | null
  ref?: Ref | null
  origin?: Origin | null
  _meta?: Record<string, unknown> | null
}

export interface ItemGetParams {
  session: string
  item: string
}

export interface ItemGetResult {
  item: OpenItemBody
}

export type ItemKind =
  | "user"
  | "assistant"
  | "reasoning"
  | "tool"
  | "script"
  | "command"
  | "file_change"
  | "plan"
  | "subagent"
  | "web"
  | "review"
  | "compaction"
  | "hook"
  | "handoff"
  | "error"

export interface ItemPatch {
  id: string
  /** Required on the first patch; never changes. */
  kind?: OpenItemKind | null
  turn?: string | null
  binding?: string | null
  parent?: string | null
  source?: ItemSource | null
  ref?: Ref | null
  origin?: Origin | null
  /** Replace fields; `null` removes one. */
  set?: Record<string, unknown> | null
  /** Append to string fields, after `set`. */
  append?: Record<string, string> | null
  streaming?: boolean | null
  /** Always `true` when present: the item is gone. */
  removed?: boolean | null
  _meta?: Record<string, unknown> | null
}

export type ItemSource = "native" | "transcript" | "tui" | "inferred" | "runtime"

export interface LevelMap {
  level: PermissionLevel
  network?: boolean
  when?: Record<string, unknown>[]
}

export interface LevelTools {
  name: PermissionLevel
  tool?: string[]
}

/**
 * `levels.kdl`: the tools each level allows, and the level each combination of option values
 * gives (spec 15.2, Appendix E4).
 */
export interface LevelsFile {
  /** `level "read" { tool "Read"; tool "mcp__*"; }`. A tool in no level is `host`. */
  level?: LevelTools[]
  /**
   * `map level="write" network=#false { when permissionMode="acceptEdits"; }`, first match
   * wins. Several `when` nodes are alternatives; the properties of one must all hold. A
   * combination no `map` matches is `host` with network.
   */
  map?: LevelMap[]
  /** Arguments that pin the harness's settings sources under a ceiling. */
  pin?: PinArgs[]
}

export interface Link {
  id: string
  name: string
  peer: LinkPeer
  /** Absent if this runtime only accepts. */
  carrier?: CarrierRef | null
  /** What the peer may do here. */
  grant: Scope[]
  offer: LinkOffer
  containment: Containment
  status: LinkStatus
  /** The carrier instance of the current connection. */
  via?: string | null
  since?: string | null
}

export interface LinkAddParams {
  name: string
  carrier: CarrierRef
  /**
   * A pairing code from the peer's `link/invite`; without it, the runtime pairs over the
   * carrier with `ask`.
   */
  code?: string | null
  grant: Scope[]
  offer: LinkOffer
  ask?: LinkAsk | null
}

/** The grant and offer `link/add` asks the peer for when it pairs over the carrier (spec 24.3). */
export interface LinkAsk {
  grant: Scope[]
  offer: LinkOffer
}

export interface LinkInviteParams {
  name: string
  grant: Scope[]
  offer: LinkOffer
  expires?: string | null
  /**
   * Set when the peer's SSH key was written to `authorized_keys` restricted to
   * `parrot connect --link`: the peer then sees its link as `restricted` (spec 24.4).
   */
  authorizedKey?: boolean | null
}

export interface LinkInviteResult {
  /** The one-time pairing code. It is a secret: shown once, never logged. */
  code: string
  link: Link
}

export type LinkListParams = Record<string, unknown>

export interface LinkListResult {
  links: Link[]
}

/** What a runtime shows a linked peer. */
export interface LinkOffer {
  /** Whether the peer sees this environment and its agents, and may create sessions in it. */
  env: boolean
  sessions: OfferedSessions
}

/** The other runtime of a link, pinned at pairing. */
export interface LinkPeer {
  runtime: string
  /** The peer's X25519 public key, in hex. */
  key: string
}

export interface LinkRemoveParams {
  link: string
}

export interface LinkResult {
  link: Link
}

export type LinkStatus = "connected" | "connecting" | "down" | "unpaired"

export interface LinkUpdateParams {
  link: string
  carrier?: CarrierRef | null
  grant?: Scope[] | null
  offer?: LinkOffer | null
}

export interface McpInjection {
  url?: string | null
  command?: string[] | null
  token: string
}

export interface McpServer {
  name: string
  status: McpStatus
  tools?: number | null
}

export type McpStatus = "connected" | "failed" | "pending" | "needs_auth"

export type MergeMethod = "merge" | "squash"

export type Message = EnvelopeRequest | EnvelopeResponse | EnvelopeNotification

/** `data` of `-32050`: the runtime that owns the session now. */
export interface MovedData {
  runtime: string
}

/** `data` of `-32011`. */
export interface NoSharedVersionData {
  supported: number[]
}

export type NoticeLevel = "info" | "warning" | "error"

export type Notify = "none" | "normal" | "urgent"

/**
 * Which sessions a link's peer sees. `linked`: only sessions created over the link or moved
 * here over it, with their descendants.
 */
export type OfferedSessions = "none" | "linked" | "all"

export type OnParentEnd = "continue" | "stop"

export type OpenActionSource = ActionSource

export type OpenBackgroundKind = BackgroundKind

export type OpenDriverUpdateKind = DriverUpdateKind

export type OpenItemBody = ItemBody

export type OpenItemKind = ItemKind

export type OpenOptionRole = OptionRole

export type OpenOriginKind = OriginKind

export type OpenRequestKind = RequestKind

export type OpenSubject = Subject

export type OpenTaskStatus = TaskStatus

export type OpenTurnStop = TurnStop

export type OpenUpdate = Update

export interface OptionCompleteParams {
  session?: string | null
  agent?: string | null
  option: string
  prefix: string
  limit?: number | null
}

export interface OptionCompleteResult {
  values: CompletionValue[]
}

/** `option "<id>" role= label= applies= default= { choice ...; depends-on ... }`. */
export interface OptionData {
  /** The option id. */
  name: string
  role?: string | null
  label: string
  description?: string | null
  /** `start`, `now` or `next-turn`; `now` when absent. */
  applies?: string | null
  default?: unknown
  /** The JSON type of a free value, such as `string`, when there are no choices. */
  type?: string | null
  /** The harness's own name for the option, such as a config key or a flag. */
  harness?: string | null
  /** The harness reports the choices at runtime; these are the fallback. */
  dynamic?: boolean | null
  choice?: ChoiceData[]
  "depends-on"?: DependsOnData[]
}

/** `option/list` (Appendix E8). */
export interface OptionListParams {
  session: string
}

export interface OptionListResult {
  options: ConfigOption[]
  level?: PermissionLevel | null
  network?: boolean | null
}

export type OptionRole = "model" | "effort" | "mode" | "sandbox" | "speed"

export interface OptionSetParams {
  session: string
  values: Record<string, unknown>
  key?: string | null
}

export interface OptionSetResult {
  options: ConfigOption[]
  reset: string[]
  level?: PermissionLevel | null
}

/** `options.kdl`: options the harness doesn't report itself (spec 15.1, 31). */
export interface OptionsFile {
  option?: OptionData[]
}

export interface Origin {
  kind: OpenOriginKind
  principal?: string | null
  name?: string | null
  cause?: OriginCause | null
  /** The well-known action that produced the item, such as `continue`. */
  action?: string | null
}

export interface OriginCause {
  session: string
  item?: string | null
}

export type OriginKind = "client" | "harness" | "remote" | "hook" | "session" | "wake" | "runtime"

export type Os = "linux" | "macos" | "windows"

export interface PeerInfo {
  kind: PeerKind
  name: string
  version: string
}

export type PeerKind = "client" | "runtime" | "driver"

/** What an agent can do without asking anyone, lowest first. */
export type PermissionLevel = "read" | "write" | "sandboxed" | "host"

/** Hold the harness to at most this level. */
export interface Pin {
  level: PermissionLevel
  network: boolean
}

/** `pin { arg "--setting-sources" "" }`: the arguments to pass, in order. */
export interface PinArgs {
  arg?: unknown[]
}

export interface PlanEntry {
  text: string
  status: PlanStatus
}

/** Fields every item kind has. */
export interface PlanItem {
  id: string
  turn?: string | null
  binding?: string | null
  parent?: string | null
  source?: ItemSource | null
  ref?: Ref | null
  origin?: Origin | null
  _meta?: Record<string, unknown> | null
  entries?: PlanEntry[] | null
  text?: string | null
}

export type PlanStatus = "pending" | "active" | "done"

export type PromptDisposition = "started" | "steered" | "queued" | "handled"

export interface QueueEditParams {
  session: string
  id: string
  text: string
}

export interface QueueEntry {
  id: string
  message?: string | null
  text: string
  origin: Origin
  kind: QueueEntryKind
}

/** Appendix E4 adds `action`. */
export type QueueEntryKind = "prompt" | "action" | "wake"

/** `queue/list` (Appendix E8). */
export interface QueueListResult {
  queue: QueueState
}

export interface QueueMoveParams {
  session: string
  id: string
  index: number
}

export interface QueueRemoveParams {
  session: string
  id: string
}

/** Params of `queue/clear`, `queue/resume` and `queue/list`. */
export interface QueueSessionParams {
  session: string
}

export interface QueueState {
  paused: boolean
  entries: QueueEntry[]
}

export interface RateLimit {
  name: string
  used: number
  limit?: number | null
  resets?: string | null
}

export type ReadDirection = "back" | "forward"

/** Fields every item kind has. */
export interface ReasoningItem {
  id: string
  turn?: string | null
  binding?: string | null
  parent?: string | null
  source?: ItemSource | null
  ref?: Ref | null
  origin?: Origin | null
  _meta?: Record<string, unknown> | null
  text: string
  summary?: boolean | null
}

export type RebindRequire = "exact" | "any"

/** A harness's own reference to a session, item or request. */
export interface Ref {
  id: string
  strength: RefStrength
}

export type RefStrength = "strong" | "weak"

export interface Request {
  id: string
  /** Omitted when a driver reports the request. */
  session?: string
  /** Sessions from the root to this one; omitted by drivers. */
  path?: string[]
  ref?: Ref | null
  turn?: string | null
  item?: string | null
  kind: OpenRequestKind
  title: string
  description?: string | null
  subject?: OpenSubject | null
  level: PermissionLevel
  network?: boolean | null
  choices?: Choice[] | null
  input?: unknown
  blocking: boolean
  answerable: Answerable
  expires?: string | null
  fallback?: Answer | null
  notify: Notify
  status: RequestStatus
  answer?: Answer | null
  /** A principal, or `harness`, `fallback`, `ceiling`. */
  answeredBy?: string | null
  _meta?: Record<string, unknown> | null
}

export interface RequestAnswerParams {
  request: string
  answer: Answer
  key?: string | null
}

export type RequestKind = "approval" | "question" | "plan" | "input" | "auth"

export interface RequestListParams {
  tree?: string | null
  session?: string | null
  status?: RequestStatus | null
  cursor?: string | null
}

export interface RequestListResult {
  requests: Request[]
  next?: string | null
}

export type RequestStatus = "open" | "answered" | "expired" | "cancelled"

export type Requirement = "terminal"

/** Fields every item kind has. */
export interface ReviewItem {
  id: string
  turn?: string | null
  binding?: string | null
  parent?: string | null
  source?: ItemSource | null
  ref?: Ref | null
  origin?: Origin | null
  _meta?: Record<string, unknown> | null
  target: unknown
  findings?: unknown
}

export interface RpcError {
  code: number
  message: string
  data?: unknown
}

/** The status of `script`, `subagent`, `command`, `file_change` and `web` items (Appendix E8). */
export type RunStatus = "running" | "ok" | "error" | "cancelled"

export interface RuntimeInfo {
  id: string
  env: string
}

export type Scope =
  | "read"
  | "prompt"
  | "answer"
  | "configure"
  | "manage"
  | "delegate"
  | "workspace"
  | "destructive"
  | "functions"

export interface ScreenPos {
  row: number
  col: number
}

export interface ScreenSize {
  rows: number
  cols: number
}

/** Fields every item kind has. */
export interface ScriptItem {
  id: string
  turn?: string | null
  binding?: string | null
  parent?: string | null
  source?: ItemSource | null
  ref?: Ref | null
  origin?: Origin | null
  _meta?: Record<string, unknown> | null
  language: string
  /** Absent when the harness doesn't expose it. */
  code?: string | null
  status: RunStatus
  background: boolean
  result?: unknown
  error?: unknown
}

export interface Session {
  id: string
  /** The owning runtime: where the current binding runs. */
  runtime: string
  movedTo?: string | null
  title?: string | null
  workspace: string
  cwd: string
  state: State
  background: Background[]
  binding?: Binding | null
  bindings: string[]
  kind: SessionKind
  parent?: SessionParent | null
  /** A `Task` (19). */
  task?: unknown
  /** A `Seed` (23). */
  seed?: unknown
  origin: Origin
  grant: Scope[]
  /** A `Ceiling` (22). */
  ceiling: unknown
  advisory: boolean
  budget?: BudgetState | null
  mcp?: McpServer[] | null
  created: string
  updated: string
  archived: boolean
  _meta?: Record<string, unknown> | null
}

export interface SessionAdoptParams {
  agent: string
  ref: Ref
  workspace?: WorkspaceRequest | null
  start?: boolean | null
  key?: string | null
}

export interface SessionArchiveParams {
  session: string
  archived: boolean
}

/** The result of `session/start` and `session/rebind`. */
export interface SessionBindingResult {
  binding: Binding
}

export interface SessionCancelParams {
  session: string
  tree?: boolean | null
}

export interface SessionCancelResult {
  interrupted: boolean
  queued: number
}

/** The second phase of a move (E9): keyed and idempotent. */
export interface SessionCommitParams {
  session: string
  key: string
}

export interface SessionDiscoverParams {
  agent: string
  cwd?: string | null
  cursor?: string | null
  limit?: number | null
}

export interface SessionDiscoverResult {
  found: DiscoveredSession[]
  next?: string | null
}

export interface SessionGetResult {
  session: Session
}

export type SessionKind = "root" | "child" | "embedded"

export interface SessionListParams {
  parent?: string | null
  workspace?: string | null
  agent?: string | null
  env?: string | null
  archived?: boolean | null
  cursor?: string | null
  limit?: number | null
}

export interface SessionListResult {
  sessions: SessionSummary[]
  next?: string | null
}

/** `NewSession` in the spec. */
export interface SessionNewParams {
  agent: string
  env?: string | null
  workspace: WorkspaceRequest
  cwd?: string | null
  options?: Record<string, unknown> | null
  title?: string | null
  control?: Control | null
  start?: boolean | null
  grant?: Scope[] | null
  parent?: string | null
  /** A `TaskRequest` (19). */
  task?: unknown
  /** A `SeedRequest` (23). */
  seed?: unknown
  /** A `Ceiling` (22). */
  ceiling?: unknown
  /** A `Wake` (20). */
  wake?: unknown
  /** A `Budget` allotment (Appendix E9). */
  budget?: Budget | null
  onParentEnd?: OnParentEnd | null
  key?: string | null
  _meta?: Record<string, unknown> | null
}

/** The result of `session/new` and `session/adopt`. */
export interface SessionNewResult {
  session: Session
  subscription: string
  head: string
}

/** The first phase of a move (E1): the owner offers a session to the target. */
export interface SessionOfferParams {
  session: string
  /** The session as the owner stores it. */
  record: unknown
  /** The session's workspace; `transfer` names its blob when it travels. */
  workspace?: unknown
  /** The transfer that carries the session's items. */
  log?: string | null
  queue?: unknown
  requests?: unknown[] | null
  turnKeys?: Record<string, unknown> | null
  tasks?: unknown[] | null
}

export interface SessionOfferResult {
  /** The binding the target started idle. */
  binding?: unknown
}

export interface SessionParent {
  session: string
  item?: string | null
}

export interface SessionPromptParams {
  session: string
  text: string
  context?: ContextEntry[] | null
  attachments?: Attachment[] | null
  delivery?: Delivery | null
  key?: string | null
  _meta?: Record<string, unknown> | null
}

export interface SessionPromptResult {
  message: string
  disposition: PromptDisposition
  queued?: string | null
}

export interface SessionReadParams {
  session: string
  cursor?: string | null
  direction?: ReadDirection | null
  limit?: number | null
}

export interface SessionReadResult {
  items: OpenItemBody[]
  before?: string | null
  after?: string | null
}

export interface SessionRebindParams {
  session: string
  agent?: string | null
  env?: string | null
  workspace?: WorkspaceRequest | null
  require?: RebindRequire | null
  interrupt?: boolean | null
  key?: string | null
}

/** Params of methods that take only `{ session }`: `session/get`, `session/delete`. */
export interface SessionRef {
  session: string
}

export interface SessionRenameParams {
  session: string
  title: string
}

export interface SessionRewindParams {
  session: string
  turn: string
  files: boolean
  key?: string | null
}

export interface SessionSnapshot {
  session: Session
  /** The newest items, oldest first. */
  items: OpenItemBody[]
  before?: string | null
  requests: Request[]
  queue: QueueState
  options: ConfigOption[]
  actions: Action[]
  usage?: Usage | null
  subscribers: ConnectionInfo[]
}

export interface SessionStartParams {
  session: string
  options?: Record<string, unknown> | null
  key?: string | null
}

export interface SessionStopParams {
  session: string
  tree?: boolean | null
}

/** What lists and the index feed carry. */
export interface SessionSummary {
  id: string
  runtime: string
  title?: string | null
  state: State
  kind: SessionKind
  parent?: string | null
  agent?: string | null
  env?: string | null
  task?: OpenTaskStatus | null
  archived: boolean
  updated: string
}

export interface SessionTreeParams {
  session: string
  cursor?: string | null
  limit?: number | null
}

export interface SessionTreeResult {
  nodes: TreeNode[]
  next?: string | null
}

export interface Snapshot {
  sessions?: SessionSnapshot[] | null
  index?: IndexSnapshot | null
}

export type State =
  | "starting"
  | "idle"
  | "running"
  | "requires_action"
  | "stopped"
  | "lost"
  | "unrecognised"
  | "unknown"

/** A subagent. Drivers report `key` (30.3); the runtime fills `session`. */
export interface SubagentItem {
  id: string
  turn?: string | null
  binding?: string | null
  parent?: string | null
  source?: ItemSource | null
  ref?: Ref | null
  origin?: Origin | null
  _meta?: Record<string, unknown> | null
  title: string
  session?: string | null
  status?: RunStatus | null
  key?: string | null
}

export type Subject =
  | {
      type: "tool"
      tool: string
      input: unknown
      editable?: boolean | null
    }
  | {
      type: "command"
      command: string
      cwd: string
    }
  | {
      type: "files"
      paths: string[]
      diff?: string | null
    }
  | {
      type: "network"
      host: string
    }
  | {
      type: "plan"
      text: string
    }
  | {
      type: "function"
      function: string
      input: unknown
    }

export interface SubscribeParams {
  target: Target
  replayFrom?: string | null
  items?: number | null
}

export interface SubscribeResult {
  subscription: string
  head: string
  snapshot?: Snapshot | null
}

export type Target =
  | {
      session: string
    }
  | {
      tree: string
    }
  | {
      index: boolean
    }

/** A task's status as summaries carry it. `interrupted` comes from Appendix E7. */
export type TaskStatus = "running" | "awaiting" | "completed" | "failed" | "cancelled" | "interrupted"

/** An `assistant` item; `text` is Markdown. */
export interface TextItem {
  id: string
  turn?: string | null
  binding?: string | null
  parent?: string | null
  source?: ItemSource | null
  ref?: Ref | null
  origin?: Origin | null
  _meta?: Record<string, unknown> | null
  text: string
}

/** `input` excludes cache reads. */
export interface TokenUsage {
  input: number
  output: number
  cacheRead?: number | null
  cacheWrite?: number | null
  reasoning?: number | null
}

/** Fields every item kind has. */
export interface ToolItem {
  id: string
  turn?: string | null
  binding?: string | null
  parent?: string | null
  source?: ItemSource | null
  ref?: Ref | null
  origin?: Origin | null
  _meta?: Record<string, unknown> | null
  tool: string
  title: string
  input: unknown
  status: ToolStatus
  output?: unknown
  server?: string | null
  caller: Caller
  function?: string | null
}

export type ToolStatus = "pending" | "running" | "paused" | "ok" | "error" | "cancelled"

export interface TreeNode {
  session: SessionSummary
  depth: number
}

export interface Turn {
  id: string
  /** Omitted when a driver reports the turn. */
  binding?: string
  status: TurnStatus
  stop?: OpenTurnStop | null
  continuable: boolean
  input?: string | null
  started: string
  ended?: string | null
  error?: TurnError | null
  usage?: Usage | null
  _meta?: Record<string, unknown> | null
}

export interface TurnError {
  code: string
  message: string
  retryable: boolean
}

export type TurnStatus = "running" | "completed" | "interrupted" | "failed"

export type TurnStop = "end" | "max_tokens" | "turn_limit" | "refusal" | "cancelled" | "budget" | "error"

export interface UnsubscribeParams {
  subscription: string
}

/** `data` of `-32001`. */
export interface UnsupportedData {
  feature: string
}

export type Update =
  | {
      type: "state"
      state: State
      background: Background[]
      detail?: string | null
    }
  | {
      type: "session"
      session: Session
    }
  | {
      type: "item"
      item: ItemPatch
    }
  | {
      type: "turn"
      turn: Turn
    }
  | {
      type: "request"
      request: Request
    }
  | {
      type: "queue"
      queue: QueueState
    }
  | {
      type: "options"
      options: ConfigOption[]
      level?: PermissionLevel | null
      network?: boolean | null
    }
  | {
      type: "actions"
      actions: Action[]
    }
  | {
      type: "usage"
      usage: Usage
    }
  | {
      type: "notice"
      level: NoticeLevel
      text: string
      code?: string | null
    }
  | {
      type: "summary"
      session: SessionSummary
    }
  | {
      type: "agent"
      agent: Agent
    }
  | {
      type: "env"
      env: Environment
    }
  | {
      type: "workspace"
      workspace: Workspace
    }
  | {
      type: "deleted"
      kind: DeletedKind
      id: string
    }

/** Params of the `update` notification. */
export interface UpdateParams {
  subscription: string
  /** The update's place in the runtime's sequence; an opaque cursor (Appendix E2). */
  seq: string
  /** The runtime that emitted it (Appendix E2). */
  runtime: string
  at: string
  session?: string | null
  /** Unknown update types are kept raw and ignored. */
  update: OpenUpdate
  _meta?: Record<string, unknown> | null
}

export interface Usage {
  tokens?: TokenUsage | null
  cost?: Cost | null
  context?: ContextUsage | null
  rateLimits?: RateLimit[] | null
  /** For embedded sessions: the session whose usage already includes this. */
  countedIn?: string | null
  _meta?: Record<string, unknown> | null
}

/** Fields every item kind has. */
export interface UserItem {
  id: string
  turn?: string | null
  binding?: string | null
  parent?: string | null
  source?: ItemSource | null
  ref?: Ref | null
  origin?: Origin | null
  _meta?: Record<string, unknown> | null
  text: string
  context?: ContextEntry[] | null
  attachments?: Attachment[] | null
  status: UserStatus
  /** Set by a driver when the item echoes a runtime prompt (Appendix E4). */
  message?: string | null
}

export type UserStatus = "queued" | "sent" | "handled" | "uncertain"

/** Fields every item kind has. */
export interface WebItem {
  id: string
  turn?: string | null
  binding?: string | null
  parent?: string | null
  source?: ItemSource | null
  ref?: Ref | null
  origin?: Origin | null
  _meta?: Record<string, unknown> | null
  query?: string | null
  url?: string | null
  status: RunStatus
  results?: unknown
}

export interface WhenChoices {
  name: string
  default?: unknown
  choice?: ChoiceData[]
}

export interface Workspace {
  id: string
  env: string
  kind: WorkspaceKind
  path: string
  repo?: string | null
  branch?: string | null
  base?: string | null
  /** The session that created a worktree. */
  owner?: string | null
  sessions: string[]
  created: string
  _meta?: Record<string, unknown> | null
}

export interface WorkspaceCreateParams {
  env: string
  request: WorkspaceRequest
  key?: string | null
}

export type WorkspaceKind = "dir" | "worktree" | "external"

export interface WorkspaceListParams {
  env?: string | null
  session?: string | null
  cursor?: string | null
  limit?: number | null
}

export interface WorkspaceListResult {
  workspaces: Workspace[]
  next?: string | null
}

export interface WorkspaceMergeParams {
  from: string
  into: string
  method: MergeMethod
  message?: string | null
  key?: string | null
}

export interface WorkspaceMergeResult {
  commit?: string | null
  conflicts?: string[] | null
}

/** What a peer's `workspace/transfer` sends: unpack the transfer as a new workspace here. */
export interface WorkspaceReceiveParams {
  workspace: string
  transfer: string
  branch?: string | null
}

export interface WorkspaceReceiveResult {
  workspace: Workspace
}

export interface WorkspaceRemoveParams {
  workspace: string
  force?: boolean | null
}

export type WorkspaceRequest =
  | {
      kind: "dir"
      path: string
    }
  | {
      kind: "worktree"
      repo: string
      branch?: string | null
      base?: string | null
    }
  | {
      kind: "use"
      workspace: string
    }

/** The result of `workspace/create` and `workspace/transfer`. */
export interface WorkspaceResult {
  workspace: Workspace
}

export interface WorkspaceStatusParams {
  workspace: string
}

export interface WorkspaceStatusResult {
  branch?: string | null
  commit?: string | null
  ahead?: number | null
  behind?: number | null
  dirty: boolean
  changed: string[]
}

export interface WorkspaceTransferParams {
  workspace: string
  /** The target environment. */
  to: string
  include?: Carry[] | null
  key?: string | null
}

/** `data` of `-32004`. */
export interface WrongStateData {
  state: string
}

/** The methods a client calls, from spec 39 and Appendix E8. */
export interface Methods {
  "action/invoke": { params: ActionInvokeParams; result: ActionInvokeResult }
  "action/list": { params: ActionListParams; result: ActionListResult }
  "agent/list": { params: AgentListParams; result: AgentListResult }
  "agent/refresh": { params: AgentRefreshParams; result: AgentRefreshResult }
  "binding/action": { params: BindingActionParams; result: BindingActionResult }
  "binding/answer": { params: BindingAnswerParams; result: Empty }
  "binding/cancel": { params: BindingCancelParams; result: BindingCancelResult }
  "binding/fork": { params: BindingForkParams; result: BindingForkResult }
  "binding/keys": { params: BindingKeysParams; result: Empty }
  "binding/options": { params: BindingOptionsParams; result: BindingOptionsResult }
  "binding/prompt": { params: BindingPromptParams; result: BindingPromptResult }
  "binding/screen": { params: BindingRef; result: BindingScreenResult }
  "binding/start": { params: BindingStart; result: BindingStartResult }
  "binding/stop": { params: BindingRef; result: Empty }
  "blob/put": { params: BlobPutParams; result: Empty }
  "carrier/list": { params: CarrierListParams; result: CarrierListResult }
  "env/list": { params: Empty; result: EnvListResult }
  "history/discover": { params: SessionDiscoverParams; result: HistoryDiscoverResult }
  "history/read": { params: HistoryReadParams; result: HistoryReadResult }
  initialize: { params: InitializeParams; result: InitializeResult }
  "item/get": { params: ItemGetParams; result: ItemGetResult }
  "link/add": { params: LinkAddParams; result: LinkResult }
  "link/invite": { params: LinkInviteParams; result: LinkInviteResult }
  "link/list": { params: LinkListParams; result: LinkListResult }
  "link/remove": { params: LinkRemoveParams; result: Empty }
  "link/update": { params: LinkUpdateParams; result: LinkResult }
  "option/complete": { params: OptionCompleteParams; result: OptionCompleteResult }
  "option/list": { params: OptionListParams; result: OptionListResult }
  "option/set": { params: OptionSetParams; result: OptionSetResult }
  "queue/clear": { params: QueueSessionParams; result: Empty }
  "queue/edit": { params: QueueEditParams; result: Empty }
  "queue/list": { params: QueueSessionParams; result: QueueListResult }
  "queue/move": { params: QueueMoveParams; result: Empty }
  "queue/pause": { params: QueueSessionParams; result: Empty }
  "queue/remove": { params: QueueRemoveParams; result: Empty }
  "queue/resume": { params: QueueSessionParams; result: Empty }
  "request/answer": { params: RequestAnswerParams; result: Empty }
  "request/list": { params: RequestListParams; result: RequestListResult }
  "session/archive": { params: SessionArchiveParams; result: Empty }
  "session/cancel": { params: SessionCancelParams; result: SessionCancelResult }
  "session/commit": { params: SessionCommitParams; result: SessionOfferResult }
  "session/delete": { params: SessionRef; result: Empty }
  "session/get": { params: SessionRef; result: SessionGetResult }
  "session/keys": { params: SessionRef; result: Empty }
  "session/list": { params: SessionListParams; result: SessionListResult }
  "session/new": { params: SessionNewParams; result: SessionNewResult }
  "session/offer": { params: SessionOfferParams; result: SessionOfferResult }
  "session/prompt": { params: SessionPromptParams; result: SessionPromptResult }
  "session/rebind": { params: SessionRebindParams; result: SessionBindingResult }
  "session/rename": { params: SessionRenameParams; result: Empty }
  "session/rewind": { params: SessionRewindParams; result: Empty }
  "session/screen": { params: SessionRef; result: BindingScreenResult }
  "session/start": { params: SessionStartParams; result: SessionBindingResult }
  "session/stop": { params: SessionStopParams; result: Empty }
  "session/tree": { params: SessionTreeParams; result: SessionTreeResult }
  subscribe: { params: SubscribeParams; result: SubscribeResult }
  unsubscribe: { params: UnsubscribeParams; result: Empty }
  "workspace/create": { params: WorkspaceCreateParams; result: WorkspaceResult }
  "workspace/list": { params: WorkspaceListParams; result: WorkspaceListResult }
  "workspace/merge": { params: WorkspaceMergeParams; result: WorkspaceMergeResult }
  "workspace/receive": { params: WorkspaceReceiveParams; result: WorkspaceReceiveResult }
  "workspace/remove": { params: WorkspaceRemoveParams; result: Empty }
  "workspace/status": { params: WorkspaceStatusParams; result: WorkspaceStatusResult }
  "workspace/transfer": { params: WorkspaceTransferParams; result: WorkspaceResult }
}

/** The notifications a peer sends (spec 36). */
export interface Notifications {
  update: UpdateParams
}

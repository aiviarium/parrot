// The protocol shapes a client uses, from spec 4, 10.2 and 13.1.

/** What a peer says it is. */
export interface PeerInfo {
  kind: "client" | "runtime" | "driver"
  name: string
  version: string
}

/** The features a peer announces; an absent one is unsupported. */
export type Features = Record<string, object>

export interface InitializeParams {
  protocol: number[]
  peer: PeerInfo
  features: Features
  provides?: unknown[]
}

export interface InitializeResult {
  protocol: number
  peer: PeerInfo
  runtime: { id: string; env: string }
  features: Features
  connection: { id: string; principal: string; scopes: string[]; sessions?: string[] }
}

/** What a subscription follows: one session, its tree, or the index. */
export type Target = { session: string } | { tree: string } | { index: true }

export interface SubscribeParams {
  target: Target
  replayFrom?: number
  items?: number
}

export interface Subscription {
  subscription: string
  head: number
  snapshot?: unknown
}

/** The `update` notification a subscription delivers. */
export interface UpdateParams {
  subscription: string
  seq: number
  at: string
  session?: string
  update: unknown
}

export interface PromptParams {
  session: string
  text: string
  context?: { uri: string; title?: string; text: string }[]
  attachments?: { name: string; type: string; data?: string; uri?: string }[]
  delivery?: "auto" | "queue" | "steer" | "interrupt"
  key?: string
}

export interface PromptResult {
  message: string
  disposition: "started" | "steered" | "queued" | "handled"
  queued?: string
}

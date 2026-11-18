// The protocol shapes a client uses, from spec 4, 10.2 and 13.1, generated in ./schema.

export type {
  InitializeParams,
  InitializeResult,
  PeerInfo,
  SessionPromptParams as PromptParams,
  SessionPromptResult as PromptResult,
  SubscribeParams,
  SubscribeResult as Subscription,
  Target,
  UpdateParams,
} from "./schema"

/** The features a peer announces; an absent one is unsupported. */
export type Features = Record<string, unknown>

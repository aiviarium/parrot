// Generated from spec/schema/parrot.schema.json by scripts/generate.ts. Do not edit.

/** `data` of `-32020` and `-32040`. */
export interface DetailData {
  detail: string
}

/** `data` of `-32003`. */
export interface ForbiddenData {
  reason: string
}

/** `data` of `-32051`. */
export interface IsolationData {
  required: string
  available?: string | null
}

export type Message = Request | Response | Notification

/** `data` of `-32050`: the runtime that owns the session now. */
export interface MovedData {
  runtime: string
}

/** `data` of `-32011`. */
export interface NoSharedVersionData {
  supported: number[]
}

export interface Notification {
  jsonrpc: string
  method: string
  params?: unknown
}

export interface Request {
  jsonrpc: string
  id: unknown
  method: string
  params?: unknown
}

export interface Response {
  jsonrpc: string
  id: unknown
  result?: unknown
  error?: RpcError | null
}

export interface RpcError {
  code: number
  message: string
  data?: unknown
}

/** `data` of `-32001`. */
export interface UnsupportedData {
  feature: string
}

/** `data` of `-32004`. */
export interface WrongStateData {
  state: string
}

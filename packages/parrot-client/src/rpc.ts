import type { EnvelopeResponse, Message } from "./schema"
import type { Transport } from "./transport"

/** A JSON-RPC error the runtime sent back, or one about the connection itself. */
export class RpcError extends Error {
  readonly code: number
  readonly data: unknown

  constructor(code: number, message: string, data?: unknown) {
    super(message)
    this.name = "RpcError"
    this.code = code
    this.data = data
  }
}

/** The error for a call that cannot run because the connection is gone. */
export const CONNECTION_CLOSED = -32000
/** The error for a method this side does not implement. */
export const METHOD_NOT_FOUND = -32601

export type NotificationHandler = (params: unknown) => void
export type RequestHandler = (params: unknown) => unknown | Promise<unknown>

interface Pending {
  resolve(value: unknown): void
  reject(error: Error): void
}

/**
 * One JSON-RPC 2.0 connection over a transport. Requests carry increasing ids, notifications are
 * dispatched by method, and requests from the runtime go to a handler registered with `expose`.
 */
export class Connection {
  #transport: Transport
  #nextId = 1
  #pending = new Map<number, Pending>()
  #handlers = new Map<string, Set<NotificationHandler>>()
  #exposed = new Map<string, RequestHandler>()
  #closed = false

  constructor(transport: Transport) {
    this.#transport = transport
    transport.setHandlers({
      message: (text) => this.#receive(text),
      close: (error) => this.#ended(error),
    })
  }

  get closed(): boolean {
    return this.#closed
  }

  /** Sends a request and settles when its response arrives. */
  request<T = unknown>(method: string, params?: unknown): Promise<T> {
    if (this.#closed) return Promise.reject(new RpcError(CONNECTION_CLOSED, "the connection is closed"))
    const id = this.#nextId++
    return new Promise<T>((resolve, reject) => {
      this.#pending.set(id, { resolve: resolve as (value: unknown) => void, reject })
      try {
        this.#send({ jsonrpc: "2.0", id, method, ...(params === undefined ? {} : { params }) })
      } catch (error) {
        this.#pending.delete(id)
        reject(error instanceof Error ? error : new Error(String(error)))
      }
    })
  }

  /** Sends a request of which no answer is expected. */
  notify(method: string, params?: unknown): void {
    if (this.#closed) return
    this.#send({ jsonrpc: "2.0", method, ...(params === undefined ? {} : { params }) })
  }

  /** Listens for a notification; the returned function stops it. */
  on(method: string, handler: NotificationHandler): () => void {
    let set = this.#handlers.get(method)
    if (!set) {
      set = new Set()
      this.#handlers.set(method, set)
    }
    set.add(handler)
    return () => {
      set.delete(handler)
    }
  }

  /** Answers a request the runtime sends, such as a function this client provides. */
  expose(method: string, handler: RequestHandler): () => void {
    this.#exposed.set(method, handler)
    return () => {
      this.#exposed.delete(method)
    }
  }

  /** Closes the transport; pending requests fail with `CONNECTION_CLOSED`. */
  close(): void {
    this.#ended()
    this.#transport.close()
  }

  #send(message: object): void {
    this.#transport.send(JSON.stringify(message))
  }

  #receive(text: string): void {
    let message: Message
    try {
      message = JSON.parse(text) as Message
    } catch {
      return
    }
    if (message !== null && typeof message === "object" && "method" in message) {
      if ("id" in message && message.id !== undefined) {
        void this.#answer(message.method, message.id, message.params)
      } else {
        this.#dispatch(message.method, message.params)
      }
      return
    }
    this.#settle(message as EnvelopeResponse)
  }

  #settle(response: EnvelopeResponse): void {
    if (response === null || typeof response !== "object" || !("id" in response)) return
    const id = typeof response.id === "number" ? response.id : Number(response.id)
    const pending = this.#pending.get(id)
    if (!pending) return
    this.#pending.delete(id)
    if (response.error) {
      pending.reject(new RpcError(response.error.code, response.error.message, response.error.data))
    } else {
      pending.resolve(response.result)
    }
  }

  #dispatch(method: string, params: unknown): void {
    const handlers = this.#handlers.get(method)
    if (!handlers) return
    for (const handler of handlers) {
      try {
        handler(params)
      } catch {
        // A throwing handler belongs to its caller, not to the connection.
      }
    }
  }

  async #answer(method: string, id: unknown, params: unknown): Promise<void> {
    const handler = this.#exposed.get(method)
    if (!handler) {
      this.#send({
        jsonrpc: "2.0",
        id,
        error: { code: METHOD_NOT_FOUND, message: `no method ${method}` },
      })
      return
    }
    try {
      this.#send({ jsonrpc: "2.0", id, result: (await handler(params)) ?? null })
    } catch (error) {
      this.#send({
        jsonrpc: "2.0",
        id,
        error: { code: CONNECTION_CLOSED, message: error instanceof Error ? error.message : String(error) },
      })
    }
  }

  #ended(error?: Error): void {
    if (this.#closed) return
    this.#closed = true
    const reason = error ?? new RpcError(CONNECTION_CLOSED, "the connection closed")
    for (const pending of this.#pending.values()) pending.reject(reason)
    this.#pending.clear()
  }
}

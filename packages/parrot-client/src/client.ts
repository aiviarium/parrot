import type {
  Features,
  InitializeParams,
  InitializeResult,
  PeerInfo,
  PromptParams,
  PromptResult,
  SubscribeParams,
  Subscription,
  Target,
} from "./protocol"
import { Connection } from "./rpc"
import type { ClientMethods, Methods, Notifications } from "./schema"
import { type StdioOptions, stdioTransport } from "./stdio"
import type { Transport } from "./transport"
import { type WebSocketOptions, webSocketTransport } from "./websocket"

const VERSION = "0.1.0"

export interface ClientOptions {
  /** What this client calls itself; the version defaults to the package version. */
  peer?: Partial<PeerInfo>
  /** The protocol versions to offer. Version 1 is the only one so far. */
  protocol?: number[]
  features?: Features
  provides?: unknown[]
}

/**
 * A client of one runtime: JSON-RPC over a WebSocket or a child process's stdio, with the
 * handshake, subscriptions and prompts of spec 4, 10.2 and 13.1.
 */
export class Client {
  readonly connection: Connection
  #initialized?: InitializeResult

  constructor(transport: Transport) {
    this.connection = new Connection(transport)
  }

  /** A client of `ws://` or `wss://` runtime, such as `parrot serve --ws`. */
  static overWebSocket(
    url: string | URL,
    options: WebSocketOptions & { client?: ClientOptions } = {},
  ): Client {
    return new Client(webSocketTransport(url, options))
  }

  /** A client of a child process that speaks JSON-RPC lines on stdio, such as `parrot serve`. */
  static overStdio(
    command: string,
    args: string[] = [],
    options: StdioOptions & { client?: ClientOptions } = {},
  ): Client {
    return new Client(stdioTransport(command, args, options))
  }

  /** The handshake result, once `initialize` succeeded. */
  get initialized(): InitializeResult | undefined {
    return this.#initialized
  }

  request<T = unknown>(method: string, params?: unknown): Promise<T> {
    return this.connection.request<T>(method, params)
  }

  /** Calls a method of the protocol; params and result are typed from the schema (spec 38). */
  call<M extends keyof Methods>(method: M, params: Methods[M]["params"]): Promise<Methods[M]["result"]> {
    return this.connection.request<Methods[M]["result"]>(method, params)
  }

  /** Sends a notification, such as `update`, with params typed from the schema. */
  notify<M extends keyof Notifications>(method: M, params: Notifications[M]): void {
    this.connection.notify(method, params)
  }

  /** Listens for a notification such as `update`; the returned function stops it. */
  on<M extends keyof Notifications>(method: M, handler: (params: Notifications[M]) => void): () => void {
    return this.connection.on(method, (params) => handler(params as Notifications[M]))
  }

  /**
   * Answers a request the runtime sends, for a function this client provides (spec 25.2). Params
   * and result are typed from the schema's `x-client-methods`.
   */
  provide<M extends keyof ClientMethods>(
    method: M,
    handler: (
      params: ClientMethods[M]["params"],
    ) => ClientMethods[M]["result"] | Promise<ClientMethods[M]["result"]>,
  ): () => void {
    return this.connection.expose(method, (params) => handler(params as ClientMethods[M]["params"]))
  }

  /** Sends `initialize` first, as the protocol requires (spec 4). */
  async initialize(options: ClientOptions = {}): Promise<InitializeResult> {
    const params: InitializeParams = {
      protocol: options.protocol ?? [1],
      peer: { kind: "client", name: "@aiviarium/parrot-client", version: VERSION, ...options.peer },
      features: options.features ?? {},
      ...(options.provides ? { provides: options.provides } : {}),
    }
    const result = await this.call("initialize", params)
    this.#initialized = result
    return result
  }

  /** Subscribes to a session, a tree or the index (spec 10.2). */
  subscribe(
    target: Target,
    options: Pick<SubscribeParams, "replayFrom" | "items"> = {},
  ): Promise<Subscription> {
    return this.call("subscribe", { target, ...options })
  }

  unsubscribe(subscription: string): Promise<unknown> {
    return this.call("unsubscribe", { subscription })
  }

  /** Sends a prompt to a session (spec 13.1). */
  prompt(
    session: string,
    text: string,
    options: Omit<PromptParams, "session" | "text"> = {},
  ): Promise<PromptResult> {
    return this.call("session/prompt", { session, text, ...options })
  }

  close(): void {
    this.connection.close()
  }
}

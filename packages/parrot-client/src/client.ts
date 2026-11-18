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

  notify(method: string, params?: unknown): void {
    this.connection.notify(method, params)
  }

  /** Listens for a notification such as `update`; the returned function stops it. */
  on(method: string, handler: (params: unknown) => void): () => void {
    return this.connection.on(method, handler)
  }

  /** Answers a request the runtime sends, for a function this client provides (spec 25.2). */
  provide(method: string, handler: (params: unknown) => unknown | Promise<unknown>): () => void {
    return this.connection.expose(method, handler)
  }

  /** Sends `initialize` first, as the protocol requires (spec 4). */
  async initialize(options: ClientOptions = {}): Promise<InitializeResult> {
    const params: InitializeParams = {
      protocol: options.protocol ?? [1],
      peer: { kind: "client", name: "@aiviarium/parrot-client", version: VERSION, ...options.peer },
      features: options.features ?? {},
      ...(options.provides ? { provides: options.provides } : {}),
    }
    const result = await this.request<InitializeResult>("initialize", params)
    this.#initialized = result
    return result
  }

  /** Subscribes to a session, a tree or the index (spec 10.2). */
  subscribe(
    target: Target,
    options: Pick<SubscribeParams, "replayFrom" | "items"> = {},
  ): Promise<Subscription> {
    return this.request<Subscription>("subscribe", { target, ...options })
  }

  unsubscribe(subscription: string): Promise<unknown> {
    return this.request("unsubscribe", { subscription })
  }

  /** Sends a prompt to a session (spec 13.1). */
  prompt(
    session: string,
    text: string,
    options: Omit<PromptParams, "session" | "text"> = {},
  ): Promise<PromptResult> {
    return this.request<PromptResult>("session/prompt", { session, text, ...options })
  }

  close(): void {
    this.connection.close()
  }
}

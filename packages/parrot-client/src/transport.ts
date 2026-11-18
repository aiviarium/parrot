/** A transport carries one JSON-RPC message at a time. It frames and unframes them. */
export interface TransportHandlers {
  /** One message, parsed as JSON by the connection. */
  message: (text: string) => void
  close: (error?: Error) => void
}

export interface Transport {
  /** Sends one message. */
  send(text: string): void
  close(): void
  setHandlers(handlers: TransportHandlers): void
}

import type { Transport, TransportHandlers } from "./transport"

export interface WebSocketOptions {
  /** The subprotocols to offer; `parrot.1` by default. */
  protocols?: string[]
  /** A client token, sent as the second subprotocol (spec 3.4). */
  token?: string
}

/**
 * A transport over a WebSocket: one JSON message per text frame. Messages sent before the socket
 * opens wait for it, so a client may call a method right after creating it.
 */
export function webSocketTransport(url: string | URL, options: WebSocketOptions = {}): Transport {
  const protocols = [...(options.protocols ?? ["parrot.1"])]
  if (options.token) protocols.push(`parrot.token.${options.token}`)
  const socket = new WebSocket(String(url), protocols)
  const queue: string[] = []
  let handlers: TransportHandlers | undefined
  let closed = false

  socket.addEventListener("open", () => {
    for (const text of queue) socket.send(text)
    queue.length = 0
  })
  socket.addEventListener("message", (event) => {
    handlers?.message(String(event.data))
  })
  socket.addEventListener("close", () => {
    closed = true
    handlers?.close()
  })
  socket.addEventListener("error", () => {
    handlers?.close(new Error("the WebSocket failed"))
  })

  return {
    send(text) {
      if (closed) return
      if (socket.readyState === WebSocket.OPEN) socket.send(text)
      else queue.push(text)
    },
    close() {
      closed = true
      socket.close()
    },
    setHandlers(next) {
      handlers = next
    },
  }
}

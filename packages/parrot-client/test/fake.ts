// A tiny runtime for the tests: it answers the handshake and the calls the client has helpers for,
// and sends one `update` notification after `subscribe`.

export function answer(text: string): string[] {
  let message: Record<string, unknown>
  try {
    message = JSON.parse(text) as Record<string, unknown>
  } catch {
    return [failure(null, -32700, "parse error")]
  }
  const id = message.id ?? null
  switch (message.method) {
    case "initialize":
      return [
        success(id, {
          protocol: 1,
          peer: { kind: "runtime", name: "fake-runtime", version: "0.1.0" },
          runtime: { id: "rt_made-up", env: "made-up" },
          features: {},
          connection: { id: "conn_made-up", principal: "owner", scopes: ["read", "prompt"] },
        }),
      ]
    case "subscribe":
      return [
        success(id, { subscription: "sub_made-up", head: "1" }),
        notification("update", {
          subscription: "sub_made-up",
          seq: "1",
          runtime: "rt_made-up",
          at: "2026-01-01T00:00:00Z",
          update: { type: "state", state: "idle", background: [] },
        }),
      ]
    case "session/prompt":
      return [success(id, { message: "msg_made-up", disposition: "started" })]
    case "unsubscribe":
      return [success(id, {})]
    default:
      return [failure(id, -32601, `no method ${String(message.method)}`)]
  }
}

function success(id: unknown, result: unknown): string {
  return JSON.stringify({ jsonrpc: "2.0", id, result })
}

function failure(id: unknown, code: number, message: string): string {
  return JSON.stringify({ jsonrpc: "2.0", id, error: { code, message } })
}

function notification(method: string, params: unknown): string {
  return JSON.stringify({ jsonrpc: "2.0", method, params })
}

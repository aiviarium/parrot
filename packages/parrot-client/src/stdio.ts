import { spawn } from "node:child_process"
import { createInterface } from "node:readline"
import type { Transport, TransportHandlers } from "./transport"

export interface StdioOptions {
  cwd?: string
  env?: Record<string, string | undefined>
}

/**
 * A transport over a child process: one JSON message per line on its standard input and output.
 * The child's standard error is drained so a chatty driver never blocks on it.
 */
export function stdioTransport(command: string, args: string[] = [], options: StdioOptions = {}): Transport {
  const child = spawn(command, args, {
    cwd: options.cwd,
    env: options.env,
    stdio: ["pipe", "pipe", "pipe"],
  })
  const lines = createInterface({ input: child.stdout })
  let handlers: TransportHandlers | undefined
  let closed = false

  lines.on("line", (line) => {
    if (line !== "") handlers?.message(line)
  })
  child.stderr.on("data", () => {})
  // A child that exits first leaves the pipe broken; the close handler reports that.
  child.stdin.on("error", () => {})
  child.on("error", (error) => {
    closed = true
    handlers?.close(error)
  })
  child.on("close", (code, signal) => {
    closed = true
    const reason =
      code === 0 || code === null ? undefined : new Error(`the process exited with ${code ?? signal}`)
    handlers?.close(reason)
  })

  return {
    send(text) {
      if (closed) return
      try {
        child.stdin.write(`${text}\n`)
      } catch {
        // The close handler has already reported the broken pipe.
      }
    },
    close() {
      closed = true
      child.stdin.end()
      child.kill()
    },
    setHandlers(next) {
      handlers = next
    },
  }
}

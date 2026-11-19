import { expect, test } from "bun:test"
import { existsSync, readFileSync } from "node:fs"
import { dirname, join, resolve } from "node:path"

const src = join(import.meta.dir, "..", "src")
const root = join(src, "index.ts")

// `import` and `export … from` with their specifier, including a multi-line list of names.
const specifiers = /(?:import|export)[^"']*?from\s*["']([^"']+)["']|import\s*["']([^"']+)["']/g

/** The specifiers a file imports, resolved to paths inside `src` or kept as external names. */
function importsOf(file: string): string[] {
  const text = readFileSync(file, "utf8")
  const found: string[] = []
  for (const match of text.matchAll(specifiers)) {
    const spec = match[1] ?? match[2]
    if (!spec.startsWith(".")) {
      found.push(spec)
      continue
    }
    const base = resolve(dirname(file), spec)
    const path = [base, `${base}.ts`, join(base, "index.ts")].find((candidate) => existsSync(candidate))
    if (!path) throw new Error(`${file} imports ${spec}, which is not a file`)
    found.push(path)
  }
  return found
}

/** Every module reachable from `entry` by static imports, and every external name it reaches. */
function reachable(entry: string): { files: string[]; external: string[] } {
  const files: string[] = []
  const external: string[] = []
  const queue = [entry]
  while (queue.length > 0) {
    const file = queue.pop() as string
    if (files.includes(file)) continue
    files.push(file)
    for (const imported of importsOf(file)) {
      if (imported.startsWith("/")) queue.push(imported)
      else external.push(imported)
    }
  }
  return { files, external }
}

test("the root entry reaches no Node-only module", () => {
  const { files, external } = reachable(root)
  const node = external.filter((name) => name.startsWith("node:") || name.startsWith("bun:"))
  expect(node).toEqual([])
  // The root really is the client, not an empty file.
  expect(files.some((file) => file.endsWith("client.ts"))).toBe(true)
})

test("the stdio entry is where the child process lives", () => {
  const { external } = reachable(join(src, "stdio.ts"))
  expect(external).toContain("node:child_process")
})

test("the root entry builds for the browser", async () => {
  const result = await Bun.build({ entrypoints: [root], target: "browser" })
  expect(result.logs.filter((log) => log.level === "error")).toEqual([])
  expect(result.success).toBe(true)
})

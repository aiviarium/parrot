#!/usr/bin/env bun
// Turns the committed JSON Schema into TypeScript types. Run with `--check` in CI to fail when the
// schema moved and the types in src/schema.ts were not generated again.

import { spawnSync } from "node:child_process"
import { readFileSync, writeFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

type Schema = Record<string, unknown> | boolean

const here = dirname(fileURLToPath(import.meta.url))
const packageDir = join(here, "..")
const schemaPath = join(here, "..", "..", "..", "spec", "schema", "parrot.schema.json")
const outputPath = join(packageDir, "src", "schema.ts")

const schema = JSON.parse(readFileSync(schemaPath, "utf8")) as Record<string, unknown>
const defs = (schema.$defs ?? {}) as Record<string, Schema>

function literal(value: unknown): string {
  return value === null ? "null" : JSON.stringify(value)
}

function docFor(description: unknown, indent: string): string {
  if (typeof description !== "string" || description === "") return ""
  const text = description.replace(/\*\//g, "*\\/")
  const lines = text.split("\n")
  if (lines.length === 1) return `${indent}/** ${lines[0]} */\n`
  return `${indent}/**\n${lines.map((line) => `${indent} * ${line}`).join("\n")}\n${indent} */\n`
}

function refName(ref: unknown): string {
  if (typeof ref !== "string") throw new Error(`a $ref must be a string, got ${String(ref)}`)
  const name = ref.split("/").pop()
  if (!name) throw new Error(`cannot read a name from ${ref}`)
  return name
}

function union(parts: string[], indent: string): string {
  const unique = [...new Set(parts)]
  if (unique.length === 0) return "unknown"
  if (unique.every((part) => !part.includes("\n"))) return unique.join(" | ")
  const inner = `${indent}  `
  return `\n${unique.map((part) => `${inner}| ${part.replace(/\n/g, `\n${inner}`)}`).join("\n")}`
}

function typeOf(spec: Schema | undefined, indent: string): string {
  if (spec === undefined || spec === true) return "unknown"
  if (spec === false) return "never"
  if (typeof spec.$ref === "string") {
    // A tagged union's variant names a type and narrows it, as in `{ $ref: "#/$defs/UserItem",
    // properties: { kind: { const: "user" } } }`; both parts make the discriminator.
    const name = refName(spec.$ref)
    const extra = { ...spec }
    delete extra.$ref
    delete extra.description
    if (Object.keys(extra).length === 0) return name
    const body = typeOf(extra as Schema, indent)
    return `${name} & ${body.includes("\n") ? `(${body})` : body}`
  }
  if (Array.isArray(spec.enum)) return spec.enum.map(literal).join(" | ")
  if ("const" in spec) return literal(spec.const)
  if (Array.isArray(spec.anyOf))
    return union(
      spec.anyOf.map((s) => typeOf(s as Schema, indent)),
      indent,
    )
  if (Array.isArray(spec.oneOf))
    return union(
      spec.oneOf.map((s) => typeOf(s as Schema, indent)),
      indent,
    )

  const type = spec.type
  if (Array.isArray(type)) {
    return union(
      type.map((one) => typeOf({ ...spec, type: one } as Schema, indent)),
      indent,
    )
  }
  switch (type) {
    case "string":
      return "string"
    case "integer":
    case "number":
      return "number"
    case "boolean":
      return "boolean"
    case "null":
      return "null"
    case "array": {
      const item = typeOf(spec.items as Schema, indent)
      return item.includes("|") || item.includes("\n") ? `Array<${item}>` : `${item}[]`
    }
    case "object":
    case undefined: {
      const properties = spec.properties as Record<string, Schema> | undefined
      if (properties && Object.keys(properties).length > 0) {
        const required = new Set((spec.required as string[] | undefined) ?? [])
        const lines = Object.keys(properties).map((key) => {
          const value = properties[key]
          const doc = docFor((value as Record<string, unknown> | undefined)?.description, `${indent}  `)
          const optional = required.has(key) ? "" : "?"
          const name = /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(key) ? key : JSON.stringify(key)
          return `${doc}${indent}  ${name}${optional}: ${typeOf(value, `${indent}  `)}`
        })
        return `{\n${lines.join("\n")}\n${indent}}`
      }
      const extra = spec.additionalProperties
      if (extra !== undefined && extra !== false) {
        return `Record<string, ${typeOf(extra as Schema, indent)}>`
      }
      return type === "object" ? "Record<string, unknown>" : "unknown"
    }
    default:
      return "unknown"
  }
}

function entries(key: string): [string, Record<string, Schema>][] {
  const table = (schema[key] ?? {}) as Record<string, Record<string, Schema>>
  return Object.keys(table)
    .sort()
    .map((name) => [name, table[name]] as [string, Record<string, Schema>])
}

/** A map of methods to their params and results, read from `key`. */
function methodTable(key: string, name: string, doc: string): string[] {
  const lines = entries(key).map(
    ([method, m]) =>
      `  ${JSON.stringify(method)}: { params: ${typeOf(m.params, "  ")}; result: ${typeOf(m.result, "  ")} }`,
  )
  if (lines.length === 0) return []
  return [`/** ${doc} */`, `export interface ${name} {`, ...lines, "}", ""]
}

/** The `Methods` map, from `x-methods`. */
function methods(): string[] {
  return methodTable("x-methods", "Methods", "The methods a client calls, from spec 39 and Appendix E8.")
}

/** The `ClientMethods` map, from `x-client-methods`. */
function clientMethods(): string[] {
  return methodTable(
    "x-client-methods",
    "ClientMethods",
    "The requests a runtime sends to a client: calls of the functions it provides (spec 25.2).",
  )
}

/** The `Notifications` map: each notification's params, read from `x-notifications`. */
function notifications(): string[] {
  const lines = entries("x-notifications").map(
    ([name, params]) => `  ${JSON.stringify(name)}: ${typeOf(params, "  ")}`,
  )
  if (lines.length === 0) return []
  return [
    "/** The notifications a peer sends (spec 36). */",
    "export interface Notifications {",
    ...lines,
    "}",
    "",
  ]
}

function emit(): string {
  const out: string[] = [
    "// Generated from spec/schema/parrot.schema.json by scripts/generate.ts. Do not edit.",
    "",
  ]
  for (const name of Object.keys(defs).sort()) {
    const def = defs[name]
    const doc = typeof def === "object" ? docFor(def.description, "").trimEnd() : ""
    if (doc !== "") out.push(doc)
    if (def === true || def === false) {
      out.push(`export type ${name} = ${typeOf(def, "")}`)
    } else if (def.type === "object" && !def.$ref && !def.anyOf && !def.oneOf) {
      const body = typeOf(def, "")
      out.push(body.startsWith("{") ? `export interface ${name} ${body}` : `export type ${name} = ${body}`)
    } else {
      out.push(`export type ${name} = ${typeOf(def, "")}`)
    }
    out.push("")
  }
  out.push(...methods())
  out.push(...clientMethods())
  out.push(...notifications())
  return `${out.join("\n").trimEnd()}\n`
}

// The repository formats TypeScript with biome; the generated file has to match it byte for byte.
function format(text: string): string {
  const bin = join(packageDir, "..", "..", "node_modules", ".bin", "biome")
  const run = spawnSync(bin, ["format", "--stdin-file-path", outputPath], { input: text, encoding: "utf8" })
  if (run.status !== 0) throw new Error(`biome format failed: ${run.stderr}`)
  return run.stdout
}

const output = format(emit())
if (process.argv.includes("--check")) {
  const current = readFileSync(outputPath, "utf8")
  if (current !== output) {
    console.error("src/schema.ts is stale: run `bun run generate` in packages/parrot-client")
    process.exit(1)
  }
} else {
  writeFileSync(outputPath, output)
}

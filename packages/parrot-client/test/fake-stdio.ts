// The stdio half of the fake runtime: one JSON message per line in and out.
import { createInterface } from "node:readline"
import { answer } from "./fake"

const lines = createInterface({ input: process.stdin })
lines.on("line", (line) => {
  if (line === "") return
  for (const reply of answer(line)) process.stdout.write(`${reply}\n`)
})

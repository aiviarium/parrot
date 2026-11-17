#!/usr/bin/env node
// A fake Claude Code for tests, in two shapes like the real CLI: with `-p` it speaks stream-json
// (./stream-json.mjs); without, it draws a terminal UI close to the real one (./tui.mjs) that the
// `claude-code` TUI profile reads, and writes the JSONL transcript a terminal driver can follow. Both
// keep what was said in one place per session id, so a session switched from one mode to the other
// resumes with its history. The directives they understand are listed in each.
const args = process.argv.slice(2)
if (args.includes("--version")) process.stdout.write("2.1.325 (Claude Code, fake)\n")
else if (args.includes("-p")) await import("./stream-json.mjs")
else await import("./tui.mjs")

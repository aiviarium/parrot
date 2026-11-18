# Parrot conformance

The `parrot-conform` crate holds golden fixtures of the Parrot protocol. It parses fixture files, matches messages against them and runs them against a target. `parrot conform` is its command line.

## Running

| Command | What it tests |
| --- | --- |
| `parrot conform <driver command>` | The driver fixtures in `fixtures/driver/` against a driver on stdio |
| `parrot conform --runtime` | The client fixtures in `fixtures/runtime/` against a fresh `parrot serve` per fixture |
| `parrot conform --runtime --socket <path>` | The client fixtures against a runtime that is already running |

Each fixture prints a `pass`, `FAIL` or `skip` line, then a summary. The command exits with 1 when a fixture fails.

| Option | Meaning |
| --- | --- |
| `--scenarios <dir>` and `--scenario-var <VAR>` | Point the fake CLI at `<dir>/<scenario>.jsonl` through `VAR` |
| `--agent <id>` | The agent the fixtures start, `claude` by default |
| `--fixtures <dir>` | Run the `.jsonl` files in a folder instead of the built-in fixtures |
| `--only <name>` | Run only the fixture `<name>`; repeat it for more |
| `--pending` | Also run fixtures marked pending |
| `--wait <seconds>` | How long to wait for each message, 10 by default |
| `--verbose` | Print every message a failing fixture received |

The Claude driver against its fake CLI:

```bash
PARROT_CLAUDE_BIN=target/debug/fake-claude parrot conform \
  --scenarios crates/parrot-driver-claude/fixtures/scenarios \
  --scenario-var PARROT_FAKE_CLAUDE_SCENARIO \
  parrot driver claude
```

## Fixtures

A fixture is JSONL. Lines starting with `//` are comments.

| Line | Meaning |
| --- | --- |
| `{ "fixture": { "scenario", "pending", "about" } }` | The scenario it needs, and why it is skipped for now |
| `{ "dir": "in", "msg": … }` | Send the message |
| `{ "dir": "out", "msg": … }` | Wait for a matching message |
| `{ "check": { "projection": …, "expect": … } }` | Wait until the projection matches |

An `out` message with an `id` matches the response to that id. Any other `out` message matches the first message received that fits it, so targets may interleave and chunk updates differently. Objects may carry fields the pattern leaves out.

| Placeholder | Stands for |
| --- | --- |
| `"$id:<name>"` | An id, bound the first time and held to it after |
| `"$seq"` | A sequence number that increases |
| `"$time"` | A timestamp |
| `"$any"` | Any value |
| `"$dir"` | A scratch folder for the workspace |
| `"$agent"` | The agent from `--agent` |

A check gives one projection, optionally narrowed with `session` or `binding`:

| Projection | Value |
| --- | --- |
| `items`, `requests`, `turns` | The list in first-seen order; `true`, or an object whose fields an entry must equal |
| `state`, `options`, `level`, `queue`, `usage` | The latest value |

Items are built with `parrot_proto::patch::apply`, and snapshots in responses seed the projection.

## Scenarios

A driver fixture gives a scenario in its header, and each agent CLI supplies its own fake for it. A fake for another CLI has to behave like this:

| Scenario | The fake CLI |
| --- | --- |
| `text-turn` | Answers the first prompt with the text `Hello there.` and ends the turn |
| `bash-allowed` | Asks to run `ls`; with the choice `allow` it prints `README.md` and `src`, then says `There are two entries.` |
| `bash-denied` | Asks to run `ls`; with the choice `deny` the tool fails and it says `I did not run it.` |
| `interrupt` | Streams text until it is interrupted |

The runner also sets `PARROT_CONFORM_SCENARIO` to the scenario's name.

# parrot-driver-tui

TUI control for the Parrot runtime. The driver runs an agent CLI on a PTY and reads each settled screen
through a TUI profile. It answers the runtime's driver calls with keys.

| Call | What the driver does |
| --- | --- |
| `binding/start` | Starts the command, through `parrot guard --` when the runtime is `parrot`. Returns once the screen is ready for a prompt or shows a dialog, known or not |
| `binding/prompt` | Runs the profile's `submit` action, or `submit-paste` for several lines |
| `binding/cancel` | Runs the profile's `interrupt` action, or presses Esc |
| `binding/answer` | Runs the profile's `choose` action for a choice. Types a `value`, or sends `{ text, press }` |
| `binding/screen` | The screen text, cursor, size, profile state, labelled tree and generic dialogs |
| `binding/keys` | Types `text`, then presses each key in `press` |

## States and requests

`Config::states` maps profile states to Parrot states. `Config::dialogs` names the profile states that are
dialogs, with the request kind each one opens.

| Screen | Update |
| --- | --- |
| A profile state | Its Parrot state, such as `idle`, `running` or `requires_action` |
| A dialog state | `requires_action` and a request whose choices are the options of the dialog |
| No state for `unrecognised_after` | `unrecognised` and an `input` request with the screen text, the cursor and the options on screen |
| The screen leaves a dialog unanswered | The request is cancelled, with `answeredBy: "harness"` |

A turn starts with a prompt and ends when the screen is idle again.

## Transcript

With `Config::transcript` set, the driver follows Claude Code's JSONL transcript and reports its
messages as items. It also reports the session id as the binding's `ref`. Prompts the driver typed are
not reported twice.

## Tests

`tests/fake/fake-tui.sh` is a scripted TUI with a prompt, a busy line, a tool approval, a trust dialog
at start, and a question the profile leaves out. `tests/fake/fake-tui.kdl` is its profile.

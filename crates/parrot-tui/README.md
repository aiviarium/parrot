# parrot-tui

A labelled tree over a terminal screen, and automation on top of it. parrot-tui segments each settled
parrot-term snapshot into nodes with roles, bounds and text, in effect an accessibility tree. A profile
for one program tags those nodes, names its states and defines its actions. Code can then wait for a
state and run an action, and never types into a screen it did not expect.

```
screen ─> segmentation ─> raw tree ─> profile rules ─> labelled tree ─> diff ─> events
                                                             │
                                                 queries, waits, actions
```

| Part | What it does |
| --- | --- |
| `tree` | Segments a screen into nodes. It reads borders, rules, gaps and style runs, and also fills, highlights, the cursor and aligned columns. A node has the same id in every frame, and `diff` lists what changed |
| `tree::surfaces` | Finds every dialog with choices or a text field, with no profile. `Automation::choose` answers one |
| `Profile` | Reads a KDL profile and checks every rule, including each action with its variables filled in |
| `Locator`, `ScreenExt` | Find text, regexes and boxes on a screen |
| `SessionExt::automate` | Tracks a session's tree and state with a profile. `wait_for`, `wait_for_state` and `act` drive it |
| `measure` | Scores a profile's state detection against labelled casts |

## Example

```rust
use std::collections::HashMap;
use std::time::Duration;
use parrot_tui::{Profile, SessionExt};

let profile = Profile::load("packages/parrot-agents/tui/claude-code.kdl")?;
let tui = session.automate(Some(profile));
tui.wait_for_state("idle", Duration::from_secs(30)).await?;
let vars = HashMap::from([("text".to_string(), "run the tests".to_string())]);
tui.act("submit", &vars).await?;
```

`examples/live_drive.rs` drives a real Claude Code or Codex through its profile's actions alone.

## Nodes

A node has an `id`, a `role`, `bounds`, `text` and `children`. It may also have a `label`, a `value`
(an input's text without its prompt glyph or placeholder), a `key` such as `1` in `1. Yes`, and a
`shortcut` such as `y` in `Yes, proceed (y)`. `tags` come from profile rules, and `source` names
the segmentation rule followed by each profile rule that tagged the node.

These are the roles:

```
screen  pane  dialog  input  menu  menuitem  list  listitem  table  row
text  heading  status  progress  hint  scroll  separator
```

## Profiles

A profile is a KDL file with these top-level nodes. The profiles for Claude Code and Codex are in
`packages/parrot-agents/tui/`, and each egg has a copy as `eggs/<agent>/parrot/profile.kdl`.

| Node | What it holds |
| --- | --- |
| `profile` | `id`, `name`, `description`, `match command= version=`, `version-from-screen` and `unrecognised-after` (default 10 s) |
| `node` | A rule that tags, relabels or regroups nodes |
| `state` | A named condition over the tree |
| `action` | A named input sequence with a condition before it and one after it |
| `variant` | Rules for a CLI version range, such as `version=">=2.1, <2.2"` |
| `hook` | A function the host provides as a `Tagger`, for what rules cannot express |

Scalar keys are properties, and tables are child nodes. A list is a node with several arguments,
or a node repeated once per element. Write `(array)` before a list that has one element,
such as `(array)role "dialog"`. The keys below keep their names.
`spec/schema/driver-data/profile.schema.json` is the schema.

```kdl
node tag="prompt" last=#true {
  where role="input" source="auto:rule-input"
}
state name="idle" {
  when exists="prompt"
}
```

### Node rules

Node rules apply in file order, each to the tree the previous ones left.

| Key | Meaning |
| --- | --- |
| `tag` | The tag to add |
| `name` | The rule's name in `source`. The default is the tag |
| `role` | Sets the node's role |
| `where` | A matcher the node must satisfy |
| `inside` | Only descendants of a node with this tag |
| `first`, `last` | Only the first or the last match |
| `label` | A regex whose first capture group becomes the label |
| `lines` | Groups rows into a new node: `{ from, to }`, `{ from, through }`, with `last` to start at the last match of `from` |
| `item` | Reads a node's rows again as items: `{ line-matches, selected-when, label, key, ignore }` |

### Matchers

Matchers appear as `where`, `find` and `has`. Every key given must hold.

| Keys | Meaning |
| --- | --- |
| `role`, `tag`, `source` | The node's role (one or a list), a tag, or a prefix of its source |
| `box`, `contains-cursor`, `focused`, `selected`, `busy` | Flags of the node |
| `position` | `top` or `bottom` third of the screen |
| `text-matches`, `text-contains`, `starts-with`, `first-line-matches` | The node's text. `text-contains` ignores whitespace and wrapping |
| `label-matches`, `value-matches` | The label or the input's value |
| `bold`, `dim`, `italic`, `reverse`, `fg`, `bg` | The dominant style. Colours are `#rrggbb` or a palette index |
| `min-rows`, `max-rows` | The node's height |
| `below`, `above` | Relative to the node with a tag |
| `has`, `not`, `any` | A descendant matches, the matcher fails, or one of several holds |

### Conditions

Conditions appear as `when`, `require`, `expect` and `wait`.

| Keys | Meaning |
| --- | --- |
| `state` | The current state, one or a list |
| `exists`, `not-exists`, `find` | Tags, or a matcher |
| `any-text-matches`, `no-text-matches`, `title-matches` | The whole screen, or the window title |
| `alt-screen`, `cursor-visible`, `focused` | Terminal modes, and the focused node |
| `text-in` with `contains` or `matches`, `empty` | The text of a tagged node, or an input with nothing typed |
| `selected-in` with `index` or `label` | The selection in a menu |
| `until-seen`, `seen` | Whether a state has been current in this session |
| `changed` | In `expect`: the tagged node is gone or its text differs from when the action started |
| `not`, `any`, `all` | Combinations |
| `within` | A timeout for `expect` and `wait` |
| `hold` | The condition must hold without a break for this long before it counts |

Inside a `state` node, `state="X"` and `(array)not "X"` test whether X's rules hold. A state may have several
rules, and any of them holding makes it hold. The current state is the first one in file order that
holds, so a fallback such as `state name="starting" { when until-seen="idle"; }` goes last.

### Actions

Steps are `steps` nodes, one per step, in order. A step is `key` (a name or a list), `type`, `paste`,
`select in= index= | label= | option= confirm= timeout=` or `wait { ... }` with an optional
`timeout`.

- `require` is checked first, and `expect` afterwards. A failed `require`, step or `expect` is an
  error that contains the labelled tree and the state.
- `select` presses one arrow key at a time and waits for the selection to move before the next. It
  ends once the selection has stayed on its target for `hold`, 200 ms by default.
- `$name` and `${name}` are filled in before a step is read, and `$$` is a dollar sign. A missing
  variable fails the action before it starts.

Snapshots for automation settle after 50 ms of quiet, or at most every 300 ms
(`SessionConfig::for_automation`). Claude Code animates while it waits. Its state then lags by up to the
maximum delay.

## Measuring a profile

Each cast in `fixtures/casts/` has a `<cast>.truth.toml` beside it. Its spans say which state a person
sees on the screen over time: `none` after exit, and `*` for redraw blips that are not scored. It also
gives the state at each marker. `parrot-term-tool measure` replays the casts through their profile and
reports these figures:

- precision and recall per state, by time
- wrong intervals, and intervals that are only detection lag
- detection latency and marker checks
- how often `unrecognised` would have fired

`fixtures/casts/holdout/` holds casts labelled after the profiles were frozen, as a check that a
profile was not tuned to the casts it is scored on. `fixtures/casts/README.md` describes every cast.

```bash
cargo run -p parrot-tui --bin parrot-term-tool -- measure crates/parrot-tui/fixtures/casts
```

## parrot-term-tool

| Command | What it does |
| --- | --- |
| `record [-o FILE] [--size COLSxROWS] -- CMD` | Runs a command on a PTY in this terminal and records it |
| `replay CAST [--at SECONDS]` | Prints every settled snapshot, or the screen at a time |
| `snapshot CAST` | Prints the screen at the end of the cast |
| `markers CAST` | Lists the markers |
| `tree CAST [--at SECONDS] [--profile FILE] [--json]` | Prints the tree with bounds, roles, tags and the rule behind each node |
| `states CAST --profile FILE` | Prints the state timeline |
| `grep CAST REGEX` | Prints when a regex over the screen starts and stops matching |
| `measure TRUTH...` | Scores state detection against truth files or folders of them |
| `inspect [--profile FILE] -- CMD` | Runs a command with its screen on the left and its tree and state on the right. Ctrl+] quits |

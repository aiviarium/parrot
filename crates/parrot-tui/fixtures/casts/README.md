# Recordings

asciinema v2 casts recorded with `crates/parrot-term/examples/record_casts.rs`, which drives each program through
a PTY with `parrot-term`'s own input encoding and waits on screen text. Every interesting moment
has a marker event (`"m"`), so tests and tools can find a screen by name rather than by time:

```sh
cargo run -p parrot-tui --bin parrot-term-tool -- markers crates/parrot-tui/fixtures/casts/claude-permission.cast
cargo run -p parrot-tui --bin parrot-term-tool -- replay crates/parrot-tui/fixtures/casts/claude-permission.cast --at 10.363
cargo run -p parrot-term --example record_casts -- --list      # scenarios
cargo run -p parrot-term --example record_casts -- --out crates/parrot-tui/fixtures/casts claude-model # re-record one
```

Times below are seconds from the start of the cast; the screen at a marker is the one right
after it (`Replay::screen_at(t)`). `crates/parrot-term/tests/term/casts.rs` and `crates/parrot-tui/tests/tui/casts.rs` check each of them.

Recorded on 2026-11-14 with a clean environment (`TERM=xterm-256color`, `COLORTERM=truecolor`,
`LANG=en_US.UTF-8`, `HOME`, `PATH`) in throwaway directories under a scratch directory, never in
the repository. `top.cast` was recorded on 2026-11-17 in a PID namespace of its own
(`unshare --user --map-root-user --pid --fork --mount-proc`), so its process list holds only the
recorder and `top`.

| Program | Version |
| --- | --- |
| Claude Code | 2.1.325 |
| Codex CLI | 0.167.1 (its app-server daemon updated itself to 0.169.0 during the session) |
| vim | 9.1 |
| less | 668 |
| top | procps-ng 4.0.7 |
| nano | 8.7.1 |
| bash | 5.3.9 |

## Ordinary programs

| Cast | Size | Contents | Markers |
| --- | --- | --- | --- |
| `shell-unicode.cast` | 60x20 | `bash --norc`: accented text, combining marks (`é`, `ộ`, `x⃗`), CJK and fullwidth, emoji with skin tone, VS16, ZWJ family and a flag, 59 `x` then `界` (wraps with a spacer in the last column), SGR styles | 0.002 prompt · 0.613 unicode-shown · 0.916 styles-shown |
| `burst-5000.cast` | 80x24 | `seq` prints 5000 lines in one go, mostly in a single read | 0.514 burst-done |
| `vim.cast` | 80x24 | `vim -u NONE`: empty buffer, insert mode with wide characters and combining marks, `:set number`, `:split`, the command line, `:wqa` | 0.406 vim-empty · 1.435 vim-text · 1.939 vim-split · 2.242 vim-command-line |
| `less.cast` | 80x24 | `less` on 300 lines: first page, page down, search for `row 150`, help screen, quit | 0.306 less-first-page · 0.609 less-page-2 · 1.216 less-search · 1.520 less-help |
| `top.cast` | 100x24 | `top -d 1` for 3 s, quit | 0.659 top-main |
| `nano.cast` | 80x24 | `nano --ignorercfile`: typing, `^O` filename prompt, save, `^X` | 0.450 nano-empty · 0.854 nano-text · 1.157 nano-save-prompt |

## Claude Code

All five run `claude --model haiku --permission-mode manual --setting-sources project,local` at
100x30: a cheap model, a permission prompt for every tool call, and no user-level settings or
`CLAUDE.md`. Only `claude-prompt` and `claude-permission` send prompts, and both are trivial.
Each ends by typing `/exit`.

| Cast | Contents | Markers |
| --- | --- | --- |
| `claude-startup.cast` | First start in a new folder: the trust dialog (inline, primary screen), selection moved to "Yes", the fullscreen idle prompt, the `?` shortcuts panel, resize to 80x24 and to 120x36 | 1.771 trust-dialog · 2.480 trust-yes-selected · 8.989 idle · 10.490 shortcuts · 11.491 shortcuts-closed · 12.992 resized-80x24 · 14.494 resized-120x36 · 15.341 exit-typed |
| `claude-prompt.cast` | A prompt typed into the input box, submitted, the working spinner, a streamed 10-line answer and the "done" line; then a second prompt interrupted with Esc while it streams | 6.502 idle · 7.229 prompt-typed · 7.330 working · 10.826 response · 12.896 working-2 · 14.979 interrupted · 15.813 exit-typed |
| `claude-permission.cast` | `echo hello > hello.txt` through Bash: working, the permission dialog, selection moved to option 2 and back, approved, result; then `rm hello.txt`, its dialog rejected with Esc | 6.502 idle · 7.131 working · 10.363 permission-prompt · 11.384 permission-option-2 · 11.931 approved · 16.185 done · 19.856 permission-prompt-2 · 22.402 rejected · 23.238 exit-typed |
| `claude-slash.cast` | `/` opens the command menu, Down twice, typing `mo` filters it, Esc closes it, Backspace clears the input | 6.504 idle · 7.751 slash-menu · 9.054 slash-menu-moved · 10.374 slash-menu-filtered · 11.575 slash-menu-closed · 12.580 input-cleared · 13.451 exit-typed |
| `claude-model.cast` | `/model` opens the model picker, Down, Esc closes it without a change | 6.501 idle · 9.085 model-picker · 10.087 model-picker-moved · 11.631 model-picker-closed · 12.449 exit-typed |

### What the recordings show about Claude Code's TUI

- **Two phases.** The trust dialog is drawn inline on the primary screen. After it, Claude Code
  switches to the alternate screen (`CSI ? 1049 h`, then `CSI 2 J`) and stays there; on exit it
  leaves it and the primary screen is blank.
- **No synchronized updates.** It never sends mode 2026. Frames are redrawn with `CSI H` and
  relative cursor moves, and spaces between words are often cursor jumps (`CSI n G`) rather
  than space characters. Snapshots therefore settle on quiet or on the max delay.
- **It never goes quiet while waiting.** In a permission dialog the tool bullet `●` blinks about
  every 0.6 s; while working the spinner animates. Wait on text, not on stability.
- **Modes.** Bracketed paste, focus events (1004), mouse 1000 + 1002 + 1003 with SGR (1006),
  `CSI ? 2031 h` (colour scheme notifications), kitty keyboard flags pushed as `CSI > 5 u`
  (disambiguate + alternate keys) and modifyOtherKeys `CSI > 4 ; 2 m`. All of them are pushed
  again after each submitted prompt and popped on exit.
- **Startup probe.** It sends `CSI > 0 q`, `CSI ? u`, `OSC 7501 ; ?` and `CSI c`, then redraws.
  Keys sent before that redraw are lost (a Down in the trust dialog was reset), so wait until
  the first screen has been up for a moment.
- **Input box.** A full-width `─` rule above and below one input row that starts with `❯`. With
  text in it the marker is `❯` followed by a no-break space (U+00A0); `parrot-term`'s matching
  treats that as a space. Empty, it shows a placeholder such as `Try "how do I log an error?"`.
  Above the top rule, right-aligned, the effort (`◐ medium · /effort`). Below the bottom rule,
  the mode line: `⏸ manual mode on · ? for shortcuts · ← for agents`.
- **Working.** A spinner line `✢ Thinking…`, `· Perambulating…`, `* Julienning…` (the glyph
  cycles through `✢ · ✻ * ✶ ✳`, the gerund is random) with `(1s · ↓ 20 tokens)`, and
  `esc to interrupt` in the mode line. When done: `✻ Sautéed for 1s · done 7:08 PM`.
- **Window title.** `✳ Claude Code` when idle, `◐`/`◑` alternating while working, and after the
  first prompt a generated session title (`◐ Counting from one to ten`). This is a cheap state
  signal.
- **Messages.** The user prompt is repeated as `❯ text`; replies start with `●`; tool calls show
  `● Removing hello.txt` then `⎿  $ rm hello.txt`; an interruption shows
  `⎿  Interrupted · What should Claude do instead?`.
- **Permission dialog.** Replaces the input box. A `─` rule, a heading (`Bash command`), a tip,
  a description, the command between two `╌` rules, `Do you want to proceed?`, numbered options
  with `❯` on the selected one (`1. Yes`, `2. Yes, and always allow access to <dir> from this
  project` which wraps, `3. Yes, and switch to auto mode`, `4. No`), and
  `Esc to cancel · Tab to amend`. No box-drawing corners: rules only.
- **Model picker.** Its top edge is a row of `▔` with the effort indicator in it, then
  `Select model`, a description, numbered models with `❯` on the selection and `✔` on the
  current one, `▸ More models`, an effort line, and `Enter to set as default · s to use this
  session only · Esc to cancel`. Enter changes the default for new sessions.
- **Slash menu.** Opens above the input box: up to five rows of `/command` and a description
  in aligned columns, `❯` on the selection; typing filters it.

## Codex

| Cast | Size | Contents | Markers |
| --- | --- | --- | --- |
| `codex.cast` | 100x30 | Codex CLI 0.167.1: the update dialog it sometimes shows first (skipped with Esc), the animated logo, the idle prompt, `?` shortcuts, `/` command menu, `/model` picker, Ctrl+C | 0.321 dialog: Update available · 1.147 idle · 5.149 idle-settled · 6.665 shortcuts · 9.178 slash-menu · 9.980 slash-menu-moved · 11.782 slash-menu-closed · 15.108 model-picker · 16.610 model-picker-closed · 18.111 exit |

The brief said Codex was not signed in, and `codex login status` said so when this work began,
but by the time of recording it reported "Logged in using ChatGPT". This cast therefore sends no
prompt: it shows only what is visible without one. During exploration one prompt ("say hi") was
sent by mistake and answered.

What it shows: Codex uses the alternate screen, wraps every frame in a synchronized update
(mode 2026), sets kitty flags 7, turns alternate scroll off (`CSI ? 1007 l`), queries the
cursor position and the foreground and background colours (OSC 10, 11), and animates a braille
spinner in the window title (`⠋ codex`). Its input box starts with `›`; selections in lists are
marked with `›`. Ctrl+C prints "Disconnected from this task" with a `codex resume` command,
since the work runs in a separate app-server daemon.

## Recordings for the labelled tree

Recorded on 2026-11-14 for the TUI automation layer, with the same clean environment.
Claude Code runs as above (`claude-plan` with `--permission-mode plan`), all of them in one
folder; Codex runs `codex --no-daemon -m gpt-6-luna -a on-request -s read-only`, each in a new
folder, so no app-server daemon is left behind and every command outside the read-only sandbox
asks for approval. Each cast has a `.truth.toml` beside it with the state a person sees on the
screen over time and at every marker (see "Measuring a profile" in the crate's README).

| Cast | Size | Contents | Markers |
| --- | --- | --- | --- |
| `claude-question.cast` | 100x30 | AskUserQuestion with three colours, moved to Green and answered | 5.102 idle · 5.836 working · 9.195 question · 10.202 question-option-2 · 10.297 answered · 13.524 done · 14.355 exit-typed |
| `claude-plan.cast` | 100x30 | Plan mode: a two-step plan, the plan approval dialog, rejected with Esc | 6.503 idle · 7.129 working · 12.305 plan-approval · 14.346 plan-rejected · 15.172 exit-typed |
| `claude-long.cast` | 100x30 | An 80-line answer that scrolls | 6.503 idle · 7.128 working · 10.985 done · 11.823 exit-typed |
| `claude-edit.cast` | 100x30 | An Edit tool approval for `notes.txt`, approved | 6.502 idle · 7.124 working · 10.675 edit-approval · 10.705 approved · 14.965 done · 15.797 exit-typed |
| `claude-resize.cast` | 100x30 | Resized to 80x24 and 120x36 while it works | 6.503 idle · 8.643 working · 10.144 resized-80x24 · 11.648 resized-120x36 · 13.651 done · 14.477 exit-typed |
| `claude-narrow.cast` | 60x24 | A Bash permission dialog at 60 columns, approved | 6.503 idle · 7.181 working · 10.409 permission-prompt · 10.478 approved · 14.003 done · 14.842 exit-typed |
| `codex-trust.cast` | 100x30 | A new git repository: update notice skipped, folder trust accepted, a one-word answer | 2.897 trust-dialog · 4.921 idle · 5.493 working · 9.400 done · 10.405 exit |
| `codex-prompt.cast` | 100x30 | A prompt, the working line, a streamed answer; a second prompt interrupted with Esc | 8.970 idle · 9.713 prompt-typed · 9.813 working · 13.135 response · 16.743 working-2 · 18.746 interrupted · 19.752 exit |
| `codex-approval.cast` | 100x30 | A command approval moved, approved, done; a second one denied with Esc | 8.978 idle · 9.549 working · 17.045 approval · 17.853 approval-option-2 · 18.373 approved · 23.940 done · 28.693 approval-2 · 31.208 denied · 32.213 exit |
| `codex-menus.cast` | 100x30 | Shortcuts, the slash menu moved and filtered, the model picker moved and closed | 8.978 idle · 10.484 shortcuts · 12.992 slash-menu · 13.793 slash-menu-moved · 14.821 slash-menu-filtered · 16.624 slash-menu-closed · 18.952 model-picker · 19.954 model-picker-moved · 21.464 model-picker-closed · 22.474 exit |

`holdout/` has three more, labelled only after the profiles were frozen: `claude-holdout-1`
(90x26: a permission dialog rejected, a long answer interrupted; the recorder's final `/exit`
failed because Claude Code had put the interrupted prompt back into the input, so it ends with
a `failed` marker and a kill), `claude-holdout-2` (the model picker, then a question answered
with Enter) and `codex-holdout` (90x28: a short answer, an approval, the model picker).

### What the new recordings show

- **Claude Code question dialog.** A rule, a header (`☐ Colour`), the question, numbered options
  each with a description line, `4. Type something.`, another rule, `5. Chat about this`, and
  `Enter to select · ↑/↓ to navigate · Esc to cancel`. The rule splits the options in two.
- **Plan approval.** `Ready to code?`, the plan between `╌` rules (its own numbered steps among
  it), then `Would you like to proceed?` with `1. Yes, and use auto mode`, `2. Yes, manually
  approve edits`, `3. Tell Claude what to change`.
- **After an interrupt** Claude Code puts the interrupted prompt back into the input. Ctrl+U
  empties it.
- **Codex first run.** The update notice comes first, with `1. Update now (runs npm install …)`
  selected; Esc skips it. In a git repository a folder trust dialog follows. Codex draws its
  prompt for 30 to 60 ms before the update notice.
- **Codex while working** shows `• Working (3s • esc to interrupt)` above the prompt, but while
  the answer streams that line is gone; only the title's braille spinner shows the turn is
  running. The spinner also turns for a second at startup with nothing to do. A turn ends with
  `Worked for 3s • 7:39 PM`, or `■ Conversation interrupted` after Esc.
- **Codex approval** is drawn on a background fill with the selected option on a second one,
  options `1. Yes, proceed (y)`, `2. Yes, and don't ask again … (p)`, `3. No, and tell Codex what
  to do differently (esc)`, and `Press enter to confirm or esc to cancel`. The title turns to
  `[ . ] Action Required | …`.
- **Codex model picker**: `Select Model and Effort` with numbered models, then `Select Reasoning
  Level for …`. A long prompt wraps with the cursor on the second line.

Profiles in truth files are frozen copies in `../profiles` of the ones in `packages/parrot-agents/tui/`,
so the crate's tests do not change when the agent package's profiles do. When updating a profile, copy it
into that directory and run `cargo test -p parrot-tui`.
Development casts also live in `crates/parrot-term/fixtures/casts` for emulator regression tests; refresh
both copies when re-recording a shared cast. Live recording examples still run real CLIs by hand.

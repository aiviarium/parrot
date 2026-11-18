#!/usr/bin/env python3
"""Stands in for Claude Code's TUI by replaying screens from a development cast.

It plays `claude-prompt.cast` (FAKE_CLAUDE_CAST) up to the idle prompt, then reads its
terminal in raw mode. A line containing "mystery" draws a dialog no profile knows and waits for
an answer; any other line replays the cast's working spinner and answer, back to idle. Every
byte read is appended to FAKE_CLAUDE_LOG when it is set.
"""

import json
import os
import sys
import termios
import time
import tty

IDLE_FROM, IDLE_TO = 0.0, 6.5  # the start and the empty idle prompt
PROMPT_AT = "\x1b[28;3H"  # where the cast draws typed input
TURN_FROM, TURN_TO = 7.2, 11.3  # the typed prompt, working, the answer, idle again
SPEED = 4.0


def events(path):
    with open(path, encoding="utf-8") as f:
        f.readline()
        for line in f:
            t, kind, data = json.loads(line)
            if kind == "o":
                yield t, data


def play(cast, start, end, paced):
    last = None
    out = sys.stdout
    for t, data in cast:
        if not start <= t < end:
            continue
        if paced and last is not None:
            time.sleep(min((t - last) / SPEED, 0.3))
        last = t
        out.write(data)
        out.flush()


def read_line(log, echo_at=""):
    """Reads until Enter, echoing printable input after `echo_at`; returns what was typed."""
    got = []
    typed = ""
    while True:
        b = os.read(0, 1024)
        if not b:
            sys.exit(0)
        if log:
            with open(log, "ab") as f:
                f.write(b)
        got.append(b)
        text = b.decode("utf-8", "replace")
        if not text.startswith("\x1b"):
            typed += "".join(ch for ch in text if ch.isprintable())
            sys.stdout.write(echo_at + typed if echo_at else "".join(ch for ch in text if ch.isprintable()))
            sys.stdout.flush()
        if b"\r" in b:
            text = b"".join(got).decode("utf-8", "replace")
            # Bracketed paste markers and other sequences carry no text.
            for seq in ("\x1b[200~", "\x1b[201~"):
                text = text.replace(seq, "")
            return text


def main():
    log = os.environ.get("FAKE_CLAUDE_LOG")
    cast = list(events(os.environ["FAKE_CLAUDE_CAST"]))
    if sys.stdin.isatty():
        old = termios.tcgetattr(0)
        tty.setraw(0)
    try:
        play(cast, IDLE_FROM, IDLE_TO, paced=False)
        while True:
            line = read_line(log, PROMPT_AT)
            if "/exit" in line:
                return
            if "mystery" in line:
                sys.stdout.write("\x1b[2J\x1b[H\r\n  Pick a colour: red, green or blue\r\n\r\n  colour: ")
                sys.stdout.flush()
                read_line(log)
                play(cast, IDLE_FROM, IDLE_TO, paced=False)
            else:
                play(cast, TURN_FROM, TURN_TO, paced=True)
    finally:
        if sys.stdin.isatty():
            termios.tcsetattr(0, termios.TCSADRAIN, old)


if __name__ == "__main__":
    main()

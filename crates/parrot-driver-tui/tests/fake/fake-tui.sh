#!/bin/sh
# A scripted TUI for the driver tests. It reads whole lines from its terminal and draws fixed
# screens: a prompt, a busy line, a tool approval, a trust dialog at start, and a question that
# no profile knows. A prompt starting with "slow" works for five seconds: a line typed meanwhile
# steers it, and Ctrl+C stops it. With FAKE_TUI_TRANSCRIPT set it writes a Claude-style
# transcript there.

out="${FAKE_TUI_TRANSCRIPT:-}"
session="fake-session-0001"
rule="────────────────────────────────────────"
n=0

log() {
  [ -n "$out" ] && printf '%s\n' "$1" >>"$out/$session.jsonl"
}

idle() {
  printf '\033[2J\033[H'
  printf 'Fake TUI 1.0\r\n%s\r\n\r\n? Type a prompt and press Enter' "$1"
  printf '\033[3;1H> '
}

busy() {
  printf '\033[2J\033[H'
  printf 'Fake TUI 1.0\r\n\r\n* Working… (esc to interrupt)'
  sleep "$1"
}

menu() {
  printf '\033[2J\033[H'
  printf 'Fake TUI 1.0\r\n%s\r\n %s\r\n ❯ 1. Yes\r\n   2. No\r\n\r\n Esc to cancel\r\n' "$rule" "$1"
}

if [ -n "${FAKE_TUI_TRUST:-}" ]; then
  menu "Do you trust the files in this folder?"
  IFS= read -r answer
  case "$answer" in 1) ;; *) exit 1 ;; esac
fi

idle "Ready."
while IFS= read -r line; do
  n=$((n + 1))
  case "$line" in
    approve*)
      busy 0.3
      menu "Do you want to run fake-tool?"
      IFS= read -r answer
      case "$answer" in 1) result="allowed" ;; *) result="denied" ;; esac
      busy 0.3
      idle "Tool $result."
      ;;
    slow*)
      log "{\"type\":\"user\",\"uuid\":\"fake-u-$n\",\"sessionId\":\"$session\",\"message\":{\"role\":\"user\",\"content\":\"$line\"}}"
      stopped=""
      trap 'stopped=1' INT
      # Reads without waiting for a line, so typing can reach the turn while it works.
      stty -icanon min 0 time 1
      buf=""
      steer=""
      i=0
      while [ $i -lt 50 ] && [ -z "$stopped" ]; do
        printf '\033[2J\033[H'
        printf 'Fake TUI 1.0\r\n%s\r\n* Working… (esc to interrupt)' "$steer"
        if IFS= read -r part; then
          steer="Steered: $buf$part"
          log "{\"type\":\"user\",\"uuid\":\"fake-s-$n-$i\",\"sessionId\":\"$session\",\"message\":{\"role\":\"user\",\"content\":\"$buf$part\"}}"
          buf=""
        else
          buf="$buf$part"
        fi
        i=$((i + 1))
      done
      stty icanon
      trap - INT
      if [ -n "$stopped" ]; then
        log "{\"type\":\"user\",\"uuid\":\"fake-i-$n\",\"sessionId\":\"$session\",\"message\":{\"role\":\"user\",\"content\":\"[Request interrupted by user]\"}}"
        idle "Interrupted."
      else
        log "{\"type\":\"assistant\",\"uuid\":\"fake-a-$n\",\"sessionId\":\"$session\",\"message\":{\"id\":\"msg_fake_$n\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Slow and done.\"}],\"stop_reason\":\"end_turn\"}}"
        idle "Done: $line"
      fi
      ;;
    mystery*)
      busy 0.3
      printf '\033[2J\033[H'
      printf 'Fake TUI 1.0\r\n\r\nPick a colour: red, green or blue\r\ncolour: '
      IFS= read -r colour
      busy 0.3
      idle "You picked $colour."
      ;;
    *)
      log "{\"type\":\"user\",\"uuid\":\"fake-u-$n\",\"sessionId\":\"$session\",\"message\":{\"role\":\"user\",\"content\":\"$line\"}}"
      busy 0.5
      log "{\"type\":\"assistant\",\"uuid\":\"fake-a-$n\",\"sessionId\":\"$session\",\"message\":{\"id\":\"msg_fake_$n\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Hi from the fake.\"}],\"stop_reason\":\"end_turn\"}}"
      idle "Done: $line"
      ;;
  esac
done

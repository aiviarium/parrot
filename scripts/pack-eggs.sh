#!/usr/bin/env bash
# Builds the driver binaries, puts them in their eggs' variant folders and packs every egg in
# eggs/ into a static Hatchery registry.
#
#   scripts/pack-eggs.sh [--out DIR] [--target TRIPLE] [--release] [--no-build]
#
# Without --target it builds for this machine. Install from the result with
#   parrot eggs install aiviarium/claude --registry DIR
# A release runs this once per platform with --no-pack, then once with --no-build to pack all
# the variants together.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/target/eggs-registry"
target=""
profile="debug"
build=1
pack=1
while [ $# -gt 0 ]; do
  case "$1" in
    --out) out="$2"; shift 2 ;;
    --target) target="$2"; shift 2 ;;
    --release) profile="release"; shift ;;
    --no-build) build=0; shift ;;
    --no-pack) pack=0; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

drivers=(claude codex pi opencode t3)
target_dir="${CARGO_TARGET_DIR:-$root/target}"

# The variant key Hatchery uses: Rust's OS and architecture names.
variant_key() {
  case "$1" in
    x86_64-unknown-linux-gnu) echo linux-x86_64 ;;
    aarch64-unknown-linux-gnu) echo linux-aarch64 ;;
    aarch64-apple-darwin) echo macos-aarch64 ;;
    x86_64-apple-darwin) echo macos-x86_64 ;;
    x86_64-pc-windows-msvc) echo windows-x86_64 ;;
    *) echo "no variant key for $1" >&2; exit 2 ;;
  esac
}

if [ "$build" = 1 ]; then
  args=(build --locked --bin parrot)
  for d in "${drivers[@]}"; do args+=(--bin "parrot-driver-$d"); done
  [ "$profile" = release ] && args+=(--release)
  if [ -n "$target" ]; then
    args+=(--target "$target")
    key="$(variant_key "$target")"
    bin_dir="$target_dir/$target/$profile"
  else
    key="$(variant_key "$(rustc -vV | sed -n 's/^host: //p')")"
    bin_dir="$target_dir/$profile"
  fi
  (cd "$root" && cargo "${args[@]}")
  exe=""
  case "$key" in windows-*) exe=".exe" ;; esac
  for d in "${drivers[@]}"; do
    dest="$root/eggs/$d/parrot/variants/$key"
    mkdir -p "$dest"
    cp "$bin_dir/parrot-driver-$d$exe" "$dest/"
  done
fi

if [ "$pack" = 1 ]; then
  parrot="${PARROT:-$target_dir/$profile/parrot}"
  [ -x "$parrot" ] || parrot="$(command -v parrot)"
  mkdir -p "$out"
  "$parrot" eggs pack "$root/eggs" --publish "$out"
  echo "registry in $out"
fi

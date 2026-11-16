#!/usr/bin/env bash
# Build Trident, run compare.py against every other terminal on PATH and write
# Markdown tables next to the results JSON (target/compare/results.md).
# Extra arguments go to compare.py, e.g. `run.sh --tests vtebench --bin ghostty=~/Ghostty.AppImage`.
# `run.sh rounds [OUT]` runs rounds.sh and aa.py instead (two rounds with A/A bounds; set BINS).
# Needs: apt install xvfb openbox python3-xlib xdotool fonts-jetbrains-mono
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
work="$repo/target/compare"
mkdir -p "$work"
cd "$repo"
command -v xdotool >/dev/null || echo "xdotool not found, latency tests will fail (apt install xdotool)" >&2

cargo build --release -p term-app

if [ -z "${DISPLAY:-}" ] || ! python3 -c 'from Xlib import display; display.Display()' 2>/dev/null; then
  export DISPLAY=:99
  Xvfb "$DISPLAY" -screen 0 1920x1080x24 -nolisten tcp &
  xvfb=$!
  trap 'kill $(jobs -p) 2>/dev/null || true' EXIT
  for _ in $(seq 50); do
    python3 -c 'from Xlib import display; display.Display()' 2>/dev/null && break
    kill -0 "$xvfb" || { echo "Xvfb failed to start on $DISPLAY" >&2; exit 1; }
    sleep 0.1
  done
  openbox 2>/dev/null &
fi

vtebench=$(command -v vtebench || true)
if [ -z "$vtebench" ]; then
  cargo install --locked --git https://github.com/alacritty/vtebench --root "$work/tools"
  vtebench="$work/tools/bin/vtebench"
fi
vtedir="$HOME/vtebench"
if [ ! -d "$vtedir/benchmarks" ]; then
  vtedir="$work/vtebench"
  [ -d "$vtedir/benchmarks" ] || git clone --depth 1 https://github.com/alacritty/vtebench "$vtedir"
fi

if [ "${1:-}" = rounds ]; then
  out=${2:-$work/rounds}
  BINS="${BINS:-} --vtebench $vtebench --vtebench-dir $vtedir" "$here/rounds.sh" "$out"
  python3 "$here/aa.py" "$out" | tee "$out/results.md"
  echo "tables: $out/results.md"
  exit
fi

json= outdir=$work prev=
for arg in "$@"; do
  case "$prev" in --json) json=$arg ;; --work) outdir=$arg ;; esac
  case "$arg" in --json=*) json=${arg#--json=} ;; --work=*) outdir=${arg#--work=} ;; esac
  prev=$arg
done
json=${json:-$outdir/results.json}

python3 "$here/compare.py" --vtebench "$vtebench" --vtebench-dir "$vtedir" "$@"
md="${json%.json}.md"
python3 "$here/summarize.py" "$json" > "$md"
cat "$md"
echo "tables: $md"

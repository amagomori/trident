#!/bin/bash
# Two full rounds of compare.py with the terminal order reversed in round B, so every
# terminal gets an A/A bound (|A - B|). Then: python3 .github/scripts/compare/aa.py OUT
#   Xvfb :99 -screen 0 1920x1080x24 & DISPLAY=:99 openbox &
#   cargo build --release && .github/scripts/compare/rounds.sh [OUT]
# Ghostty only gets startup, memory and vtebench: on Xvfb+llvmpipe it spins at high idle CPU,
# ignores XTest keys and takes minutes per 100 MiB cat. Pass --bin overrides via BINS.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd); OUT=${1:-$HERE/../../../target/compare/rounds}; mkdir -p "$OUT"
export DISPLAY=${DISPLAY:-:99}
run() { python3 "$HERE/compare.py" --terms "$2" --tests "$3" ${BINS:-} --json "$OUT/round$1.json" 2>&1 | tee -a "$OUT/run.log"; }
FOUR_A=terminal,alacritty,kitty,wezterm; FOUR_B=wezterm,kitty,alacritty,terminal
for r in ${ROUNDS:-A B}; do
  f=FOUR_$r; rm -f "$OUT/round$r.json"
  run $r ${!f} startup,memory,latency,latency_load,cat
  run $r ghostty startup,memory
  if [ $r = A ]; then run A $FOUR_A,ghostty vtebench; else run B ghostty,$FOUR_B vtebench; fi
done

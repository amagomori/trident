#!/bin/bash
# Trident vs Alacritty, Ghostty and WezTerm on macOS, black-box, with an A/A arm (Trident again) for noise bounds.
# One script, no manual steps. Needs: release build (cargo build --release), Screen Recording + Accessibility
# for the shell running it (mlat captures the display and injects keys), JetBrains Mono, the terminals in
# /Applications or ~/.cargo/bin, vtebench (cargo install vtebench) and a ~/vtebench checkout.
# kitty is optional (ARMS="... kitty"); it needs OpenGL 3.3, which macOS VMs lack. WezTerm uses macos/wezterm.lua (WebGpu).
set -e
HERE=$(cd "$(dirname "$0")" && pwd); REPO=$(cd "$HERE/../../../.." && pwd)
OUT=${COMPARE_OUT:-$REPO/target/compare-macos}; export COMPARE_OUT=$OUT; mkdir -p "$OUT/res"
C=$HERE/../cfg; M=$HERE/marker.sh; R=$OUT/res
TRIDENT=${TRIDENT:-${CARGO_TARGET_DIR:-$REPO/target}/release/trident}; ALACRITTY=${ALACRITTY:-$HOME/.cargo/bin/alacritty}
ARMS=${ARMS:-"trident trident-aa alacritty ghostty wezterm"}
[ -x "$OUT/mlat" ] && [ "$OUT/mlat" -nt "$HERE/mlat.swift" ] || swiftc -O "$HERE/mlat.swift" -o "$OUT/mlat"
[ -f "$OUT/100mib.txt" ] || python3 -c "
import random; random.seed(1); w = open('$OUT/100mib.txt', 'w'); n = 0
words = 'lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor'.split()
while n < 100 << 20: l = ' '.join(random.choice(words) for _ in range(14)) + '\n'; w.write(l); n += len(l)"
cmd() { case $1 in
  trident*) echo "$TRIDENT --config $C/terminal.toml -e $M";;
  alacritty) echo "$ALACRITTY --config-file $C/alacritty.toml -e $M";;
  ghostty) echo "/Applications/Ghostty.app/Contents/MacOS/ghostty --config-default-files=false --config-file=$C/ghostty.conf -e $M";;
  kitty) echo "/Applications/kitty.app/Contents/MacOS/kitty --config $C/kitty.conf $M";;
  wezterm) echo "/Applications/WezTerm.app/Contents/MacOS/wezterm-gui --config-file $HERE/wezterm.lua start --always-new-process -- $M";;
esac; }
order() { if [ $(( $1 % 2 )) = 0 ]; then echo $ARMS; else echo $ARMS | tr ' ' '\n' | tail -r | tr '\n' ' '; fi; }
for r in 1 2 3 4; do for a in $(order $r); do echo "== startup $a $r"; timeout 300 "$OUT/mlat" startup 11 0 "$R/$a-startup-$r.json" 0 -- $(cmd $a) | tail -1; done; done
for r in 1 2 3 4; do for a in $(order $r); do echo "== latency $a $r"; timeout 150 "$OUT/mlat" latency 1 100 "$R/$a-latency-$r.json" 0 -- $(cmd $a) | tail -1; done; done
A=$(echo $ARMS | tr ' ' ,)
python3 "$HERE/compare_mac.py" memory 3 "$R/memory.json" $A
python3 "$HERE/compare_mac.py" cat 3 "$R/cat.json" $A
VSECS=${VSECS:-6} python3 "$HERE/compare_mac.py" vtebench 2 "$R/vtebench.json" $A
python3 "$HERE/summarize.py" "$R" > "$OUT/RESULTS.md"; echo "wrote $OUT/RESULTS.md"

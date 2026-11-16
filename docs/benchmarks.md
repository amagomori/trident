# Benchmarks

Trident `ca6b30d` against other terminals, each with the same font, size and scrollback. Lower is
better. These numbers compare terminals on one machine; they aren't real-display latency.

## macOS

Alacritty 0.17.0, WezTerm 20240203 and Ghostty 1.4.0 on a macOS 26.6 VM (Apple M4 Pro, 12 vCPU,
paravirtual Metal). Each value is the median of interleaved rounds.

| | Trident | Alacritty | kitty | WezTerm | Ghostty |
|---|---|---|---|---|---|
| First paint (ms) | 71.6 | 68.8 | n/a | 76.6 | **66.1** |
| Memory at open (MiB) | **91.6** | 105.0 | n/a | 101.4 | 104.6 |
| Keypress latency, median (ms) | **18.2** | 22.6 | n/a | 49.8 | n/a |
| `cat` of 100 MiB (s) | **0.54** | 1.67 | n/a | 2.77 | 0.80 |
| vtebench dense cells (ms) | **9.0** | 11.5 | n/a | 18.0 | 15.5 |
| vtebench scrolling (ms) | **19.5** | 29.5 | n/a | 79.0 | 24.5 |
| vtebench unicode (ms) | **7.0** | 9.0 | n/a | 24.5 | **7.0** |

Trident leads or ties within noise everywhere; first paint is within noise for all four. The one
trail is memory after a 50,000-line burst: 117.7 MiB against WezTerm's 114.0 (+3.3%), from the
app icon loading added in [#104](https://github.com/amagomori/trident/pull/104). kitty needs OpenGL 3.3, which the VM lacks, and Ghostty's
keypress latency on the VM (about 600 ms) is a compositing artifact, so neither is shown.

## Linux

Alacritty 0.17.0, kitty 0.50.0, WezTerm 20240203 and Ghostty 1.4.0 on a Linux VM (8 vCPU, Xvfb,
software GL). Each value is the mean of two rounds.

| | Trident | Alacritty | kitty | WezTerm | Ghostty |
|---|---|---|---|---|---|
| First paint (ms) | **115** | 189 | 386 | 216 | 1350 |
| Memory at open (MiB) | **108** | **108** | 204 | 140 | 551 |
| Keypress latency, median (ms) | **1.9** | 2.3 | 25.2 | 58.4 | n/a |
| `cat` of 100 MiB (s) | **2.0** | 3.0 | 3.6 | 9.7 | n/a |
| vtebench dense cells (ms) | **12.7** | 38.4 | 58.6 | 76.3 | 29.7 |
| vtebench scrolling (ms) | **383** | 385 | 469 | 425 | 433 |
| vtebench unicode (ms) | 9.7 | 21.4 | 55.3 | 101.7 | **7.3** |

Trident leads or ties within noise everywhere except vtebench unicode, where Ghostty is faster.
On software GL, Ghostty ignores simulated keypresses and spins its renderer, so it has no latency
or `cat` numbers.

## Run it

```sh
# Linux
sudo apt install xvfb openbox python3-xlib xdotool fonts-jetbrains-mono
.github/scripts/compare/run.sh rounds   # about 50 minutes; prints every metric with noise bounds

# macOS (the shell needs Screen Recording and Accessibility permission)
brew install --cask ghostty wezterm font-jetbrains-mono
cargo install alacritty vtebench && git clone https://github.com/alacritty/vtebench ~/vtebench
cargo build --release && .github/scripts/compare/macos/run.sh   # writes target/compare-macos/RESULTS.md
```

See [Compare with other terminals](development.md#compare-with-other-terminals) for options.

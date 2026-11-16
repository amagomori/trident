# Benchmarks

These benchmarks compare Trident with other terminals using the same font, size and scrollback.
The latency numbers reflect the test environment rather than a physical display.

## macOS

The macOS tests compare Trident `0c8392b`, Alacritty 0.17.0, WezTerm 20240203 and Ghostty 1.4.0
on a macOS 26.6 VM with an Apple M4, 8 vCPUs and paravirtual Metal. Memory tests use Trident
`89f684d`. Values are medians of interleaved rounds from the
[comparison harness](../.github/scripts/compare/macos/run.sh).

| | Trident | Alacritty | kitty | WezTerm | Ghostty |
|---|---|---|---|---|---|
| First paint (ms) | 67.6 | 65.7 | n/a | 68.1 | **65.6** |
| Memory at open (MiB) | **87.9** | 108.1 | n/a | 99.8 | 105.3 |
| Keypress latency, median (ms) | 30.9 | **21.6** | n/a | 45.8 | n/a |
| `cat` of 100 MiB (s) | **0.58** | 1.68 | n/a | 2.88 | 1.15 |
| vtebench dense cells (ms) | **13.5** | 22.5 | n/a | 28.5 | 27.5 |
| vtebench scrolling (ms) | **23.5** | 28.0 | n/a | 79.5 | 26.5 |
| vtebench unicode (ms) | **5.5** | 7.8 | n/a | 28.5 | 10.5 |

Trident uses the least memory at open and has the lowest `cat` and vtebench times. Scrolling is
within noise of Alacritty and Ghostty. First paint is 3.1% slower than Ghostty
with a ±3.0% noise bound. Keypress latency is 43% higher than Alacritty with a ±28.5% noise bound.
After a 50,000-line burst, memory use is 115.9 MiB for Trident and 112.5 MiB for WezTerm,
within the ±4.2% noise bound.

The VM lacks OpenGL 3.3, which kitty requires, so kitty has no results. Ghostty's keypress latency
of about 600 ms is a VM compositing artifact and is excluded.

## Linux

The Linux tests compare Trident `ca6b30d`, Alacritty 0.17.0, kitty 0.50.0, WezTerm 20240203 and
Ghostty 1.4.0 on a VM with 8 vCPUs, Xvfb and software GL. Each value is the mean of two rounds.

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

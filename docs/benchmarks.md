# Benchmarks

Trident `ca6b30d` against Alacritty 0.17.0, kitty 0.50.0, WezTerm 20240203 and Ghostty 1.4.0
on Linux (8 vCPU VM, Xvfb, software GL), each with the same font, size and scrollback. Lower is
better; each value is the mean of two rounds.

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
or `cat` numbers. These numbers compare terminals on one machine; they aren't real-display latency.

## Run it

```sh
sudo apt install xvfb openbox python3-xlib fonts-jetbrains-mono
.github/scripts/compare/run.sh rounds   # about 50 minutes; prints every metric with noise bounds
```

See [Compare with other terminals](development.md#compare-with-other-terminals) for options.

# Roadmap

From the first usable build (v0.1) to a signed, packaged 1.0 on macOS and Linux. Tracked in Jira: [Terminal board (KAN)](https://jarettforzano.atlassian.net/jira/software/projects/KAN/boards). Tickets follow the team standards in [amagomori/.github](https://github.com/amagomori/.github): [ticket format](https://github.com/amagomori/.github/blob/main/jira/ticket_format.md) and [workflow](https://github.com/amagomori/.github/blob/main/jira/workflow.md).

## Milestones

| Milestone | Meaning | Epics |
|---|---|---|
| v0.1 | Opens, runs your shell, renders on the GPU | [KAN-4](https://jarettforzano.atlassian.net/browse/KAN-4) Ship first usable terminal (v0.1) |
| v0.2 | Full-screen apps (vim, tmux, htop) behave correctly; CI and tests in place | [KAN-5](https://jarettforzano.atlassian.net/browse/KAN-5) Set up engineering foundations, [KAN-10](https://jarettforzano.atlassian.net/browse/KAN-10) Reach xterm-level VT correctness |
| v0.3 | Fast, good-looking, usable all day | [KAN-20](https://jarettforzano.atlassian.net/browse/KAN-20) Make rendering fast and beautiful, [KAN-28](https://jarettforzano.atlassian.net/browse/KAN-28) Build daily-driver UX |
| v1.0 | Native on both platforms, signed, installable in one command | [KAN-40](https://jarettforzano.atlassian.net/browse/KAN-40) Integrate natively with macOS and Linux, [KAN-44](https://jarettforzano.atlassian.net/browse/KAN-44) Release and distribute 1.0 |

## Labels

- Milestones: `v0.1`, `v0.2`, `v0.3`, `v1.0`
- Areas: `area-app` (window, PTY, input), `area-core` (VT parser and grid), `area-render` (GPU, fonts), `area-ux` (selection, config, tabs), `area-platform` (macOS/Linux integration), `area-infra` (CI, tests, benchmarks), `area-release` (packaging, docs)

## Architecture

- `term-core`: escape-sequence parser (vte), cell grid, modes, scrollback. No GPU or window dependencies; tested headless.
- `term-render`: wgpu renderer, glyph atlas, shaping and font fallback.
- `term-app`: winit window, PTY (portable-pty), input, config, clipboard (arboard).

## v0.1: Ship first usable terminal (v0.1) ([KAN-4](https://jarettforzano.atlassian.net/browse/KAN-4))

A window you can open on macOS and Linux that runs your login shell, renders it on the GPU and accepts typing. Being built now in devin session b2efff19 (winit 0.30 + wgpu 30 + fontdue + vte 0.15 + portable-pty 0.9 + arboard).

| Ticket | Priority | Blocked by |
|---|---|---|
| [KAN-2](https://jarettforzano.atlassian.net/browse/KAN-2) Ship first usable terminal build | Highest | - |
| [KAN-3](https://jarettforzano.atlassian.net/browse/KAN-3) Verify v0.1 runs on macOS and Linux | High | KAN-2 |

## v0.2: Set up engineering foundations ([KAN-5](https://jarettforzano.atlassian.net/browse/KAN-5))

Make every later ticket cheap and safe to land: CI on both OSes, a clean crate layout, and a headless test harness for terminal state.

| Ticket | Priority | Blocked by |
|---|---|---|
| [KAN-1](https://jarettforzano.atlassian.net/browse/KAN-1) Set up CI on Linux and macOS | Highest | KAN-2 |
| [KAN-6](https://jarettforzano.atlassian.net/browse/KAN-6) Split code into core, renderer, app crates | High | KAN-2 |
| [KAN-7](https://jarettforzano.atlassian.net/browse/KAN-7) Add headless golden tests for terminal state | High | KAN-6 |
| [KAN-8](https://jarettforzano.atlassian.net/browse/KAN-8) Fuzz the escape sequence parser | Medium | KAN-6 |
| [KAN-9](https://jarettforzano.atlassian.net/browse/KAN-9) Add throughput and latency benchmarks | Medium | KAN-6 |

## v0.2: Reach xterm-level VT correctness ([KAN-10](https://jarettforzano.atlassian.net/browse/KAN-10))

Full-screen apps (vim, tmux, htop, less, fzf) behave exactly as in xterm/Alacritty.

| Ticket | Priority | Blocked by |
|---|---|---|
| [KAN-11](https://jarettforzano.atlassian.net/browse/KAN-11) Support 256-color and truecolor SGR | Highest | KAN-7 |
| [KAN-12](https://jarettforzano.atlassian.net/browse/KAN-12) Support alternate screen and scroll regions | Highest | KAN-7 |
| [KAN-13](https://jarettforzano.atlassian.net/browse/KAN-13) Support mouse reporting modes | High | KAN-7 |
| [KAN-14](https://jarettforzano.atlassian.net/browse/KAN-14) Support bracketed paste and app cursor keys | High | KAN-7 |
| [KAN-15](https://jarettforzano.atlassian.net/browse/KAN-15) Handle wide chars, emoji and graphemes | High | KAN-7 |
| [KAN-16](https://jarettforzano.atlassian.net/browse/KAN-16) Reflow text on resize | Medium | KAN-7 |
| [KAN-17](https://jarettforzano.atlassian.net/browse/KAN-17) Support OSC title, clipboard and hyperlinks | Medium | KAN-7 |
| [KAN-18](https://jarettforzano.atlassian.net/browse/KAN-18) Ship a terminfo entry | Medium | KAN-11, KAN-12 |
| [KAN-19](https://jarettforzano.atlassian.net/browse/KAN-19) Pass vttest and esctest core suites | High | KAN-11, KAN-12, KAN-13, KAN-14 |

## v0.3: Make rendering fast and beautiful ([KAN-20](https://jarettforzano.atlassian.net/browse/KAN-20))

Text looks as good as native macOS/Linux terminals and stays fast under load.

| Ticket | Priority | Blocked by |
|---|---|---|
| [KAN-21](https://jarettforzano.atlassian.net/browse/KAN-21) Render crisply on HiDPI displays | Highest | KAN-2 |
| [KAN-22](https://jarettforzano.atlassian.net/browse/KAN-22) Add text shaping and ligatures | High | KAN-6 |
| [KAN-23](https://jarettforzano.atlassian.net/browse/KAN-23) Add font fallback and color emoji | High | KAN-22 |
| [KAN-24](https://jarettforzano.atlassian.net/browse/KAN-24) Draw box and powerline glyphs natively | Medium | KAN-21 |
| [KAN-25](https://jarettforzano.atlassian.net/browse/KAN-25) Add cursor styles and styled underlines | Medium | KAN-11 |
| [KAN-26](https://jarettforzano.atlassian.net/browse/KAN-26) Redraw only damaged rows | High | KAN-9 |
| [KAN-27](https://jarettforzano.atlassian.net/browse/KAN-27) Hit throughput and latency targets | High | KAN-9, KAN-26 |

## v0.3: Build daily-driver UX ([KAN-28](https://jarettforzano.atlassian.net/browse/KAN-28))

Everything you need to use it all day instead of Terminal.app / GNOME Terminal.

| Ticket | Priority | Blocked by |
|---|---|---|
| [KAN-29](https://jarettforzano.atlassian.net/browse/KAN-29) Select text with the mouse | Highest | KAN-2 |
| [KAN-30](https://jarettforzano.atlassian.net/browse/KAN-30) Scroll back through history | Highest | KAN-2 |
| [KAN-31](https://jarettforzano.atlassian.net/browse/KAN-31) Search the scrollback | High | KAN-30 |
| [KAN-32](https://jarettforzano.atlassian.net/browse/KAN-32) Add a TOML config with hot reload | Highest | KAN-6 |
| [KAN-33](https://jarettforzano.atlassian.net/browse/KAN-33) Support color themes | Medium | KAN-32 |
| [KAN-34](https://jarettforzano.atlassian.net/browse/KAN-34) Make keybindings configurable | Medium | KAN-32 |
| [KAN-35](https://jarettforzano.atlassian.net/browse/KAN-35) Zoom font size in and out | Medium | KAN-21 |
| [KAN-36](https://jarettforzano.atlassian.net/browse/KAN-36) Detect and open URLs | Medium | KAN-17, KAN-29 |
| [KAN-37](https://jarettforzano.atlassian.net/browse/KAN-37) Support IME composition input | High | KAN-2 |
| [KAN-38](https://jarettforzano.atlassian.net/browse/KAN-38) Open multiple windows and tabs | Medium | KAN-32 |
| [KAN-39](https://jarettforzano.atlassian.net/browse/KAN-39) Split panes within a window | Low | KAN-38 |

## v1.0: Integrate natively with macOS and Linux ([KAN-40](https://jarettforzano.atlassian.net/browse/KAN-40))

It feels like a native app on each platform.

| Ticket | Priority | Blocked by |
|---|---|---|
| [KAN-41](https://jarettforzano.atlassian.net/browse/KAN-41) Polish macOS app behavior | High | KAN-32, KAN-34 |
| [KAN-42](https://jarettforzano.atlassian.net/browse/KAN-42) Support Wayland and X11 on Linux | High | KAN-21 |
| [KAN-43](https://jarettforzano.atlassian.net/browse/KAN-43) Add shell integration | Medium | KAN-30 |

## v1.0: Release and distribute 1.0 ([KAN-44](https://jarettforzano.atlassian.net/browse/KAN-44))

Anyone can install it in one command and it updates cleanly.

| Ticket | Priority | Blocked by |
|---|---|---|
| [KAN-45](https://jarettforzano.atlassian.net/browse/KAN-45) Automate release builds | Highest | KAN-1 |
| [KAN-46](https://jarettforzano.atlassian.net/browse/KAN-46) Sign and notarize the macOS app | High | KAN-45 |
| [KAN-47](https://jarettforzano.atlassian.net/browse/KAN-47) Publish to Homebrew and Linux packages | Medium | KAN-45 |
| [KAN-48](https://jarettforzano.atlassian.net/browse/KAN-48) Write user docs and config reference | Medium | KAN-32 |
| [KAN-49](https://jarettforzano.atlassian.net/browse/KAN-49) Run a 1.0 dogfood and bug bash | High | KAN-19, KAN-27, KAN-41, KAN-42, KAN-45 |

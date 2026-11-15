# Trident documentation

- [Usage](usage.md): command line, terminfo, keyboard and mouse, search, selection, pasting, close confirmation, desktop notifications, app icons, dropping files, and `trident show`.
- [Configuration](configuration.md): the config file and every setting, key bindings, scrollback budgets, shell integration, live reload, and color themes.
- [Development](development.md): crate layout, tests, latency measurement, and comparisons against other terminals.

## Features

A fast GPU-rendered terminal emulator written from scratch in Rust, for macOS and Linux.

- **GPU rendering** with [wgpu](https://wgpu.rs): Metal on macOS, Vulkan or OpenGL on Linux.
  Every frame is one instanced draw call of cell backgrounds and glyphs from a texture atlas.
- **Easy on low-end hardware**: it asks for the low-power (integrated) GPU, so dual-GPU
  MacBooks stay on the integrated one, and only redraws when something on screen changes.
  Idle, it uses no CPU or GPU.
- **xterm-compatible**: 256 colors and truecolor, bold/italic/underline/strikethrough/dim,
  alternate screen, scroll regions, bracketed paste, focus events, mouse reporting (X10,
  click, drag and motion tracking; SGR and legacy encodings), window titles, and wide (CJK)
  characters. Runs vim, tmux, htop, less and friends.
- **Configurable scrollback** (10,000 lines by default), pixel-smooth trackpad scrolling,
  mouse selection (double-click for words, triple-click for
  lines), and clipboard support, including the primary selection on Linux.
- Bundled [JetBrains Mono](https://www.jetbrains.com/lp/mono/), with fallback to system fonts
  (found through fontconfig on Linux, Core Text on macOS) for symbols and CJK, and color emoji
  (COLR, sbix and CBDT fonts such as Apple Color Emoji and Noto Color Emoji).

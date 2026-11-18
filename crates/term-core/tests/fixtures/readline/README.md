# Native Readline repaint fixtures

Byte-identical `all.bin` captures of startup, the OSC 133 prompt and the input repaint from native
macOS 26.6.2 arm64 at 44 columns by 34 rows: Bash 3.2.57 and Bash 5.3.20, prompt `$ `, input
`: ` + 39 `a` + `日本😀bc`. Tests replay the captured bytes on the core.

Readline prints a last-column space and erases it with EL before wrapping the wide glyph. The
cases pin that behavior:

- `wide` needs 40 Lefts to reach input offset 6, not the formerly returned 41.
- `typed-space` inserts a real space before `日`: same final visible grid, but it retains
  printed-input occupancy and needs 41 Lefts.
- `ascii` uses `: ` + 43 `a` + `bc` and also needs 41 Lefts.

This is a regression contract for that observed repaint, not a general classifier for erased
editor characters.

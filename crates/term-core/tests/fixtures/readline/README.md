# Native Readline repaint fixtures

These are byte-identical `all.bin` captures (startup, OSC 133 prompt, then
the input repaint) from native macOS 26.6.2 arm64 at 44 columns by 34 rows:
Bash 3.2.57 and Bash 5.3.20, prompt `$ `, input `: ` + 39 `a` + `日本😀bc`.
The `typed-space` control inserts a real space before `日`; the `ascii`
control uses `: ` + 43 `a` + `bc`. Both controls need 41 Lefts to reach input
offset 6; `wide` needs 40, not the formerly returned 41.

Readline prints a last-column space and erases it with EL before wrapping
the wide glyph. The real-space control has the same final visible grid but
retains printed-input occupancy. This is a regression contract for that
observed repaint, not a general classifier for erased editor characters.

Source: https://app.devin.ai/sessions/b2a09d77d2134a1db385fd669681fbe8
Archive SHA256: `622146a37c899c4c319d1fb9382196949aad5e4af548af470494f48c5d2ba3d9`.
The archive retains native history round trips, raw PTY streams, original
QA rc, source corroboration and its manifest. Tests replay these bytes on
the core; they do not qualify application pointer delivery or native UI.

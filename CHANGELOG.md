# Changelog

## Unreleased

- `Tty::report`: pointer reports - moves, presses and the wheel, for a program tracking the
  mouse - go through it rather than `Tty::write`, so a handle that takes keystrokes as an
  answer can tell that nobody typed. Defaults to `write`, as `reply` does.
- `TerminalStyle` takes a `bold_font` and an `italic_font`. Given a face, bold and italic
  text is set in it; without one, bold is still brighter ink and italic the regular face
  sheared, as before.

## 0.1.0

First release. A terminal widget on Ghostty's VT engine: selection, links, search, and a
light color scheme for light themes.

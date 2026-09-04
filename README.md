# egui_tty

A terminal emulator widget for [egui](https://github.com/emilk/egui), drawn on top of
[Ghostty](https://ghostty.org)'s VT engine.

Ghostty's engine — via [`libghostty-vt`](https://crates.io/crates/libghostty-vt) — owns
everything that makes a terminal a terminal: the parser, the screen and its scrollback, reflow
on resize, selections, and input encoding. `egui_tty` is the part it deliberately leaves out:
turning that grid into pixels, and turning window events into terminal events.

So this is a real terminal, not a log view. Full-screen programs, mouse reporting, bracketed
paste, OSC 8 links, the Kitty keyboard protocol — all of it works, because the engine
underneath is the one shipping in Ghostty.

- text selection, including double-click words, triple-click lines, alt-drag blocks and
  shift-click to carry a selection on, across a scroll if need be
- clickable links, both OSC 8 and plain printed URLs
- search over the screen and the scrollback
- a light color scheme for light themes, where Ghostty's own would be unreadable
- bold and italic set in real faces, given a `bold_font` and an `italic_font`; brighter ink
  and a shear of the regular face without
- attach anything: a local pty, a socket to another machine, a test harness

```rust
let stream = egui_tty::TtyStream { output, tty: Arc::new(my_pty) };
let mut terminal = egui_tty::Terminal::new(stream)?;

// then, once per frame
let style = egui_tty::TerminalStyle::from_visuals(ui.visuals());
terminal.ui(ui, &style);
```

`output` is an `mpsc::Receiver<Vec<u8>>` of what the program printed, and `my_pty` implements
the three-method [`Tty`](https://docs.rs/egui_tty/latest/egui_tty/trait.Tty.html) trait. A
whole shell in a window is about forty lines:

```sh
cargo run --example shell
```

## Building

`libghostty-vt` compiles Ghostty's VT engine from source, so building this crate needs a
[Zig](https://ziglang.org) 0.15.x toolchain on `PATH`.

## License

MIT

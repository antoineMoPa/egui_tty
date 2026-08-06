//! A terminal emulator widget for [egui](https://github.com/emilk/egui).
//!
//! [libghostty-vt](https://crates.io/crates/libghostty-vt) — the VT engine
//! [Ghostty](https://ghostty.org) is built on — owns everything that makes a terminal a
//! terminal: the parser, the screen and its scrollback, reflow on resize, selections, and
//! input encoding. This crate is the part Ghostty deliberately leaves out: turning the
//! resulting grid into pixels, and turning window events into terminal events.
//!
//! What you get is a real terminal, not a log view. Full-screen programs, mouse reporting,
//! bracketed paste, OSC 8 links, the Kitty keyboard protocol — they work because the engine
//! underneath is the one shipping in Ghostty.
//!
//! # Attaching a program
//!
//! The crate takes no view on where the bytes come from: a local pty, a websocket to another
//! machine, or a test harness. You provide a [`Tty`] to write to, and an
//! [`std::sync::mpsc::Receiver`] the program's output arrives on.
//!
//! ```no_run
//! use std::sync::{Arc, mpsc};
//!
//! struct Echo;
//!
//! impl egui_tty::Tty for Echo {
//!     fn write(&self, _bytes: &[u8]) -> egui_tty::Result<()> {
//!         Ok(())
//!     }
//!
//!     fn resize(&self, _cols: u16, _rows: u16) -> egui_tty::Result<()> {
//!         Ok(())
//!     }
//! }
//!
//! # fn main() -> egui_tty::Result<()> {
//! let (writer, output) = mpsc::channel();
//! writer.send(b"hello from a terminal\r\n".to_vec()).ok();
//!
//! let mut terminal = egui_tty::Terminal::new(egui_tty::TtyStream {
//!     output,
//!     tty: Arc::new(Echo),
//! })?;
//! # let _ = &mut terminal;
//! # Ok(())
//! # }
//! ```
//!
//! Then draw it, once per frame, wherever you want it:
//!
//! ```no_run
//! # fn frame(ui: &mut egui::Ui, terminal: &mut egui_tty::Terminal) {
//! let style = egui_tty::TerminalStyle::from_visuals(ui.visuals());
//! terminal.ui(ui, &style);
//! # }
//! ```
//!
//! See `examples/shell.rs` for the whole thing against a real login shell.
//!
//! # Building
//!
//! `libghostty-vt` compiles Ghostty's VT engine from source, so a
//! [Zig](https://ziglang.org) 0.15.x toolchain has to be on `PATH` to build this crate.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::doc_markdown)]

mod colors;
mod error;
mod keys;
mod links;
mod search;
mod selection;
mod style;
mod terminal;

pub use error::{Error, Result};
pub use style::{ColorScheme, TerminalStyle};
pub use terminal::{DEFAULT_SCROLLBACK, Terminal, cell_size};

use std::sync::{Arc, mpsc};

/// The program behind a terminal: somewhere to send keystrokes, and a grid size to keep it
/// informed of.
///
/// Implementations are usually a pty ([`portable-pty`](https://crates.io/crates/portable-pty)
/// is the easy way) or a connection to a shell running on another machine.
pub trait Tty: Send + Sync {
    /// Send bytes the terminal has encoded — keystrokes, pastes, replies to the program's own
    /// queries — to the program.
    fn write(&self, bytes: &[u8]) -> Result<()>;

    /// Tell the program its window changed size, so its foreground process gets `SIGWINCH`.
    fn resize(&self, cols: u16, rows: u16) -> Result<()>;

    /// Whether the program behind this handle has ended.
    ///
    /// A terminal notices an ended program on its own when the output channel closes. Override
    /// this when the channel can outlive the program — which happens when what holds the
    /// session open is the terminal itself.
    fn has_exited(&self) -> bool {
        false
    }
}

/// One attached program: the bytes it writes, and the handle to write back to it.
pub struct TtyStream {
    /// Everything the program has written, delivered to the thread that draws the terminal.
    ///
    /// Send whatever the program printed before the terminal existed as the first chunk, and
    /// it is replayed into the screen and the scrollback like anything else.
    pub output: mpsc::Receiver<Vec<u8>>,
    /// Shared, so the terminal's reply hook can hold a handle without borrowing the widget.
    pub tty: Arc<dyn Tty>,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, mpsc};

    use super::*;

    /// A program that records what it was sent and prints whatever a test hands it.
    #[derive(Default)]
    struct Recorder {
        written: Mutex<Vec<u8>>,
        size: Mutex<(u16, u16)>,
    }

    impl Tty for Recorder {
        fn write(&self, bytes: &[u8]) -> Result<()> {
            self.written.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        }

        fn resize(&self, cols: u16, rows: u16) -> Result<()> {
            *self.size.lock().unwrap() = (cols, rows);
            Ok(())
        }
    }

    fn terminal() -> (Terminal, mpsc::Sender<Vec<u8>>, Arc<Recorder>) {
        let (writer, output) = mpsc::channel();
        let recorder = Arc::new(Recorder::default());
        let terminal = Terminal::new(TtyStream {
            output,
            tty: Arc::clone(&recorder) as Arc<dyn Tty>,
        })
        .expect("expected a terminal");
        (terminal, writer, recorder)
    }

    #[test]
    fn output_from_the_program_lands_on_the_screen() {
        let (mut terminal, writer, _) = terminal();
        writer
            .send(b"hello\r\nworld\r\n".to_vec())
            .expect("expected the terminal to be listening");

        assert!(terminal.poll(), "the terminal read what was written");
        let screen = terminal.visible_text().expect("expected the screen");
        let lines: Vec<&str> = screen.lines().take(2).collect();
        assert_eq!(lines, vec!["hello", "world"]);
    }

    #[test]
    fn escape_sequences_are_interpreted_rather_than_printed() {
        let (mut terminal, writer, _) = terminal();
        // Set a title, then print in red: neither is text that belongs on the grid.
        writer
            .send(b"\x1b]0;a title\x07\x1b[31mred\x1b[0m".to_vec())
            .expect("expected the terminal to be listening");
        terminal.poll();

        assert_eq!(terminal.title().as_deref(), Some("a title"));
        assert_eq!(
            terminal.visible_text().expect("expected the screen").lines().next(),
            Some("red")
        );
    }

    #[test]
    fn keystrokes_reach_the_program() {
        let (terminal, _writer, recorder) = terminal();

        terminal.send(b"ls\r").expect("expected the write to land");

        assert_eq!(&*recorder.written.lock().unwrap(), b"ls\r");
    }

    /// One frame of the widget with a chord held, which is the whole path a keystroke takes:
    /// egui's event, the physical key it stands for, and the bytes the program is written.
    fn typed(terminal: &mut Terminal, key: egui::Key, modifiers: egui::Modifiers) {
        let ctx = egui::Context::default();
        // Keystrokes only reach a focused widget, and focus is taken on the frame after it is
        // asked for — so the chord goes in on the second one.
        terminal.request_focus();
        ctx.run_ui(egui::RawInput::default(), |ui| {
            terminal.ui(ui, &TerminalStyle::from_visuals(ui.visuals()));
        })
        .drop_without_applying_deltas();

        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers,
            }],
            ..Default::default()
        };
        ctx.run_ui(input, |ui| {
            terminal.ui(ui, &TerminalStyle::from_visuals(ui.visuals()));
        })
        .drop_without_applying_deltas();
    }

    /// ⌃← reaching a shell as `ESC [ 1 ; 5 D` is what typed a stray `;5D` into the prompt:
    /// readline binds `ESC b` and none of the other.
    #[test]
    fn a_shell_is_sent_the_word_motion_it_binds() {
        let (mut terminal, _writer, recorder) = terminal();

        typed(
            &mut terminal,
            egui::Key::ArrowLeft,
            egui::Modifiers {
                ctrl: true,
                ..Default::default()
            },
        );

        assert_eq!(&*recorder.written.lock().unwrap(), b"\x1bb");
    }

    /// And a program reading chords for itself is sent the real one.
    #[test]
    fn a_program_on_the_kitty_protocol_is_sent_the_chord() {
        let (mut terminal, writer, recorder) = terminal();
        writer
            .send(b"\x1b[>1u".to_vec())
            .expect("expected the terminal to be listening");
        terminal.poll();

        typed(
            &mut terminal,
            egui::Key::ArrowLeft,
            egui::Modifiers {
                ctrl: true,
                ..Default::default()
            },
        );

        let written = recorder.written.lock().unwrap().clone();
        assert_ne!(written, b"\x1bb", "the chord should not have been rewritten");
        assert!(
            String::from_utf8_lossy(&written).contains("[1;5"),
            "expected the chord itself, got {:?}",
            String::from_utf8_lossy(&written)
        );
    }

    #[test]
    fn a_closed_output_channel_means_the_program_has_gone() {
        let (mut terminal, writer, _) = terminal();
        assert!(!terminal.has_exited());

        drop(writer);
        terminal.poll();

        assert!(terminal.has_exited());
    }

    #[test]
    fn searching_finds_matches_in_the_scrollback() {
        let (mut terminal, writer, _) = terminal();
        let mut output = String::new();
        for index in 0..60 {
            output.push_str(&format!("line {index}\r\n"));
        }
        output.push_str("the needle\r\n");
        writer
            .send(output.into_bytes())
            .expect("expected the terminal to be listening");
        terminal.poll();

        assert_eq!(terminal.find("needle", 0), 1);
        assert_eq!(terminal.selected_text().as_deref(), Some("needle"));
        assert_eq!(terminal.find("line", 0), 60, "the scrollback counts too");
    }

    #[test]
    fn the_light_scheme_is_readable_where_the_dark_one_is_not() {
        let (mut terminal, _writer, _) = terminal();

        terminal.set_color_scheme(ColorScheme::Dark);
        let (dark_fg, dark_bg) = terminal.drawn_colors().expect("expected colors");
        terminal.set_color_scheme(ColorScheme::Light);
        let (light_fg, light_bg) = terminal.drawn_colors().expect("expected colors");

        assert!(dark_bg.r() < light_bg.r(), "the light background is paper");
        assert!(light_fg.r() < dark_fg.r(), "and its text is ink");
    }
}

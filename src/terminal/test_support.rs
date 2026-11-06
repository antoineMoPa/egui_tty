//! Helpers the terminal's tests share: a recording tty, and one keystroke encoded.

use super::*;
use std::sync::{Mutex, mpsc};

#[derive(Default)]
pub(super) struct Recorder(pub(super) Mutex<Vec<u8>>);

impl Tty for Recorder {
    fn write(&self, bytes: &[u8]) -> Result<()> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    }

    fn resize(&self, _cols: u16, _rows: u16) -> Result<()> {
        Ok(())
    }
}

/// What one keystroke sends to the program: the physical key, the modifiers, and the text
/// the platform produced for it, which is exactly what `handle_input` pairs up.
pub(super) fn encoded(key: key::Key, mods: key::Mods, text: Option<&str>) -> Vec<u8> {
    encoded_with(key, mods, text, b"")
}

/// The same, after the program has asked for something — the Kitty keyboard protocol,
/// say, which is a different encoding entirely.
pub(super) fn encoded_with(
    key: key::Key,
    mods: key::Mods,
    text: Option<&str>,
    asked_for: &[u8],
) -> Vec<u8> {
    let (writer, output) = mpsc::channel();
    let mut terminal = Terminal::new(TtyStream {
        output,
        tty: Arc::new(Recorder::default()) as Arc<dyn Tty>,
    })
    .expect("expected a terminal");
    if !asked_for.is_empty() {
        writer
            .send(asked_for.to_vec())
            .expect("expected a listener");
        terminal.poll();
    }
    drop(writer);

    terminal.configure_key_encoder();
    let mut out = Vec::new();
    terminal
        .encode_key(key, mods, text.map(str::to_string), false, &mut out)
        .expect("expected the keystroke to encode");
    out
}

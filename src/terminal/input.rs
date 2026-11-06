//! Keystrokes and text, turned into the bytes a program is sent.

use super::*;

/// The line editing chords a shell is sent, in place of the encoding a terminal would
/// otherwise give them.
///
/// ⌃← is `ESC [ 1 ; 5 D` in every terminal, and readline binds none of it: it takes the
/// `ESC [ 1` and types the rest, which is where a stray `;5D` in the prompt comes from. What
/// readline does bind is `ESC b`, so that is what it is sent — and the same for the rest of
/// the chords a line is edited with. A program that asked for the Kitty keyboard protocol
/// reads the chord itself and gets the real thing instead; see [`Terminal::reads_the_real_chord`].
pub(super) const LEGACY_EDITING_CHORDS: &[(key::Key, key::Mods, &[u8])] = &[
    // Word left and right, and the word behind the cursor.
    (key::Key::ArrowLeft, key::Mods::CTRL, b"\x1bb"),
    (key::Key::ArrowRight, key::Mods::CTRL, b"\x1bf"),
    (key::Key::Backspace, key::Mods::CTRL, b"\x17"),
    // The ends of the line, and the whole of it.
    (key::Key::ArrowLeft, key::Mods::SUPER, b"\x01"),
    (key::Key::ArrowRight, key::Mods::SUPER, b"\x05"),
    (key::Key::Backspace, key::Mods::SUPER, b"\x15"),
];

/// What a shell is sent for one of the editing chords, if this is one of them.
///
/// The modifiers have to match exactly: ⌃⇧← is a selection in whatever is reading it, not a
/// word motion someone held an extra key for.
pub(super) fn legacy_editing_chord(key: key::Key, mods: key::Mods) -> Option<&'static [u8]> {
    LEGACY_EDITING_CHORDS
        .iter()
        .find(|(chord, with, _)| *chord == key && *with == mods)
        .map(|(_, _, bytes)| *bytes)
}

/// Whether option typed the text that came with this key, so that the text is what the program
/// is sent rather than the key with Alt.
///
/// Only on macOS, where option is the key that types accents and symbols: option+shift+/ is ¿
/// and option+2 is ™, which a program would never see if they were sent as ESC followed by the
/// key. A letter stays the meta chord it is for a shell, where option+b is ESC b, the word
/// motion, even though macOS types ∫ for it. The accents themselves do not come through here at
/// all: option+e is a dead key, and the platform hands its composition over as input-method
/// events instead.
pub(super) fn option_types_the_text(
    key: key::Key,
    mods: key::Mods,
    text: Option<&str>,
    macos: bool,
) -> bool {
    macos
        && mods.contains(key::Mods::ALT)
        && !mods.intersects(key::Mods::CTRL | key::Mods::SUPER)
        && text.is_some_and(|text| !text.is_empty())
        && !keys::unshifted_codepoint(key).is_some_and(|letter| letter.is_ascii_alphabetic())
}

impl Terminal {
    /// Encode this frame's key and text events and send them to the program.
    ///
    /// libghostty decides what the bytes are — application cursor keys, the Kitty keyboard
    /// protocol, `modifyOtherKeys` — so all that happens here is pairing each physical key
    /// press with the text the platform produced for it.
    pub(super) fn handle_input(&mut self, ui: &Ui) {
        let events = ui.input(|input| input.events.clone());
        let mut pending_text: Vec<String> = events
            .iter()
            .filter_map(|event| match event {
                egui::Event::Text(text) => Some(text.clone()),
                _ => None,
            })
            .collect();
        let mut text_index = 0usize;

        self.configure_key_encoder();
        let mut encoded = Vec::new();

        for event in &events {
            let egui::Event::Key {
                key,
                pressed,
                repeat,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            let Some(vt) = keys::vt_key(*key) else {
                continue;
            };
            if keys::is_modifier_key(vt) {
                continue;
            }
            // Ghostty encodes releases only when the program asked for them, and egui does
            // not tell us that, so presses and repeats are what get sent.
            if !pressed {
                continue;
            }
            // Escape with something selected only clears the selection, and is not sent on.
            // With nothing selected it goes to the program, as vim and the rest expect.
            if vt == key::Key::Escape && selection::has_selection(&self.emulator) {
                report(selection::clear(&self.emulator));
                continue;
            }

            let mods = keys::vt_mods(*modifiers);
            if !self.reads_the_real_chord()
                && let Some(bytes) = legacy_editing_chord(vt, mods)
            {
                encoded.extend_from_slice(bytes);
                continue;
            }
            // A control or command combo is a command, not text: the platform's character
            // for it (or lack of one) must not override libghostty's encoding.
            let text = if mods.intersects(key::Mods::CTRL | key::Mods::SUPER) {
                None
            } else {
                let text = pending_text.get(text_index).cloned();
                if text.is_some() {
                    text_index += 1;
                }
                text
            };
            // Option typing a character, as on macOS, is the character and not a meta chord.
            let mods =
                if option_types_the_text(vt, mods, text.as_deref(), cfg!(target_os = "macos")) {
                    mods.difference(key::Mods::ALT)
                } else {
                    mods
                };
            // A key that types a character, with no character beside it: a browser's on-screen
            // keyboard hands the key and the text it typed over in separate frames. The text
            // is sent when it comes; encoding the bare key as well would type it twice.
            if text.is_none()
                && !mods.intersects(key::Mods::CTRL | key::Mods::ALT | key::Mods::SUPER)
                && keys::unshifted_codepoint(vt).is_some()
            {
                continue;
            }

            report(self.encode_key(vt, mods, text, *repeat, &mut encoded));
        }

        // Anything left is text with no key event behind it, a paste say.
        for text in pending_text.drain(text_index.min(pending_text.len())..) {
            encoded.extend_from_slice(text.as_bytes());
        }
        // What an input method composed, which it hands over whole once it is done: macOS
        // dictation writes what it has heard so far as a composition and commits it at the end.
        // Until then the composition is only drawn - the program sees the committed text.
        for event in &events {
            match event {
                egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                    self.composing.clone_from(text);
                }
                egui::Event::Ime(egui::ImeEvent::Commit(text)) => {
                    self.composing.clear();
                    encoded.extend_from_slice(text.as_bytes());
                }
                // An on-screen keyboard taking back what it typed: Android's backspace, and a
                // word it corrects, which it deletes before typing it again. The program has
                // been sent those characters already, so it is sent a backspace for each.
                egui::Event::Ime(egui::ImeEvent::DeleteSurrounding { before_chars, .. }) => {
                    encoded.extend(std::iter::repeat_n(BACKSPACE, *before_chars));
                }
                _ => {}
            }
        }

        if !encoded.is_empty() {
            self.send_to_program(&encoded);
        }
    }

    /// Send input to the program, and bring the bottom of the screen back into view.
    ///
    /// Input is aimed at the prompt, and the prompt is at the bottom: someone who scrolled up
    /// to read something and then typed would otherwise be typing out of sight.
    pub(super) fn send_to_program(&mut self, bytes: &[u8]) {
        self.emulator.scroll_viewport(ScrollViewport::Bottom);
        report(self.tty.write(bytes));
    }

    /// Point the key encoder at the program's current protocol: application cursor keys,
    /// the Kitty keyboard protocol and the rest are the program's to ask for.
    ///
    /// Option as the meta key is not, and libghostty forgets it each time the program's
    /// options are read, so it is set again after them. It is what a terminal a shell is
    /// worked in is set to: option+y is the yank-pop readline binds after a ctrl+k, rather
    /// than the yen sign macOS types for it. The cost is that option on a letter is never the
    /// character it types: option+c is not ç. Option on anything else still types its
    /// character (see `option_types_the_text`), and accents come in through input-method
    /// composition (see `ask_for_text_input`).
    pub(super) fn configure_key_encoder(&mut self) {
        self.encoder
            .set_options_from_terminal(&self.emulator)
            .set_macos_option_as_alt(key::OptionAsAlt::True);
    }

    /// Whether the running program asked for the Kitty keyboard protocol, and so reads the
    /// chord itself rather than an escape someone once chose to stand for it.
    pub(super) fn reads_the_real_chord(&self) -> bool {
        self.emulator
            .kitty_keyboard_flags()
            .is_ok_and(|flags| !flags.is_empty())
    }

    pub(super) fn encode_key(
        &mut self,
        vt: key::Key,
        mods: key::Mods,
        text: Option<String>,
        repeat: bool,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        let mut event = key::Event::new()
            .map_err(|error| Error::context("failed to create a key event", error))?;
        event.set_action(if repeat {
            Action::Repeat
        } else {
            Action::Press
        });
        event.set_key(vt);
        event.set_mods(mods);
        let unshifted = keys::unshifted_codepoint(vt);
        if let Some(codepoint) = unshifted {
            event.set_unshifted_codepoint(codepoint);
        }
        // Shift that went into making the character is not a modifier the program should be
        // told about — it is already in the text. Saying so is what the Kitty keyboard
        // protocol needs to send `?` rather than the `/` key with shift held.
        if keys::shift_was_consumed(mods, unshifted, text.as_deref()) {
            event.set_consumed_mods(key::Mods::SHIFT);
        }
        if let Some(text) = text {
            event.set_utf8(Some(text));
        }

        self.encoder
            .encode_to_vec(&event, out)
            .map_err(|error| Error::context("failed to encode a keystroke", error))
    }
}

/// What the backspace key sends: DEL, which is what a terminal's erase character is set to.
pub(super) const BACKSPACE: u8 = 0x7f;

/// Tell the platform this terminal takes text while it has the keyboard.
///
/// On a touch screen that is what brings the on-screen keyboard up. On a desktop it is what
/// turns the platform's input method on: macOS only composes dead keys (option+e, then e) and
/// hands over IME commits while the window allows an input method, and winit allows one only
/// while a widget asks for text. Without this, option+e types nothing at all. Keys that are not
/// composed still arrive as plain key and text events, as they did before.
pub(super) fn ask_for_text_input(ui: &Ui, response: &Response, cell: egui::Vec2) {
    // Where the text agent stands, which a browser scrolls into view above its keyboard: the
    // bottom line, where a prompt is.
    let cursor_rect = Rect::from_min_size(response.rect.left_bottom() - vec2(0.0, cell.y), cell);
    ui.output_mut(|output| {
        output.ime = Some(egui::output::IMEOutput {
            purpose: egui::IMEPurpose::Terminal,
            rect: response.rect,
            cursor_rect,
            should_interrupt_composition: false,
        });
    });
}

#[cfg(test)]
mod tests;

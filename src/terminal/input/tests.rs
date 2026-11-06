use std::sync::{Arc, mpsc};

use super::*;
use crate::terminal::test_support::{Recorder, encoded, encoded_with};

/// The `;5D` bug: readline binds none of `ESC [ 1 ; 5 D`, so ⌃← used to type the tail of
/// it into the prompt. What a shell is sent is the escape it already means by it.
#[test]
fn the_editing_chords_are_sent_as_what_a_shell_binds() {
    assert_eq!(
        legacy_editing_chord(key::Key::ArrowLeft, key::Mods::CTRL),
        Some(b"\x1bb".as_slice())
    );
    assert_eq!(
        legacy_editing_chord(key::Key::ArrowRight, key::Mods::CTRL),
        Some(b"\x1bf".as_slice())
    );
    assert_eq!(
        legacy_editing_chord(key::Key::ArrowLeft, key::Mods::SUPER),
        Some(b"\x01".as_slice())
    );
}

/// Only those chords, and only on their own: everything else is left to the encoder, which
/// is what knows about application cursor keys and the rest.
#[test]
fn every_other_key_is_left_to_the_encoder() {
    assert_eq!(
        legacy_editing_chord(key::Key::ArrowLeft, key::Mods::empty()),
        None
    );
    assert_eq!(
        legacy_editing_chord(key::Key::ArrowUp, key::Mods::CTRL),
        None
    );
    assert_eq!(legacy_editing_chord(key::Key::A, key::Mods::CTRL), None);
    // Shift held as well is a selection in whatever reads it, not a word motion.
    assert_eq!(
        legacy_editing_chord(key::Key::ArrowLeft, key::Mods::CTRL | key::Mods::SHIFT),
        None
    );
}

/// And a program that asked to read chords itself gets the real one — which is why ⌃←
/// already worked in claude while a shell was typing `;5D`.
#[test]
fn a_program_that_asked_for_the_kitty_protocol_reads_the_chord_itself() {
    let (writer, output) = mpsc::channel();
    let mut terminal = Terminal::new(TtyStream {
        output,
        tty: Arc::new(Recorder::default()) as Arc<dyn Tty>,
    })
    .expect("expected a terminal");

    assert!(
        !terminal.reads_the_real_chord(),
        "a shell asks for nothing, and is sent what it binds"
    );

    writer
        .send(b"\x1b[>1u".to_vec())
        .expect("expected a listener");
    terminal.poll();

    assert!(
        terminal.reads_the_real_chord(),
        "a program that turned the protocol on should be sent the chord"
    );
}

#[test]
fn a_plain_letter_is_sent_as_itself() {
    assert_eq!(encoded(key::Key::A, key::Mods::empty(), Some("a")), b"a");
}

/// Option is the meta key. macOS types a yen sign for option+y, and a shell given that
/// would insert it; what a shell binds is ESC y, readline's yank-pop.
#[test]
fn an_option_letter_is_sent_as_the_meta_chord_rather_than_the_symbol_it_types() {
    assert_eq!(
        encoded(key::Key::Y, key::Mods::ALT, Some("\u{a5}")),
        b"\x1by"
    );
    assert_eq!(
        encoded(key::Key::B, key::Mods::ALT, Some("\u{222b}")),
        b"\x1bb"
    );
}

#[test]
fn a_shifted_letter_is_sent_as_the_capital() {
    assert_eq!(encoded(key::Key::A, key::Mods::SHIFT, Some("A")), b"A");
}

/// Shift and the slash key is a question mark, and a program that reads `/` instead of `?`
/// puts the user in a menu they did not ask for.
#[test]
fn shifted_punctuation_is_sent_as_the_character_it_types() {
    assert_eq!(encoded(key::Key::Slash, key::Mods::SHIFT, Some("?")), b"?");
}

/// What claude and friends turn on. The protocol reports the key that was pressed rather
/// than the character it typed, so a key whose character came out of shift has to say so —
/// otherwise `?` arrives as "the `/` key, with shift", and a program reads `/`.
#[test]
fn the_kitty_protocol_still_sends_the_character_that_was_typed() {
    let bytes = encoded_with(key::Key::Slash, key::Mods::SHIFT, Some("?"), b"\x1b[>1u");

    assert_eq!(
        String::from_utf8_lossy(&bytes),
        "?",
        "a question mark should reach the program as a question mark"
    );
}

/// Every character that only exists with shift held goes the same way as `?`: the digits
/// row is where most of them live.
#[test]
fn the_shifted_characters_of_the_number_row_survive_the_kitty_protocol() {
    for (key, unshifted, typed) in [
        (key::Key::Digit9, '9', "("),
        (key::Key::Digit0, '0', ")"),
        (key::Key::Digit1, '1', "!"),
        (key::Key::Digit8, '8', "*"),
        (key::Key::Minus, '-', "_"),
        (key::Key::Equal, '=', "+"),
    ] {
        let _ = unshifted;
        let bytes = encoded_with(key, key::Mods::SHIFT, Some(typed), b"\x1b[>1u");

        assert_eq!(
            String::from_utf8_lossy(&bytes),
            typed,
            "{typed} should reach the program as itself"
        );
    }
}

/// The same protocol still has to report shift when it did not make the character, or a
/// program loses shift-tab and friends.
#[test]
fn the_kitty_protocol_still_reports_shift_when_it_typed_nothing() {
    let bytes = encoded_with(key::Key::Tab, key::Mods::SHIFT, None, b"\x1b[>1u");

    let text = String::from_utf8_lossy(&bytes).to_string();
    assert!(
        text.contains(";2"),
        "shift should still be reported: {text:?}"
    );
}

#[test]
fn a_control_combo_is_a_command_rather_than_text() {
    assert_eq!(encoded(key::Key::C, key::Mods::CTRL, None), b"\x03");
}

/// Option types the character a key stands for on macOS, so the text is what goes to the
/// program, and not the key with Alt beside it.
#[test]
fn option_types_the_text_of_a_symbol_key_on_macos() {
    assert!(option_types_the_text(
        key::Key::Slash,
        key::Mods::ALT | key::Mods::SHIFT,
        Some("\u{bf}"),
        true,
    ));
    assert!(option_types_the_text(
        key::Key::Digit2,
        key::Mods::ALT,
        Some("\u{2122}"),
        true,
    ));
}

/// A letter with option is still the meta chord: option+b is the word motion, and the ∫ macOS
/// would type for it is not sent.
#[test]
fn option_stays_meta_for_a_letter() {
    assert!(!option_types_the_text(
        key::Key::B,
        key::Mods::ALT,
        Some("\u{222b}"),
        true,
    ));
}

/// Off macOS option is never the character: Alt+1 on Linux is the meta chord it was.
#[test]
fn option_is_meta_everywhere_but_macos() {
    assert!(!option_types_the_text(
        key::Key::Slash,
        key::Mods::ALT | key::Mods::SHIFT,
        Some("?"),
        false,
    ));
}

/// With control or command held the key is a command, and the text is not what it types.
#[test]
fn option_with_a_command_modifier_is_not_typing() {
    assert!(!option_types_the_text(
        key::Key::Slash,
        key::Mods::ALT | key::Mods::CTRL,
        Some("\u{bf}"),
        true,
    ));
}

/// An option chord with no text beside it, like option+left, is sent as the chord it is.
#[test]
fn an_option_chord_with_no_text_is_not_typing() {
    assert!(!option_types_the_text(
        key::Key::ArrowLeft,
        key::Mods::ALT,
        None,
        true,
    ));
}

/// A composed character reaches the program as its UTF-8 bytes, with no Alt in front of it.
#[test]
fn a_composed_character_is_sent_as_its_utf8() {
    assert_eq!(
        encoded(key::Key::Slash, key::Mods::SHIFT, Some("\u{bf}")),
        "\u{bf}".as_bytes()
    );
}

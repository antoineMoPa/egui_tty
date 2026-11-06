use std::sync::{Arc, mpsc};

use super::*;
use crate::terminal::test_support::Recorder;

/// A terminal showing one printed URL, and where its cells landed on screen: the widget
/// is drawn once so the grid has a position to point at.
struct Clicking {
    /// The write end of the program's output, held so the terminal does not report the
    /// program as gone mid-test.
    _writer: mpsc::Sender<Vec<u8>>,
    /// Everything the terminal sent to the program.
    sent: Arc<Recorder>,
    ctx: egui::Context,
    terminal: Terminal,
    style: TerminalStyle,
    origin: egui::Pos2,
    cell: egui::Vec2,
    /// The widget's clock, in seconds, which is what tells a double-click from two
    /// clicks. Each frame is a sixtieth of a second unless a test waits longer.
    clock: f64,
    /// Whether the last frame told the platform the terminal takes text, which is what
    /// brings a touch screen's keyboard up.
    took_text: bool,
}

impl Clicking {
    fn at(shown: &str) -> Self {
        let (writer, output) = mpsc::channel();
        let sent = Arc::new(Recorder::default());
        let terminal = Terminal::new(TtyStream {
            output,
            tty: sent.clone() as Arc<dyn Tty>,
        })
        .expect("expected a terminal");
        writer
            .send(shown.as_bytes().to_vec())
            .expect("expected a listener");
        let mut clicking = Self {
            _writer: writer,
            sent,
            ctx: egui::Context::default(),
            terminal,
            style: TerminalStyle::default(),
            origin: egui::Pos2::ZERO,
            cell: vec2(1.0, 1.0),
            clock: 0.0,
            took_text: false,
        };
        clicking.frame(Vec::new());
        clicking
    }

    /// Let this much time pass before the next frame.
    fn wait(&mut self, seconds: f64) {
        self.clock += seconds;
    }

    /// One frame of the widget filling the window, with `events` as its input, reporting
    /// whichever URL the frame asked the host to open.
    fn frame(&mut self, events: Vec<egui::Event>) -> Option<String> {
        self.frame_with(events, egui::Modifiers::NONE)
    }

    /// The same frame with shift held throughout it.
    fn frame_shifted(&mut self, events: Vec<egui::Event>) -> Option<String> {
        self.frame_with(shifted(events), egui::Modifiers::SHIFT)
    }

    fn frame_with(
        &mut self,
        events: Vec<egui::Event>,
        modifiers: egui::Modifiers,
    ) -> Option<String> {
        self.clock += 1.0 / 60.0;
        // egui learns which modifiers are down from this event, not from the frame.
        let events = std::iter::once(egui::Event::ModifiersChanged(modifiers))
            .chain(events)
            .collect();
        let Self {
            ctx,
            terminal,
            style,
            origin,
            cell,
            clock,
            ..
        } = self;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(800.0, 600.0))),
            events,
            time: Some(*clock),
            ..Default::default()
        };

        let output = ctx.run_ui(input, |ui| {
            let response = terminal.ui(ui, style);
            *cell = cell_size(ui.painter(), &style.font);
            *origin = response.rect.min + vec2(style.padding, style.padding);
        });
        let opened = output
            .platform_output
            .commands
            .iter()
            .find_map(|command| match command {
                egui::OutputCommand::OpenUrl(url) => Some(url.url.clone()),
                _ => None,
            });
        self.took_text = output.platform_output.ime.is_some();
        output.drop_without_applying_deltas();
        opened
    }

    /// The middle of a cell of the grid, which is where a pointer over it would be.
    fn over(&self, x: u16, y: u16) -> egui::Pos2 {
        self.origin
            + vec2(
                (f32::from(x) + 0.5) * self.cell.x,
                (f32::from(y) + 0.5) * self.cell.y,
            )
    }
}

fn moved_to(at: egui::Pos2) -> egui::Event {
    egui::Event::PointerMoved(at)
}

fn button(at: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

/// The same events with shift held on each button press and release.
fn shifted(events: Vec<egui::Event>) -> Vec<egui::Event> {
    events
        .into_iter()
        .map(|event| match event {
            egui::Event::PointerButton {
                pos,
                button,
                pressed,
                ..
            } => egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: egui::Modifiers::SHIFT,
            },
            other => other,
        })
        .collect()
}

/// The whole gesture: a press and a release on a printed URL asks the host to open it.
/// The press starts a selection gesture, which is what used to clear the hovered link
/// before the release could read it — so a click opened nothing at all.
#[test]
fn a_click_on_a_url_opens_it() {
    let mut clicking = Clicking::at("see https://example.com/docs now");
    let at = clicking.over(10, 0);

    assert_eq!(clicking.frame(vec![moved_to(at), button(at, true)]), None);
    assert_eq!(
        clicking.frame(vec![button(at, false)]).as_deref(),
        Some("https://example.com/docs")
    );
}

/// Select some, let go, scroll if need be, then shift-click further on: the selection
/// runs from where it started to the shift-click, the way it does in every terminal.
#[test]
fn a_shift_click_carries_the_selection_on_from_where_it_started() {
    let mut clicking = Clicking::at("hello world again");
    let from = clicking.over(0, 0);
    let to = clicking.over(4, 0);
    clicking.frame(vec![moved_to(from), button(from, true)]);
    clicking.frame(vec![moved_to(to)]);
    clicking.frame(vec![button(to, false)]);
    assert_eq!(
        selection::selected_text(&clicking.terminal.emulator).as_deref(),
        Some("hello")
    );

    clicking.wait(2.0);
    let further = clicking.over(10, 0);
    clicking.frame_shifted(vec![moved_to(further), button(further, true)]);
    clicking.frame_shifted(vec![button(further, false)]);

    assert_eq!(
        selection::selected_text(&clicking.terminal.emulator).as_deref(),
        Some("hello world"),
        "the selection should run on to the shift-click"
    );
}

/// Without shift, a click after a selection starts over — and clears it.
#[test]
fn a_plain_click_after_a_selection_starts_over() {
    let mut clicking = Clicking::at("hello world again");
    let from = clicking.over(0, 0);
    let to = clicking.over(4, 0);
    clicking.frame(vec![moved_to(from), button(from, true)]);
    clicking.frame(vec![moved_to(to)]);
    clicking.frame(vec![button(to, false)]);

    clicking.wait(2.0);
    let further = clicking.over(10, 0);
    clicking.frame(vec![moved_to(further), button(further, true)]);
    clicking.frame(vec![button(further, false)]);

    assert_eq!(selection::selected_text(&clicking.terminal.emulator), None);
}

/// Escape clears a selection and is kept from the program; once nothing is selected it is
/// sent on as usual, which is what vim and the rest of the terminal programs rely on.
#[test]
fn escape_clears_a_selection_then_reaches_the_program() {
    let mut clicking = Clicking::at("hello world again");
    let from = clicking.over(0, 0);
    let to = clicking.over(4, 0);
    clicking.frame(vec![moved_to(from), button(from, true)]);
    clicking.frame(vec![moved_to(to)]);
    clicking.frame(vec![button(to, false)]);
    assert_eq!(
        selection::selected_text(&clicking.terminal.emulator).as_deref(),
        Some("hello")
    );

    let escape = vec![egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }];
    clicking.frame(escape.clone());
    assert_eq!(selection::selected_text(&clicking.terminal.emulator), None);
    assert!(
        clicking.sent.0.lock().unwrap().is_empty(),
        "the escape that cleared the selection was not sent on"
    );

    clicking.frame(escape);
    assert_eq!(
        clicking.sent.0.lock().unwrap().as_slice(),
        b"\x1b",
        "with nothing selected, escape goes to the program"
    );
}

/// A drag over the same URL is a selection, and selecting text must not open anything.
#[test]
fn a_drag_across_a_url_selects_rather_than_opening_it() {
    let mut clicking = Clicking::at("see https://example.com/docs now");
    let from = clicking.over(6, 0);
    let to = clicking.over(20, 0);

    clicking.frame(vec![moved_to(from), button(from, true)]);
    clicking.frame(vec![moved_to(to)]);

    assert_eq!(
        clicking.frame(vec![button(to, false)]),
        None,
        "a selection should not have opened a url"
    );
}

/// A letter typed with the keyboard in the terminal, as the platform reports one.
fn typed(key: egui::Key, text: &str) -> Vec<egui::Event> {
    vec![
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        },
        egui::Event::Text(text.to_string()),
    ]
}

/// Scrolled up to read something, then typing: the prompt is at the bottom, so that is
/// where the keystroke takes the view.
#[test]
fn typing_brings_the_view_back_to_the_bottom() {
    let lines: String = (0..200).map(|n| format!("line {n}\r\n")).collect();
    let mut clicking = Clicking::at(&lines);
    let here = clicking.over(0, 0);
    clicking.frame(vec![moved_to(here), button(here, true)]);
    clicking.frame(vec![button(here, false)]);

    clicking
        .terminal
        .emulator
        .scroll_viewport(ScrollViewport::Delta(-100));
    let shown = clicking
        .terminal
        .visible_text()
        .expect("expected the screen");
    assert!(
        !shown.contains("line 199"),
        "the view should have been scrolled up into the scrollback, showed {shown:?}"
    );

    clicking.frame(typed(egui::Key::A, "a"));

    let shown = clicking
        .terminal
        .visible_text()
        .expect("expected the screen");
    assert!(
        shown.contains("line 199"),
        "typing should have brought the bottom back into view, showed {shown:?}"
    );
}

/// macOS dictation composes what it has heard so far and commits it once it is done, with
/// no key pressed: the composition is only shown, and the committed text is what the
/// program is sent.
#[test]
fn a_composition_is_shown_and_its_commit_sent_to_the_program() {
    let mut clicking = Clicking::at("$ ");
    let here = clicking.over(0, 0);
    clicking.frame(vec![moved_to(here), button(here, true)]);
    clicking.frame(vec![button(here, false)]);

    clicking.frame(vec![egui::Event::Ime(egui::ImeEvent::Preedit {
        text: "hello wor".to_string(),
        active_range_chars: None,
    })]);
    assert_eq!(clicking.terminal.composing, "hello wor");
    assert!(
        clicking.sent.0.lock().unwrap().is_empty(),
        "a composition should not reach the program before it is committed"
    );

    clicking.frame(vec![egui::Event::Ime(egui::ImeEvent::Commit(
        "hello world".to_string(),
    ))]);
    assert_eq!(clicking.terminal.composing, "");
    assert_eq!(
        String::from_utf8_lossy(&clicking.sent.0.lock().unwrap()),
        "hello world"
    );
}

/// An on-screen keyboard in a browser hands over the key and the letter it typed in
/// separate frames: the letter reaches the program once.
#[test]
fn a_key_and_its_letter_in_separate_frames_type_it_once() {
    let mut clicking = Clicking::at("$ ");
    let here = clicking.over(0, 0);
    clicking.frame(vec![moved_to(here), button(here, true)]);
    clicking.frame(vec![button(here, false)]);

    clicking.frame(vec![egui::Event::Key {
        key: egui::Key::A,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }]);
    clicking.frame(vec![egui::Event::Text("a".to_string())]);
    assert_eq!(*clicking.sent.0.lock().unwrap(), b"a");
}

/// Option+e is a dead key: macOS composes nothing until the next letter, and the accent it is
/// holding is shown as a composition. Then e commits the accented letter, which is what the
/// program is sent, with no escape or stray e in front of it.
#[test]
fn option_e_then_e_types_an_accented_e_through_the_input_method() {
    let mut clicking = Clicking::at("$ ");
    let here = clicking.over(0, 0);
    clicking.frame(vec![moved_to(here), button(here, true)]);
    clicking.frame(vec![button(here, false)]);

    clicking.frame(vec![egui::Event::Ime(egui::ImeEvent::Preedit {
        text: "\u{b4}".to_string(),
        active_range_chars: None,
    })]);
    assert!(
        clicking.sent.0.lock().unwrap().is_empty(),
        "a dead key should not send anything while it is held"
    );

    clicking.frame(vec![egui::Event::Ime(egui::ImeEvent::Commit(
        "\u{e9}".to_string(),
    ))]);
    assert_eq!(
        String::from_utf8_lossy(&clicking.sent.0.lock().unwrap()),
        "\u{e9}",
        "option+e then e should type é and nothing else"
    );
}

/// Option+shift+/ types an inverted question mark on macOS, and the program is sent that
/// character rather than an escape for the key.
#[cfg(target_os = "macos")]
#[test]
fn option_shift_slash_types_the_inverted_question_mark() {
    let mut clicking = Clicking::at("$ ");
    let here = clicking.over(0, 0);
    clicking.frame(vec![moved_to(here), button(here, true)]);
    clicking.frame(vec![button(here, false)]);

    let option_shift = egui::Modifiers {
        alt: true,
        shift: true,
        ..Default::default()
    };
    clicking.frame(vec![
        egui::Event::Key {
            key: egui::Key::Slash,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: option_shift,
        },
        egui::Event::Text("\u{bf}".to_string()),
    ]);
    assert_eq!(
        String::from_utf8_lossy(&clicking.sent.0.lock().unwrap()),
        "\u{bf}",
        "option+shift+/ should type ¿ and not an escape"
    );
}

/// Option on a letter is the meta chord a shell binds, even though macOS types a symbol for it:
/// option+b is ESC b, whatever text came with it.
#[cfg(target_os = "macos")]
#[test]
fn option_b_is_still_the_word_motion_a_shell_is_sent() {
    let mut clicking = Clicking::at("$ ");
    let here = clicking.over(0, 0);
    clicking.frame(vec![moved_to(here), button(here, true)]);
    clicking.frame(vec![button(here, false)]);

    let option = egui::Modifiers {
        alt: true,
        ..Default::default()
    };
    clicking.frame(vec![
        egui::Event::Key {
            key: egui::Key::B,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: option,
        },
        egui::Event::Text("\u{222b}".to_string()),
    ]);
    assert_eq!(*clicking.sent.0.lock().unwrap(), b"\x1bb");
}

/// A finger on the screen, as the platform reports it beside the press it stands for.
fn touch(at: egui::Pos2, phase: egui::TouchPhase) -> egui::Event {
    egui::Event::Touch {
        device_id: egui::TouchDeviceId(0),
        id: egui::TouchId(0),
        phase,
        pos: at,
        force: None,
    }
}

/// A terminal asks the platform for text while it has the keyboard, so that the input method is
/// on (dead keys compose there), and a touch screen gets its keyboard up. One that does not
/// have the keyboard asks for nothing.
#[test]
fn a_focused_terminal_asks_for_text_input_and_an_unfocused_one_does_not() {
    let mut unfocused = Clicking::at("$ ");
    unfocused.frame(vec![]);
    assert!(
        !unfocused.took_text,
        "a terminal nobody has clicked into should not ask for text"
    );

    let mut clicked = Clicking::at("$ ");
    let here = clicked.over(0, 0);
    clicked.frame(vec![moved_to(here), button(here, true)]);
    clicked.frame(vec![button(here, false)]);
    assert!(
        clicked.took_text,
        "a terminal clicked into with a mouse should ask for text, or dead keys never compose"
    );

    let mut tapped = Clicking::at("$ ");
    let here = tapped.over(0, 0);
    tapped.frame(vec![
        touch(here, egui::TouchPhase::Start),
        moved_to(here),
        button(here, true),
    ]);
    tapped.frame(vec![
        touch(here, egui::TouchPhase::End),
        button(here, false),
    ]);
    assert!(
        tapped.took_text,
        "a terminal tapped on a touch screen should ask for its keyboard"
    );
}

/// A finger carried down the terminal scrolls back into what went past, the way a wheel
/// does, rather than selecting the lines it crosses.
#[test]
fn a_finger_carried_down_the_terminal_scrolls_rather_than_selects() {
    let lines: String = (0..200).map(|at| format!("line {at}\r\n")).collect();
    let mut clicking = Clicking::at(&lines);
    let from = clicking.over(2, 2);
    let to = clicking.over(2, 20);
    let touch_and_button = |at, phase, pressed: Option<bool>| {
        let mut events = vec![touch(at, phase), moved_to(at)];
        events.extend(pressed.map(|pressed| button(at, pressed)));
        events
    };
    clicking.frame(touch_and_button(from, egui::TouchPhase::Start, Some(true)));
    for step in 1..=6 {
        let at = from + (to - from) * (step as f32 / 6.0);
        clicking.frame(touch_and_button(at, egui::TouchPhase::Move, None));
    }
    clicking.frame(touch_and_button(to, egui::TouchPhase::End, Some(false)));

    assert_eq!(
        clicking.terminal.selected_text(),
        None,
        "nothing was selected"
    );
    let shown = clicking
        .terminal
        .visible_text()
        .expect("expected the screen");
    assert!(
        !shown.contains("line 199"),
        "the terminal should have scrolled back from the bottom, showed {shown:?}"
    );
}

/// Android's keyboard sends no backspace key: it takes back what it typed, which the
/// program has already been sent, so the program is sent a backspace for each character.
#[test]
fn an_on_screen_keyboard_taking_back_what_it_typed_is_backspaces() {
    let mut clicking = Clicking::at("$ ");
    let here = clicking.over(0, 0);
    clicking.frame(vec![moved_to(here), button(here, true)]);
    clicking.frame(vec![button(here, false)]);

    clicking.frame(vec![egui::Event::Ime(egui::ImeEvent::DeleteSurrounding {
        before_chars: 2,
        after_chars: 0,
    })]);
    assert_eq!(*clicking.sent.0.lock().unwrap(), b"\x7f\x7f");
}

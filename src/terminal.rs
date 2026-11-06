//! The widget: a grid of cells drawn from the emulator's screen, and window events turned
//! back into terminal events.

use std::{
    sync::{Arc, mpsc},
    time::Duration,
};

use egui::{
    Align2, Color32, CornerRadius, FontId, Rect, Response, Sense, Stroke, TextFormat, Ui,
    text::LayoutJob, vec2,
};
use libghostty_vt::{
    Terminal as Emulator, TerminalOptions,
    key::{self, Action},
    mouse,
    render::{CellIterator, CursorVisualStyle, Dirty, RenderState, RowIterator},
    screen::Screen,
    style::RgbColor,
    terminal::{Mode, ScrollViewport},
};

use crate::{
    ColorScheme, Error, Result, TerminalStyle, Tty, TtyStream, colors, keys, links, search,
    selection,
};

mod draw;
mod input;
mod pointer;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use draw::cell_size;

use draw::color_of;
use input::ask_for_text_input;

/// Lines of scrollback a terminal keeps by default.
pub const DEFAULT_SCROLLBACK: usize = 10_000;

/// How often a terminal with a live program asks to be drawn again, so output that arrives
/// while nothing else is happening still shows up promptly.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(33);
/// A blinking cursor needs finer steps than the poll interval to blink evenly.
const BLINK_POLL_INTERVAL: Duration = Duration::from_millis(16);

/// The narrowest and shortest grid a program is ever given. A widget squeezed below this
/// reports these instead, rather than a grid with no cells in it.
const MIN_COLS: u16 = 20;
const MIN_ROWS: u16 = 4;

/// A terminal: an emulator, the program attached to it, and the grid both end up drawn as.
///
/// Bytes the program writes are read from the [`TtyStream`] it was built with, on whichever
/// thread draws the widget. The emulator is `!Send`, so the terminal lives on that thread for
/// good — which is where a UI wants it anyway.
///
/// ```no_run
/// # fn demo(ui: &mut egui::Ui, stream: egui_tty::TtyStream) -> egui_tty::Result<()> {
/// let mut terminal = egui_tty::Terminal::new(stream)?;
/// let style = egui_tty::TerminalStyle::from_visuals(ui.visuals());
/// terminal.ui(ui, &style);
/// # Ok(())
/// # }
/// ```
pub struct Terminal {
    emulator: Emulator<'static, 'static>,
    render_state: RenderState<'static>,
    row_iterator: RowIterator<'static>,
    cell_iterator: CellIterator<'static>,
    encoder: key::Encoder<'static>,
    output: mpsc::Receiver<Vec<u8>>,
    tty: Arc<dyn Tty>,
    cols: u16,
    rows: u16,
    /// What the exit notice calls this terminal, if the host gave it a name.
    label: Option<String>,
    poll_interval: Duration,
    /// Whether the output channel wakes the UI itself yet — see `wake_on_output`.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    wakes_on_output: bool,
    /// Set once the program is gone, so the widget says so instead of looking merely idle.
    exited: bool,
    /// Set by [`Terminal::request_focus`], and cleared by the frame that acts on it.
    pending_focus: bool,
    /// Cursor blink phase, in seconds since the widget was first drawn.
    blink_clock: f32,
    /// How much of a line the wheel has asked for and not yet been given. A terminal scrolls
    /// in whole lines and a trackpad scrolls in pixels, so this is what is left over.
    scroll_remainder: f32,
    /// Ghostty's pointer-to-selection state machine, driven by this widget's mouse events.
    pointer: selection::Pointer,
    /// Encodes pointer events for a program that asked for mouse reporting, in whichever
    /// protocol it asked for.
    mouse_encoder: mouse::Encoder<'static>,
    /// The one mouse event, reused for every encode.
    mouse_event: mouse::Event<'static>,
    /// The button a forwarded press put down and its release will lift, so the motion in
    /// between is reported as a drag of that button.
    mouse_button_down: Option<mouse::Button>,
    /// The URL under the pointer, found afresh each frame it moves. Held so the row it is on
    /// can be drawn underlined without looking it up a second time.
    hovered_link: Option<links::Link>,
    /// The link the pointer was over when the button went down, kept until the button comes
    /// back up. `hovered_link` is cleared for as long as a gesture is in progress — the press
    /// starts one — so the release has nothing left to open without this.
    pressed_link: Option<links::Link>,
    /// The scheme the emulator's colors were last set for. Setting them is a whole palette's
    /// worth of work, so it happens when the scheme changes rather than every frame.
    colored_for: Option<ColorScheme>,
    /// The colors the emulator arrived with, read before a scheme was first applied. The dark
    /// scheme is these, put back explicitly — asking the emulator to reset to its own
    /// defaults does not restore them.
    emulator_colors: Option<colors::Scheme>,
    /// What an input method is composing and has not committed yet - macOS dictation's words
    /// so far. The program is sent none of it until the commit; it is drawn at the cursor
    /// meanwhile, so the person sees what is being heard.
    composing: String,
}

impl Terminal {
    /// A terminal attached to a program, with [`DEFAULT_SCROLLBACK`] lines of scrollback.
    pub fn new(stream: TtyStream) -> Result<Self> {
        Self::with_scrollback(stream, DEFAULT_SCROLLBACK)
    }

    /// The same, keeping however many lines of scrollback you ask for.
    pub fn with_scrollback(stream: TtyStream, max_scrollback: usize) -> Result<Self> {
        let TtyStream { output, tty } = stream;
        let cols = 80;
        let rows = 24;
        let mut emulator = Emulator::new(TerminalOptions {
            cols,
            rows,
            max_scrollback,
        })
        .map_err(|error| Error::context("failed to create a terminal", error))?;

        // Programs probe the terminal on startup and wait for the answers, so the replies
        // have to go back to the program or the widget would look hung.
        let reply_to = Arc::clone(&tty);
        emulator
            .on_pty_write(move |_emulator, data| {
                let _ = reply_to.reply(data);
            })
            .map_err(|error| Error::context("failed to install the terminal reply hook", error))?;

        Ok(Self {
            emulator,
            render_state: RenderState::new()
                .map_err(|error| Error::context("failed to create a render state", error))?,
            row_iterator: RowIterator::new()
                .map_err(|error| Error::context("failed to create a row iterator", error))?,
            cell_iterator: CellIterator::new()
                .map_err(|error| Error::context("failed to create a cell iterator", error))?,
            encoder: key::Encoder::new()
                .map_err(|error| Error::context("failed to create a key encoder", error))?,
            output,
            tty,
            cols,
            rows,
            label: None,
            poll_interval: DEFAULT_POLL_INTERVAL,
            wakes_on_output: false,
            exited: false,
            pending_focus: false,
            blink_clock: 0.0,
            scroll_remainder: 0.0,
            pointer: selection::Pointer::new()?,
            mouse_encoder: mouse::Encoder::new()
                .map_err(|error| Error::context("failed to create a mouse encoder", error))?,
            mouse_event: mouse::Event::new()
                .map_err(|error| Error::context("failed to create a mouse event", error))?,
            mouse_button_down: None,
            hovered_link: None,
            pressed_link: None,
            colored_for: None,
            emulator_colors: None,
            composing: String::new(),
        })
    }

    /// Name this terminal, which is what its "exited" notice ends up saying.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// How often a terminal with a live program asks to be drawn again. The default is 33ms,
    /// which is fast enough to feel immediate and slow enough to leave the CPU alone.
    #[must_use]
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Take the keyboard on the next frame, the way a click into the grid would.
    pub fn request_focus(&mut self) {
        self.pending_focus = true;
    }

    /// A persistent failure of this terminal's connection, if it needs reconnecting.
    pub fn connection_error(&self) -> Option<String> {
        self.tty.connection_error()
    }

    /// Whether the program behind this terminal has ended.
    pub fn has_exited(&self) -> bool {
        self.exited
    }

    /// The size of the grid the program is talking to, in cells.
    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// The title the running program set, for a tab strip or a window title.
    pub fn title(&self) -> Option<String> {
        self.emulator
            .title()
            .ok()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(ToOwned::to_owned)
    }

    /// Send bytes to the program as if they had been typed.
    pub fn send(&self, bytes: &[u8]) -> Result<()> {
        self.tty.write(bytes)
    }

    /// What a copy would put on the clipboard right now.
    pub fn selected_text(&self) -> Option<String> {
        selection::selected_text(&self.emulator)
    }

    /// Search the screen and the scrollback, show the `at`th match, and say how many there
    /// were.
    ///
    /// Call this when a query or the current match changes rather than every frame: it reads
    /// the whole scrollback back as text.
    pub fn find(&mut self, query: &str, at: usize) -> usize {
        let found = search::find_all(&self.emulator, query);
        match found.get(at) {
            Some(found) => report(search::show(&mut self.emulator, *found, self.rows)),
            // Nothing to show, and nothing left highlighted from the last query either.
            None => report(
                self.emulator
                    .set_selection(None)
                    .map(|_| ())
                    .map_err(|error| Error::context("failed to clear the selection", error)),
            ),
        }
        found.len()
    }

    /// Everything on screen, one line per row and trailing blanks trimmed.
    ///
    /// This reads the emulator's own grid, which is what the widget paints from, so it is how
    /// a test checks that a program's output made it through the VT parser.
    pub fn visible_text(&mut self) -> Result<String> {
        let Self {
            emulator,
            render_state,
            row_iterator,
            cell_iterator,
            ..
        } = self;

        let snapshot = render_state
            .update(emulator)
            .map_err(|error| Error::context("failed to read the screen", error))?;
        let mut rows = row_iterator
            .update(&snapshot)
            .map_err(|error| Error::context("failed to walk the rows", error))?;

        let mut out = String::new();
        let mut cell_text = String::new();
        while let Some(row) = rows.next() {
            let mut cells = cell_iterator
                .update(row)
                .map_err(|error| Error::context("failed to walk the cells", error))?;
            let mut line = String::new();
            while let Some(view) = cells.next() {
                cell_text.clear();
                if view.graphemes_len().unwrap_or(0) > 0 {
                    let _ = view.graphemes_utf8(&mut cell_text);
                }
                line.push_str(if cell_text.is_empty() {
                    " "
                } else {
                    &cell_text
                });
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }

        Ok(out)
    }

    /// The default foreground and background the grid would be painted with right now.
    pub fn drawn_colors(&mut self) -> Result<(Color32, Color32)> {
        let Self {
            emulator,
            render_state,
            ..
        } = self;
        let snapshot = render_state
            .update(emulator)
            .map_err(|error| Error::context("failed to read the screen", error))?;
        let colors = snapshot
            .colors()
            .map_err(|error| Error::context("failed to read the colors", error))?;
        Ok((color_of(colors.foreground), color_of(colors.background)))
    }

    /// Put a scheme's colors on the emulator. Drawing does this for you; call it directly to
    /// set the colors of a terminal that is not on screen yet.
    pub fn set_color_scheme(&mut self, scheme: ColorScheme) {
        if self.colored_for == Some(scheme) {
            return;
        }
        // The first call is also when the emulator's own colors are worth reading: nothing
        // has overwritten them yet, and the dark scheme is made of them.
        if self.emulator_colors.is_none() {
            let Self {
                emulator,
                render_state,
                ..
            } = self;
            match render_state
                .update(emulator)
                .and_then(|snapshot| snapshot.colors())
            {
                Ok(colors) => self.emulator_colors = Some(colors::emulator_scheme(&colors)),
                Err(error) => {
                    report(Err(Error::context(
                        "failed to read the terminal's own colors",
                        error,
                    )));
                    return;
                }
            }
        }

        let Some(emulator) = self.emulator_colors.clone() else {
            return;
        };
        report(colors::apply(&mut self.emulator, &emulator, scheme));
        self.colored_for = Some(scheme);
    }

    /// Feed everything the program has written since the last call into the emulator, without
    /// drawing. Returns whether anything arrived.
    ///
    /// Drawing does this first, so a UI never has to; a test that waits on output does.
    pub fn poll(&mut self) -> bool {
        let mut received = false;
        loop {
            match self.output.try_recv() {
                Ok(chunk) => {
                    self.emulator.vt_write(&chunk);
                    received = true;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.exited = true;
                    break;
                }
            }
        }
        // A local program's output channel can stay open after the program is gone, when what
        // holds the session alive is this terminal, so the handle is asked directly.
        if self.tty.has_exited() {
            self.exited = true;
        }
        received
    }

    /// Wake the UI the moment the program writes, instead of on the widget's next poll tick.
    ///
    /// A browser has no thread to wait on the channel with, so there the widget is woken by its
    /// poll tick alone - see [`with_poll_interval`](Terminal::with_poll_interval) - and output
    /// shows within one tick of arriving.
    #[cfg(target_arch = "wasm32")]
    fn wake_on_output(&mut self, _ctx: &egui::Context) {}

    /// Wake the UI the moment the program writes, instead of on the widget's next poll tick.
    #[cfg(not(target_arch = "wasm32"))]
    fn wake_on_output(&mut self, ctx: &egui::Context) {
        if self.wakes_on_output {
            return;
        }
        self.wakes_on_output = true;
        let (sender, receiver) = mpsc::channel();
        let upstream = std::mem::replace(&mut self.output, receiver);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            while let Ok(chunk) = upstream.recv() {
                if sender.send(chunk).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
            // The program is gone: dropping the sender carries the disconnect through, and
            // the wake is how the widget hears about it before its next tick.
            drop(sender);
            ctx.request_repaint();
        });
    }

    /// Draw the terminal into all the space the `ui` has left, and handle its input.
    ///
    /// While the program is alive the widget asks for the next frame itself, so output that
    /// arrives with nothing else going on still appears — see
    /// [`with_poll_interval`](Terminal::with_poll_interval).
    pub fn ui(&mut self, ui: &mut Ui, style: &TerminalStyle) -> Response {
        self.wake_on_output(ui.ctx());
        self.poll();
        self.set_color_scheme(style.scheme);

        let padding = style.padding;
        let cell = cell_size(ui.painter(), &style.font);
        let available = ui.available_size();
        let cols = ((available.x - 2.0 * padding) / cell.x)
            .floor()
            .max(f32::from(MIN_COLS)) as u16;
        let rows = ((available.y - 2.0 * padding) / cell.y)
            .floor()
            .max(f32::from(MIN_ROWS)) as u16;
        report(self.resize(cols, rows, cell));

        let (response, painter) = ui.allocate_painter(available, Sense::click_and_drag());
        // A drag as well as a click: selecting text is a drag, and a pointer that never
        // clicked would leave the terminal without the keyboard — so the copy that a selection
        // is made for would go to whatever had it instead.
        if response.clicked() || response.drag_started() || std::mem::take(&mut self.pending_focus)
        {
            response.request_focus();
        }
        let focused = response.has_focus();

        if focused {
            // Tab, the arrows and Escape belong to the program, not to egui's focus
            // traversal: without this, Tab moves the keyboard to the next widget instead of
            // completing a path.
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
            self.handle_input(ui);
            ask_for_text_input(ui, &response, cell);
            // Only the terminal with the keyboard: a copy chord meant for one must not be
            // answered by every other terminal open beside it.
            self.handle_clipboard(ui);
        } else {
            // A composition belongs to the terminal with the keyboard, which this one no
            // longer is.
            self.composing.clear();
        }
        let origin = response.rect.min + vec2(padding, padding);
        // A finger has no wheel, so on a touch screen a finger carried up or down the terminal
        // is the scroll - and not a selection, which is what the same drag is with a mouse.
        let touch = ui.input(|input| input.has_touch_screen());
        if touch && response.dragged() {
            ui.input_mut(|input| input.smooth_scroll_delta.y += input.pointer.delta().y);
        }
        self.handle_scroll(ui, &response, origin, cell);
        if !touch {
            self.handle_pointer(ui, &response, origin, cell);
        }

        self.blink_clock += ui.input(|input| input.stable_dt).min(0.1);
        let cursor_blinking = match self.draw(&painter, origin, cell, style, focused) {
            Ok(blinking) => blinking,
            Err(error) => {
                painter.text(
                    origin,
                    Align2::LEFT_TOP,
                    format!("terminal render failed: {error}"),
                    style.font.clone(),
                    style.error_ink,
                );
                false
            }
        };

        if self.exited {
            let what = self.label.as_deref().unwrap_or("the program");
            painter.text(
                response.rect.left_bottom() + vec2(padding, -padding - cell.y),
                Align2::LEFT_TOP,
                format!("[{what} exited]"),
                FontId::monospace(cell.y * 0.7),
                style.notice_ink,
            );
        } else {
            let interval = if cursor_blinking {
                self.poll_interval.min(BLINK_POLL_INTERVAL)
            } else {
                self.poll_interval
            };
            ui.ctx().request_repaint_after(interval);
        }

        response
    }

    fn resize(&mut self, cols: u16, rows: u16, cell: egui::Vec2) -> Result<()> {
        if cols == self.cols && rows == self.rows {
            return Ok(());
        }
        self.cols = cols;
        self.rows = rows;
        self.emulator
            .resize(cols, rows, cell.x as u32, cell.y as u32)
            .map_err(|error| Error::context("failed to resize the terminal", error))?;
        // The program needs to hear about it too, so its foreground process gets SIGWINCH.
        self.tty.resize(cols, rows)
    }
}

/// On macOS the copy chord is ⌘C, which no program would ever have been sent. Everywhere else
/// it is Ctrl+C — the chord that interrupts a program — so with nothing selected it has to
/// reach the program instead of being swallowed.
const COPY_CHORD_IS_ALSO_INTERRUPT: bool = !cfg!(target_os = "macos");

/// A terminal keeps running whatever the emulator says: a failed selection is not worth
/// tearing a shell down over, but it is worth saying out loud.
fn report(result: Result<()>) {
    if let Err(error) = result {
        eprintln!("[egui_tty] {error}");
    }
}

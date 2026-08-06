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
    render::{CellIterator, CursorVisualStyle, Dirty, RenderState, RowIterator},
    style::RgbColor,
    terminal::ScrollViewport,
};

use crate::{
    ColorScheme, Error, Result, TerminalStyle, Tty, TtyStream, colors, keys, links, search,
    selection,
};

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
    /// Set once the program is gone, so the widget says so instead of looking merely idle.
    exited: bool,
    /// Set by [`Terminal::request_focus`], and cleared by the frame that acts on it.
    pending_focus: bool,
    /// Cursor blink phase, in seconds since the widget was first drawn.
    blink_clock: f32,
    /// Ghostty's pointer-to-selection state machine, driven by this widget's mouse events.
    pointer: selection::Pointer,
    /// The URL under the pointer, found afresh each frame it moves. Held so the row it is on
    /// can be drawn underlined without looking it up a second time.
    hovered_link: Option<links::Link>,
    /// The scheme the emulator's colors were last set for. Setting them is a whole palette's
    /// worth of work, so it happens when the scheme changes rather than every frame.
    colored_for: Option<ColorScheme>,
    /// The colors the emulator arrived with, read before a scheme was first applied. The dark
    /// scheme is these, put back explicitly — asking the emulator to reset to its own
    /// defaults does not restore them.
    emulator_colors: Option<colors::Scheme>,
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
                let _ = reply_to.write(data);
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
            exited: false,
            pending_focus: false,
            blink_clock: 0.0,
            pointer: selection::Pointer::new()?,
            hovered_link: None,
            colored_for: None,
            emulator_colors: None,
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
                line.push_str(if cell_text.is_empty() { " " } else { &cell_text });
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

    /// Draw the terminal into all the space the `ui` has left, and handle its input.
    ///
    /// While the program is alive the widget asks for the next frame itself, so output that
    /// arrives with nothing else going on still appears — see
    /// [`with_poll_interval`](Terminal::with_poll_interval).
    pub fn ui(&mut self, ui: &mut Ui, style: &TerminalStyle) -> Response {
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
        if response.clicked() || std::mem::take(&mut self.pending_focus) {
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
            // Only the terminal with the keyboard: a copy chord meant for one must not be
            // answered by every other terminal open beside it.
            self.handle_clipboard(ui);
        }
        self.handle_scroll(ui, &response, cell);

        let origin = response.rect.min + vec2(padding, padding);
        self.handle_pointer(ui, &response, origin, cell);

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

    /// Turn the pointer into a selection, and into a click on a link.
    ///
    /// Ghostty's gesture machine decides what a press means — one click clears, two select a
    /// word, three a line — so all this does is tell it where the pointer is and when the
    /// button went down and up.
    fn handle_pointer(
        &mut self,
        ui: &Ui,
        response: &Response,
        origin: egui::Pos2,
        cell_size: egui::Vec2,
    ) {
        let (pointer_at, pressed, released, now) = ui.input(|input| {
            (
                input.pointer.interact_pos(),
                input.pointer.primary_pressed(),
                input.pointer.primary_released(),
                Duration::from_secs_f64(input.time.max(0.0)),
            )
        });

        // A pointer past the edge of the grid still belongs to the row and column nearest it,
        // which is what makes a drag off the bottom select to the end of the line.
        let cell = pointer_at.map(|at| selection::Cell {
            x: (((at.x - origin.x) / cell_size.x).floor().max(0.0) as u16).min(self.cols - 1),
            y: (((at.y - origin.y) / cell_size.y).floor().max(0.0) as u16).min(self.rows - 1),
        });

        self.hovered_link = match (response.contains_pointer(), cell) {
            // A link is only worth looking for when nothing is being dragged over it.
            (true, Some(cell)) if !self.pointer.dragging => {
                links::link_at(&self.emulator, self.cols, cell.x, cell.y)
            }
            _ => None,
        };
        if self.hovered_link.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        let Some((cell, at)) = cell.zip(pointer_at) else {
            return;
        };
        let at = (at.x - response.rect.min.x, at.y - response.rect.min.y);

        if pressed && response.contains_pointer() {
            report(self.pointer.press(&self.emulator, cell, at, now));
        } else if self.pointer.dragging && released {
            // A click that never moved is not a selection: it is how a link is opened.
            let was_a_click = !self.pointer.dragged(&self.emulator);
            report(self.pointer.release(&self.emulator));
            if was_a_click
                && let Some(link) = &self.hovered_link
            {
                ui.ctx().open_url(egui::OpenUrl::new_tab(&link.url));
            }
        } else if self.pointer.dragging {
            let geometry = selection::geometry(
                self.cols,
                cell_size.x,
                origin.x - response.rect.min.x,
                response.rect.height().max(1.0),
            );
            // Alt turns a drag into a block selection, which is how a column of output is
            // taken out of a table without its neighbours.
            let rectangle = ui.input(|input| input.modifiers.alt);
            report(
                self.pointer
                    .drag(&self.emulator, cell, at, geometry, rectangle),
            );
        }
    }

    /// Copy takes whatever is selected, and paste goes to the program.
    ///
    /// The platform turns those chords into events of their own before egui ever sees a key,
    /// which is why they are handled here rather than as keystrokes.
    fn handle_clipboard(&mut self, ui: &Ui) {
        let events = ui.input(|input| input.events.clone());
        for event in &events {
            match event {
                egui::Event::Copy | egui::Event::Cut => {
                    match selection::selected_text(&self.emulator) {
                        Some(text) if !text.is_empty() => ui.ctx().copy_text(text),
                        // Nothing is selected, so on the platforms where the copy chord is
                        // also the interrupt the program is the one that wanted it. The key
                        // event itself never arrives, so the byte is sent here instead.
                        _ if COPY_CHORD_IS_ALSO_INTERRUPT => {
                            report(self.tty.write(&[0x03]));
                        }
                        _ => {}
                    }
                }
                egui::Event::Paste(text) => match selection::encode_paste(&self.emulator, text) {
                    Ok(bytes) => report(self.tty.write(&bytes)),
                    Err(error) => report(Err(error)),
                },
                _ => {}
            }
        }
    }

    fn handle_scroll(&mut self, ui: &Ui, response: &Response, cell_size: egui::Vec2) {
        if !response.hovered() {
            return;
        }
        let scroll = ui.input(|input| input.smooth_scroll_delta.y);
        if scroll.abs() < 0.5 {
            return;
        }
        // Wheel notches are lines, and up on a wheel means back into the scrollback.
        let lines = (scroll / cell_size.y).round() as isize;
        if lines != 0 {
            self.emulator.scroll_viewport(ScrollViewport::Delta(-lines));
        }
    }

    /// Encode this frame's key and text events and send them to the program.
    ///
    /// libghostty decides what the bytes are — application cursor keys, the Kitty keyboard
    /// protocol, `modifyOtherKeys` — so all that happens here is pairing each physical key
    /// press with the text the platform produced for it.
    fn handle_input(&mut self, ui: &Ui) {
        let events = ui.input(|input| input.events.clone());
        let mut pending_text: Vec<String> = events
            .iter()
            .filter_map(|event| match event {
                egui::Event::Text(text) => Some(text.clone()),
                _ => None,
            })
            .collect();
        let mut text_index = 0usize;

        self.encoder.set_options_from_terminal(&self.emulator);
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

            let mods = keys::vt_mods(*modifiers);
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

            report(self.encode_key(vt, mods, text, *repeat, &mut encoded));
        }

        // Anything left is text with no key event behind it: an IME commit, or a paste.
        for text in pending_text.drain(text_index.min(pending_text.len())..) {
            encoded.extend_from_slice(text.as_bytes());
        }

        if !encoded.is_empty() {
            report(self.tty.write(&encoded));
        }
    }

    fn encode_key(
        &mut self,
        vt: key::Key,
        mods: key::Mods,
        text: Option<String>,
        repeat: bool,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        let mut event = key::Event::new()
            .map_err(|error| Error::context("failed to create a key event", error))?;
        event.set_action(if repeat { Action::Repeat } else { Action::Press });
        event.set_key(vt);
        event.set_mods(mods);
        if let Some(text) = text {
            event.set_utf8(Some(text));
        }
        if let Some(codepoint) = keys::unshifted_codepoint(vt) {
            event.set_unshifted_codepoint(codepoint);
        }

        self.encoder
            .encode_to_vec(&event, out)
            .map_err(|error| Error::context("failed to encode a keystroke", error))
    }

    /// Paint one frame of the grid. Returns whether a blinking cursor wants a repaint.
    fn draw(
        &mut self,
        painter: &egui::Painter,
        origin: egui::Pos2,
        cell: egui::Vec2,
        style: &TerminalStyle,
        focused: bool,
    ) -> Result<bool> {
        let Self {
            emulator,
            render_state,
            row_iterator,
            cell_iterator,
            blink_clock,
            hovered_link,
            ..
        } = self;
        // Read before the fields below are borrowed, so the cursor's phase does not depend
        // on how the emulator's own borrows are arranged.
        let blink_clock = *blink_clock;
        let hovered_link = hovered_link.clone();

        let snapshot = render_state
            .update(emulator)
            .map_err(|error| Error::context("failed to read the screen", error))?;
        let colors = snapshot
            .colors()
            .map_err(|error| Error::context("failed to read the colors", error))?;
        let default_fg = color_of(colors.foreground);
        let default_bg = color_of(colors.background);

        let mut rows = row_iterator
            .update(&snapshot)
            .map_err(|error| Error::context("failed to walk the rows", error))?;

        let mut y = origin.y;
        let mut text = String::with_capacity(16);
        let mut row_index: u16 = 0;

        while let Some(row) = rows.next() {
            // The link, if any, is on one row: the cells of every other row skip the check.
            let underlined = hovered_link
                .as_ref()
                .filter(|link| link.row == row_index)
                .map(|link| link.columns.clone());
            let mut column_index: u16 = 0;

            let mut cells = cell_iterator
                .update(row)
                .map_err(|error| Error::context("failed to walk the cells", error))?;
            let mut x = origin.x;
            // One layout job per row: neighbouring cells that share a style become one run,
            // so a full screen costs a handful of galleys rather than a couple of thousand.
            let mut job = LayoutJob::default();
            let mut run: Option<Run> = None;

            while let Some(view) = cells.next() {
                let selected = view.is_selected().unwrap_or(false);
                let grapheme_count = view.graphemes_len().unwrap_or(0);
                let cell_bg = view.bg_color().ok().flatten().map(color_of);

                let mut fg = view
                    .fg_color()
                    .ok()
                    .flatten()
                    .map(color_of)
                    .unwrap_or(default_fg);
                let mut bg = cell_bg;
                let mut bold = false;
                let mut italic = false;
                let mut underline = false;
                let mut strikethrough = false;

                if view.has_styling().unwrap_or(false)
                    && let Ok(cell_style) = view.style()
                {
                    if cell_style.inverse {
                        let swapped = bg.unwrap_or(default_bg);
                        bg = Some(fg);
                        fg = swapped;
                    }
                    bold = cell_style.bold;
                    italic = cell_style.italic;
                    underline = cell_style.underline != libghostty_vt::style::Underline::None;
                    strikethrough = cell_style.strikethrough;
                }
                if selected {
                    let swapped = bg.unwrap_or(default_bg);
                    bg = Some(fg);
                    fg = swapped;
                }
                // The link the pointer is on is underlined, which is what says it can be
                // clicked at all.
                if underlined
                    .as_ref()
                    .is_some_and(|columns| columns.contains(&column_index))
                {
                    underline = true;
                }

                if let Some(bg) = bg {
                    painter.rect_filled(
                        Rect::from_min_size(egui::pos2(x, y), cell),
                        CornerRadius::ZERO,
                        bg,
                    );
                }

                text.clear();
                if grapheme_count > 0 {
                    let _ = view.graphemes_utf8(&mut text);
                }
                // A blank cell still has to occupy its column in the row's layout.
                let glyph = if text.is_empty() { " " } else { text.as_str() };

                let cell_style = CellStyle {
                    fg,
                    bold,
                    italic,
                    underline,
                    strikethrough,
                };
                match &mut run {
                    Some(current) if current.style == cell_style => current.text.push_str(glyph),
                    Some(current) => {
                        current.flush(&mut job, style);
                        *current = Run::new(cell_style, glyph);
                    }
                    None => run = Some(Run::new(cell_style, glyph)),
                }

                x += cell.x;
                column_index += 1;
            }

            if let Some(mut current) = run.take() {
                current.flush(&mut job, style);
            }
            if !job.sections.is_empty() {
                let galley = painter.layout_job(job);
                painter.galley(egui::pos2(origin.x, y), galley, default_fg);
            }

            let _ = row.set_dirty(false);
            y += cell.y;
            row_index += 1;
        }

        let mut blinking = false;
        if snapshot.cursor_visible().unwrap_or(false)
            && let Ok(Some(viewport)) = snapshot.cursor_viewport()
        {
            blinking = snapshot.cursor_blinking().unwrap_or(false);
            let visible = !blinking || blink_clock.rem_euclid(1.06) < 0.53;

            if visible {
                let color = colors.cursor.map(color_of).unwrap_or(style.cursor);
                let at = egui::pos2(
                    origin.x + f32::from(viewport.x) * cell.x,
                    origin.y + f32::from(viewport.y) * cell.y,
                );
                draw_cursor(
                    painter,
                    Rect::from_min_size(at, cell),
                    snapshot
                        .cursor_visual_style()
                        .unwrap_or(CursorVisualStyle::Block),
                    color,
                    focused,
                );
            }
        }

        let _ = snapshot.set_dirty(Dirty::Clean);
        Ok(blinking)
    }
}

/// A window that is not focused shows a hollow cursor, the way terminals normally do.
fn draw_cursor(
    painter: &egui::Painter,
    rect: Rect,
    style: CursorVisualStyle,
    color: Color32,
    focused: bool,
) {
    // An unfocused terminal, and a program that asked for a hollow cursor, both draw an
    // outline.
    if !focused || style == CursorVisualStyle::BlockHollow {
        painter.rect_stroke(
            rect.shrink(0.5),
            CornerRadius::ZERO,
            Stroke::new(1.0, color),
            egui::StrokeKind::Inside,
        );
        return;
    }

    let filled = match style {
        CursorVisualStyle::Bar => Rect::from_min_size(rect.min, vec2(2.0, rect.height())),
        CursorVisualStyle::Underline => Rect::from_min_size(
            egui::pos2(rect.min.x, rect.max.y - 2.0),
            vec2(rect.width(), 2.0),
        ),
        _ => rect,
    };
    painter.rect_filled(filled, CornerRadius::ZERO, color);
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

#[derive(Clone, Copy, PartialEq)]
struct CellStyle {
    fg: Color32,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
}

/// A run of neighbouring cells that share a style, accumulated so the row can be laid out
/// in as few sections as possible.
struct Run {
    style: CellStyle,
    text: String,
}

impl Run {
    fn new(style: CellStyle, glyph: &str) -> Self {
        Self {
            style,
            text: glyph.to_string(),
        }
    }

    fn flush(&mut self, job: &mut LayoutJob, style: &TerminalStyle) {
        if self.text.is_empty() {
            return;
        }
        let mut format = TextFormat {
            font_id: style.font.clone(),
            color: self.style.fg,
            italics: self.style.italic,
            ..Default::default()
        };
        if self.style.underline {
            format.underline = Stroke::new(1.0, self.style.fg);
        }
        if self.style.strikethrough {
            format.strikethrough = Stroke::new(1.0, self.style.fg);
        }
        // egui's bundled monospace font has no bold face, so bold is rendered as a brighter
        // ink against the same background rather than a heavier stroke.
        if self.style.bold {
            format.color = mix(self.style.fg, style.bold_ink);
        }
        job.append(&self.text, 0.0, format);
        self.text.clear();
    }
}

fn mix(color: Color32, toward: Color32) -> Color32 {
    let mix = |a: u8, b: u8| ((u16::from(a) + u16::from(b)) / 2) as u8;
    Color32::from_rgb(
        mix(color.r(), toward.r()),
        mix(color.g(), toward.g()),
        mix(color.b(), toward.b()),
    )
}

fn color_of(rgb: RgbColor) -> Color32 {
    Color32::from_rgb(rgb.r, rgb.g, rgb.b)
}

/// The advance and line height of a monospace font, which is what a terminal cell is.
///
/// Measured from a laid-out glyph, so the background rects the grid draws line up with the
/// text egui puts inside them.
pub fn cell_size(painter: &egui::Painter, font: &FontId) -> egui::Vec2 {
    let galley = painter.layout_no_wrap("M".to_string(), font.clone(), Color32::WHITE);
    vec2(galley.size().x.max(1.0), galley.size().y.max(1.0))
}

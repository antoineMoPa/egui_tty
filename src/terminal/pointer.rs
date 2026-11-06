//! The mouse and the wheel: selections, forwarded presses, and scrolling.

use super::*;

/// The mouse encoder takes its geometry in whole pixels, and a cell measured in points is
/// not a whole number of them — truncating it would put a click a couple of columns off by
/// the right edge of a wide grid. Everything the encoder sees (positions, cell sizes,
/// padding) is scaled by this instead, which keeps the units consistent and the truncation
/// error under a thousandth of a cell.
pub(super) const MOUSE_PIXEL_SCALE: f32 = 256.0;

/// How many lines one wheel button press stands for, to a program that reads the mouse:
/// xterm's three, which is what such a program scrolls itself by per press it hears.
pub(super) const LINES_PER_WHEEL_PRESS: f32 = 3.0;

impl Terminal {
    /// Turn the pointer into a selection, and into a click on a link.
    ///
    /// Ghostty's gesture machine decides what a press means — one click clears, two select a
    /// word, three a line — so all this does is tell it where the pointer is and when the
    /// button went down and up. The one press it is not told about as a press is one with
    /// shift held, which goes in as a drag so the last selection carries on to it.
    pub(super) fn handle_pointer(
        &mut self,
        ui: &Ui,
        response: &Response,
        origin: egui::Pos2,
        cell_size: egui::Vec2,
    ) {
        // A program that asked for mouse reporting reads clicks itself — claude and
        // opencode move their cursor with them — so the pointer belongs to it, not to a
        // selection. Shift held keeps the selection, the escape every terminal offers.
        if self.reports_mouse() && !ui.input(|input| input.modifiers.shift) {
            self.hovered_link = None;
            self.pressed_link = None;
            self.forward_mouse(ui, response, origin, cell_size);
            return;
        }
        self.mouse_button_down = None;

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

        let geometry = selection::geometry(
            self.cols,
            cell_size.x,
            origin.x - response.rect.min.x,
            response.rect.height().max(1.0),
        );
        // Alt turns a drag into a block selection, which is how a column of output is
        // taken out of a table without its neighbours.
        let (shift, rectangle) = ui.input(|input| (input.modifiers.shift, input.modifiers.alt));

        if pressed && response.contains_pointer() {
            if shift && self.pointer.can_extend(&self.emulator, now) {
                // Shift carries the last selection on to here — after a scroll, if need
                // be — rather than starting over. Not a click, so nothing to open.
                self.pressed_link = None;
                report(
                    self.pointer
                        .extend(&self.emulator, cell, at, geometry, rectangle),
                );
            } else {
                self.pressed_link = self.hovered_link.clone();
                report(self.pointer.press(&self.emulator, cell, at, now));
            }
        } else if self.pointer.dragging && released {
            // A click that never moved is not a selection: it is how a link is opened.
            let was_a_click = !self.pointer.dragged(&self.emulator);
            report(self.pointer.release(&self.emulator));
            if let Some(link) = self.pressed_link.take()
                && was_a_click
            {
                ui.ctx().open_url(egui::OpenUrl::new_tab(&link.url));
            }
        } else if self.pointer.dragging {
            report(
                self.pointer
                    .drag(&self.emulator, cell, at, geometry, rectangle),
            );
        }
    }

    /// Whether the running program asked for mouse reporting, and so reads the pointer
    /// itself rather than leaving it to the widget's selections.
    pub(super) fn reports_mouse(&self) -> bool {
        self.emulator.is_mouse_tracking().unwrap_or(false)
    }

    /// Send this frame's pointer events to the program, encoded in whichever mouse
    /// protocol it asked for. libghostty owns the encoding — which events the tracking
    /// mode reports, and what SGR or X10 or the rest look like — so all that happens here
    /// is turning egui's events into presses, releases and motion at a position.
    pub(super) fn forward_mouse(
        &mut self,
        ui: &Ui,
        response: &Response,
        origin: egui::Pos2,
        cell_size: egui::Vec2,
    ) {
        self.configure_mouse_encoder(response, origin, cell_size);
        let events = ui.input(|input| input.events.clone());
        let mut encoded = Vec::new();

        for event in &events {
            let (action, button, modifiers, at) = match event {
                egui::Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers,
                } => {
                    // A press has to land on the grid; a release is reported wherever
                    // the drag it ends took the pointer.
                    if *pressed && !response.contains_pointer() {
                        continue;
                    }
                    let button = mouse_button(*button);
                    let action = if *pressed {
                        self.mouse_button_down = Some(button);
                        mouse::Action::Press
                    } else {
                        self.mouse_button_down = None;
                        mouse::Action::Release
                    };
                    (action, Some(button), *modifiers, *pos)
                }
                egui::Event::PointerMoved(pos) => {
                    if !response.contains_pointer() && self.mouse_button_down.is_none() {
                        continue;
                    }
                    let modifiers = ui.input(|input| input.modifiers);
                    (
                        mouse::Action::Motion,
                        self.mouse_button_down,
                        modifiers,
                        *pos,
                    )
                }
                _ => continue,
            };

            self.mouse_encoder
                .set_any_button_pressed(self.mouse_button_down.is_some());
            let at = response.rect.clamp(at) - response.rect.min;
            self.mouse_event
                .set_action(action)
                .set_button(button)
                .set_mods(keys::vt_mods(modifiers))
                .set_position(mouse::Position {
                    x: at.x * MOUSE_PIXEL_SCALE,
                    y: at.y * MOUSE_PIXEL_SCALE,
                });
            report(
                self.mouse_encoder
                    .encode_to_vec(&self.mouse_event, &mut encoded)
                    .map_err(|error| Error::context("failed to encode a mouse event", error)),
            );
        }

        if !encoded.is_empty() {
            report(self.tty.report(&encoded));
        }
    }

    /// Point the mouse encoder at the program's current protocol and this frame's
    /// geometry, which is how a position in points becomes a row and a column.
    pub(super) fn configure_mouse_encoder(
        &mut self,
        response: &Response,
        origin: egui::Pos2,
        cell_size: egui::Vec2,
    ) {
        let rect = response.rect;
        let padding = ((origin.x - rect.min.x).max(0.0) * MOUSE_PIXEL_SCALE) as u32;
        self.mouse_encoder
            .set_options_from_terminal(&self.emulator)
            .set_size(mouse::EncoderSize {
                screen_width: (rect.width().max(0.0) * MOUSE_PIXEL_SCALE) as u32,
                screen_height: (rect.height().max(0.0) * MOUSE_PIXEL_SCALE) as u32,
                cell_width: (cell_size.x.max(1.0) * MOUSE_PIXEL_SCALE) as u32,
                cell_height: (cell_size.y.max(1.0) * MOUSE_PIXEL_SCALE) as u32,
                padding_top: padding,
                padding_bottom: padding,
                padding_right: padding,
                padding_left: padding,
            })
            .set_track_last_cell(true);
    }

    /// Copy takes whatever is selected, and paste goes to the program.
    ///
    /// The platform turns those chords into events of their own before egui ever sees a key,
    /// which is why they are handled here rather than as keystrokes.
    pub(super) fn handle_clipboard(&mut self, ui: &Ui) {
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
                            self.send_to_program(&[0x03]);
                        }
                        _ => {}
                    }
                }
                egui::Event::Paste(text) => match selection::encode_paste(&self.emulator, text) {
                    Ok(bytes) => self.send_to_program(&bytes),
                    Err(error) => report(Err(error)),
                },
                _ => {}
            }
        }
    }

    pub(super) fn handle_scroll(
        &mut self,
        ui: &Ui,
        response: &Response,
        origin: egui::Pos2,
        cell_size: egui::Vec2,
    ) {
        if !response.hovered() {
            return;
        }
        let scroll = ui.input(|input| input.smooth_scroll_delta.y);
        if scroll == 0.0 || cell_size.y <= 0.0 {
            return;
        }

        // A wheel report is a notch, not a line: a program reading the mouse scrolls itself
        // by xterm's three lines for each press it hears, so a press per line
        // tripled every gesture and a flick teleported the transcript.
        let shift = ui.input(|input| input.modifiers.shift);
        let to_program = !shift && self.reports_mouse();
        let unit = if to_program {
            cell_size.y * LINES_PER_WHEEL_PRESS
        } else {
            cell_size.y
        };

        // A terminal scrolls in whole lines, and a trackpad scrolls in pixels — most of its
        // frames worth a fraction of one. Rounding each frame on its own threw those away, so
        // a gesture made of them moved nothing at all however far it went; what is left over
        // is kept instead, and the next frame adds to it until it is worth a line.
        self.scroll_remainder += scroll / unit;
        let lines = self.scroll_remainder.trunc();
        self.scroll_remainder -= lines;
        let lines = lines as i32;
        if lines == 0 {
            return;
        }

        // The wheel belongs to a program that asked for it — as the button presses mouse
        // reporting spells it, or as the arrow keys the alternate screen turns it into.
        // Shift held keeps the wheel on the viewport, like the pointer.
        if to_program {
            self.forward_wheel(ui, response, origin, cell_size, lines);
            return;
        }
        if !shift && self.wheel_is_arrow_keys() {
            self.send_wheel_arrows(lines);
            return;
        }

        // Up on a wheel means back into the scrollback.
        self.emulator
            .scroll_viewport(ScrollViewport::Delta(-(lines as isize)));
    }

    /// Send the wheel to a program reading the mouse, as presses of the buttons a wheel
    /// stands for: four is a notch up and five a notch down, one press per line.
    pub(super) fn forward_wheel(
        &mut self,
        ui: &Ui,
        response: &Response,
        origin: egui::Pos2,
        cell_size: egui::Vec2,
        lines: i32,
    ) {
        let Some(at) = ui.input(|input| input.pointer.hover_pos()) else {
            return;
        };
        self.configure_mouse_encoder(response, origin, cell_size);
        self.mouse_encoder
            .set_any_button_pressed(self.mouse_button_down.is_some());

        let button = if lines > 0 {
            mouse::Button::Four
        } else {
            mouse::Button::Five
        };
        let modifiers = keys::vt_mods(ui.input(|input| input.modifiers));
        let at = response.rect.clamp(at) - response.rect.min;

        let mut encoded = Vec::new();
        for _ in 0..lines.unsigned_abs() {
            self.mouse_event
                .set_action(mouse::Action::Press)
                .set_button(Some(button))
                .set_mods(modifiers)
                .set_position(mouse::Position {
                    x: at.x * MOUSE_PIXEL_SCALE,
                    y: at.y * MOUSE_PIXEL_SCALE,
                });
            report(
                self.mouse_encoder
                    .encode_to_vec(&self.mouse_event, &mut encoded)
                    .map_err(|error| Error::context("failed to encode a wheel event", error)),
            );
        }
        if !encoded.is_empty() {
            report(self.tty.report(&encoded));
        }
    }

    /// Whether the wheel belongs to the program rather than the scrollback: the alternate
    /// screen has no scrollback, and a full-screen program that never asked for the mouse
    /// still expects the wheel to move something — so terminals send arrow keys instead,
    /// the "alternate scroll" mode the emulator keeps on by default the way Ghostty does.
    pub(super) fn wheel_is_arrow_keys(&self) -> bool {
        self.emulator
            .active_screen()
            .is_ok_and(|screen| screen == Screen::Alternate)
            && self.emulator.mode(Mode::ALT_SCROLL).unwrap_or(false)
    }

    /// A notch of the wheel as the arrow key it scrolls by, in the encoding the cursor key
    /// mode picks — which is what a terminal on the alternate screen sends.
    pub(super) fn send_wheel_arrows(&mut self, lines: i32) {
        let application = self.emulator.mode(Mode::DECCKM).unwrap_or(false);
        let arrow: &[u8] = match (lines > 0, application) {
            (true, true) => b"\x1bOA",
            (true, false) => b"\x1b[A",
            (false, true) => b"\x1bOB",
            (false, false) => b"\x1b[B",
        };
        let encoded: Vec<u8> = arrow.repeat(lines.unsigned_abs() as usize);
        // Arrows, but for the wheel: scrolling a pager is not typing at it.
        report(self.tty.report(&encoded));
    }
}

/// The terminal button an egui pointer button stands for. The extra pair are the
/// back/forward buttons, which xterm numbers eight and nine.
pub(super) fn mouse_button(button: egui::PointerButton) -> mouse::Button {
    match button {
        egui::PointerButton::Primary => mouse::Button::Left,
        egui::PointerButton::Secondary => mouse::Button::Right,
        egui::PointerButton::Middle => mouse::Button::Middle,
        egui::PointerButton::Extra1 => mouse::Button::Eight,
        egui::PointerButton::Extra2 => mouse::Button::Nine,
    }
}

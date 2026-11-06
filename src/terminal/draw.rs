//! Painting the grid: cell runs, the cursor and an input method's composition.

use super::*;

impl Terminal {
    /// Paint one frame of the grid. Returns whether a blinking cursor wants a repaint.
    pub(super) fn draw(
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
            composing,
            cols,
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

        if !composing.is_empty()
            && let Ok(Some(viewport)) = snapshot.cursor_viewport()
        {
            draw_composition(
                painter,
                composing,
                origin,
                cell,
                (viewport.x, viewport.y),
                *cols,
                style,
                default_fg,
                default_bg,
            );
        }

        let _ = snapshot.set_dirty(Dirty::Clean);
        Ok(blinking)
    }
}

/// An input method's composition, from the cursor on and wrapping at the grid's right edge a
/// cell at a time the way the text will once the program echoes it, drawn over the cells it
/// covers and underlined the way a composition is everywhere on macOS.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_composition(
    painter: &egui::Painter,
    composing: &str,
    origin: egui::Pos2,
    cell: egui::Vec2,
    cursor: (u16, u16),
    cols: u16,
    style: &TerminalStyle,
    ink: Color32,
    background: Color32,
) {
    let chars: Vec<char> = composing.chars().collect();
    let cols = usize::from(cols);
    let (mut col, mut row) = (usize::from(cursor.0), usize::from(cursor.1));
    let mut rest = chars.as_slice();
    while !rest.is_empty() {
        let (line, after) = rest.split_at(rest.len().min(cols.saturating_sub(col).max(1)));
        let at = egui::pos2(
            origin.x + col as f32 * cell.x,
            origin.y + row as f32 * cell.y,
        );
        let covered = Rect::from_min_size(at, vec2(line.len() as f32 * cell.x, cell.y));
        painter.rect_filled(covered, CornerRadius::ZERO, background);
        painter.text(
            at,
            Align2::LEFT_TOP,
            line.iter().collect::<String>(),
            style.font.clone(),
            ink,
        );
        painter.hline(
            covered.x_range(),
            covered.max.y - 1.0,
            Stroke::new(1.0, ink),
        );
        rest = after;
        col = 0;
        row += 1;
    }
}

/// A window that is not focused shows a hollow cursor, the way terminals normally do.
pub(super) fn draw_cursor(
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

#[derive(Clone, Copy, PartialEq)]
pub(super) struct CellStyle {
    fg: Color32,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
}

/// A run of neighbouring cells that share a style, accumulated so the row can be laid out
/// in as few sections as possible.
pub(super) struct Run {
    style: CellStyle,
    text: String,
}

impl Run {
    pub(super) fn new(style: CellStyle, glyph: &str) -> Self {
        Self {
            style,
            text: glyph.to_string(),
        }
    }

    pub(super) fn flush(&mut self, job: &mut LayoutJob, style: &TerminalStyle) {
        if self.text.is_empty() {
            return;
        }
        let mut format = TextFormat {
            font_id: style.font.clone(),
            color: self.style.fg,
            ..Default::default()
        };
        if self.style.underline {
            format.underline = Stroke::new(1.0, self.style.fg);
        }
        if self.style.strikethrough {
            format.strikethrough = Stroke::new(1.0, self.style.fg);
        }
        // A real face where the style has one; otherwise bold is a brighter ink against the
        // same background and italic is the regular face sheared, since egui's own
        // monospace font comes in one weight and no slant.
        if self.style.italic {
            match &style.italic_font {
                Some(font) => format.font_id = font.clone(),
                None => format.italics = true,
            }
        }
        if self.style.bold {
            match &style.bold_font {
                // Bold and italic together is the bold face sheared: two faces are what
                // ship, not four.
                Some(font) => {
                    format.font_id = font.clone();
                    format.italics = self.style.italic;
                }
                None => format.color = mix(self.style.fg, style.bold_ink),
            }
        }
        job.append(&self.text, 0.0, format);
        self.text.clear();
    }
}

pub(super) fn mix(color: Color32, toward: Color32) -> Color32 {
    let mix = |a: u8, b: u8| ((u16::from(a) + u16::from(b)) / 2) as u8;
    Color32::from_rgb(
        mix(color.r(), toward.r()),
        mix(color.g(), toward.g()),
        mix(color.b(), toward.b()),
    )
}

pub(super) fn color_of(rgb: RgbColor) -> Color32 {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(bold: bool, italic: bool) -> CellStyle {
        CellStyle {
            fg: Color32::RED,
            bold,
            italic,
            underline: false,
            strikethrough: false,
        }
    }

    fn laid_out(style: &TerminalStyle, cell: CellStyle) -> TextFormat {
        let mut job = LayoutJob::default();
        Run::new(cell, "x").flush(&mut job, style);
        job.sections
            .pop()
            .expect("expected the run to have been laid out")
            .format
    }

    /// Given faces to set them in, bold and italic runs are set in those faces, in the
    /// program's own color: nothing is faked once there is a real face for it.
    #[test]
    fn bold_and_italic_are_set_in_their_own_faces_when_given_them() {
        let bold = FontId::new(12.0, egui::FontFamily::Name("bold".into()));
        let italic = FontId::new(12.0, egui::FontFamily::Name("italic".into()));
        let style = TerminalStyle {
            bold_font: Some(bold.clone()),
            italic_font: Some(italic.clone()),
            ..TerminalStyle::default()
        };

        let format = laid_out(&style, styled(true, false));
        assert_eq!(format.font_id, bold);
        assert_eq!(format.color, Color32::RED);
        assert!(!format.italics);

        let format = laid_out(&style, styled(false, true));
        assert_eq!(format.font_id, italic);
        assert!(!format.italics);

        // Both at once is the bold face sheared: two faces ship, not four.
        let format = laid_out(&style, styled(true, true));
        assert_eq!(format.font_id, bold);
        assert!(format.italics);
    }

    /// Without faces, what there was before: bold as brighter ink, italic as a shear.
    #[test]
    fn bold_and_italic_are_faked_without_faces_of_their_own() {
        let style = TerminalStyle::default();

        let format = laid_out(&style, styled(true, false));
        assert_eq!(format.font_id, style.font);
        assert_eq!(format.color, mix(Color32::RED, style.bold_ink));

        let format = laid_out(&style, styled(false, true));
        assert_eq!(format.font_id, style.font);
        assert!(format.italics);
        assert_eq!(format.color, Color32::RED);
    }
}

use egui::{Color32, FontId, Visuals};

/// Which way round a terminal's own colors run.
///
/// Programs ask for colors by name — "red", "bright blue", "the default foreground" — and the
/// emulator resolves those against a scheme. Ghostty's built-in scheme assumes a dark
/// background, so on a light background it draws pale text on paper and is unreadable.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ColorScheme {
    /// The emulator's own scheme: light text, dark background.
    #[default]
    Dark,
    /// A scheme dark enough to read on paper, every entry of it at least 4.5:1 against the
    /// light background it comes with.
    Light,
}

/// How a [`Terminal`](crate::Terminal) is drawn.
///
/// The grid's own colors come from the emulator rather than from here — see [`ColorScheme`].
/// What is left is the handful of things the widget draws itself.
#[derive(Clone, Debug)]
pub struct TerminalStyle {
    /// The font the grid is drawn in. A cell is one advance of it wide and one line tall, so
    /// it has to be monospace.
    pub font: FontId,
    /// Which way round the emulator's colors run.
    pub scheme: ColorScheme,
    /// The gap between the grid and the edge of the space the widget was given.
    pub padding: f32,
    /// The cursor, when the running program has not asked for a color of its own.
    pub cursor: Color32,
    /// The face bold text is drawn in, when there is one to draw it in. It has to have the
    /// same advance as [`font`](Self::font), or a bold word steps out of the grid's columns.
    ///
    /// egui's bundled monospace font comes in one weight, so there is none by default and
    /// bold is drawn as ink mixed toward [`bold_ink`](Self::bold_ink) instead.
    pub bold_font: Option<FontId>,
    /// The face italic text is drawn in, when there is one; the same advance as
    /// [`font`](Self::font), as for [`bold_font`](Self::bold_font). Without one, italic is
    /// the regular face sheared to a slant.
    pub italic_font: Option<FontId>,
    /// What bold text is drawn in when there is no [`bold_font`](Self::bold_font): ink
    /// mixed toward this, rather than a heavier stroke.
    pub bold_ink: Color32,
    /// The "[… exited]" notice, and anything else the widget says rather than draws.
    pub notice_ink: Color32,
    /// What a failure to render is reported in.
    pub error_ink: Color32,
}

impl Default for TerminalStyle {
    fn default() -> Self {
        Self::from_visuals(&Visuals::dark())
    }
}

impl TerminalStyle {
    /// The style that suits an egui theme: dark colors for a dark theme, and the ink for
    /// everything the widget writes taken from the same place the rest of the UI takes it.
    pub fn from_visuals(visuals: &Visuals) -> Self {
        Self {
            font: FontId::monospace(12.0),
            bold_font: None,
            italic_font: None,
            scheme: if visuals.dark_mode {
                ColorScheme::Dark
            } else {
                ColorScheme::Light
            },
            padding: 6.0,
            cursor: visuals.selection.stroke.color,
            bold_ink: visuals.strong_text_color(),
            notice_ink: visuals.weak_text_color(),
            error_ink: visuals.error_fg_color,
        }
    }
}

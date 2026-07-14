//! VT100 screen parser and rendering helpers.
//!
//! [`Parser`] wraps `vt100::Parser` with scrollback, resize, reset, and
//! alternate-screen accessors. It accepts terminal output through
//! [`Parser::parse`] and keeps the emulated screen available through
//! [`Parser::screen`].
//!
//! With the `ratatui` feature enabled, the parser can convert the visible screen
//! to styled `ratatui::Text`, including foreground/background colors, common
//! text modifiers, cursor rendering, and trimmed inline output.

#[cfg(feature = "ratatui")]
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};
use std::cell::{Ref, RefCell};
#[cfg(feature = "ratatui")]
use vt100::{Cell, Screen};

/// VT100 parser with optional `ratatui` conversion helpers.
#[derive(Default)]
pub struct Parser {
    parser: RefCell<vt100::Parser>,
    scrollback_len: usize,
    mouse_reporting: bool,
    sgr_mouse: bool,
    /// Partial DEC private mode sequence (`ESC[?…`) carried over from a
    /// previous byte-split PTY read. Prepend to the next chunk so that
    /// `update_mouse_modes` can see the complete sequence.
    pending_csi: String,
    #[cfg(feature = "ratatui")]
    cache: RefCell<Option<Text<'static>>>,
    #[cfg(feature = "ratatui")]
    dirty: RefCell<bool>,
}

impl Parser {
    /// Creates a parser with the given viewport and scrollback size.
    pub fn new(rows: u16, cols: u16, scrollback_len: usize) -> Self {
        Self {
            parser: RefCell::new(vt100::Parser::new(rows, cols, scrollback_len)),
            scrollback_len,
            mouse_reporting: false,
            sgr_mouse: false,
            pending_csi: String::new(),
            #[cfg(feature = "ratatui")]
            cache: RefCell::new(None),
            #[cfg(feature = "ratatui")]
            dirty: RefCell::new(true),
        }
    }

    /// Parses raw terminal output.
    pub fn parse(&mut self, s: &str) {
        // Guard: if the pending fragment exceeds 256 bytes, assume garbage
        // (e.g. a stream of bytes that keeps matching the partial pattern)
        // and discard it to prevent unbounded memory growth.
        if self.pending_csi.len() > 256 {
            self.pending_csi.clear();
        }

        let scan_input = if self.pending_csi.is_empty() {
            std::borrow::Cow::Borrowed(s)
        } else {
            let mut input = std::mem::take(&mut self.pending_csi);
            input.push_str(s);
            std::borrow::Cow::Owned(input)
        };

        update_mouse_modes(
            &scan_input,
            &mut self.pending_csi,
            &mut self.mouse_reporting,
            &mut self.sgr_mouse,
        );
        self.parser.borrow_mut().process(s.as_bytes());
        #[cfg(feature = "ratatui")]
        {
            *self.dirty.borrow_mut() = true;
        }
    }

    /// Resizes the emulated terminal screen.
    pub fn resize(&mut self, rows: u16, cols: u16) -> &mut Self {
        self.parser.borrow_mut().screen_mut().set_size(rows, cols);
        #[cfg(feature = "ratatui")]
        {
            *self.dirty.borrow_mut() = true;
        }
        self
    }

    /// Clears screen state while preserving dimensions and scrollback capacity.
    pub fn reset(&mut self) -> &mut Self {
        let (rows, cols) = self.parser.borrow().screen().size();
        *self.parser.borrow_mut() = vt100::Parser::new(rows, cols, self.scrollback_len);
        self.mouse_reporting = false;
        self.sgr_mouse = false;
        self.pending_csi.clear();
        #[cfg(feature = "ratatui")]
        {
            *self.dirty.borrow_mut() = true;
        }
        self
    }

    /// Sets the scrollback position.
    pub fn scroll(&self, y: usize) {
        self.parser.borrow_mut().screen_mut().set_scrollback(y);
        #[cfg(feature = "ratatui")]
        {
            *self.dirty.borrow_mut() = true;
        }
    }

    /// Borrows the underlying VT100 screen.
    pub fn screen(&self) -> impl std::ops::Deref<Target = vt100::Screen> + '_ {
        Ref::map(self.parser.borrow(), |p: &vt100::Parser| p.screen())
    }

    /// Returns whether the alternate screen buffer is active.
    pub fn is_alternate_screen(&self) -> bool {
        self.screen().alternate_screen()
    }

    /// Returns true when the PTY application requested SGR mouse reporting.
    ///
    /// We only emit SGR mouse input when both a mouse tracking mode and SGR
    /// coordinate encoding are enabled. Plain command output never receives
    /// mouse bytes, so scrollback commands like `ls` keep using inline scroll.
    pub fn sgr_mouse_enabled(&self) -> bool {
        self.mouse_reporting && self.sgr_mouse
    }

    #[cfg(feature = "ratatui")]
    /// Converts the visible screen to styled `ratatui` text.
    pub fn contents(&self) -> Text<'_> {
        if *self.dirty.borrow() {
            let screen = self.screen();
            *self.cache.borrow_mut() = Some(ansi_to_text(&screen, false));
            *self.dirty.borrow_mut() = false;
        }

        self.cache.borrow().as_ref().cloned().unwrap_or_default()
    }

    #[cfg(feature = "ratatui")]
    /// Converts inline output to styled text and trims trailing empty rows.
    pub fn inline_contents(&self, show_cursor: bool) -> Text<'static> {
        let parser = self.parser.borrow();
        let mut text = ansi_to_text(parser.screen(), show_cursor);

        let cursor_y = if show_cursor && !parser.screen().hide_cursor() {
            Some(parser.screen().cursor_position().0 as usize)
        } else {
            None
        };
        drop(parser);

        trim_trailing_empty_lines(&mut text, cursor_y);
        text
    }

    #[cfg(feature = "ratatui")]
    /// Returns visible screen contents as plain text.
    pub fn contents_plain(&self) -> String {
        let text = self.contents();
        text.lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// Tracks the subset of DEC private modes needed to decide whether host mouse
// events should be forwarded into the PTY. Full-screen TUIs usually emit
// sequences like `ESC[?1002;1006h` when enabling mouse support and matching
// `...l` sequences on shutdown.
//
// Enables SGR mouse forwarding only when both pieces are present:
// - a tracking mode (`1000`, `1002`, or `1003`)
// - SGR coordinate encoding (`1006`)
//
// Keeps plain command output on inline scrollback because it normally emits
// none of these mode sequences.
fn update_mouse_modes(
    s: &str,
    pending: &mut String,
    mouse_reporting: &mut bool,
    sgr_mouse: &mut bool,
) {
    let bytes = s.as_bytes();
    let mut i = 0;

    while i + 3 < bytes.len() {
        // DEC private mode set/reset has the shape `ESC [ ? params h|l`,
        // where params is one or more semicolon-separated numbers.
        if bytes[i] != b'\x1b' || bytes[i + 1] != b'[' || bytes[i + 2] != b'?' {
            i += 1;
            continue;
        }

        let mut j = i + 3;
        while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
            j += 1;
        }

        // Incomplete DEC private mode sequence at end of input.
        // Save the fragment from ESC onward so the next chunk completes it.
        if j == bytes.len() {
            pending.push_str(&s[i..j]);
            return;
        }

        // `h` enables the listed modes; `l` disables them. An unrelated
        // CSI sequence is left for the vt100 parser below.
        if !matches!(bytes[j], b'h' | b'l') {
            i += 1;
            continue;
        }

        let enabled = bytes[j] == b'h';
        let mut value = 0u16;
        let mut in_value = false;

        for &byte in &bytes[i + 3..=j] {
            if byte == b';' || byte == bytes[j] {
                if in_value {
                    match value {
                        1000 | 1002 | 1003 => *mouse_reporting = enabled,
                        1006 => *sgr_mouse = enabled,
                        _ => {}
                    }
                }
                value = 0;
                in_value = false;
            } else {
                value = value.saturating_mul(10) + u16::from(byte - b'0');
                in_value = true;
            }
        }

        i = j + 1;
    }

    save_incomplete_csi_prefix(s, pending);
}

fn save_incomplete_csi_prefix(s: &str, pending: &mut String) {
    let bytes = s.as_bytes();
    let prefix = b"\x1b[?";

    for len in (1..=prefix.len()).rev() {
        if bytes.len() >= len && bytes[bytes.len() - len..] == prefix[..len] {
            // Safe: the suffix is ASCII bytes from `prefix`, so it is valid UTF-8
            // and starts at a character boundary.
            pending.push_str(std::str::from_utf8(&bytes[bytes.len() - len..]).unwrap_or(""));
            return;
        }
    }
}

#[cfg(feature = "ratatui")]
fn trim_trailing_empty_lines(text: &mut Text<'static>, cursor_y: Option<usize>) {
    while let Some(last) = text.lines.last() {
        let line_idx = text.lines.len() - 1;
        if Some(line_idx) == cursor_y {
            break;
        }

        if last.spans.is_empty()
            || last.spans.iter().all(|s| {
                s.content.trim().is_empty()
                    && s.style.bg.is_none()
                    && !s.style.add_modifier.contains(Modifier::REVERSED)
            })
        {
            text.lines.pop();
        } else {
            break;
        }
    }
}

#[cfg(feature = "ratatui")]
fn last_content_col(screen: &Screen, row: u16, cursor: Option<(u16, u16)>) -> u16 {
    let (_, cols) = screen.size();
    let mut last = 0;
    for x in (0..cols).rev() {
        let Some(cell) = screen.cell(row, x) else {
            continue;
        };
        if cell.has_contents() || cell.bgcolor() != vt100::Color::Default {
            last = x + 1;
            break;
        }
    }
    if let Some((cy, cx)) = cursor {
        if row == cy && cx >= last && cx < cols {
            last = cx + 1;
        }
    }
    last
}

#[cfg(feature = "ratatui")]
fn cursor_position(screen: &Screen, show_cursor: bool) -> Option<(u16, u16)> {
    if show_cursor && !screen.hide_cursor() {
        Some(screen.cursor_position())
    } else {
        None
    }
}

#[cfg(feature = "ratatui")]
fn row_to_line(
    screen: &Screen,
    y: u16,
    cursor: Option<(u16, u16)>,
    last_col: u16,
) -> Line<'static> {
    let mut line_spans = vec![];
    let mut current_span_content = String::with_capacity(last_col as usize);
    let mut current_style = Style::default();
    let mut first = true;

    for x in 0..last_col {
        let Some(cell) = screen.cell(y, x) else {
            continue;
        };

        if cell.is_wide_continuation() {
            continue;
        }

        let mut style = ansi_to_style(cell);
        if let Some((cy, cx)) = cursor {
            if y == cy && x == cx {
                style = style.add_modifier(Modifier::REVERSED);
            }
        }

        let contents = if cell.has_contents() {
            cell.contents()
        } else {
            " "
        };

        if first {
            current_style = style;
            current_span_content.push_str(contents);
            first = false;
        } else if style == current_style {
            current_span_content.push_str(contents);
        } else {
            line_spans.push(Span::styled(current_span_content, current_style));
            current_style = style;
            current_span_content = contents.to_string();
        }
    }

    if !first {
        line_spans.push(Span::styled(current_span_content, current_style));
    }

    Line::from(line_spans)
}

#[cfg(feature = "ratatui")]
pub fn ansi_to_text(screen: &Screen, show_cursor: bool) -> Text<'static> {
    let (rows, _) = screen.size();
    let cursor = cursor_position(screen, show_cursor);

    let mut lines = Vec::with_capacity(rows as usize);
    for y in 0..rows {
        let last_col = last_content_col(screen, y, cursor);
        lines.push(row_to_line(screen, y, cursor, last_col));
    }

    Text::from(lines)
}

#[cfg(feature = "ratatui")]
fn ansi_to_style(cell: &Cell) -> Style {
    Style {
        fg: Some(ansi_to_color(cell.fgcolor())),
        bg: Some(ansi_to_color(cell.bgcolor())),
        add_modifier: ansi_to_modifier(cell),
        sub_modifier: Modifier::empty(),
        underline_color: None,
    }
}

#[cfg(feature = "ratatui")]
fn ansi_to_color(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(x) => Color::Indexed(x),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[cfg(feature = "ratatui")]
fn ansi_to_modifier(cell: &Cell) -> Modifier {
    let mut m = Modifier::empty();

    if cell.bold() {
        m |= Modifier::BOLD;
    }
    if cell.italic() {
        m |= Modifier::ITALIC;
    }
    if cell.underline() {
        m |= Modifier::UNDERLINED;
    }
    if cell.inverse() {
        m |= Modifier::REVERSED;
    }

    m
}

#[cfg(all(test, feature = "ratatui"))]
mod tests {
    use super::*;

    #[test]
    fn test_ansi_alignment() {
        let mut p = vt100::Parser::new(1, 10, 0);
        p.process(b"\x1b[1;5HX");
        let text = ansi_to_text(p.screen(), false);
        let line = &text.lines[0];
        let contents: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(contents.trim_end(), "    X");
    }

    #[test]
    fn test_ansi_background_preservation() {
        let mut p = vt100::Parser::new(1, 10, 0);
        p.process(b"\x1b[44m \x1b[0m");
        let text = ansi_to_text(p.screen(), false);
        let line = &text.lines[0];
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, " ");
        assert_eq!(line.spans[0].style.bg, Some(Color::Indexed(4)));
    }

    #[test]
    fn test_ansi_wide_characters() {
        let mut p = vt100::Parser::new(1, 10, 0);
        p.process("กA".as_bytes());
        let text = ansi_to_text(p.screen(), false);
        let line = &text.lines[0];
        let contents: String = line.spans.iter().map(|s| s.content.as_ref()).collect();

        assert_eq!(contents, "กA");
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "กA");
    }

    #[test]
    fn test_scroll_contents_no_crash() {
        let mut p = Parser::new(3, 10, 5);
        for i in 0..8u8 {
            p.parse(&format!("line {i}\n"));
        }

        p.scroll(3);
        let t = p.contents();
        assert_eq!(t.lines.len(), 3);
        p.scroll(9);
        let t = p.contents();
        assert_eq!(t.lines.len(), 3);
    }

    #[test]
    fn test_aggressive_scroll_no_crash() {
        let mut p = Parser::new(5, 20, 100);
        for i in 0..50u8 {
            p.parse(&format!("line {i}\n\x1b[31mred {i}\x1b[0m\n"));
        }

        for s in 0..=20 {
            p.scroll(s);
            let _ = p.contents();
        }

        p.scroll(usize::MAX);
        let _ = p.contents();

        for i in 0..30u8 {
            p.parse(&format!("new data {i}\n"));
            if i % 3 == 0 {
                p.scroll(5);
                let _ = p.contents();
            }
        }
    }

    #[test]
    fn test_scroll_after_resize_wider_no_crash() {
        let mut p = Parser::new(3, 10, 20);
        for i in 0..15u8 {
            p.parse(&format!("line {i}\n"));
        }
        p.resize(3, 20);
        p.scroll(10);
        let t = p.contents();
        assert_eq!(t.lines.len(), 3);
    }

    #[test]
    fn test_sgr_mouse_enabled_requires_tracking_and_sgr_modes() {
        let mut p = Parser::new(3, 10, 5);
        assert!(!p.sgr_mouse_enabled());

        p.parse("\x1b[?1006h");
        assert!(!p.sgr_mouse_enabled());

        p.parse("\x1b[?1000h");
        assert!(p.sgr_mouse_enabled());
    }

    #[test]
    fn test_sgr_mouse_tracks_combined_enable_disable_and_reset() {
        let mut p = Parser::new(3, 10, 5);
        p.parse("\x1b[?1002;1006h");
        assert!(p.sgr_mouse_enabled());

        p.parse("\x1b[?1002l");
        assert!(!p.sgr_mouse_enabled());

        p.parse("\x1b[?1003;1006h");
        assert!(p.sgr_mouse_enabled());

        p.reset();
        assert!(!p.sgr_mouse_enabled());
    }

    #[test]
    fn test_sgr_mouse_enable_survives_split_private_mode_sequences() {
        for (first, second) in [
            ("\x1b", "[?1002;1006hX"),
            ("\x1b[", "?1002;1006hX"),
            ("\x1b[?", "1002;1006hX"),
            ("\x1b[?1002;", "1006hX"),
        ] {
            let mut p = Parser::new(3, 10, 5);
            p.parse(first);
            assert!(!p.sgr_mouse_enabled());

            p.parse(second);
            assert!(p.sgr_mouse_enabled());
            assert_eq!(p.contents_plain().trim_end(), "X");
        }
    }

    #[test]
    fn test_sgr_mouse_disable_survives_split_private_mode_sequence() {
        let mut p = Parser::new(3, 10, 5);
        p.parse("\x1b[?1002;1006h");
        assert!(p.sgr_mouse_enabled());

        p.parse("\x1b[?1002;");
        assert!(p.sgr_mouse_enabled());

        p.parse("1006l");
        assert!(!p.sgr_mouse_enabled());
    }

    #[test]
    fn test_reset_discards_pending_mouse_mode_fragment() {
        let mut p = Parser::new(3, 10, 5);
        p.parse("\x1b[?1002;");

        p.reset();
        p.parse("1006h");

        assert!(!p.sgr_mouse_enabled());
    }
}

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Padding, Paragraph},
};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, LazyLock};
use syntect::{
    easy::HighlightLines,
    highlighting::{self as syn, ThemeSet},
    parsing::{ScopeStack, SyntaxReference, SyntaxSet},
    util::LinesWithEndings,
};

use crate::apps::config::{DEFAULT_SYNTAX, DEFAULT_THEME};

include!(concat!(env!("OUT_DIR"), "/themes.rs"));

pub static SYNTAX: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

static BUILTIN_THEMES: LazyLock<HashMap<String, Arc<syn::Theme>>> = LazyLock::new(|| {
    ThemeSet::load_defaults()
        .themes
        .into_iter()
        .map(|(name, theme)| (name, Arc::new(theme)))
        .collect()
});

fn bundled_theme_source(name: &str) -> Option<&'static str> {
    bundled_theme_entries()
        .iter()
        .find(|(entry, _)| *entry == name)
        .map(|(_, source)| *source)
}

fn load_bundled_theme(name: &str) -> Option<syn::Theme> {
    let source = bundled_theme_source(name)?;
    ThemeSet::load_from_reader(&mut std::io::Cursor::new(source.as_bytes())).ok()
}

pub fn available_themes() -> Vec<String> {
    let mut names: Vec<_> = bundled_theme_entries()
        .iter()
        .map(|(name, _)| name.to_string())
        .chain(BUILTIN_THEMES.keys().cloned())
        .collect();
    names.sort();
    names.dedup();
    names
}

fn theme(name: &str) -> Arc<syn::Theme> {
    load_bundled_theme(name)
        .map(Arc::new)
        .or_else(|| BUILTIN_THEMES.get(name).cloned())
        .or_else(|| load_bundled_theme(DEFAULT_THEME).map(Arc::new))
        .or_else(|| BUILTIN_THEMES.values().next().cloned())
        .unwrap_or_default()
}

/// Brand accent for the "up" portion of the logo #00B8D4.
pub const LOGO_ACCENT: Color = Color::Rgb(0x00, 0xB8, 0xD4);

/// Converts a ratatui [`Color`] to an ANSI foreground color escape sequence.
pub fn ansi_fg(color: Color) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("\x1b[38;2;{};{};{}m", r, g, b),
        Color::Indexed(i) => format!("\x1b[38;5;{}m", i),
        _ => "\x1b[39m".to_string(),
    }
}

/// Converts a ratatui [`Color`] to an ANSI background color escape sequence.
pub fn ansi_bg(color: Color) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("\x1b[48;2;{};{};{}m", r, g, b),
        Color::Indexed(i) => format!("\x1b[48;5;{}m", i),
        _ => "\x1b[49m".to_string(),
    }
}

/// Converts a ratatui [`Style`] to an ANSI escape sequence string.
pub fn ansi_style(style: Style) -> String {
    let mut out = String::new();
    if let Some(fg) = style.fg {
        out.push_str(&ansi_fg(fg));
    }
    if let Some(bg) = style.bg {
        out.push_str(&ansi_bg(bg));
    }
    if style.add_modifier.contains(Modifier::BOLD) {
        out.push_str("\x1b[1m");
    }
    if style.add_modifier.contains(Modifier::DIM) {
        out.push_str("\x1b[2m");
    }
    if style.add_modifier.contains(Modifier::ITALIC) {
        out.push_str("\x1b[3m");
    }
    if style.add_modifier.contains(Modifier::UNDERLINED) {
        out.push_str("\x1b[4m");
    }
    if style.add_modifier.contains(Modifier::SLOW_BLINK) {
        out.push_str("\x1b[5m");
    }
    if style.add_modifier.contains(Modifier::RAPID_BLINK) {
        out.push_str("\x1b[6m");
    }
    if style.add_modifier.contains(Modifier::REVERSED) {
        out.push_str("\x1b[7m");
    }
    if style.add_modifier.contains(Modifier::HIDDEN) {
        out.push_str("\x1b[8m");
    }
    if style.add_modifier.contains(Modifier::CROSSED_OUT) {
        out.push_str("\x1b[9m");
    }
    out
}

#[derive(Debug, Clone, Copy, Default)]
struct MarkdownStyles {
    text: Style,
    heading_marker: Style,
    heading: Style,
}

/// The current theme.
#[derive(Debug, Clone)]
pub struct Theme {
    name: String,
    pub accent: Color,
    syntect: Arc<syn::Theme>,
    pub background: Color,
    pub foreground: Color,
    pub code_background: Color,
    pub output_background: Color,
    pub info_background: Color,
    pub info_foreground: Color,
    pub active: Color,
    pub success: Color,
    pub error: Color,
    pub logo: Color,
    pub muted: Color,
    pub warning: Color,
    markdown: MarkdownStyles,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: DEFAULT_THEME.to_owned(),
            syntect: theme(DEFAULT_THEME),
            accent: Color::Reset,
            background: Color::Reset,
            foreground: Color::Reset,
            code_background: Color::Reset,
            output_background: Color::Reset,
            info_background: Color::Reset,
            info_foreground: Color::Reset,
            active: Color::Reset,
            success: Color::Reset,
            error: Color::Reset,
            logo: Color::Reset,
            muted: Color::Reset,
            warning: Color::Reset,
            markdown: MarkdownStyles::default(),
        }
    }
}

impl Theme {
    pub fn new(name: &str, transparent: bool) -> Self {
        let syntect = theme(name);
        let theme = syntect.as_ref();
        let settings = &theme.settings;

        let (background, foreground) = if transparent {
            (Color::Reset, Color::Reset)
        } else {
            (to_color(settings.background), to_color(settings.foreground))
        };

        let accent = to_color(settings.accent.or(settings.selection));
        let active = to_color(find_active_color(theme));
        let muted = to_color(find_muted_color(theme));
        let code_background = calculate_code_background(settings.background);
        let output_background = adjust_contrast(&code_background, -7);
        let info_background = calculate_info_background(Some(&code_background));
        let info_foreground = calculate_info_foreground(info_background);
        let warning = to_color(find_warning_color(theme).or_else(|| find_active_color(theme)));
        let success = to_color(find_success_color(theme));
        let error = to_color(find_error_color(theme));
        let fallback_markdown = Style::default().fg(foreground);
        let markdown = syntect_markdown_styles(theme).unwrap_or(MarkdownStyles {
            text: fallback_markdown,
            heading_marker: fallback_markdown,
            heading: fallback_markdown,
        });

        Self {
            name: name.into(),
            syntect,
            accent,
            active,
            background,
            foreground,
            code_background,
            output_background,
            info_background,
            info_foreground,
            logo: LOGO_ACCENT,
            success,
            error,
            muted,
            warning,
            markdown,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn style(&self) -> Style {
        Style::default().fg(self.foreground)
    }

    /// Style Syntect assigns to ordinary Markdown text in this theme.
    pub fn markdown_text_style(&self) -> Style {
        self.markdown.text
    }

    /// Style Syntect assigns to the `#` marker of a Markdown heading.
    pub fn markdown_heading_marker_style(&self) -> Style {
        self.markdown.heading_marker
    }

    /// Style Syntect assigns to Markdown heading text in this theme.
    pub fn markdown_heading_style(&self) -> Style {
        self.markdown.heading
    }

    /// The base style for the entire application, including the background color.
    pub fn global_style(&self) -> Style {
        Style::default().bg(self.background).fg(self.foreground)
    }

    pub fn active_style(&self) -> Style {
        Style::default()
            .bg(self.active)
            .fg(adjust_contrast(&self.active, 100))
    }

    pub fn inactive_style(&self) -> Style {
        Style::default().fg(self.info_background)
    }

    pub fn table_header_style(&self) -> Style {
        self.code_info_style()
    }

    pub fn rule_style(&self) -> Style {
        Style::default()
            .fg(self.info_background)
            .add_modifier(Modifier::DIM)
    }

    pub fn code_style(&self) -> Style {
        Style::default().bg(self.code_background)
    }

    pub fn code_info_style(&self) -> Style {
        Style::default()
            .bg(self.info_background)
            .fg(self.info_foreground)
    }

    pub fn success_style(&self) -> Style {
        Style::default().fg(self.success)
    }

    pub fn success_badge_style(&self) -> Style {
        Style::default()
            .bg(self.success)
            .fg(adjust_contrast(&self.success, 100))
    }

    pub fn error_style(&self) -> Style {
        Style::default().fg(self.error)
    }

    pub fn error_badge_style(&self) -> Style {
        Style::default()
            .bg(self.error)
            .fg(adjust_contrast(&self.error, 100))
    }

    pub fn muted_style(&self) -> Style {
        Style::default().fg(self.muted)
    }

    pub fn info_badge_style(&self) -> Style {
        Style::default()
            .bg(self.active)
            .fg(adjust_contrast(&self.active, 100))
    }

    pub fn active_fg_style(&self) -> Style {
        Style::default().fg(self.active)
    }

    pub fn warning_fg_style(&self) -> Style {
        Style::default().fg(self.warning)
    }

    /// Style for inline code runs inside markdown text.
    pub fn inline_code_style(&self) -> Style {
        Style::default().fg(self.active).bg(self.code_background)
    }

    /// Style for inline links in markdown text.
    pub fn link_style(&self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::UNDERLINED)
    }

    /// Style for inline images (rendered as their alt text) in markdown text.
    pub fn image_style(&self) -> Style {
        Style::default()
            .fg(self.muted)
            .add_modifier(Modifier::ITALIC)
    }

    /// Alias for task-running color.
    pub fn running_style(&self) -> Style {
        self.warning_fg_style()
    }

    pub fn selection_style(&self) -> Style {
        Style::default()
            .bg(self.accent)
            .fg(adjust_contrast(&self.accent, 100))
    }

    pub fn search_highlight_style(&self) -> Style {
        Style::default()
            .bg(self.accent)
            .fg(adjust_contrast(&self.accent, 100))
    }

    pub fn logo_style(&self) -> Style {
        Style::default().fg(self.logo)
    }

    /// Returns a styled badge `Line` for the given mode label.
    ///
    /// Each primary mode gets a distinct background drawn from theme colors
    /// so the badge feels native to the active theme.
    pub fn mode_badge(&self, label: &'static str, color: Color) -> Line<'static> {
        let fg = adjust_contrast(&color, 100);
        Line::from(format!(" {label} ")).style(Style::default().bg(color).fg(fg))
    }

    pub fn shortcuts(&self, keys: &[(String, String)]) -> Line<'static> {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(keys.len() * 3);
        for (i, (key, desc)) in keys.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled("  ", self.inactive_style()));
            }
            spans.push(Span::styled(key.clone(), self.active_fg_style()));
            spans.push(Span::raw(format!(" {}", desc)).style(self.muted_style()));
        }
        Line::from(spans)
    }

    pub fn keymap_shortcuts<T, F>(
        &self,
        items: &[(T, keymap::Item)],
        mut predicate: F,
    ) -> Line<'static>
    where
        F: FnMut(&T) -> bool,
    {
        let shortcuts: Vec<(String, String)> = items
            .iter()
            .filter(|(action, _)| predicate(action))
            .filter_map(|(_, item)| {
                let symbol = item.symbol.as_deref().unwrap_or_default();
                let help = item.help.as_deref().unwrap_or_default();
                if symbol.is_empty() || help.is_empty() {
                    None
                } else {
                    Some((symbol.to_string(), help.to_string()))
                }
            })
            .collect();
        self.shortcuts(&shortcuts)
    }

    pub fn footer<'a, T>(&self, text: T) -> Paragraph<'a>
    where
        T: Into<Text<'a>>,
    {
        Paragraph::new(text).block(
            Block::default()
                .padding(Padding::horizontal(1))
                .style(Style::default().bg(self.background)),
        )
    }

    pub fn block(&self) -> Block<'_> {
        Block::default().style(Style::default().fg(self.foreground).bg(self.background))
    }

    pub fn popup_block<'a>(&self, title: &'a str) -> Block<'a> {
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.active))
            .style(self.global_style())
            .title(format!(" {} ", title))
            .title_alignment(ratatui::layout::Alignment::Center)
            .padding(Padding::horizontal(1))
    }

    /// Highlights code using the current theme's syntax highlighting.
    /// Returns ratatui [`Text`] with styled spans, or plain text on failure.
    #[tracing::instrument(
        level = "debug",
        name = "syntax_highlight",
        skip_all,
        fields(language = ext, bytes = code.len())
    )]
    pub fn highlight(&self, code: &str, ext: &str) -> Text<'static> {
        let syntax = find_syntax_or_default(ext, code);
        let mut h = HighlightLines::new(syntax, &self.syntect);

        let mut lines: Vec<Line<'static>> = Vec::new();
        for line in LinesWithEndings::from(code) {
            let Ok(ranges) = h.highlight_line(line, &SYNTAX) else {
                return Text::raw(code.to_owned());
            };

            let spans: Vec<Span<'static>> = ranges
                .into_iter()
                .map(|(style, text)| {
                    Span::styled(
                        strip_line_ending(text).to_owned(),
                        syntect_style_to_ratatui(style),
                    )
                })
                .collect();
            lines.push(Line::from(spans));
        }
        Text::from(lines)
    }
}

fn strip_line_ending(text: &str) -> &str {
    text.strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .or_else(|| text.strip_suffix('\r'))
        .unwrap_or(text)
}

fn syntect_style_to_ratatui(style: syn::Style) -> Style {
    let mut out = Style::default();
    let fg = style.foreground;
    if fg.a > 0 {
        out = out.fg(Color::Rgb(fg.r, fg.g, fg.b));
    }

    // Syntax-scope backgrounds are applied by the render pipeline instead.
    if style.font_style.contains(syn::FontStyle::BOLD) {
        out = out.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(syn::FontStyle::ITALIC) {
        out = out.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(syn::FontStyle::UNDERLINE) {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    out
}

/// Resolves Markdown styles directly from the theme's scope rules.
fn syntect_markdown_styles(theme: &syn::Theme) -> Option<MarkdownStyles> {
    const HEADING: &str = "text.html.markdown markup.heading.markdown markup.heading.1.markdown entity.name.section.markdown";
    const HEADING_MARKER: &str = "text.html.markdown markup.heading.markdown markup.heading.1.markdown punctuation.definition.heading.markdown";

    let highlighter = syn::Highlighter::new(theme);
    let style = |scope: &str| {
        let stack = ScopeStack::from_str(scope).ok()?;
        Some(syntect_style_to_ratatui(
            highlighter.style_for_stack(stack.as_slice()),
        ))
    };

    Some(MarkdownStyles {
        text: syntect_style_to_ratatui(highlighter.get_default()),
        heading_marker: style(HEADING_MARKER)?,
        heading: style(HEADING)?,
    })
}

/// Finds syntax by name or its content.
fn find_syntax(name: &str, code: &str) -> Option<&'static SyntaxReference> {
    SYNTAX
        .find_syntax_by_token(name)
        .or_else(|| SYNTAX.find_syntax_by_first_line(code))
}

/// Finds syntax by name or returns the default syntax.
pub(crate) fn find_syntax_or_default(name: &str, code: &str) -> &'static SyntaxReference {
    find_syntax(name, code).unwrap_or_else(|| {
        SYNTAX
            .find_syntax_by_extension(DEFAULT_SYNTAX)
            .unwrap_or_else(|| {
                tracing::warn!("Default syntax not found, falling back to first available");
                SYNTAX.syntaxes().first().expect("No syntaxes loaded")
            })
    })
}

fn to_color(color: Option<syn::Color>) -> Color {
    match color {
        Some(c) => Color::Rgb(c.r, c.g, c.b),
        None => Color::Reset,
    }
}

fn find_active_color(theme: &syn::Theme) -> Option<syn::Color> {
    let hl = syn::Highlighter::new(theme);

    // Prioritize vibrant syntax colors for the UI accent
    ["keyword", "entity.name.function", "constant.numeric"]
        .iter()
        .find_map(|scope| {
            ScopeStack::from_str(scope)
                .ok()
                .map(|stack| hl.style_for_stack(stack.as_slice()).foreground)
        })
        .or(theme.settings.active_guide)
        .or(theme.settings.accent)
}

fn is_greenish(r: u8, g: u8, b: u8) -> bool {
    g > r && g > b || (g > r && b > r && g >= 150 && b >= 150)
}

fn is_reddish(r: u8, g: u8, b: u8) -> bool {
    r > g && r > b || (r > g && b > g && r >= 200)
}

fn find_success_color(theme: &syn::Theme) -> Option<syn::Color> {
    let hl = syn::Highlighter::new(theme);

    [
        "markup.inserted.diff",
        "markup.inserted",
        "string",
        "support.type",
        "storage.type",
    ]
    .iter()
    .find_map(|scope| {
        ScopeStack::from_str(scope).ok().and_then(|stack| {
            let color = hl.style_for_stack(stack.as_slice()).foreground;
            if is_greenish(color.r, color.g, color.b) {
                Some(color)
            } else {
                None
            }
        })
    })
    .or(theme.settings.accent)
}

fn find_error_color(theme: &syn::Theme) -> Option<syn::Color> {
    let hl = syn::Highlighter::new(theme);
    [
        "markup.deleted.diff",
        "markup.deleted",
        "constant.language",
        "invalid",
    ]
    .iter()
    .find_map(|scope| {
        ScopeStack::from_str(scope).ok().and_then(|stack| {
            let color = hl.style_for_stack(stack.as_slice()).foreground;
            if is_reddish(color.r, color.g, color.b) {
                Some(color)
            } else {
                None
            }
        })
    })
    .or(theme.settings.accent)
}

fn is_yellowish(r: u8, g: u8, b: u8) -> bool {
    r > b && g > b && r.abs_diff(g) < 80
}

fn find_warning_color(theme: &syn::Theme) -> Option<syn::Color> {
    let hl = syn::Highlighter::new(theme);

    [
        "markup.changed.diff",
        "markup.changed",
        "meta.warning",
        "constant.numeric",
    ]
    .iter()
    .find_map(|scope| {
        ScopeStack::from_str(scope).ok().and_then(|stack| {
            let color = hl.style_for_stack(stack.as_slice()).foreground;
            if is_yellowish(color.r, color.g, color.b) {
                Some(color)
            } else {
                None
            }
        })
    })
    .or(theme.settings.accent)
}

fn find_muted_color(theme: &syn::Theme) -> Option<syn::Color> {
    let hl = syn::Highlighter::new(theme);

    ScopeStack::from_str("comment")
        .ok()
        .map(|stack| hl.style_for_stack(stack.as_slice()).foreground)
}

/// Determines if a color is light based on its luminance.
fn is_light(r: f32, g: f32, b: f32) -> bool {
    0.299 * r + 0.587 * g + 0.114 * b >= 128.0
}

/// Adjusts color lightness towards the opposite of the current contrast.
/// If color is dark, lightens it. If light, darkens it.
fn adjust_contrast(color: &Color, amount: i16) -> Color {
    let dir = if let Color::Rgb(r, g, b) = color {
        if is_light(*r as f32, *g as f32, *b as f32) {
            -1
        } else {
            1
        }
    } else {
        0
    };
    adjust_lightness(color, amount * dir)
}

/// Adjusts color lightness by the given amount (positive = lighter, negative = darker).
fn adjust_lightness(color: &Color, amount: i16) -> Color {
    let Color::Rgb(r, g, b) = color else {
        return *color;
    };
    let r = (*r as i16 + amount).clamp(0, 255) as u8;
    let g = (*g as i16 + amount).clamp(0, 255) as u8;
    let b = (*b as i16 + amount).clamp(0, 255) as u8;
    Color::Rgb(r, g, b)
}

/// Calculates code background based on main background
/// If background is dark, lighten it. If light, darken it.
fn calculate_code_background(bg: Option<syn::Color>) -> Color {
    if let Some(bg) = bg {
        adjust_contrast(&Color::Rgb(bg.r, bg.g, bg.b), 15)
    } else {
        Color::Indexed(236)
    }
}

/// Calculates contrast text color for accent background (Info Line)
fn calculate_info_foreground(base: Color) -> Color {
    match base {
        Color::Rgb(_, _, _) => adjust_contrast(&base, 70),
        _ => base,
    }
}

/// Calculates info background based on code background
/// If code background is dark, lighten it. If light, darken it.
fn calculate_info_background(base: Option<&Color>) -> Color {
    if let Some(color) = base {
        adjust_contrast(color, 15)
    } else {
        Color::Indexed(238)
    }
}

#[cfg(test)]
mod tests {

    use super::find_syntax_or_default;

    #[test]
    fn test_theme_loading() {
        // Test hardcoded theme (like btop)
        let theme = super::Theme::new("base16-ocean.dark", false);
        assert_ne!(theme.background, super::Color::Reset);
        assert_ne!(theme.foreground, super::Color::Reset);
        // Base16 Ocean Dark background is #2b303b (43, 48, 59)
        assert_eq!(theme.background, super::Color::Rgb(43, 48, 59));

        // Test transparent theme (respect terminal)
        let theme_transparent = super::Theme::new("base16-ocean.dark", true);
        assert_eq!(theme_transparent.background, super::Color::Reset);
        assert_eq!(theme_transparent.foreground, super::Color::Reset);
    }

    #[test]
    fn test_highlight_splits_lines_without_embedded_line_endings() {
        let theme = super::Theme::new("base16-ocean.dark", false);
        let highlighted = theme.highlight("one\ntwo\r\nthree", "txt");

        let rendered: Vec<String> = highlighted
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();

        assert_eq!(rendered, ["one", "two", "three"]);
        assert!(
            highlighted
                .lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .all(|span| !span.content.contains(['\r', '\n'])),
            "highlighted ratatui spans must not contain line endings"
        );
    }

    #[test]
    fn test_find_language() {
        [
            ("sh", "", "sh"),
            ("rb", "", "rb"),
            ("php", "", "php"),
            ("", "", "txt"),
            ("", "#!/usr/bin/sh", "sh"),
        ]
        .iter()
        .for_each(|(name, code, expected)| {
            let result = find_syntax_or_default(name, code);

            assert_eq!(result.file_extensions.first().unwrap(), *expected);
        })
    }
}

//! Source-preserving Markdown rendering for the preview's Markup mode.

use ratatui::{style::Style, text::Span};
use std::ops::Range;

use upmd_parser::nodes::{Node, NodeKind};

use super::{LogicalLine, LogicalLineSource, MarkdownRenderer, RenderState};

impl MarkdownRenderer<'_> {
    pub(super) fn render_markup(
        &self,
        nodes: &[Node],
        lines: &mut Vec<LogicalLine>,
        state: &mut RenderState,
    ) {
        let mut cursor = 0;
        for node in nodes {
            self.render_markup_gap(cursor..node.range.start, lines, state);
            self.render_markup_node(node, lines, state);
            cursor = cursor.max(node.range.end);
        }
        self.render_markup_gap(cursor..self.source.len(), lines, state);
    }

    fn markup_code_prefix(&self, content: String) -> Span<'static> {
        Span::styled(
            content,
            Style::default()
                .fg(self.theme.muted)
                .bg(self.theme.background),
        )
    }

    fn render_markup_node(
        &self,
        node: &Node,
        lines: &mut Vec<LogicalLine>,
        state: &mut RenderState,
    ) {
        let parent_identity = state.begin_node();
        let start_line = lines.len();

        match &node.kind {
            NodeKind::Code(code_id) => self.render_markup_code(*code_id, lines, state),
            NodeKind::BlockQuote(children) => {
                let mut codes = Vec::new();
                collect_codes(children, state.quote_depth + 1, &mut codes);
                self.render_markup_code_ranges(node.range.clone(), &codes, lines, state);
            }
            NodeKind::List(items) => {
                let mut codes = Vec::new();
                for item in items {
                    collect_codes(&item.children, state.quote_depth, &mut codes);
                }
                self.render_markup_code_ranges(node.range.clone(), &codes, lines, state);
            }
            _ => self.render_markup_source(node.range.clone(), lines, state),
        }

        match node.kind {
            NodeKind::Heading { .. } => state.snap.title_line = Some(start_line),
            NodeKind::Paragraph(_) => state.snap.description_line = Some(start_line),
            _ => {}
        }
        state.end_node(parent_identity);
    }

    fn render_markup_code_ranges(
        &self,
        range: Range<usize>,
        codes: &[(&Node, usize)],
        lines: &mut Vec<LogicalLine>,
        state: &mut RenderState,
    ) {
        let mut cursor = range.start;
        for (code, quote_depth) in codes {
            let raw_end = self.code_prefix_start(cursor, code.range.start, *quote_depth);
            self.render_markup_source(cursor..raw_end, lines, state);

            let parent_depth = std::mem::replace(&mut state.quote_depth, *quote_depth);
            let prefix = self.source[raw_end..code.range.start].to_owned();
            let prefixes = if prefix.is_empty() {
                Vec::new()
            } else {
                vec![self.markup_code_prefix(prefix)]
            };
            let parent_prefixes = std::mem::replace(&mut state.prefixes, prefixes);
            self.render_markup_node(code, lines, state);
            state.prefixes = parent_prefixes;
            state.quote_depth = parent_depth;
            cursor = self.after_line_ending(cursor.max(code.range.end));
        }
        self.render_markup_source(cursor..range.end, lines, state);
    }

    fn render_markup_code(
        &self,
        code_id: u32,
        lines: &mut Vec<LogicalLine>,
        state: &mut RenderState,
    ) {
        self.render_code(code_id, lines, state);
        if lines.last().is_some_and(|line| {
            line.code_id == Some(code_id) && matches!(line.source, LogicalLineSource::Newline)
        }) {
            lines.pop();
        }
    }

    fn code_prefix_start(&self, start: usize, end: usize, quote_depth: usize) -> usize {
        let line_start = self.source[start..end]
            .rfind('\n')
            .map_or(start, |offset| start + offset + 1);
        let prefix = &self.source[line_start..end];
        let marker_count = prefix.bytes().filter(|byte| *byte == b'>').count();
        let only_quote_prefix = marker_count == quote_depth
            && prefix
                .bytes()
                .all(|byte| byte == b'>' || byte.is_ascii_whitespace());
        if only_quote_prefix {
            line_start
        } else {
            end
        }
    }

    fn after_line_ending(&self, offset: usize) -> usize {
        match self.source.get(offset..) {
            Some(rest) if rest.starts_with("\r\n") => offset + 2,
            Some(rest) if rest.starts_with('\n') => offset + 1,
            _ => offset,
        }
    }

    fn render_markup_source(
        &self,
        range: Range<usize>,
        lines: &mut Vec<LogicalLine>,
        state: &mut RenderState,
    ) {
        let Some(source) = self.source.get(range) else {
            return;
        };
        for (index, line) in source.lines().enumerate() {
            self.push_unquoted_line(lines, LogicalLine::markup_text(line, index == 0), state);
        }
    }

    fn render_markup_gap(
        &self,
        range: Range<usize>,
        lines: &mut Vec<LogicalLine>,
        state: &mut RenderState,
    ) {
        let Some(gap) = self.source.get(range.clone()) else {
            return;
        };
        let follows_line_ending = range.start == 0
            || self
                .source
                .get(..range.start)
                .is_some_and(|source| source.ends_with('\n'));
        let newline_count = gap.bytes().filter(|byte| *byte == b'\n').count();
        let blank_lines = if follows_line_ending {
            newline_count
        } else {
            newline_count.saturating_sub(1)
        };
        for _ in 0..blank_lines {
            self.push_unquoted_line(lines, LogicalLine::newline(), state);
        }
    }
}

fn collect_codes<'a>(nodes: &'a [Node], quote_depth: usize, codes: &mut Vec<(&'a Node, usize)>) {
    for node in nodes {
        match &node.kind {
            NodeKind::Code(_) => codes.push((node, quote_depth)),
            NodeKind::BlockQuote(children) => {
                collect_codes(children, quote_depth + 1, codes);
            }
            NodeKind::List(items) => {
                for item in items {
                    collect_codes(&item.children, quote_depth, codes);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::collections::HashMap;

    fn test_theme() -> Theme {
        Theme::new("base16-ocean.dark", false)
    }

    fn render_markup(markdown: &str) -> Vec<LogicalLine> {
        let doc = upmd_parser::new().parse(markdown);
        let theme = test_theme();
        let outputs = HashMap::new();
        let renderer = MarkdownRenderer::new(&doc.source, &theme, &outputs, &doc.codes, 10, 80)
            .mode(RenderMode::Markup);
        renderer.render(&doc.nodes).lines
    }

    fn markup_text(lines: &[LogicalLine]) -> String {
        let theme = test_theme();
        let ctx = RenderContext {
            theme: &theme,
            active_code_id: None,
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: 80,
        };
        lines
            .iter()
            .map(|l| l.render_plain(&ctx).to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn markup_preserves_non_code_source() {
        for markdown in [
            "[**bold** text](url \"title\")",
            "***bold italic***",
            "**bold *italic* tail**",
            "x ![**styled** alt](image.png) y",
            "literal \\* [ ] | text",
            "[label \\] text](<path with spaces> \"title with &quot;quote&quot;\")",
            "# heading\n\nparagraph",
            "> quoted\n>\n> next",
            "> outer\n>\n> > nested",
            "- first\n\n- second",
            "![alt \\] text](<image path.png>)",
        ] {
            assert_eq!(
                markup_text(&render_markup(markdown)),
                markdown,
                "input: {markdown}"
            );
        }
    }

    #[test]
    fn markup_preserves_spacing_around_interactive_code() {
        let quoted = markup_text(&render_markup(
            "> quoted\n>\n> ```bash\n> echo hello\n> ```",
        ));
        assert!(quoted.starts_with("> quoted\n"), "got: {quoted}");
        assert!(!quoted.contains("> >"), "got: {quoted}");
        assert!(!quoted.ends_with('\n'), "got: {quoted}");

        let followed = markup_text(&render_markup("```bash\necho hello\n```\n\nnext"));
        assert!(followed.contains("echo hello\n\nnext"), "got: {followed}");
        assert!(
            !followed.contains("echo hello\n\n\nnext"),
            "got: {followed}"
        );
    }

    #[test]
    fn markup_reuses_visual_code_lines() {
        let doc = upmd_parser::new().parse("```bash\necho hello\n```");
        let theme = test_theme();
        let outputs = HashMap::new();
        let render = |mode| {
            MarkdownRenderer::new(&doc.source, &theme, &outputs, &doc.codes, 10, 80)
                .mode(mode)
                .render(&doc.nodes)
                .lines
        };
        let code_lines = |lines: &[LogicalLine]| {
            lines
                .iter()
                .filter(|line| {
                    line.code_id.is_some() && !matches!(line.source, LogicalLineSource::Newline)
                })
                .map(|line| {
                    (
                        line.text_content(),
                        line.code_id,
                        line.is_block_start,
                        line.is_code_start,
                        line.is_running,
                        line.gutter_fg,
                    )
                })
                .collect::<Vec<_>>()
        };
        let visual = render(RenderMode::Visual);
        let markup = render(RenderMode::Markup);
        let markup_code_lines = code_lines(&markup);

        assert!(markup_code_lines
            .iter()
            .any(|line| line.0 == "echo hello" && line.1 == Some(1)));
        assert_eq!(markup_code_lines, code_lines(&visual));
    }
}

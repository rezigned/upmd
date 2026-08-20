//! Semantic terminal rendering for the preview's Visual mode.

use std::rc::Rc;

use ratatui::{style::Style, text::Span};
use upmd_parser::nodes::{InlineSpan, ListItem, ListKind, Node, NodeKind, TaskStatus};

use crate::apps::config::PREVIEW_FRAME_OVERHEAD;

use super::{
    owned_table, render_table, split_span_lines, FrontmatterBlock, LogicalLine, LogicalLineSource,
    MarkdownHtml, MarkdownRenderer, MarkdownTable, RenderState, MAX_BLOCKQUOTE_MARKER_INDENT,
};

impl MarkdownRenderer<'_> {
    pub(super) fn render_visual(
        &self,
        nodes: &[upmd_parser::nodes::Node],
        lines: &mut Vec<LogicalLine>,
        state: &mut RenderState,
    ) {
        self.render_nodes(nodes, lines, state, Self::render_node);
        if lines.last().is_some_and(LogicalLine::is_newline) {
            lines.pop();
        }
    }

    /// Whether this heading renders its own visual separator.
    fn has_heading_rule(node: &Node) -> bool {
        matches!(
            &node.kind,
            NodeKind::Heading { level, .. } if *level <= 2
        )
    }

    /// Renders each node with one trailing separator in the current nesting
    /// context, except after H1/H2. The outermost render removes the final one.
    fn render_nodes(
        &self,
        nodes: &[Node],
        lines: &mut Vec<LogicalLine>,
        state: &mut RenderState,
        mut render: impl FnMut(&Self, &Node, &mut Vec<LogicalLine>, &mut RenderState),
    ) {
        for node in nodes {
            render(self, node, lines, state);
            if lines.last().is_some_and(LogicalLine::is_newline) {
                lines.pop();
            }
            if !Self::has_heading_rule(node) {
                self.push_line(lines, LogicalLine::newline(), state);
            }
        }
    }

    fn visual_quote_prefix(&self) -> Span<'static> {
        Span::styled(
            "▎ ",
            Style::default()
                .fg(self.theme.muted)
                .bg(self.theme.background),
        )
    }

    pub(super) fn render_node(
        &self,
        node: &upmd_parser::nodes::Node,
        lines: &mut Vec<LogicalLine>,
        state: &mut RenderState,
    ) {
        use upmd_parser::nodes::NodeKind;
        let parent_identity = state.begin_node();
        match &node.kind {
            NodeKind::HtmlBlock => {
                let html = self.source[node.range.clone()].to_owned();
                let block = Rc::new(MarkdownHtml::new(html));
                for row_idx in 0..block.len() {
                    self.push_line(
                        lines,
                        LogicalLine::html(Rc::clone(&block), row_idx, row_idx == 0),
                        state,
                    );
                }
            }
            NodeKind::Frontmatter { style, raw } => {
                let block = Rc::new(FrontmatterBlock::new(
                    *style,
                    raw.resolve(self.source).to_owned(),
                ));
                for row_idx in 0..block.len() {
                    self.push_line(
                        lines,
                        LogicalLine::frontmatter(Rc::clone(&block), row_idx, row_idx == 0),
                        state,
                    );
                }
            }
            NodeKind::Text(t) => {
                self.render_highlighted_lines(t, lines, true, state);
            }
            NodeKind::Paragraph(t) => {
                if let Some(idx) = self.render_highlighted_lines(t, lines, true, state) {
                    state.snap.description_line = Some(idx);
                }
            }
            NodeKind::BlockQuote(children) => {
                // Blockquotes are visual nesting, but title/description snap
                // context is scoped: a quoted paragraph should not become the
                // snap target for a following non-quoted code block, and an
                // outer paragraph should not snap to quoted code.
                let parent_snap = std::mem::take(&mut state.snap);
                state.prefixes.push(self.visual_quote_prefix());
                self.render_nodes(children, lines, state, Self::render_node);
                state.prefixes.pop();
                state.snap = parent_snap;
            }
            NodeKind::Heading { text: t, level } => {
                let line_idx = lines.len();
                let prefix = "#".repeat(*level as usize);
                let mut content = t.clone();
                if let Some(first) = content.first_mut() {
                    first.text = first
                        .text(self.source)
                        .trim_start_matches('#')
                        .trim_start()
                        .into();
                }
                content.insert(
                    0,
                    InlineSpan {
                        text: format!("{prefix} ").into(),
                        style: Vec::new(),
                    },
                );
                self.push_line(
                    lines,
                    LogicalLine::heading_lazy_spans(content, self.source, *level),
                    state,
                );
                if Self::has_heading_rule(node) {
                    self.push_line(lines, LogicalLine::heading_rule(), state);
                }
                state.snap.title_line = Some(line_idx);
            }
            NodeKind::List(items) => self.render_list(items, lines, state),
            NodeKind::Code(code_id) => self.render_code(*code_id, lines, state),
            NodeKind::Table(table) => {
                let table_width = self
                    .viewport_width
                    .saturating_sub(PREVIEW_FRAME_OVERHEAD)
                    .saturating_sub(state.prefix_width())
                    .max(1);
                let table = owned_table(table, self.source);
                let rendered = render_table(&table, "", self.theme, table_width);
                let row_count = rendered.len();
                let table = Rc::new(MarkdownTable::new(table, table_width, rendered));
                for row_idx in 0..row_count {
                    let initial = table.line(row_idx, self.theme, table_width);
                    self.push_line(
                        lines,
                        LogicalLine::table(Rc::clone(&table), row_idx, initial),
                        state,
                    );
                }
            }
            NodeKind::ThematicBreak => {
                self.push_line(lines, LogicalLine::thematic_break(), state);
            }
            NodeKind::Image { alt, src } => {
                let line = LogicalLine {
                    source: LogicalLineSource::Image {
                        alt: alt.clone(),
                        src: src.clone(),
                    },
                    is_block_start: true,
                    ..LogicalLine::default()
                };
                self.push_line(lines, line, state);
            }
        }
        state.end_node(parent_identity);
    }

    fn render_highlighted_lines(
        &self,
        spans: &[InlineSpan],
        lines: &mut Vec<LogicalLine>,
        block_start: bool,
        state: &mut RenderState,
    ) -> Option<usize> {
        let start_idx = lines.len();
        let mut first = block_start;
        let mut emitted = false;
        for line in split_span_lines(spans, self.source) {
            self.push_line(
                lines,
                LogicalLine::text_lazy_spans(line, self.source, first),
                state,
            );
            first = false;
            emitted = true;
        }
        if emitted {
            Some(start_idx)
        } else {
            None
        }
    }

    /// Returns source indentation for a nested block after stripping enclosing
    /// blockquote prefixes.
    fn nested_block_indent(&self, offset: usize) -> usize {
        let line_start = self.source[..offset]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let mut prefix = &self.source[line_start..offset];

        loop {
            let spaces = prefix
                .bytes()
                .take(MAX_BLOCKQUOTE_MARKER_INDENT)
                .take_while(|byte| *byte == b' ')
                .count();
            let Some(rest) = prefix.get(spaces..) else {
                break;
            };
            let Some(rest) = rest.strip_prefix('>') else {
                break;
            };
            prefix = rest
                .strip_prefix(' ')
                .or_else(|| rest.strip_prefix('\t'))
                .unwrap_or(rest);
        }

        prefix
            .chars()
            .fold(0, |width, ch| width + if ch == '\t' { 4 } else { 1 })
    }

    fn render_list(
        &self,
        items: &[ListItem],
        lines: &mut Vec<LogicalLine>,
        state: &mut RenderState,
    ) {
        for (index, item) in items.iter().enumerate() {
            self.render_list_item(item, index == 0, lines, state);
        }
    }

    fn render_list_item(
        &self,
        item: &ListItem,
        is_list_start: bool,
        lines: &mut Vec<LogicalLine>,
        state: &mut RenderState,
    ) {
        let indent = " ".repeat(item.depth.saturating_sub(1) * 4);
        let (marker, color) = match &item.kind {
            ListKind::Bullet => ("• ".to_string(), self.theme.foreground),
            ListKind::Ordered(number) => (format!("{number}. "), self.theme.foreground),
            ListKind::Task(TaskStatus::Checked) => ("󰱒  ".to_string(), self.theme.success),
            ListKind::Task(TaskStatus::InProgress) => ("󰡖  ".to_string(), self.theme.muted),
            ListKind::Task(TaskStatus::Unchecked) => ("󰄱  ".to_string(), self.theme.muted),
        };
        let continuation = format!("{}{}", indent, " ".repeat(marker.chars().count()));

        let (paragraph, children) = match item.children.split_first() {
            Some((
                Node {
                    kind: NodeKind::Paragraph(spans),
                    ..
                },
                children,
            )) => (Some(spans.as_slice()), children),
            _ => (None, item.children.as_slice()),
        };
        let paragraph_lines = paragraph.map_or_else(
            || vec![Vec::new()],
            |spans| split_span_lines(spans, self.source),
        );

        for (line_index, line) in paragraph_lines.into_iter().enumerate() {
            let prefix = if line_index == 0 {
                Span::styled(format!("{indent}{marker}"), Style::default().fg(color))
            } else {
                Span::raw(continuation.clone())
            };
            self.push_line(
                lines,
                LogicalLine::list_item_spans(
                    line,
                    self.source,
                    prefix,
                    Span::raw(continuation.clone()),
                    is_list_start && line_index == 0,
                ),
                state,
            );
        }

        self.render_nodes(children, lines, state, Self::render_list_child);
    }

    fn render_list_child(
        &self,
        child: &Node,
        lines: &mut Vec<LogicalLine>,
        state: &mut RenderState,
    ) {
        let indent = if matches!(child.kind, NodeKind::List(_)) {
            0
        } else {
            self.nested_block_indent(child.range.start)
        };
        if indent == 0 {
            self.render_node(child, lines, state);
            return;
        }

        state.prefixes.push(Span::styled(
            " ".repeat(indent),
            Style::default().bg(self.theme.background),
        ));
        self.render_node(child, lines, state);
        state.prefixes.pop();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use insta::assert_snapshot;
    use ratatui::style::Color;

    use crate::apps::theme::Theme;
    use crate::apps::tui::testutil::ansi_line;

    use super::super::*;

    fn test_theme() -> Theme {
        Theme::new("base16-ocean.dark", false)
    }

    fn test_ctx(theme: &Theme, width: usize) -> RenderContext<'_> {
        RenderContext {
            theme,
            active_code_id: None,
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: width,
        }
    }

    fn render_markdown(markdown: &str) -> RenderedMarkdown {
        let doc = upmd_parser::new().parse(markdown);
        let theme = test_theme();
        let outputs = HashMap::new();
        let renderer = MarkdownRenderer::new(&doc.source, &theme, &outputs, &doc.codes, 10, 80);
        renderer.render(&doc.nodes)
    }

    fn render_nodes(markdown: &str) -> Vec<LogicalLine> {
        render_markdown(markdown).lines
    }

    fn source_label(line: &LogicalLine) -> String {
        match &line.source {
            LogicalLineSource::Text(_) => "Text".to_string(),
            LogicalLineSource::ListItem(_) => "ListItem".to_string(),
            LogicalLineSource::Heading { level, .. } => format!("Heading({level})"),
            LogicalLineSource::CodeInfo(_) => "CodeInfo".to_string(),
            LogicalLineSource::CodeBody(_) => "CodeBody".to_string(),
            LogicalLineSource::Output(_) => "Output".to_string(),
            LogicalLineSource::Html { .. } => "Html".to_string(),
            LogicalLineSource::Frontmatter { .. } => "Frontmatter".to_string(),
            LogicalLineSource::TableRow { .. } => "Table".to_string(),
            LogicalLineSource::Image { .. } => "Image".to_string(),
            LogicalLineSource::ThematicBreak => "ThematicBreak".to_string(),
            LogicalLineSource::Newline => "Newline".to_string(),
        }
    }

    fn logical_line_summary(lines: &[LogicalLine]) -> String {
        lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let text = l.text_content();
                let char_count = text.chars().count();
                let preview = if char_count > 60 {
                    let truncated: String = text.chars().take(57).collect();
                    format!("{}...", truncated)
                } else {
                    text
                };
                if preview.is_empty() {
                    format!("{:2}: [{}]", i, source_label(l))
                } else {
                    format!("{:2}: [{}] {}", i, source_label(l), preview)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn code_start_lines(lines: &[LogicalLine]) -> Vec<(usize, String, Option<CodeId>, String)> {
        lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.is_code_start)
            .map(|(idx, line)| (idx, source_label(line), line.code_id, line.text_content()))
            .collect()
    }

    #[test]
    fn test_snap_heading_to_code_start() {
        let lines = render_nodes("# Title\n\n```bash\necho hi\n```");
        let starts = code_start_lines(&lines);

        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].0, 0);
        assert_eq!(starts[0].1, "Heading(1)");
        assert_eq!(starts[0].3, "# Title");
    }

    #[test]
    fn test_snap_paragraph_fallback_to_code_start() {
        let lines = render_nodes("Intro paragraph.\n\n```bash\necho hi\n```");
        let starts = code_start_lines(&lines);

        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].0, 0);
        assert_eq!(starts[0].1, "Text");
        assert_eq!(starts[0].3, "Intro paragraph.");
    }

    #[test]
    fn test_adjacent_code_blocks_do_not_reuse_snap_context() {
        let lines = render_nodes("# Title\n\n```bash\necho one\n```\n\n```bash\necho two\n```");
        let starts = code_start_lines(&lines);

        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0].1, "Heading(1)");
        assert_eq!(starts[0].3, "# Title");
        assert_eq!(starts[1].1, "CodeInfo");
        assert_ne!(starts[0].2, starts[1].2);
    }

    #[test]
    fn test_blockquote_snap_context_does_not_leak_outward() {
        let lines = render_nodes("> quoted note\n\n```bash\necho hi\n```");
        let starts = code_start_lines(&lines);

        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].1, "CodeInfo");
        assert_eq!(lines[0].text_content(), "quoted note");
        assert!(lines[0].code_id.is_none());
    }

    #[test]
    fn test_blockquote_snap_context_does_not_leak_inward() {
        let lines = render_nodes("Intro paragraph.\n\n> ```bash\n> echo hi\n> ```");
        let starts = code_start_lines(&lines);

        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].1, "CodeInfo");
        assert_eq!(lines[0].text_content(), "Intro paragraph.");
        assert!(lines[0].code_id.is_none());
    }

    #[test]
    fn test_code_prefix_overhead_tracks_blockquote_depth() {
        for (name, markdown, expected) in [
            ("flat", "```bash\necho hi\n```", 0),
            ("single blockquote", "> ```bash\n> echo hi\n> ```", 2),
            ("nested blockquote", "> > ```bash\n> > echo hi\n> > ```", 4),
        ] {
            let rendered = render_markdown(markdown);
            let code_id = rendered
                .lines
                .iter()
                .find(|line| line.is_code_body())
                .and_then(|line| line.code_id)
                .unwrap_or_else(|| panic!("expected code body for {name}"));

            assert_eq!(
                rendered
                    .code_prefix_overhead
                    .get(&code_id)
                    .copied()
                    .unwrap_or(0),
                expected,
                "{name} code prefix overhead should match its quote depth"
            );
        }
    }

    #[test]
    fn test_blockquote_list_item_renders_quote_and_list_prefixes() {
        let theme = test_theme();
        let ctx = test_ctx(&theme, 80);
        let lines = render_nodes("> - quoted item");
        let list_item = lines
            .iter()
            .find(|line| matches!(line.source, LogicalLineSource::ListItem(_)))
            .expect("expected a list item inside the blockquote");

        assert_eq!(list_item.prefix_width(), 4);
        assert_eq!(list_item.render(&ctx).to_string(), "▎ • quoted item");
    }

    #[test]
    fn test_nested_list_blocks_preserve_source_indentation() {
        let theme = test_theme();
        let ctx = test_ctx(&theme, 80);
        let lines = render_nodes(
            "1. First ordered item\n\n   Paragraph nested under the item.\n\n   ```python\n   print(\"list code\")\n   ```\n\n   > Quote nested under the item.\n   >\n   > - Quoted bullet\n\n2. Second ordered item\n\n   | A | B |\n   |---|---|\n   | x | y |\n\n- [ ] Parent task\n\n  > Task quote.\n\n  ```sh\n  echo task\n  ```",
        );
        let rendered = lines
            .iter()
            .map(|line| line.render(&ctx).to_string())
            .collect::<Vec<_>>();

        for expected in [
            "   Paragraph nested under the item.",
            "   ▎ 1",
            "   ▎ print(\"list code\")",
            "   ▎ Quote nested under the item.",
            "   ▎ • Quoted bullet",
            "   ┌",
            "  ▎ Task quote.",
            "  ▎ 2",
            "  ▎ echo task",
        ] {
            assert!(
                rendered.iter().any(|line| line.starts_with(expected)),
                "missing {expected:?} in {rendered:#?}"
            );
        }
    }

    #[test]
    fn test_render_headings() {
        let lines = render_nodes("# Hello\n\n## World\n\n### Rust");
        assert_snapshot!("headings", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_paragraph() {
        let lines = render_nodes("This is a paragraph.\n\nWith a blank line.");
        assert_snapshot!("paragraph", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_fenced_code_block() {
        let lines = render_nodes("```bash\necho hello\n```");
        assert_snapshot!("fenced_code", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_code_with_language_attr() {
        let lines = render_nodes("```python [os:linux]\nprint('hi')\n```");
        assert_snapshot!("code_with_attr", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_bullet_list() {
        let lines = render_nodes("- item one\n- item two\n- item three");
        assert_snapshot!("bullet_list", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_ordered_list() {
        let lines = render_nodes("1. first\n2. second\n3. third");
        assert_snapshot!("ordered_list", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_task_list() {
        let lines = render_nodes("- [ ] unchecked\n- [x] checked\n- [-] in progress");
        assert_snapshot!("task_list", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_thematic_break() {
        let lines = render_nodes("above\n\n-----\n\nbelow");
        assert_snapshot!("thematic_break", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_github_heading_rules() {
        for level in 1..=6 {
            let lines = render_nodes(&format!("{} Heading", "#".repeat(level)));
            assert_eq!(
                lines
                    .iter()
                    .any(|line| matches!(line.source, LogicalLineSource::ThematicBreak)),
                level <= 2,
                "H{level}"
            );
        }
    }

    #[test]
    fn test_render_blockquote() {
        let lines = render_nodes(
            "> quoted paragraph\n\
             > \n\
             > ```bash\n\
             > echo hi\n\
             > ```\n\
             \n\
             > - quoted item\n\
             > - nested quote\n\
             >   - deeper",
        );
        assert_snapshot!("blockquote", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_html_tabs_preserved_logically_expanded_visually() {
        let lines = render_nodes("<pre>\n\tindented\n</pre>\n");
        let html: Vec<&LogicalLine> = lines
            .iter()
            .filter(|l| matches!(l.source, LogicalLineSource::Html { .. }))
            .collect();
        assert_eq!(html.len(), 3);
        assert_eq!(html[1].text_content(), "\tindented");

        let theme = test_theme();
        let ctx = test_ctx(&theme, 80);
        let painted = html[1].render(&ctx).to_string();
        assert!(!painted.contains('\t'));
        assert!(painted.contains("    indented"));
        assert_eq!(html[1].render_plain(&ctx).to_string(), painted);
    }

    #[test]
    fn test_render_html_styles() {
        let theme = test_theme();
        let ctx = test_ctx(&theme, 80);

        for (name, line) in [
            (
                "block",
                render_nodes("<div class=\"card\">\n</div>\n")
                    .iter()
                    .find(|l| matches!(l.source, LogicalLineSource::Html { .. }))
                    .unwrap()
                    .render(&ctx),
            ),
            (
                "inline",
                render_nodes("x <img src=\"a.png\"> y")
                    .iter()
                    .find(|l| matches!(l.source, LogicalLineSource::Text(_)))
                    .unwrap()
                    .render(&ctx),
            ),
        ] {
            let mut distinct: Vec<Color> = Vec::new();
            for span in &line.spans {
                if let Some(fg) = span.style.fg {
                    if !distinct.contains(&fg) {
                        distinct.push(fg);
                    }
                }
            }
            assert!(
                distinct.len() >= 2,
                "{name} HTML should use multiple syntax colors, got {distinct:?}"
            );
        }
    }

    #[test]
    fn test_render_inline_heading_styles() {
        let theme = test_theme();
        let ctx = test_ctx(&theme, 80);
        let lines = render_nodes("# **Bold** title");
        let heading = lines
            .iter()
            .find(|l| l.heading_level().is_some())
            .expect("expected a heading");
        let rendered = heading.render(&ctx);

        assert_snapshot!("inline_heading_styles", ansi_line(&rendered));
    }

    #[test]
    fn test_render_inline_list_item_styles() {
        let theme = test_theme();
        let ctx = test_ctx(&theme, 80);
        let lines = render_nodes("- **bold item**");
        let item = lines
            .iter()
            .find(|l| matches!(l.source, LogicalLineSource::ListItem(_)))
            .expect("expected a list item");
        let rendered = item.render(&ctx);

        assert_snapshot!("inline_list_item_styles", ansi_line(&rendered));
    }

    #[test]
    fn test_split_span_lines() {
        let spans = vec![
            InlineSpan {
                text: "line one\n".into(),
                style: vec![InlineStyle::Bold],
            },
            InlineSpan {
                text: "line two".into(),
                style: vec![],
            },
        ];
        let lines = split_span_lines(&spans, "");
        assert_eq!(lines.len(), 2);
        assert_eq!(inline_text(&lines[0], ""), "line one");
        assert_eq!(lines[0][0].style, vec![InlineStyle::Bold]);
        assert_eq!(inline_text(&lines[1], ""), "line two");

        // Empty input yields no lines.
        assert!(split_span_lines(&[], "").is_empty());
        // Trailing newline drops the empty tail.
        assert_eq!(
            split_span_lines(
                &[InlineSpan {
                    text: "a\n".into(),
                    style: vec![],
                }],
                "",
            )
            .len(),
            1
        );
    }

    #[test]
    fn test_render_inline_styles_snapshot() {
        let lines = render_nodes(
            "**bold** *italic* ~~strike~~ `code` [link](https://x.dev) ![alt text](img.png)\n\
             \n\
             ## Heading with **bold**\n\
             \n\
             - **bold item**\n\
             - *italic item*",
        );
        assert_snapshot!("inline_styles", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_table() {
        let lines =
            render_nodes("| Name  | Age |\n|-------|-----|\n| Alice | 30  |\n| Bob   | 25  |");
        assert_snapshot!("table", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_table_inline_styles() {
        let theme = test_theme();
        let ctx = test_ctx(&theme, 80);
        let lines = render_nodes(
            "| Name | Reference |\n\
             |------|-----------|\n\
             | **bold** | [docs](https://x.dev) |",
        );
        let row = lines
            .iter()
            .filter(|line| matches!(line.source, LogicalLineSource::TableRow { .. }))
            .map(|line| line.render(&ctx))
            .find(|line| line.to_string().contains("bold"))
            .expect("expected styled table body row");

        assert_snapshot!("table_inline_styles", ansi_line(&row));
    }

    #[test]
    fn test_render_mixed_runbook() {
        let input = r#"# Setup

Install dependencies.

```bash
npm install
```

## Test

Run the test suite.

```python [os:linux]
pytest tests/
```
"#;
        let lines = render_nodes(input);
        assert_snapshot!("mixed_runbook", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_empty_input() {
        let lines = render_nodes("");
        assert!(lines.is_empty());
    }

    #[test]
    fn test_render_code_ids_sequence() {
        let lines = render_nodes("```bash\necho a\n```\n\n```python\nprint(1)\n```");
        // Both code blocks should have distinct IDs
        let code_lines: Vec<_> = lines.iter().filter(|line| line.is_code_info()).collect();
        assert_eq!(code_lines.len(), 2);
        let id0 = code_lines[0].code_id;
        let id1 = code_lines[1].code_id;
        assert!(id0.is_some());
        assert!(id1.is_some());
        assert_ne!(id0, id1);
    }

    #[test]
    fn test_render_code_info_line_has_code_id() {
        let lines = render_nodes("```bash\necho test\n```");
        let code_info = lines.iter().find(|line| line.is_code_info());
        assert!(code_info.is_some());
        assert!(code_info.unwrap().code_id.is_some());
    }

    #[test]
    fn test_render_code_body_associated_with_code_id() {
        let lines = render_nodes("```bash\necho test\n```");
        let code_bodies: Vec<_> = lines.iter().filter(|line| line.is_code_body()).collect();
        assert!(!code_bodies.is_empty());
        for body in code_bodies {
            assert!(body.code_id.is_some(), "code body missing code_id");
        }
    }

    #[test]
    fn test_render_code_tabs_preserved_logically_expanded_visually() {
        let lines = render_nodes(
            "```go\npackage main\n\t\"fmt\"\n\tos.Setenv(\"FROM_GO\", \"set by go\")\n```",
        );
        let code_bodies: Vec<_> = lines.iter().filter(|line| line.is_code_body()).collect();
        assert_eq!(code_bodies.len(), 3);
        assert_eq!(code_bodies[0].text_content(), "package main");
        assert_eq!(code_bodies[1].text_content(), "\t\"fmt\"");
        assert_eq!(
            code_bodies[2].text_content(),
            "\tos.Setenv(\"FROM_GO\", \"set by go\")"
        );

        let theme = test_theme();
        let ctx = test_ctx(&theme, 80);
        let import_line = code_bodies[1].render(&ctx).to_string();
        let call_line = code_bodies[2].render(&ctx).to_string();

        assert!(!import_line.contains('\t'));
        assert!(!call_line.contains('\t'));
        assert!(import_line.contains("    \"fmt\""));
        assert!(call_line.contains("    os.Setenv"));
        assert!(!call_line.contains("os. Setenv"));
    }

    #[test]
    fn test_render_heading_source() {
        let lines = render_nodes("# Title\n\n## Subtitle");
        let headings: Vec<_> = lines
            .iter()
            .filter(|line| line.heading_level().is_some())
            .collect();
        assert_eq!(headings.len(), 2);
        assert_eq!(headings[0].heading_level(), Some(1));
        assert_eq!(headings[1].heading_level(), Some(2));
    }

    #[test]
    fn test_render_heading_level() {
        let lines = render_nodes("# H1\n## H2\n### H3\n#### H4");
        let levels: Vec<u8> = lines
            .iter()
            .filter_map(LogicalLine::heading_level)
            .collect();
        assert_eq!(levels, [1, 2, 3, 4]);
    }

    #[test]
    fn test_render_table_narrow() {
        let doc = upmd_parser::new().parse("| Name | Age | City |\n|------|-----|------|\n| Alice | 30 | New York |\n| Bob | 25 | London |");
        let theme = test_theme();
        let ctx = test_ctx(&theme, 25);
        let outputs = HashMap::new();
        let renderer = MarkdownRenderer::new(&doc.source, &theme, &outputs, &doc.codes, 10, 25);
        let lines = renderer.render(&doc.nodes).lines;
        let summary: Vec<String> = lines
            .iter()
            .filter(|l| l.is_table())
            .map(|l| l.render(&ctx).to_string())
            .collect();
        assert_snapshot!("table_narrow", summary.join("\n"));
    }

    #[test]
    fn test_render_table_wide() {
        let doc = upmd_parser::new().parse("| Name | Age | City |\n|------|-----|------|\n| Alice | 30 | New York |\n| Bob | 25 | London |");
        let theme = test_theme();
        let ctx = test_ctx(&theme, 80);
        let outputs = HashMap::new();
        let renderer = MarkdownRenderer::new(&doc.source, &theme, &outputs, &doc.codes, 10, 80);
        let lines = renderer.render(&doc.nodes).lines;
        let summary: Vec<String> = lines
            .iter()
            .filter(|l| l.is_table())
            .map(|l| l.render(&ctx).to_string())
            .collect();
        assert_snapshot!("table_wide", summary.join("\n"));
    }
}

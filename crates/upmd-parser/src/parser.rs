use pulldown_cmark::{
    Alignment as CmarkAlignment, CodeBlockKind, Event, HeadingLevel, MetadataBlockKind, Options,
    Parser as CmarkParser, Tag, TagEnd,
};

use super::nodes::{
    inline_text, semantic_text, Alignment, Codes, FrontmatterStyle, InlineSpan, InlineStyle,
    ListItem, ListKind, Node, Table, TableCell, TaskStatus,
};
use super::options;

pub struct Cmark;

impl Default for Cmark {
    fn default() -> Self {
        Self::new()
    }
}

impl Cmark {
    pub fn new() -> Self {
        Self {}
    }
}

// Public API

impl super::Parser for Cmark {
    fn parse(&self, text: &str) -> super::Document {
        let options = Options::ENABLE_TABLES
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
            | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS;
        let parser = CmarkParser::new_ext(text, options).into_offset_iter();
        let mut parser = Parser {
            source: text,
            iter: parser.peekable(),
            codes: Codes::default(),
            headings: Vec::new(),
            line_starts: line_starts(text),
        };
        let nodes = parser.parse_document();
        super::Document {
            nodes,
            codes: parser.codes,
            headings: parser.headings,
            nodes_state: super::NodesState::Full,
        }
    }
}

// Internal recursive-descent parser

struct Parser<'a> {
    source: &'a str,
    iter: std::iter::Peekable<pulldown_cmark::OffsetIter<'a>>,
    codes: Codes,
    headings: Vec<super::Heading>,
    line_starts: Vec<usize>,
}

fn line_starts(input: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(input.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

fn byte_to_line(line_starts: &[usize], byte: usize) -> usize {
    match line_starts.binary_search(&byte) {
        Ok(idx) => idx + 1,
        Err(idx) => idx,
    }
    .max(1)
}

impl<'a> Parser<'a> {
    fn parse_document(&mut self) -> Vec<Node> {
        let frontmatter = match self.iter.peek() {
            Some((Event::Start(Tag::MetadataBlock(kind)), range)) if range.start == 0 => {
                Some(*kind)
            }
            _ => None,
        }
        .map(|kind| self.parse_frontmatter(kind));

        let mut nodes = self.parse_blocks(None);
        if let Some(frontmatter) = frontmatter {
            nodes.insert(0, frontmatter);
        }
        nodes
    }

    fn parse_frontmatter(&mut self, kind: MetadataBlockKind) -> Node {
        self.iter.next();
        let mut payload = None;
        for (event, range) in self.iter.by_ref() {
            match event {
                Event::Text(_) => payload.get_or_insert(range).end = range.end,
                Event::End(TagEnd::MetadataBlock(end_kind)) if end_kind == kind => break,
                _ => {}
            }
        }
        let style = match kind {
            MetadataBlockKind::YamlStyle => FrontmatterStyle::Yaml,
            MetadataBlockKind::PlusesStyle => FrontmatterStyle::Toml,
        };
        // Pulldown rejects empty metadata blocks.
        let raw = self.source[payload.expect("metadata block has content")].to_owned();
        Node::Frontmatter { style, raw }
    }

    // Root dispatch
    //
    // Blocks ::= (Paragraph | Heading | CodeBlock | Table | List | BlockQuote
    //             | ThematicBreak | Text)*

    fn parse_blocks(&mut self, stop_at: Option<TagEnd>) -> Vec<Node> {
        let mut nodes = Vec::new();
        while let Some((event, range)) = self.iter.next() {
            if let Some(ref stop) = stop_at {
                if matches!(&event, Event::End(tag) if *tag == *stop) {
                    break;
                }
            }
            match event {
                Event::End(_) => {}
                Event::Start(Tag::Paragraph) => {
                    nodes.push(self.parse_paragraph());
                }
                Event::Start(Tag::Heading { level, .. }) => {
                    nodes.push(self.parse_heading(level, range));
                }
                Event::Start(Tag::List(start)) => {
                    nodes.push(Node::List(self.parse_list(1, start)));
                }
                Event::Start(Tag::BlockQuote(_)) => {
                    nodes.push(Node::BlockQuote(
                        self.parse_blocks(Some(TagEnd::BlockQuote(None))),
                    ));
                }
                Event::Start(Tag::CodeBlock(kind)) => {
                    if let Some(node) = self.parse_code_block(kind) {
                        nodes.push(node);
                    }
                }
                Event::Start(Tag::HtmlBlock) => {
                    nodes.push(self.parse_html_block());
                }
                Event::Start(Tag::Table(alignments)) => {
                    nodes.push(self.parse_table(&alignments));
                }
                Event::Rule => {
                    nodes.push(Node::ThematicBreak);
                }
                Event::Text(t) => {
                    nodes.push(Node::Text(vec![text_span(t.into_string(), &[])]));
                }
                Event::Code(t) => {
                    nodes.push(Node::Text(vec![code_span(&t, &[])]));
                }
                _ => {}
            }
        }
        nodes
    }

    // Paragraph

    fn parse_paragraph(&mut self) -> Node {
        if matches!(self.iter.peek(), Some((Event::Start(Tag::Image { .. }), _))) {
            return self.parse_block_image();
        }

        Node::Paragraph(trim_spans(self.parse_inline_content(TagEnd::Paragraph)))
    }

    /// Parses a paragraph whose first event is an image.
    #[inline]
    fn parse_block_image(&mut self) -> Node {
        let Some((Event::Start(Tag::Image { dest_url, .. }), _)) = self.iter.next() else {
            unreachable!("peeked image event must still be present");
        };
        let src = dest_url.into_string();
        let mut spans = Vec::new();
        let mut stack = Vec::new();
        self.push_image(&src, &mut stack, &mut spans);

        if matches!(self.iter.peek(), Some((Event::End(TagEnd::Paragraph), _))) {
            self.iter.next();
            return Node::Image {
                alt: inline_text(&spans),
                src,
            };
        }

        self.parse_inline_until(TagEnd::Paragraph, &mut stack, &mut spans);
        Node::Paragraph(trim_spans(spans))
    }

    // Heading

    fn parse_heading(&mut self, level: HeadingLevel, source_range: std::ops::Range<usize>) -> Node {
        let heading_level = match level {
            HeadingLevel::H1 => 1,
            HeadingLevel::H2 => 2,
            HeadingLevel::H3 => 3,
            HeadingLevel::H4 => 4,
            HeadingLevel::H5 => 5,
            HeadingLevel::H6 => 6,
        };
        let spans = self.parse_inline_content(TagEnd::Heading(level));
        let text = semantic_text(&spans);
        let text = text.trim().to_string();
        let spans = trim_spans(spans);
        self.headings.push(super::Heading {
            level: heading_level,
            text: text.clone(),
            source_range: source_range.clone(),
            start_line: byte_to_line(&self.line_starts, source_range.start),
            end_line: byte_to_line(&self.line_starts, source_range.end.max(1) - 1),
        });
        Node::Heading {
            level: heading_level,
            text: spans,
        }
    }

    // Code block

    fn parse_code_block(&mut self, kind: CodeBlockKind<'a>) -> Option<Node> {
        let opts = match &kind {
            CodeBlockKind::Fenced(info) => info.to_string(),
            CodeBlockKind::Indented => String::new(),
        };
        let mut content = String::new();
        loop {
            match self.iter.next() {
                Some((Event::End(TagEnd::CodeBlock), _)) => break,
                Some((Event::Text(t), _)) => content.push_str(&t),
                Some((Event::SoftBreak | Event::HardBreak, _)) => content.push('\n'),
                None => break,
                _ => {}
            }
        }
        let content = content.trim_end_matches('\n').to_string();
        if content.trim().is_empty() {
            return None;
        }
        let options = options::parse(&opts);
        let code_id = self.codes.push(content, options);
        Some(Node::Code(code_id))
    }

    // HTML block

    fn parse_html_block(&mut self) -> Node {
        let mut content = String::new();
        loop {
            match self.iter.next() {
                Some((Event::End(TagEnd::HtmlBlock), _)) => break,
                Some((Event::Html(html), _)) => content.push_str(&html),
                Some((Event::Text(text), _)) => content.push_str(&text),
                None => break,
                _ => {}
            }
        }
        Node::HtmlBlock(content)
    }

    // Table

    fn parse_table(&mut self, alignments: &[CmarkAlignment]) -> Node {
        let mapped: Vec<Alignment> = alignments.iter().map(Self::map_alignment).collect();
        let mut headers = Vec::new();
        let mut rows = Vec::new();
        let mut in_header = true;
        let mut row_idx = 0usize;
        loop {
            match self.iter.next() {
                Some((Event::End(TagEnd::Table), _)) => break,
                Some((Event::Start(Tag::TableCell), _)) => {
                    let cell = TableCell {
                        spans: trim_spans(self.parse_inline_content(TagEnd::TableCell)),
                    };
                    if in_header {
                        headers.push(cell);
                    } else {
                        while rows.len() <= row_idx {
                            rows.push(Vec::new());
                        }
                        rows[row_idx].push(cell);
                    }
                }
                Some((Event::End(TagEnd::TableHead), _)) => in_header = false,
                Some((Event::End(TagEnd::TableRow), _)) => row_idx += 1,
                None => break,
                _ => {}
            }
        }
        Node::Table(Table {
            headers,
            rows,
            alignments: mapped,
        })
    }

    fn map_alignment(a: &CmarkAlignment) -> Alignment {
        match a {
            CmarkAlignment::Left => Alignment::Left,
            CmarkAlignment::Center => Alignment::Center,
            CmarkAlignment::Right => Alignment::Right,
            CmarkAlignment::None => Alignment::None,
        }
    }

    // List
    //
    // List       ::= Item+
    // ListItem   ::= (TaskMarker? InlineContent BlockChildren*)

    fn parse_list(&mut self, depth: usize, start_num: Option<u64>) -> Vec<ListItem> {
        let mut items = Vec::new();
        loop {
            match self.iter.next() {
                Some((Event::End(TagEnd::List(_)), _)) => break,
                Some((Event::Start(Tag::Item), _)) => {
                    items.push(self.parse_list_item(depth, items.len(), start_num));
                }
                None => break,
                _ => {}
            }
        }
        items
    }

    fn parse_list_item(&mut self, depth: usize, index: usize, start_num: Option<u64>) -> ListItem {
        let mut spans = Vec::new();
        let mut stack = Vec::new();
        let mut children = Vec::new();
        let mut task_kind: Option<ListKind> = None;

        while let Some((event, range)) = self.iter.next() {
            let Some(event) = self.consume_inline_event(event, &mut stack, &mut spans) else {
                continue;
            };

            match event {
                Event::End(TagEnd::Item) => break,
                Event::TaskListMarker(checked) => {
                    task_kind = Some(ListKind::Task(if checked {
                        TaskStatus::Checked
                    } else {
                        TaskStatus::Unchecked
                    }));
                }
                Event::Start(Tag::CodeBlock(kind)) => {
                    if let Some(node) = self.parse_code_block(kind) {
                        children.push(node);
                    }
                }
                Event::Start(Tag::HtmlBlock) => {
                    children.push(self.parse_html_block());
                }
                Event::Start(Tag::List(start)) => {
                    children.push(Node::List(self.parse_list(depth + 1, start)));
                }
                Event::Start(Tag::BlockQuote(_)) => {
                    children.push(Node::BlockQuote(
                        self.parse_blocks(Some(TagEnd::BlockQuote(None))),
                    ));
                }
                Event::Start(Tag::Table(alignments)) => {
                    children.push(self.parse_table(&alignments));
                }
                Event::Start(Tag::Heading { level, .. }) => {
                    children.push(self.parse_heading(level, range));
                }
                Event::Start(Tag::Paragraph) => {}
                _ => {}
            }
        }

        let kind = if let Some(tk) = task_kind {
            tk
        } else if let Some(start) = start_num {
            ListKind::Ordered(start + index as u64)
        } else {
            ListKind::Bullet
        };

        ListItem {
            depth,
            kind,
            text: trim_spans(spans),
            children,
        }
    }

    // Shared inline content parser
    //
    // InlineContent ::= (Text | Code | Break | Emphasis | Strong | Strike
    //                    | Link | Image)*

    /// Consumes events until `stop`, producing styled inline spans.
    fn parse_inline_content(&mut self, stop: TagEnd) -> Vec<InlineSpan> {
        let mut spans = Vec::new();
        let mut stack = Vec::new();
        self.parse_inline_until(stop, &mut stack, &mut spans);
        spans
    }

    /// Consumes one inline event, returning block/container events to the caller.
    fn consume_inline_event(
        &mut self,
        event: Event<'a>,
        stack: &mut Vec<InlineStyle>,
        out: &mut Vec<InlineSpan>,
    ) -> Option<Event<'a>> {
        match event {
            Event::Text(text) => out.push(text_span(text.into_string(), stack)),
            Event::Code(code) => out.push(code_span(&code, stack)),
            Event::InlineHtml(tag) => out.push(html_span(&tag, stack)),
            Event::SoftBreak | Event::HardBreak => out.push(break_span(stack)),
            Event::Start(Tag::Emphasis) => {
                self.parse_styled_until(InlineStyle::Italic, TagEnd::Emphasis, stack, out);
            }
            Event::Start(Tag::Strong) => {
                self.parse_styled_until(InlineStyle::Bold, TagEnd::Strong, stack, out);
            }
            Event::Start(Tag::Strikethrough) => {
                self.parse_styled_until(
                    InlineStyle::Strikethrough,
                    TagEnd::Strikethrough,
                    stack,
                    out,
                );
            }
            Event::Start(Tag::Link {
                dest_url, title, ..
            }) => {
                let title = (!title.is_empty()).then(|| title.to_string());
                self.parse_styled_until(
                    InlineStyle::Link {
                        destination: dest_url.to_string(),
                        title,
                    },
                    TagEnd::Link,
                    stack,
                    out,
                );
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                self.push_image(&dest_url, stack, out);
            }
            event => return Some(event),
        }
        None
    }

    /// Parses nested inline content while `style` is active.
    fn parse_styled_until(
        &mut self,
        style: InlineStyle,
        stop: TagEnd,
        stack: &mut Vec<InlineStyle>,
        out: &mut Vec<InlineSpan>,
    ) {
        stack.push(style);
        self.parse_inline_until(stop, stack, out);
        stack.pop();
    }

    /// Appends inline spans until `stop`, tracking the active style stack.
    fn parse_inline_until(
        &mut self,
        stop: TagEnd,
        stack: &mut Vec<InlineStyle>,
        out: &mut Vec<InlineSpan>,
    ) {
        while let Some((event, _)) = self.iter.next() {
            if matches!(&event, Event::End(tag) if tag == &stop) {
                break;
            }
            let _ = self.consume_inline_event(event, stack, out);
        }
    }

    /// Captures an image's alt text (its inner content) as a styled span.
    fn push_image(
        &mut self,
        dest_url: &str,
        stack: &mut Vec<InlineStyle>,
        out: &mut Vec<InlineSpan>,
    ) {
        let start = out.len();
        stack.push(InlineStyle::Image {
            alt: String::new(),
            src: dest_url.to_string(),
        });
        self.parse_inline_until(TagEnd::Image, stack, out);
        stack.pop();
        let alt = inline_text(&out[start..]);

        // Empty alt (`![](path)`) yields no spans, so emit one to keep the image.
        if out.len() == start {
            out.push(text_span(
                String::new(),
                &[InlineStyle::Image {
                    alt: String::new(),
                    src: dest_url.to_string(),
                }],
            ));
        }
        for span in &mut out[start..] {
            if let Some(InlineStyle::Image { alt: a, .. }) = span
                .style
                .iter_mut()
                .find(|s| matches!(s, InlineStyle::Image { .. }))
            {
                *a = alt.clone();
            }
        }
    }
}

/// Builds a plain inline span carrying the active style stack.
fn text_span(text: String, stack: &[InlineStyle]) -> InlineSpan {
    InlineSpan {
        text,
        style: stack.to_vec(),
    }
}

/// Builds an inline-code span, wrapping the code in backticks.
fn code_span(code: &str, stack: &[InlineStyle]) -> InlineSpan {
    let mut style = stack.to_vec();
    style.push(InlineStyle::InlineCode);
    InlineSpan {
        text: format!("`{}`", code),
        style,
    }
}

/// Builds a span for an inline HTML tag, preserving its source text.
fn html_span(html: &str, stack: &[InlineStyle]) -> InlineSpan {
    let mut style = stack.to_vec();
    style.push(InlineStyle::HtmlTag);
    InlineSpan {
        text: html.to_string(),
        style,
    }
}

/// Builds a span for a soft/hard break.
fn break_span(stack: &[InlineStyle]) -> InlineSpan {
    InlineSpan {
        text: "\n".into(),
        style: stack.to_vec(),
    }
}

/// Characters [`trim_spans`] removes from the edges of inline content.
const TRIM_CHARS: [char; 3] = [' ', '\t', '\n'];

/// Removes leading/trailing whitespace from the first/last span and drops any
/// spans that become empty as a result.
fn trim_spans(mut spans: Vec<InlineSpan>) -> Vec<InlineSpan> {
    let start = spans
        .iter()
        .position(|s| !s.text.trim_start_matches(TRIM_CHARS).is_empty())
        .unwrap_or(spans.len());
    let end = spans
        .iter()
        .rposition(|s| !s.text.trim_end_matches(TRIM_CHARS).is_empty())
        .map_or(start, |i| i + 1);

    spans.drain(end..);
    spans.drain(..start);

    if let Some(s) = spans.first_mut() {
        s.text = s.text.trim_start_matches(TRIM_CHARS).to_string();
    }
    if let Some(s) = spans.last_mut() {
        s.text = s.text.trim_end_matches(TRIM_CHARS).to_string();
    }
    spans
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Code, Parser as _};

    fn code_from_node<'a>(doc: &'a crate::Document, node: &'a Node) -> &'a Code {
        match node {
            Node::Code(id) => doc.codes.by_id(*id).unwrap(),
            _ => panic!("Expected Code"),
        }
    }

    #[test]
    fn test_parse_code_blocks() {
        for (input, expected_node_count, expected_checks) in [
            (
                "### Hello\n\nWorld\n\n```bash\necho 1\n```\n",
                3usize,
                vec![(2usize, "bash", "echo 1", "")],
            ),
            (
                "```bash [name:setup]\necho \"hello\"\n```\n",
                1,
                vec![(0, "bash", "echo \"hello\"", "setup")],
            ),
            (
                "```bash\necho \"hello\"\n```\n",
                1,
                vec![(0, "bash", "echo \"hello\"", "")],
            ),
            (
                "Example 2\n\n```sh\necho first\n```\n\n```sh\necho second\n```\n",
                3,
                vec![(1, "sh", "echo first", ""), (2, "sh", "echo second", "")],
            ),
            (
                "Some text\n\n    echo hello\n    world\n",
                2,
                vec![(1, "", "echo hello\nworld", "")],
            ),
            (
                "    just code\n    no lang\n",
                1,
                vec![(0, "", "just code\nno lang", "")],
            ),
            (
                "```rust\nfn main() {}\n```\n\n    some indented code\n",
                2,
                vec![
                    (0, "rust", "fn main() {}", ""),
                    (1, "", "some indented code", ""),
                ],
            ),
        ] {
            let doc = Cmark::new().parse(input);
            let nodes = &doc.nodes;
            assert_eq!(nodes.len(), expected_node_count, "input: {input:?}");
            for &(node_idx, lang, content, name) in &expected_checks {
                let c = code_from_node(&doc, &nodes[node_idx]);
                assert_eq!(c.language, lang, "node {node_idx}, input: {input:?}");
                assert_eq!(c.content, content, "node {node_idx}, input: {input:?}");
                assert_eq!(c.name, name, "node {node_idx}, input: {input:?}");
            }
        }
    }

    #[test]
    fn test_parse_code_block_whitespace_and_recovery() {
        // Leading indentation preserved.
        let doc = Cmark::new().parse("```python\n    def foo():\n        pass\n```\n");
        let c = doc.codes.first().unwrap();
        assert_eq!(c.content, "    def foo():\n        pass");

        // Leading blank lines preserved.
        let doc = Cmark::new().parse("```bash\n\necho hello\n```\n");
        let c = doc.codes.first().unwrap();
        assert_eq!(c.content, "\necho hello");

        // Whitespace-only blocks are empty (no Code node).
        let text = "```bash\n   \n```\n";
        let nodes = Cmark::new().parse(text).nodes;
        assert!(!nodes.iter().any(|n| matches!(n, Node::Code(_))));

        // Bad attrs recover valid metadata.
        let text = "```bash [name:foo bad, bin:zsh]\necho hi\n```\n";
        let doc = Cmark::new().parse(text);
        let code = doc.codes.first().unwrap();
        assert_eq!(code.language, "bash");
        assert_eq!(code.name, "foo");
        assert_eq!(code.attrs.get("bin").map(String::as_str), Some("zsh"));
        assert!(code.errors.iter().any(|e| e.contains("bad")));
    }

    #[test]
    fn test_parse_lists() {
        for (input, expected_checks) in [
            (
                "- Item 1\n- Item 2\n- Item 3\n",
                vec![
                    (0usize, "Item 1", ListKind::Bullet),
                    (1, "Item 2", ListKind::Bullet),
                    (2, "Item 3", ListKind::Bullet),
                ],
            ),
            (
                "1. First\n2. Second\n3. Third\n",
                vec![
                    (0, "First", ListKind::Ordered(1)),
                    (1, "Second", ListKind::Ordered(2)),
                    (2, "Third", ListKind::Ordered(3)),
                ],
            ),
            (
                "- [ ] Unchecked task\n",
                vec![(0, "Unchecked task", ListKind::Task(TaskStatus::Unchecked))],
            ),
            (
                "- [x] Completed task\n",
                vec![(0, "Completed task", ListKind::Task(TaskStatus::Checked))],
            ),
            (
                "- [-] In progress task\n",
                vec![(0, "[-] In progress task", ListKind::Bullet)],
            ),
        ] {
            let nodes = Cmark::new().parse(input).nodes;
            assert_eq!(nodes.len(), 1, "input: {input:?}");
            match &nodes[0] {
                Node::List(items) => {
                    for &(idx, expected_text, ref expected_kind) in &expected_checks {
                        assert_eq!(
                            inline_text(&items[idx].text),
                            expected_text,
                            "item {idx}, input: {input:?}"
                        );
                        assert_eq!(
                            items[idx].kind, *expected_kind,
                            "item {idx}, input: {input:?}"
                        );
                    }
                }
                _ => panic!("Expected List, input: {input:?}"),
            }
        }
    }

    #[test]
    fn test_parse_tables() {
        for (input, expected_headers, expected_rows, expected_alignments) in [
            (
                "| Header 1 | Header 2 |\n|----------|----------|\n| Cell 1   | Cell 2   |\n| Cell 3   | Cell 4   |\n",
                &["Header 1", "Header 2"] as &[&str],
                2usize,
                &[Alignment::None, Alignment::None] as &[Alignment],
            ),
            (
                "| A | B |\n|---|---|\n| 1 | 2 |\n",
                &["A", "B"],
                1,
                &[Alignment::None, Alignment::None],
            ),
        ] {
            let nodes = Cmark::new().parse(input).nodes;
            assert_eq!(nodes.len(), 1, "input: {input:?}");
            match &nodes[0] {
                Node::Table(t) => {
                    assert_eq!(
                        t.headers.iter().map(TableCell::text).collect::<Vec<_>>(),
                        expected_headers,
                        "input: {input:?}"
                    );
                    assert_eq!(t.rows.len(), expected_rows, "input: {input:?}");
                    assert_eq!(t.alignments, expected_alignments, "input: {input:?}");
                }
                _ => panic!("Expected Table, input: {input:?}"),
            }
        }
    }

    #[test]
    fn test_parse_headings() {
        for (input, expected_level, expected_text) in [
            ("### My Heading", 3u8, "My Heading"),
            ("# Title\n\n## Run `make`\n", 1, "Title"),
        ] {
            let doc = Cmark::new().parse(input);
            let node = &doc.nodes[0];
            match node {
                Node::Heading { level, text } => {
                    assert_eq!(*level, expected_level);
                    assert_eq!(inline_text(text), expected_text);
                }
                _ => panic!("Expected Heading"),
            }
        }

        // headings collection
        let doc = Cmark::new().parse("# Title\n\n## Run `make`\n");
        assert_eq!(doc.headings.len(), 2);
        assert_eq!(doc.headings[0].level, 1);
        assert_eq!(doc.headings[0].text, "Title");
        assert_eq!(doc.headings[0].start_line, 1);
        assert_eq!(doc.headings[1].level, 2);
        assert_eq!(doc.headings[1].text, "Run `make`");
        assert_eq!(doc.nodes_state, crate::NodesState::Full);
    }

    #[test]
    fn test_frontmatter_recognition() {
        for (name, input, expected) in [
            (
                "yaml",
                "---\ntitle: Hi\nfoo: bar\n---\n\n# Doc\n",
                Some((crate::FrontmatterStyle::Yaml, "title: Hi\nfoo: bar\n")),
            ),
            (
                "toml",
                "+++\ntitle = \"Hi\"\n+++\n\n# Doc\n",
                Some((crate::FrontmatterStyle::Toml, "title = \"Hi\"\n")),
            ),
            (
                "crlf",
                "---\r\ntitle: Hi\r\n---\r\n# Doc\r\n",
                Some((crate::FrontmatterStyle::Yaml, "title: Hi\r\n")),
            ),
            (
                "indented",
                "---\n  indented: true\n\nlast: value\n---\n# Doc\n",
                Some((
                    crate::FrontmatterStyle::Yaml,
                    "  indented: true\n\nlast: value\n",
                )),
            ),
            ("unclosed", "---\ntitle: x\n\n# Doc\n", None),
        ] {
            let doc = Cmark::new().parse(input);
            let actual = doc.nodes.first().and_then(|node| match node {
                Node::Frontmatter { style, raw } => Some((*style, raw.as_str())),
                _ => None,
            });
            assert_eq!(actual, expected, "{name}");
        }
    }

    #[test]
    fn test_frontmatter_edge_cases_stay_normal_markdown() {
        for (name, input) in [
            ("immediate-close", "---\n---\n# Doc\n"),
            ("blank-first", "---\n\nfoo\n---\n"),
        ] {
            let doc = Cmark::new().parse(input);
            assert!(
                !doc.nodes
                    .iter()
                    .any(|n| matches!(n, Node::Frontmatter { .. })),
                "{name}: must not be frontmatter"
            );
            assert!(
                doc.nodes.iter().any(|n| matches!(n, Node::ThematicBreak)),
                "{name}: should parse as normal Markdown"
            );
        }
    }

    #[test]
    fn test_parse_blockquote_and_thematic_break() {
        let text = "> This is a blockquote\n> with multiple lines\n";
        let nodes = Cmark::new().parse(text).nodes;
        assert_eq!(nodes.len(), 1);
        match &nodes[0] {
            Node::BlockQuote(children) => {
                assert_eq!(children.len(), 1);
                match &children[0] {
                    Node::Paragraph(s) => assert!(inline_text(s).contains("blockquote")),
                    _ => panic!("Expected Paragraph in BlockQuote"),
                }
            }
            _ => panic!("Expected BlockQuote"),
        }

        assert!(Cmark::new()
            .parse("Some text\n\n---\n\nMore text\n")
            .nodes
            .iter()
            .any(|n| matches!(n, Node::ThematicBreak)));
    }

    #[test]
    fn test_parse_code_block_in_list() {
        let text = "- item 1\n  ```sh\n  echo hi\n  ```\n- item 2\n";
        let doc = Cmark::new().parse(text);
        let nodes = &doc.nodes;
        assert_eq!(nodes.len(), 1);
        let items = match &nodes[0] {
            Node::List(items) => items,
            _ => panic!("expected list"),
        };
        assert_eq!(items.len(), 2);
        assert_eq!(inline_text(&items[0].text), "item 1");
        assert_eq!(inline_text(&items[1].text), "item 2");
        assert_eq!(items[0].children.len(), 1);
        assert!(matches!(&items[0].children[0], Node::Code(code_id)
            if doc.codes.iter().any(|c| c.id == *code_id && c.content.trim() == "echo hi")));
    }

    #[test]
    fn test_parse_inline_styles() {
        let cases = [
            ("plain", "plain", vec![]),
            ("**bold**", "bold", vec![InlineStyle::Bold]),
            ("*italic*", "italic", vec![InlineStyle::Italic]),
            ("~~strike~~", "strike", vec![InlineStyle::Strikethrough]),
            ("`code`", "`code`", vec![InlineStyle::InlineCode]),
            (
                "[link](https://x.dev)",
                "link",
                vec![InlineStyle::Link {
                    destination: "https://x.dev".into(),
                    title: None,
                }],
            ),
            (
                "[link](https://x.dev \"API\")",
                "link",
                vec![InlineStyle::Link {
                    destination: "https://x.dev".into(),
                    title: Some("API".into()),
                }],
            ),
            (
                "***both***",
                "both",
                vec![InlineStyle::Italic, InlineStyle::Bold],
            ),
            (
                "**`code`**",
                "`code`",
                vec![InlineStyle::Bold, InlineStyle::InlineCode],
            ),
            (
                "~~*struck italic*~~",
                "struck italic",
                vec![InlineStyle::Strikethrough, InlineStyle::Italic],
            ),
        ];

        for (markdown, expected_text, expected_styles) in cases {
            let nodes = Cmark::new().parse(markdown).nodes;
            let spans = match &nodes[0] {
                Node::Paragraph(spans) => spans,
                other => panic!("Expected Paragraph for {markdown:?}, got {other:?}"),
            };

            assert_eq!(inline_text(spans), expected_text, "input: {markdown:?}");
            assert_eq!(spans.len(), 1, "input: {markdown:?}");
            assert_eq!(spans[0].style, expected_styles, "input: {markdown:?}");
        }
    }

    #[test]
    fn test_parse_block_image() {
        for (markdown, expected_alt, expected_src) in [
            ("![alt](image.png)", "alt", "image.png"),
            ("![alt](./img/a.png)", "alt", "./img/a.png"),
            ("![](/abs/path.png)", "", "/abs/path.png"),
        ] {
            let nodes = Cmark::new().parse(markdown).nodes;
            match &nodes[0] {
                Node::Image { alt, src } => {
                    assert_eq!(alt, expected_alt, "input: {markdown:?}");
                    assert_eq!(src, expected_src, "input: {markdown:?}");
                }
                other => panic!("Expected standalone Image for {markdown:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_paragraph_with_mixed_text_and_image_stays_paragraph() {
        let nodes = Cmark::new().parse("text ![alt](image.png)").nodes;
        match &nodes[0] {
            Node::Paragraph(spans) => {
                assert!(spans.iter().any(|s| s.style.contains(&InlineStyle::Image {
                    alt: "alt".into(),
                    src: "image.png".into(),
                })));
            }
            other => panic!("Expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn test_multiple_images_stay_paragraph() {
        for markdown in [
            "![alt](image.png)![alt](image.png)",
            "![](image.png)![](image.png)",
        ] {
            assert!(
                matches!(Cmark::new().parse(markdown).nodes[0], Node::Paragraph(_)),
                "input: {markdown:?}"
            );
        }
    }

    #[test]
    fn test_parse_inline_styles_in_table_cells() {
        let cases = [
            ("**bold**", "bold", vec![InlineStyle::Bold]),
            (
                "[docs](https://x.dev \"API\")",
                "docs",
                vec![InlineStyle::Link {
                    destination: "https://x.dev".into(),
                    title: Some("API".into()),
                }],
            ),
            ("`code`", "`code`", vec![InlineStyle::InlineCode]),
        ];

        for (markdown, expected_text, expected_styles) in cases {
            let input = format!("| Value |\n|---|\n| {markdown} |");
            let nodes = Cmark::new().parse(&input).nodes;
            let table = match &nodes[0] {
                Node::Table(table) => table,
                other => panic!("Expected Table for {markdown:?}, got {other:?}"),
            };
            let cell = &table.rows[0][0];

            assert_eq!(cell.text(), expected_text, "input: {markdown:?}");
            assert_eq!(cell.spans.len(), 1, "input: {markdown:?}");
            assert_eq!(cell.spans[0].style, expected_styles, "input: {markdown:?}");
        }
    }

    #[test]
    fn test_parse_html() {
        for (input, expected) in [
            (
                "<div class=\"card\">\n<p>Hello</p>\n</div>\n",
                "<div class=\"card\">\n<p>Hello</p>\n</div>\n",
            ),
            ("**bold** <b>tag</b> <br/>", "bold <b>tag</b> <br/>"),
            ("# Title <b>with tag</b>\n", "Title <b>with tag</b>"),
        ] {
            let doc = Cmark::new().parse(input);
            let text = match &doc.nodes[0] {
                Node::HtmlBlock(content) => content.clone(),
                Node::Paragraph(spans) => inline_text(spans),
                Node::Heading { text, .. } => inline_text(text),
                other => panic!("Expected {input:?} to start with a text node, got {other:?}"),
            };
            assert_eq!(text, expected, "input: {input:?}");
        }

        // Semantic heading labels exclude HTML tags.
        let doc = Cmark::new().parse("# Title <b>with tag</b>\n");
        assert_eq!(doc.headings[0].text, "Title with tag");
    }

    #[test]
    fn test_parse_inline_styles_in_list_items() {
        let cases = [
            ("**bold item**", "bold item", vec![InlineStyle::Bold]),
            ("*italic item*", "italic item", vec![InlineStyle::Italic]),
            ("`code`", "`code`", vec![InlineStyle::InlineCode]),
            ("~~strike~~", "strike", vec![InlineStyle::Strikethrough]),
            (
                "[link](https://x.dev)",
                "link",
                vec![InlineStyle::Link {
                    destination: "https://x.dev".into(),
                    title: None,
                }],
            ),
            (
                "![alt](image.png)",
                "alt",
                vec![InlineStyle::Image {
                    alt: "alt".into(),
                    src: "image.png".into(),
                }],
            ),
            (
                "***both***",
                "both",
                vec![InlineStyle::Italic, InlineStyle::Bold],
            ),
        ];

        for (markdown, expected_text, expected_styles) in cases {
            let input = format!("- {markdown}");
            let nodes = Cmark::new().parse(&input).nodes;
            let items = match &nodes[0] {
                Node::List(items) => items,
                other => panic!("Expected List for {markdown:?}, got {other:?}"),
            };

            assert_eq!(inline_text(&items[0].text), expected_text);
            assert_eq!(items[0].text.len(), 1, "input: {markdown:?}");
            assert_eq!(
                items[0].text[0].style, expected_styles,
                "input: {markdown:?}"
            );
        }
    }
}

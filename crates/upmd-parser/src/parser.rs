use std::ops::Range;

use pulldown_cmark::{
    Alignment as CmarkAlignment, CodeBlockKind, Event, HeadingLevel, MetadataBlockKind, Options,
    Parser as CmarkParser, Tag, TagEnd,
};

use super::nodes::{
    inline_text, semantic_text, Alignment, Codes, FrontmatterStyle, InlineSpan, InlineStyle,
    ListItem, ListKind, Node, NodeKind, SourceText, Table, TableCell, TaskStatus,
};
use super::options;

/// Markdown parser producing a complete [`super::Document`].
pub struct Parser {
    options: Options,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Self {
            options: Options::ENABLE_TABLES
                | Options::ENABLE_TASKLISTS
                | Options::ENABLE_STRIKETHROUGH
                | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
                | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS,
        }
    }

    pub fn parse(&self, source: impl Into<String>) -> super::Document {
        ParseState::parse(source.into(), self.options)
    }
}

// Internal recursive-descent parser state for one document.
struct ParseState<'a> {
    source: &'a str,
    iter: std::iter::Peekable<pulldown_cmark::OffsetIter<'a>>,
    codes: Codes,
    headings: Vec<super::Heading>,
}

impl<'a> ParseState<'a> {
    fn new(source: &'a str, options: Options) -> Self {
        Self {
            source,
            iter: CmarkParser::new_ext(source, options)
                .into_offset_iter()
                .peekable(),
            codes: Codes::default(),
            headings: Vec::new(),
        }
    }
}

impl ParseState<'_> {
    fn parse(source: String, options: Options) -> super::Document {
        let (nodes, codes, headings) = {
            let mut state = ParseState::new(&source, options);
            (state.parse_nodes(), state.codes, state.headings)
        };
        super::Document {
            source,
            nodes,
            codes,
            headings,
            nodes_state: super::NodesState::Full,
        }
    }
}

// Document    ::= Frontmatter? Blocks
// Frontmatter ::= Text+
// Frontmatter is recognized only as the first source construct.

impl<'a> ParseState<'a> {
    fn parse_nodes(&mut self) -> Vec<Node> {
        let frontmatter = self.parse_frontmatter();
        let mut nodes = self.parse_blocks();
        if let Some(frontmatter) = frontmatter {
            nodes.insert(0, frontmatter);
        }
        nodes
    }

    fn parse_frontmatter(&mut self) -> Option<Node> {
        let (kind, range) = match self.iter.peek() {
            Some((Event::Start(Tag::MetadataBlock(kind)), range)) if range.start == 0 => {
                (*kind, range.clone())
            }
            _ => return None,
        };

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
        let raw = SourceText::Source(payload.expect("metadata block has content"));
        Some(Node::new(NodeKind::Frontmatter { style, raw }, range))
    }

    // Block container
    //
    // Blocks     ::= Block*
    // Block      ::= Paragraph | Heading | CodeBlock | HtmlBlock | Table | List
    //                | BlockQuote | ThematicBreak | ImageBlock | Text
    // BlockQuote ::= Blocks

    fn parse_blocks(&mut self) -> Vec<Node> {
        let mut nodes = Vec::new();
        while let Some((event, range)) = self.iter.next() {
            if matches!(event, Event::End(_)) {
                break;
            }
            if let Some(kind) = self.parse_block_event(event, range.clone(), 1) {
                nodes.push(Node::new(kind, range));
            }
        }
        nodes
    }

    fn parse_block_event(
        &mut self,
        event: Event<'a>,
        range: Range<usize>,
        list_depth: usize,
    ) -> Option<NodeKind> {
        match event {
            Event::Start(Tag::Paragraph) => Some(self.parse_paragraph()),
            Event::Start(Tag::Heading { level, .. }) => {
                Some(self.parse_heading(level, range.clone()))
            }
            Event::Start(Tag::List(start)) => {
                Some(NodeKind::List(self.parse_list(list_depth, start)))
            }
            Event::Start(Tag::BlockQuote(_)) => Some(NodeKind::BlockQuote(self.parse_blocks())),
            Event::Start(Tag::CodeBlock(kind)) => self.parse_code_block(kind),
            Event::Start(Tag::HtmlBlock) => Some(self.parse_html_block()),
            Event::Start(Tag::Table(alignments)) => Some(self.parse_table(&alignments)),
            Event::Rule => Some(NodeKind::ThematicBreak),
            Event::Text(text) => Some(NodeKind::Text(vec![text_span(
                self.source,
                range,
                &text,
                &[],
            )])),
            Event::Code(code) => Some(NodeKind::Text(vec![code_span(&code, &[])])),
            _ => None,
        }
    }

    // Paragraph
    //
    // Paragraph  ::= InlineContent
    // ImageBlock ::= Image
    // A paragraph containing only one image is normalized to ImageBlock.
    fn parse_paragraph(&mut self) -> NodeKind {
        if matches!(self.iter.peek(), Some((Event::Start(Tag::Image { .. }), _))) {
            return self.parse_block_image();
        }

        let spans = self.parse_inline_content(TagEnd::Paragraph);
        NodeKind::Paragraph(trim_spans(spans, self.source))
    }

    /// Parses a paragraph whose first event is an image.
    #[inline]
    fn parse_block_image(&mut self) -> NodeKind {
        let Some((Event::Start(Tag::Image { dest_url, .. }), _)) = self.iter.next() else {
            unreachable!("peeked image event must still be present");
        };
        let src = dest_url.into_string();
        let mut spans = Vec::new();
        let mut stack = Vec::new();
        self.push_image(&src, &mut stack, &mut spans);

        if matches!(self.iter.peek(), Some((Event::End(TagEnd::Paragraph), _))) {
            self.iter.next();
            return NodeKind::Image {
                alt: inline_text(&spans, self.source),
                src,
            };
        }

        self.parse_inline_until(TagEnd::Paragraph, &mut stack, &mut spans);
        NodeKind::Paragraph(trim_spans(spans, self.source))
    }

    // Heading
    //
    // Heading ::= InlineContent
    fn parse_heading(&mut self, level: HeadingLevel, source_range: Range<usize>) -> NodeKind {
        let spans = self.parse_inline_content(TagEnd::Heading(level));
        let text = semantic_text(&spans, self.source).trim().to_string();
        let spans = trim_spans(spans, self.source);
        let level = level as u8;
        self.headings.push(super::Heading {
            level,
            text: text.clone(),
            source_range,
        });
        NodeKind::Heading { level, text: spans }
    }

    // Code block
    //
    // CodeBlock ::= (Text | Break)*
    // Empty code blocks do not produce a node.
    fn parse_code_block(&mut self, kind: CodeBlockKind<'a>) -> Option<NodeKind> {
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
        Some(NodeKind::Code(code_id))
    }

    // HTML block
    //
    // HtmlBlock ::= Html*
    fn parse_html_block(&mut self) -> NodeKind {
        for (event, _) in self.iter.by_ref() {
            if matches!(event, Event::End(TagEnd::HtmlBlock)) {
                break;
            }
        }
        NodeKind::HtmlBlock
    }

    // Table
    //
    // Table     ::= TableHead TableRow*
    // TableHead ::= TableCell*
    // TableRow  ::= TableCell*
    // TableCell ::= InlineContent
    fn parse_table(&mut self, alignments: &[CmarkAlignment]) -> NodeKind {
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
                        spans: trim_spans(
                            self.parse_inline_content(TagEnd::TableCell),
                            self.source,
                        ),
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
        NodeKind::Table(Table {
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
    // List     ::= ListItem+
    // ListItem ::= Block*
    // Tight inline content is normalized to a Paragraph child. A leading task
    // marker sets ListKind and is omitted from that paragraph's content.

    fn parse_list(&mut self, depth: usize, start_num: Option<u64>) -> Vec<ListItem> {
        let mut items = Vec::new();
        loop {
            match self.iter.next() {
                Some((Event::End(TagEnd::List(_)), _)) => break,
                Some((Event::Start(Tag::Item), range)) => {
                    items.push(self.parse_list_item(depth, items.len(), start_num, range));
                }
                None => break,
                _ => {}
            }
        }
        items
    }

    fn parse_list_item(
        &mut self,
        depth: usize,
        index: usize,
        start_num: Option<u64>,
        item_range: Range<usize>,
    ) -> ListItem {
        let mut children = Vec::new();
        let mut task_kind: Option<ListKind> = None;
        // A tight-list item such as `- item` emits bare inline events; a loose
        // item separated by blank lines emits a `Paragraph` container. Collect
        // the tight item's implicit paragraph separately.
        let mut tight_spans = Vec::new();
        let mut tight_range: Option<Range<usize>> = None;
        let mut stack = Vec::new();

        while let Some((event, range)) = self.iter.next() {
            let Some(event) =
                self.consume_inline_event(event, range.clone(), &mut stack, &mut tight_spans)
            else {
                tight_range.get_or_insert_with(|| range.clone()).end = range.end;
                continue;
            };
            match event {
                Event::End(TagEnd::Item) => break,
                Event::Start(Tag::Paragraph) => {
                    if let Some(kind) = self.take_task_marker() {
                        task_kind = Some(kind);
                    }
                    let paragraph =
                        trim_spans(self.parse_inline_content(TagEnd::Paragraph), self.source);
                    children.push(Node::new(NodeKind::Paragraph(paragraph), range));
                }
                Event::TaskListMarker(checked) => {
                    task_kind = Some(task_list_kind(checked));
                }
                event => {
                    if let Some(kind) = self.parse_block_event(event, range.clone(), depth + 1) {
                        children.push(Node::new(kind, range));
                    }
                }
            }
        }

        if let Some(range) = tight_range {
            let paragraph = trim_spans(tight_spans, self.source);
            if !paragraph.is_empty() {
                let index = children.partition_point(|child| child.range.start < range.start);
                children.insert(index, Node::new(NodeKind::Paragraph(paragraph), range));
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
            range: item_range,
            children,
        }
    }

    fn take_task_marker(&mut self) -> Option<ListKind> {
        let checked = match self.iter.peek() {
            Some((Event::TaskListMarker(checked), _)) => *checked,
            _ => return None,
        };
        self.iter.next();
        Some(task_list_kind(checked))
    }

    // Shared inline content parser
    //
    // InlineContent ::= Inline*
    // Inline        ::= Text | Code | InlineHtml | Break | Emphasis | Strong
    //                   | Strike | Link | Image
    // Break         ::= SoftBreak | HardBreak
    // Emphasis      ::= InlineContent
    // Strong        ::= InlineContent
    // Strike        ::= InlineContent
    // Link          ::= InlineContent
    // Image         ::= InlineContent

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
        range: std::ops::Range<usize>,
        stack: &mut Vec<InlineStyle>,
        out: &mut Vec<InlineSpan>,
    ) -> Option<Event<'a>> {
        match event {
            Event::Text(text) => out.push(text_span(self.source, range, &text, stack)),
            Event::Code(code) => out.push(code_span(&code, stack)),
            Event::InlineHtml(tag) => {
                let mut style = stack.clone();
                style.push(InlineStyle::HtmlTag);
                out.push(text_span(self.source, range, &tag, &style));
            }
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
        while let Some((event, range)) = self.iter.next() {
            if matches!(&event, Event::End(tag) if tag == &stop) {
                break;
            }
            let _ = self.consume_inline_event(event, range, stack, out);
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
        let alt = inline_text(&out[start..], self.source);

        // Empty alt (`![](path)`) yields no spans, so emit one to keep the image.
        if out.len() == start {
            out.push(text_span(
                self.source,
                0..0,
                "",
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

fn task_list_kind(checked: bool) -> ListKind {
    ListKind::Task(if checked {
        TaskStatus::Checked
    } else {
        TaskStatus::Unchecked
    })
}

/// Builds a plain inline span carrying the active style stack.
fn text_span(
    source: &str,
    range: std::ops::Range<usize>,
    text: &str,
    stack: &[InlineStyle],
) -> InlineSpan {
    let text = if source.get(range.clone()) == Some(text) {
        SourceText::Source(range)
    } else {
        SourceText::from(text)
    };
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
        text: format!("`{}`", code).into(),
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
fn trim_spans(mut spans: Vec<InlineSpan>, source: &str) -> Vec<InlineSpan> {
    let start = spans
        .iter()
        .position(|span| !span.text(source).trim_start_matches(TRIM_CHARS).is_empty())
        .unwrap_or(spans.len());
    let end = spans
        .iter()
        .rposition(|span| !span.text(source).trim_end_matches(TRIM_CHARS).is_empty())
        .map_or(start, |index| index + 1);
    spans.drain(end..);
    spans.drain(..start);

    if let Some(first) = spans.first_mut() {
        trim_span_start(first, source);
    }
    if let Some(last) = spans.last_mut() {
        trim_span_end(last, source);
    }
    spans
}

fn trim_span_start(span: &mut InlineSpan, source: &str) {
    let trim_bytes =
        span.text(source).len() - span.text(source).trim_start_matches(TRIM_CHARS).len();
    match &mut span.text {
        SourceText::Source(range) => range.start += trim_bytes,
        SourceText::Owned(text) => *text = text[trim_bytes..].into(),
    }
}

fn trim_span_end(span: &mut InlineSpan, source: &str) {
    let keep_bytes = span.text(source).trim_end_matches(TRIM_CHARS).len();
    match &mut span.text {
        SourceText::Source(range) => range.end = range.start + keep_bytes,
        SourceText::Owned(text) => *text = text[..keep_bytes].into(),
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Code;

    fn code_from_node<'a>(doc: &'a crate::Document, node: &'a Node) -> &'a Code {
        match &node.kind {
            NodeKind::Code(id) => doc.codes.by_id(*id).unwrap(),
            _ => panic!("Expected Code"),
        }
    }

    fn list_item_text(item: &ListItem, source: &str) -> String {
        match item.children.first().map(|node| &node.kind) {
            Some(NodeKind::Paragraph(spans)) => inline_text(spans, source),
            _ => String::new(),
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
            let doc = Parser::new().parse(input);
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
        let doc = Parser::new().parse("```python\n    def foo():\n        pass\n```\n");
        let c = doc.codes.first().unwrap();
        assert_eq!(c.content, "    def foo():\n        pass");

        // Leading blank lines preserved.
        let doc = Parser::new().parse("```bash\n\necho hello\n```\n");
        let c = doc.codes.first().unwrap();
        assert_eq!(c.content, "\necho hello");

        // Whitespace-only blocks are empty (no Code node).
        let text = "```bash\n   \n```\n";
        let nodes = Parser::new().parse(text).nodes;
        assert!(!nodes
            .iter()
            .any(|node| matches!(node.kind, NodeKind::Code(_))));

        // Bad attrs recover valid metadata.
        let text = "```bash [name:foo bad, bin:zsh]\necho hi\n```\n";
        let doc = Parser::new().parse(text);
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
            let nodes = Parser::new().parse(input).nodes;
            assert_eq!(nodes.len(), 1, "input: {input:?}");
            match &nodes[0].kind {
                NodeKind::List(items) => {
                    for &(idx, expected_text, ref expected_kind) in &expected_checks {
                        assert_eq!(
                            list_item_text(&items[idx], input),
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
    fn test_list_item_stores_all_blocks_as_children() {
        let input = "1. First item\n\n   Nested paragraph.\n\n   ```sh\n   echo nested\n   ```";
        let doc = Parser::new().parse(input);
        let NodeKind::List(items) = &doc.nodes[0].kind else {
            panic!("expected list");
        };
        let item = &items[0];
        assert_eq!(&input[item.range.clone()], input);

        assert_eq!(item.children.len(), 3);
        let NodeKind::Paragraph(first) = &item.children[0].kind else {
            panic!("expected first paragraph");
        };
        assert_eq!(inline_text(first, input), "First item");
        let NodeKind::Paragraph(paragraph) = &item.children[1].kind else {
            panic!("expected nested paragraph before code");
        };
        assert_eq!(inline_text(paragraph, input), "Nested paragraph.");
        assert!(matches!(item.children[2].kind, NodeKind::Code(_)));
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
            let nodes = Parser::new().parse(input).nodes;
            assert_eq!(nodes.len(), 1, "input: {input:?}");
            match &nodes[0].kind {
                NodeKind::Table(t) => {
                    assert_eq!(
                        t.headers
                            .iter()
                            .map(|cell| cell.text(input))
                            .collect::<Vec<_>>(),
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
            let doc = Parser::new().parse(input);
            let node = &doc.nodes[0];
            match &node.kind {
                NodeKind::Heading { level, text } => {
                    assert_eq!(*level, expected_level);
                    assert_eq!(inline_text(text, input), expected_text);
                }
                _ => panic!("Expected Heading"),
            }
        }

        // headings collection
        let doc = Parser::new().parse("# Title\n\n## Run `make`\n");
        assert_eq!(doc.headings.len(), 2);
        assert_eq!(doc.headings[0].level, 1);
        assert_eq!(doc.headings[0].text, "Title");
        assert_eq!(doc.headings[0].source_range, 0..8);
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
            let doc = Parser::new().parse(input);
            let actual = doc.nodes.first().and_then(|node| match &node.kind {
                NodeKind::Frontmatter { style, raw } => Some((*style, raw.resolve(&doc.source))),
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
            let doc = Parser::new().parse(input);
            assert!(
                !doc.nodes
                    .iter()
                    .any(|node| matches!(node.kind, NodeKind::Frontmatter { .. })),
                "{name}: must not be frontmatter"
            );
            assert!(
                doc.nodes
                    .iter()
                    .any(|node| matches!(node.kind, NodeKind::ThematicBreak)),
                "{name}: should parse as normal Markdown"
            );
        }
    }

    #[test]
    fn test_parse_blockquote_and_thematic_break() {
        let text = "> This is a blockquote\n> with multiple lines\n";
        let nodes = Parser::new().parse(text).nodes;
        assert_eq!(nodes.len(), 1);
        match &nodes[0].kind {
            NodeKind::BlockQuote(children) => {
                assert_eq!(children.len(), 1);
                match &children[0].kind {
                    NodeKind::Paragraph(spans) => {
                        assert_eq!(
                            inline_text(spans, text),
                            "This is a blockquote\nwith multiple lines"
                        );
                    }
                    _ => panic!("Expected Paragraph"),
                }
            }
            _ => panic!("Expected BlockQuote"),
        }

        assert!(Parser::new()
            .parse("Some text\n\n---\n\nMore text\n".to_owned())
            .nodes
            .iter()
            .any(|node| matches!(node.kind, NodeKind::ThematicBreak)));
    }

    #[test]
    fn test_parse_code_block_in_list() {
        let text = "- item 1\n  ```sh\n  echo hi\n  ```\n- item 2\n";
        let doc = Parser::new().parse(text);
        let nodes = &doc.nodes;
        assert_eq!(nodes.len(), 1);
        let items = match &nodes[0].kind {
            NodeKind::List(items) => items,
            _ => panic!("expected list"),
        };
        assert_eq!(items.len(), 2);
        assert_eq!(list_item_text(&items[0], text), "item 1");
        assert_eq!(list_item_text(&items[1], text), "item 2");
        assert_eq!(items[0].children.len(), 2);
        assert!(matches!(&items[0].children[1].kind, NodeKind::Code(code_id)
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
            let nodes = Parser::new().parse(markdown).nodes;
            let spans = match &nodes[0].kind {
                NodeKind::Paragraph(spans) => spans,
                other => panic!("Expected Paragraph for {markdown:?}, got {other:?}"),
            };

            assert_eq!(
                inline_text(spans, markdown),
                expected_text,
                "input: {markdown:?}"
            );
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
            let nodes = Parser::new().parse(markdown).nodes;
            match &nodes[0].kind {
                NodeKind::Image { alt, src } => {
                    assert_eq!(alt, expected_alt, "input: {markdown:?}");
                    assert_eq!(src, expected_src, "input: {markdown:?}");
                }
                other => panic!("Expected standalone Image for {markdown:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_paragraph_with_mixed_text_and_image_stays_paragraph() {
        let nodes = Parser::new().parse("text ![alt](image.png)").nodes;
        match &nodes[0].kind {
            NodeKind::Paragraph(spans) => {
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
                matches!(
                    Parser::new().parse(markdown.to_owned()).nodes[0].kind,
                    NodeKind::Paragraph(_)
                ),
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
            let nodes = Parser::new().parse(&input).nodes;
            let table = match &nodes[0].kind {
                NodeKind::Table(table) => table,
                other => panic!("Expected Table for {markdown:?}, got {other:?}"),
            };
            let cell = &table.rows[0][0];

            assert_eq!(cell.text(&input), expected_text, "input: {markdown:?}");
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
            let doc = Parser::new().parse(input);
            let text = match &doc.nodes[0].kind {
                NodeKind::HtmlBlock => doc.source[doc.nodes[0].range.clone()].to_owned(),
                NodeKind::Paragraph(spans) => inline_text(spans, input),
                NodeKind::Heading { text, .. } => inline_text(text, input),
                other => panic!("Expected {input:?} to start with a text node, got {other:?}"),
            };
            assert_eq!(text, expected, "input: {input:?}");
        }

        // Semantic heading labels exclude HTML tags.
        let doc = Parser::new().parse("# Title <b>with tag</b>\n");
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
            let nodes = Parser::new().parse(&input).nodes;
            let items = match &nodes[0].kind {
                NodeKind::List(items) => items,
                other => panic!("Expected List for {markdown:?}, got {other:?}"),
            };

            let NodeKind::Paragraph(spans) = &items[0].children[0].kind else {
                panic!("expected list-item paragraph");
            };
            assert_eq!(inline_text(spans, &input), expected_text);
            assert_eq!(spans.len(), 1, "input: {markdown:?}");
            assert_eq!(spans[0].style, expected_styles, "input: {markdown:?}");
        }
    }
    #[test]
    fn nodes_and_inline_text_reference_the_document_source() {
        let markdown = "plain **bold**";
        let doc = Parser::new().parse(markdown);
        let node = &doc.nodes[0];
        assert_eq!(&doc.source[node.range.clone()], markdown);
        let NodeKind::Paragraph(spans) = &node.kind else {
            panic!("expected paragraph");
        };
        assert!(spans
            .iter()
            .all(|span| matches!(span.text, SourceText::Source(_))));
        assert_eq!(inline_text(spans, &doc.source), "plain bold");
    }
}

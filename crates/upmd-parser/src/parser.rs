use pulldown_cmark::{
    Alignment as CmarkAlignment, CodeBlockKind, Event, HeadingLevel, Options,
    Parser as CmarkParser, Tag, TagEnd,
};

use super::nodes::{Alignment, Codes, ListItem, ListKind, Node, Table, TaskStatus};
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
        let options = Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
        let parser = CmarkParser::new_ext(text, options).into_offset_iter();
        let line_starts = line_starts(text);
        let mut p = Parser {
            iter: parser.peekable(),
            codes: Codes::default(),
            headings: Vec::new(),
            line_starts,
        };
        let nodes = p.parse_blocks(None);
        super::Document {
            nodes,
            codes: p.codes,
            headings: p.headings,
            nodes_state: super::NodesState::Full,
        }
    }
}

// Internal recursive-descent parser

struct Parser<'a> {
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
                Event::Start(Tag::Table(alignments)) => {
                    nodes.push(self.parse_table(&alignments));
                }
                Event::Rule => {
                    nodes.push(Node::ThematicBreak);
                }
                Event::Text(t) | Event::Code(t) => {
                    nodes.push(Node::Text(t.into_string()));
                }
                _ => {}
            }
        }
        nodes
    }

    // Paragraph

    fn parse_paragraph(&mut self) -> Node {
        let text = self
            .parse_inline_content(TagEnd::Paragraph)
            .trim()
            .to_string();
        Node::Paragraph(text)
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
        let mut text = String::new();
        loop {
            match self.iter.next() {
                Some((Event::End(TagEnd::Heading(_)), _)) => break,
                Some((Event::Text(t) | Event::Code(t), _)) => text.push_str(&t),
                None => break,
                _ => {}
            }
        }
        let text = text.trim().to_string();
        self.headings.push(super::Heading {
            level: heading_level,
            text: text.clone(),
            source_range: source_range.clone(),
            start_line: byte_to_line(&self.line_starts, source_range.start),
            end_line: byte_to_line(&self.line_starts, source_range.end.max(1) - 1),
        });
        Node::Heading {
            level: heading_level,
            text,
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
                    if in_header {
                        headers.push(String::new());
                    } else {
                        while rows.len() <= row_idx {
                            rows.push(Vec::new());
                        }
                        rows[row_idx].push(String::new());
                    }
                }
                Some((Event::Text(t) | Event::Code(t), _)) => {
                    if in_header {
                        if let Some(cell) = headers.last_mut() {
                            cell.push_str(&t);
                        }
                    } else if let Some(row) = rows.get_mut(row_idx) {
                        if let Some(cell) = row.last_mut() {
                            cell.push_str(&t);
                        }
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
        let mut text = String::new();
        let mut children = Vec::new();
        let mut task_kind: Option<ListKind> = None;

        loop {
            match self.iter.next() {
                Some((Event::End(TagEnd::Item), _)) => break,
                Some((Event::TaskListMarker(checked), _)) => {
                    task_kind = Some(ListKind::Task(if checked {
                        TaskStatus::Checked
                    } else {
                        TaskStatus::Unchecked
                    }));
                }
                Some((Event::Text(t), _)) => text.push_str(&t),
                Some((Event::Code(t), _)) => {
                    text.push('`');
                    text.push_str(&t);
                    text.push('`');
                }
                Some((Event::SoftBreak | Event::HardBreak, _)) => text.push('\n'),
                Some((Event::Start(Tag::CodeBlock(kind)), _)) => {
                    if let Some(node) = self.parse_code_block(kind) {
                        children.push(node);
                    }
                }
                Some((Event::Start(Tag::List(start)), _)) => {
                    children.push(Node::List(self.parse_list(depth + 1, start)));
                }
                Some((Event::Start(Tag::BlockQuote(_)), _)) => {
                    children.push(Node::BlockQuote(
                        self.parse_blocks(Some(TagEnd::BlockQuote(None))),
                    ));
                }
                Some((Event::Start(Tag::Table(alignments)), _)) => {
                    children.push(self.parse_table(&alignments));
                }
                Some((Event::Start(Tag::Heading { level, .. }), range)) => {
                    children.push(self.parse_heading(level, range));
                }
                Some((Event::Start(Tag::Paragraph), _)) => {}
                None => break,
                _ => {}
            }
        }

        let raw = text.trim();
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
            text: raw.to_string(),
            children,
        }
    }

    // Shared inline content parser
    //
    // InlineContent ::= (Text | Code | Break)*

    /// Consumes events until `stop`, producing a single text string.
    /// Inline code is wrapped in backticks; soft/hard breaks become newlines.
    fn parse_inline_content(&mut self, stop: TagEnd) -> String {
        let mut text = String::new();
        loop {
            match self.iter.next() {
                Some((Event::End(tag), _)) if tag == stop => break,
                Some((Event::Text(t), _)) => text.push_str(&t),
                Some((Event::Code(t), _)) => {
                    text.push('`');
                    text.push_str(&t);
                    text.push('`');
                }
                Some((Event::SoftBreak | Event::HardBreak, _)) => text.push('\n'),
                None => break,
                _ => {}
            }
        }
        text
    }
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
                            items[idx].text, expected_text,
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
                    assert_eq!(t.headers, expected_headers, "input: {input:?}");
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
                    assert_eq!(text, expected_text);
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
        assert_eq!(doc.headings[1].text, "Run make");
        assert_eq!(doc.nodes_state, crate::NodesState::Full);
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
                    Node::Paragraph(s) => assert!(s.contains("blockquote")),
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
        assert_eq!(items[0].text, "item 1");
        assert_eq!(items[1].text, "item 2");
        assert_eq!(items[0].children.len(), 1);
        assert!(matches!(&items[0].children[0], Node::Code(code_id)
            if doc.codes.iter().any(|c| c.id == *code_id && c.content.trim() == "echo hi")));
    }
}

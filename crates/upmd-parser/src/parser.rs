use pulldown_cmark::{
    Alignment as CmarkAlignment, CodeBlockKind, Event, HeadingLevel, Options,
    Parser as CmarkParser, Tag, TagEnd,
};

use super::nodes::{Alignment, Code, ListItem, ListKind, Node, Table, TaskStatus};
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
            code_id_counter: 1,
            codes: Vec::new(),
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
    code_id_counter: u32,
    codes: Vec<Code>,
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
        let language = options::parse_language(&opts);
        let mut options = options::parse(&opts).unwrap_or_default();
        options.language = language;
        let code = Code::new(self.code_id_counter, content, options);
        self.code_id_counter += 1;
        let code_id = code.id;
        self.codes.push(code);
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
    use crate::Parser;

    /// Retrieves the Code for a Node::Code variant by resolving its CodeId
    /// against the Document's codes.
    fn code_from_node<'a>(doc: &'a crate::Document, node: &'a Node) -> &'a Code {
        match node {
            Node::Code(id) => doc.codes.iter().find(|c| c.id == *id).unwrap(),
            _ => panic!("Expected Code"),
        }
    }

    #[test]
    fn test_parse_table() {
        let text = "| Header 1 | Header 2 |\n|----------|----------|\n| Cell 1   | Cell 2   |\n| Cell 3   | Cell 4   |\n";
        let nodes = Cmark::new().parse(text).nodes;
        assert_eq!(nodes.len(), 1);
        match &nodes[0] {
            Node::Table(t) => {
                assert_eq!(t.headers, vec!["Header 1", "Header 2"]);
                assert_eq!(t.rows.len(), 2);
            }
            _ => panic!("Expected Table"),
        }
    }

    #[test]
    fn test_parse_heading() {
        let nodes = Cmark::new().parse("### My Heading").nodes;
        assert_eq!(nodes.len(), 1);
        match &nodes[0] {
            Node::Heading { level, text } => {
                assert_eq!(*level, 3);
                assert_eq!(text, "My Heading");
            }
            _ => panic!("Expected Heading"),
        }
    }

    #[test]
    fn test_document_headings_are_collected() {
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
    fn test_parse_code_metadata_is_no_longer_stored() {
        let text = "### Hello\n\nWorld\n\n```bash\necho 1\n```\n";
        let doc = Cmark::new().parse(text);
        let nodes = &doc.nodes;
        assert_eq!(nodes.len(), 3);
        if let Node::Code(code_id) = &nodes[2] {
            let c = code_from_node(&doc, &nodes[2]);
            assert_eq!(c.content, "echo 1");
            assert_eq!(c.language, "bash");
        } else {
            panic!("Expected Code");
        }
    }

    #[test]
    fn test_parse_named_code_block() {
        let text = "```bash [name:setup]\necho \"hello\"\n```\n";
        let doc = Cmark::new().parse(text);
        let nodes = &doc.nodes;
        assert_eq!(nodes.len(), 1);
        if let Node::Code(code_id) = &nodes[0] {
            let c = code_from_node(&doc, &nodes[0]);
            assert_eq!(c.name, "setup");
            assert_eq!(c.language, "bash");
        } else {
            panic!("Expected Code");
        }
    }

    #[test]
    fn test_parse_code_block_without_name_is_empty() {
        let doc = Cmark::new().parse("```bash\necho \"hello\"\n```\n");
        let nodes = &doc.nodes;
        if let Node::Code(code_id) = &nodes[0] {
            let c = code_from_node(&doc, &nodes[0]);
            assert!(c.name.is_empty());
        } else {
            panic!("Expected Code");
        }
    }

    #[test]
    fn test_consecutive_code_blocks_dont_inherit_title_desc() {
        // Title/desc were removed (C5); this test verifies code blocks still
        // resolve correctly without title/desc fields.
        let text = "Example 2\n\n```sh\necho first\n```\n\n```sh\necho second\n```\n";
        let doc = Cmark::new().parse(text);
        let nodes = &doc.nodes;
        assert_eq!(nodes.len(), 3);
        if let Node::Code(code_id) = &nodes[1] {
            let c = code_from_node(&doc, &nodes[1]);
            assert_eq!(c.content, "echo first");
        } else {
            panic!();
        }
        if let Node::Code(code_id) = &nodes[2] {
            let c = code_from_node(&doc, &nodes[2]);
            assert_eq!(c.content, "echo second");
        } else {
            panic!();
        }
    }

    #[test]
    fn test_parse_blockquote() {
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
    }

    #[test]
    fn test_parse_bullet_list() {
        let text = "- Item 1\n- Item 2\n- Item 3\n";
        let nodes = Cmark::new().parse(text).nodes;
        assert_eq!(nodes.len(), 1);
        match &nodes[0] {
            Node::List(items) => {
                assert_eq!(items.len(), 3);
                assert!(matches!(items[0].kind, ListKind::Bullet));
                assert_eq!(items[0].text, "Item 1");
            }
            _ => panic!("Expected List"),
        }
    }

    #[test]
    fn test_parse_ordered_list() {
        let text = "1. First\n2. Second\n3. Third\n";
        let nodes = Cmark::new().parse(text).nodes;
        assert_eq!(nodes.len(), 1);
        match &nodes[0] {
            Node::List(items) => {
                assert_eq!(items.len(), 3);
                assert!(matches!(items[0].kind, ListKind::Ordered(1)));
                assert_eq!(items[0].text, "First");
            }
            _ => panic!("Expected List"),
        }
    }

    #[test]
    fn test_parse_task_list_unchecked() {
        let nodes = Cmark::new().parse("- [ ] Unchecked task\n").nodes;
        assert_eq!(nodes.len(), 1);
        match &nodes[0] {
            Node::List(items) => {
                assert_eq!(items.len(), 1);
                assert!(matches!(
                    items[0].kind,
                    ListKind::Task(TaskStatus::Unchecked)
                ));
                assert_eq!(items[0].text, "Unchecked task");
            }
            _ => panic!("Expected List"),
        }
    }

    #[test]
    fn test_parse_task_list_checked() {
        let nodes = Cmark::new().parse("- [x] Completed task\n").nodes;
        assert_eq!(nodes.len(), 1);
        match &nodes[0] {
            Node::List(items) => {
                assert!(matches!(items[0].kind, ListKind::Task(TaskStatus::Checked)));
                assert_eq!(items[0].text, "Completed task");
            }
            _ => panic!("Expected List"),
        }
    }

    #[test]
    fn test_parse_task_list_in_progress() {
        let nodes = Cmark::new().parse("- [-] In progress task\n").nodes;
        assert_eq!(nodes.len(), 1);
        match &nodes[0] {
            Node::List(items) => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].text, "[-] In progress task");
            }
            _ => panic!("Expected List"),
        }
    }

    #[test]
    fn test_parse_thematic_break() {
        let nodes = Cmark::new().parse("Some text\n\n---\n\nMore text\n").nodes;
        assert!(nodes.iter().any(|n| matches!(n, Node::ThematicBreak)));
    }

    #[test]
    fn test_parse_indented_code_block() {
        let text = "Some text\n\n    echo hello\n    world\n";
        let doc = Cmark::new().parse(text);
        let nodes = &doc.nodes;
        assert_eq!(nodes.len(), 2);
        match &nodes[1] {
            Node::Code(code_id) => {
                let c = code_from_node(&doc, &nodes[1]);
                assert_eq!(c.content, "echo hello\nworld");
            }
            _ => panic!("Expected Code, got {:?}", nodes[1]),
        }
    }

    #[test]
    fn test_parse_indented_code_block_no_language() {
        let text = "    just code\n    no lang\n";
        let doc = Cmark::new().parse(text);
        let nodes = &doc.nodes;
        assert_eq!(nodes.len(), 1);
        match &nodes[0] {
            Node::Code(code_id) => {
                let c = code_from_node(&doc, &nodes[0]);
                assert_eq!(c.language, "");
                assert_eq!(c.content, "just code\nno lang");
            }
            _ => panic!("Expected Code, got {:?}", nodes[0]),
        }
    }

    #[test]
    fn test_parse_fenced_and_indented_code_blocks() {
        let text = "```rust\nfn main() {}\n```\n\n    some indented code\n";
        let doc = Cmark::new().parse(text);
        let nodes = &doc.nodes;
        assert_eq!(nodes.len(), 2);
        match &nodes[0] {
            Node::Code(code_id) => {
                let c = code_from_node(&doc, &nodes[0]);
                assert_eq!(c.language, "rust");
                assert_eq!(c.content, "fn main() {}");
            }
            _ => panic!(),
        }
        match &nodes[1] {
            Node::Code(code_id) => {
                let c = code_from_node(&doc, &nodes[1]);
                assert_eq!(c.language, "");
                assert_eq!(c.content, "some indented code");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_parse_nested_list_trailing_content() {
        let text = "- Item with\n  - nested\n\n  trailing text\n";
        let nodes = Cmark::new().parse(text).nodes;
        assert_eq!(nodes.len(), 1);
        match &nodes[0] {
            Node::List(items) => {
                let outer = items.iter().find(|i| i.depth == 1).expect("depth-1 item");
                assert!(outer.text.contains("Item with"));
                assert!(outer.text.contains("trailing text"));
            }
            _ => panic!("Expected List"),
        }
    }

    #[test]
    fn test_parse_alignment_none() {
        let text = "| A | B |\n|---|---|\n| 1 | 2 |\n";
        let nodes = Cmark::new().parse(text).nodes;
        assert_eq!(nodes.len(), 1);
        match &nodes[0] {
            Node::Table(t) => {
                assert_eq!(t.alignments.len(), 2);
                assert_eq!(t.alignments[0], Alignment::None);
                assert_eq!(t.alignments[1], Alignment::None);
            }
            _ => panic!(),
        }
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

    #[test]
    fn test_parse_code_block_preserves_leading_whitespace() {
        // Leading indentation (Python-style) is no longer stripped by trim().
        let doc = Cmark::new().parse("```python\n    def foo():\n        pass\n```\n");
        let c = doc.codes.first().unwrap();
        assert_eq!(c.content, "    def foo():\n        pass");

        // Leading blank lines between opening fence and content are preserved.
        let doc = Cmark::new().parse("```bash\n\necho hello\n```\n");
        let c = doc.codes.first().unwrap();
        assert_eq!(c.content, "\necho hello");

        // Whitespace-only blocks are still treated as empty (no Code node).
        let text = "```bash\n   \n```\n";
        let nodes = Cmark::new().parse(text).nodes;
        assert!(!nodes.iter().any(|n| matches!(n, Node::Code(_))));
    }

    #[test]
    fn test_parse_code_block_preserves_language_on_bad_attrs() {
        let text = "```bash [name:foo bad]\necho hi\n```\n";
        let doc = Cmark::new().parse(text);
        let c = doc.codes.first().unwrap();
        assert_eq!(c.language, "bash");
    }
}

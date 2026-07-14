use std::collections::HashMap;
use std::ops::Range;
#[derive(Debug, Clone)]
pub enum Node {
    Heading { level: u8, text: String },
    Paragraph(String),
    BlockQuote(Vec<Node>),
    List(Vec<ListItem>),
    Code(CodeId),
    Table(Table),
    Text(String),
    ThematicBreak,
}

/// Heading metadata collected during parsing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heading {
    pub level: u8,
    pub text: String,
    pub source_range: Range<usize>,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    Unchecked,
    Checked,
    InProgress,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ListKind {
    Bullet,
    Ordered(u64),
    Task(TaskStatus),
}

/// A single list entry with depth, kind, text, and nested children.
#[derive(Debug, Clone)]
pub struct ListItem {
    pub depth: usize,
    pub kind: ListKind,
    pub text: String,
    pub children: Vec<Node>,
}
/// Markdown table with headers, rows, and column alignments.
#[derive(Debug, Clone)]
pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub alignments: Vec<Alignment>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Alignment {
    Left,
    Center,
    Right,
    None,
}

pub type CodeId = u32;

/// Parsed code block with language, content, and execution metadata.
#[derive(Debug, Default, Clone)]
pub struct Code {
    pub id: CodeId,
    pub language: String,
    pub name: String,
    pub content: String,
    pub options: Options,
}

/// Parser options for a code block: language and custom attributes.
#[derive(Debug, Default, Clone)]
pub struct Options {
    pub language: String,
    /// Arbitrary key:value attributes from fence info (e.g. `name`, `bin`, ...).
    pub attrs: HashMap<String, String>,
}

impl Code {
    pub fn new(id: u32, content: String, options: Options) -> Self {
        let name = options.attrs.get("name").cloned().unwrap_or_default();
        Self {
            id,
            name,
            content,
            language: options.language.clone(),
            options,
        }
    }

    /// Returns an excerpt from the code.
    pub fn excerpt(&self, lines: usize) -> String {
        self.content
            .lines()
            .take(lines)
            .collect::<Vec<&str>>()
            .join("\n")
    }
}

/// Resolves a block spec (name or numeric ID) to matching code IDs by
/// operating directly on a slice of [`Code`].
///
/// If `spec` parses as a valid integer, returns blocks whose ID equals that
/// number. Otherwise, matches blocks whose `name` field equals `spec`.
pub fn resolve_code_block(codes: &[Code], spec: &str) -> Vec<CodeId> {
    if let Ok(id) = spec.parse::<CodeId>() {
        codes
            .iter()
            .filter_map(|c| if c.id == id { Some(c.id) } else { None })
            .collect()
    } else {
        codes
            .iter()
            .filter_map(|c| if c.name == spec { Some(c.id) } else { None })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_excerpt_single_line() {
        let code = Code {
            content: "line1\nline2\nline3".into(),
            ..Default::default()
        };
        assert_eq!(code.excerpt(1), "line1");
        assert_eq!(code.excerpt(2), "line1\nline2");
        assert_eq!(code.excerpt(10), "line1\nline2\nline3");
    }

    #[test]
    fn test_code_excerpt_empty() {
        let code = Code::default();
        assert_eq!(code.excerpt(5), "");
    }

    #[test]
    fn test_resolve_block_by_id() {
        let codes = vec![
            Code {
                id: 1,
                name: "".into(),
                content: "a".into(),
                ..Default::default()
            },
            Code {
                id: 2,
                name: "setup".into(),
                content: "b".into(),
                ..Default::default()
            },
        ];
        assert_eq!(resolve_code_block(&codes, "1"), vec![1]);
        assert_eq!(resolve_code_block(&codes, "2"), vec![2]);
    }

    #[test]
    fn test_resolve_block_by_name() {
        let codes = vec![
            Code {
                id: 1,
                name: "".into(),
                content: "a".into(),
                ..Default::default()
            },
            Code {
                id: 2,
                name: "setup".into(),
                content: "b".into(),
                ..Default::default()
            },
        ];
        assert_eq!(resolve_code_block(&codes, "setup"), vec![2]);
    }

    #[test]
    fn test_resolve_code_block() {
        let codes = vec![
            Code {
                id: 1,
                name: "".into(),
                content: "a".into(),
                ..Default::default()
            },
            Code {
                id: 2,
                name: "build".into(),
                content: "b".into(),
                ..Default::default()
            },
        ];
        assert_eq!(resolve_code_block(&codes, "1"), vec![1]);
        assert_eq!(resolve_code_block(&codes, "build"), vec![2]);
        assert!(resolve_code_block(&codes, "99").is_empty());
    }

    #[test]
    fn test_resolve_block_no_match() {
        let codes = vec![Code {
            id: 1,
            name: "setup".into(),
            content: "a".into(),
            ..Default::default()
        }];
        assert!(resolve_code_block(&codes, "nonexistent").is_empty());
        assert!(resolve_code_block(&codes, "99").is_empty());
    }
}

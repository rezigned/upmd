use std::collections::HashMap;
use std::ops::Range;

#[derive(Debug, Clone)]
pub enum Node {
    Heading {
        level: u8,
        text: Vec<InlineSpan>,
    },
    Paragraph(Vec<InlineSpan>),
    BlockQuote(Vec<Node>),
    List(Vec<ListItem>),
    Code(CodeId),
    Table(Table),
    Text(Vec<InlineSpan>),
    HtmlBlock(String),
    ThematicBreak,
    /// A standalone image paragraph (`![alt](src)` as its only content).
    Image {
        alt: String,
        src: String,
    },
}

/// A run of inline text with the formatting applied to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineSpan {
    pub text: String,
    /// Active styles for this run. Empty means plain text; styles nest in
    /// source order (e.g. bold inside italic becomes `[Italic, Bold]`).
    pub style: Vec<InlineStyle>,
}

/// Inline markdown formatting applied to a [`InlineSpan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InlineStyle {
    Bold,
    Italic,
    Strikethrough,
    InlineCode,
    Link {
        destination: String,
        title: Option<String>,
    },
    Image {
        alt: String,
        src: String,
    },
    HtmlTag,
}

/// Concatenates span text into a single plain string (for search, copy, menus).
pub fn inline_text(spans: &[InlineSpan]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

/// Concatenates span text, excluding HTML tags (for semantic labels).
pub fn semantic_text(spans: &[InlineSpan]) -> String {
    spans
        .iter()
        .filter(|s| !s.style.contains(&InlineStyle::HtmlTag))
        .map(|s| s.text.as_str())
        .collect()
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
    pub text: Vec<InlineSpan>,
    pub children: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableCell {
    pub spans: Vec<InlineSpan>,
}

impl TableCell {
    pub fn text(&self) -> String {
        inline_text(&self.spans)
    }

    pub fn char_len(&self) -> usize {
        self.spans
            .iter()
            .map(|span| span.text.chars().count())
            .sum()
    }
}

/// Markdown table with headers, rows, and column alignments.
#[derive(Debug, Clone)]
pub struct Table {
    pub headers: Vec<TableCell>,
    pub rows: Vec<Vec<TableCell>>,
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

/// Parsed dependency groups from a code block's `deps` attribute.
///
/// `Ok(groups)` groups are comma-separated, `|` separates parallel items.
/// `Err(error)` the attribute could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependencies(pub Result<DepsGroups, String>);
pub(crate) type DepsGroups = Vec<Vec<String>>;

impl std::ops::Deref for Dependencies {
    type Target = Result<DepsGroups, String>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Default for Dependencies {
    fn default() -> Self {
        Self(Ok(Vec::new()))
    }
}

impl Dependencies {
    /// Parsed dependency groups, or an error message.
    pub fn groups(&self) -> Result<&[Vec<String>], &str> {
        self.as_deref().map_err(|e| e.as_str())
    }

    /// Returns `true` if there's no dependencies declared.
    pub fn is_empty(&self) -> bool {
        self.as_deref().is_ok_and(|groups| groups.is_empty())
    }

    /// Tokenizes the dependency declaration for styled display.
    pub fn segments(&self) -> Vec<DepsToken<'_>> {
        match &**self {
            Ok(groups) if groups.is_empty() => vec![],
            Ok(groups) => {
                let mut tokens = vec![DepsToken::Punct(" [")];
                for (gi, group) in groups.iter().enumerate() {
                    if gi > 0 {
                        tokens.push(DepsToken::Punct(" → "));
                    }
                    for (ni, name) in group.iter().enumerate() {
                        if ni > 0 {
                            tokens.push(DepsToken::Punct(" | "));
                        }
                        tokens.push(DepsToken::Name(name));
                    }
                }
                tokens.push(DepsToken::Punct("]"));
                tokens
            }
            Err(_) => vec![DepsToken::Punct(" [invalid]")],
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum DepsToken<'a> {
    Punct(&'a str),
    Name(&'a str),
}

impl DepsToken<'_> {
    pub fn text(&self) -> &str {
        match self {
            Self::Punct(s) | Self::Name(s) => s,
        }
    }
}

use std::fmt;

impl fmt::Display for Dependencies {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for token in self.segments() {
            write!(f, "{}", token.text())?;
        }
        Ok(())
    }
}

/// Parsed code block with language, content, and execution metadata.
#[derive(Debug, Default, Clone)]
pub struct Code {
    pub id: CodeId,
    pub language: String,
    pub name: String,
    pub deps: Dependencies,
    pub attrs: HashMap<String, String>,
    /// Errors found while parsing the fence attributes.
    pub errors: Vec<String>,
    pub content: String,
}

/// Parsed language, attributes, and execution metadata for a code block.
#[derive(Debug, Default, Clone)]
pub struct Options {
    pub language: String,
    pub name: String,
    pub deps: Dependencies,
    /// Arbitrary key:value attributes from fence info (e.g. `name`, `bin`, ...).
    pub attrs: HashMap<String, String>,
    /// Recoverable errors found while parsing the fence attributes.
    pub errors: Vec<String>,
}

impl Code {
    pub fn new(id: u32, content: String, options: Options) -> Self {
        let Options {
            language,
            name,
            deps,
            attrs,
            errors,
        } = options;

        Self {
            id,
            language,
            name,
            deps,
            attrs,
            errors,
            content,
        }
    }
}

/// Code blocks in document order, with each ID equal to its one-based position.
#[derive(Clone, Debug, Default)]
pub struct Codes(Vec<Code>);

impl Codes {
    pub(crate) fn push(&mut self, content: String, options: Options) -> CodeId {
        let id = CodeId::try_from(self.len() + 1)
            .expect("document contains more code blocks than CodeId supports");
        self.0.push(Code::new(id, content, options));
        id
    }

    pub fn by_id(&self, id: CodeId) -> Option<&Code> {
        self.index_of(id).and_then(|index| self.0.get(index))
    }

    pub fn index_of(&self, id: CodeId) -> Option<usize> {
        let index = usize::try_from(id.checked_sub(1)?).ok()?;
        self.0.get(index).map(|_| index)
    }

    pub fn resolve(&self, spec: &str) -> Vec<CodeId> {
        match spec.parse::<CodeId>() {
            Ok(id) => self.by_id(id).map(|code| vec![code.id]).unwrap_or_default(),
            Err(_) => self
                .0
                .iter()
                .filter(|code| code.name == spec)
                .map(|code| code.id)
                .collect(),
        }
    }
}

impl std::ops::Deref for Codes {
    type Target = [Code];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a> IntoIterator for &'a Codes {
    type Item = &'a Code;
    type IntoIter = std::slice::Iter<'a, Code>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl TryFrom<Vec<Code>> for Codes {
    type Error = String;

    fn try_from(codes: Vec<Code>) -> Result<Self, Self::Error> {
        for (index, code) in codes.iter().enumerate() {
            let expected = CodeId::try_from(index + 1)
                .map_err(|_| "code collection exceeds CodeId capacity".to_string())?;
            if code.id != expected {
                return Err(format!(
                    "code at index {index} has ID {}, expected {expected}",
                    code.id
                ));
            }
        }
        Ok(Self(codes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_code_block() {
        let codes = Codes::try_from(vec![
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
            Code {
                id: 3,
                name: "build".into(),
                content: "c".into(),
                ..Default::default()
            },
            Code {
                id: 4,
                name: "2".into(),
                ..Default::default()
            },
        ])
        .unwrap();
        for (spec, expected) in [
            ("1", &[1u32] as &[u32]),
            ("2", &[2u32]),
            ("setup", &[2u32]),
            ("build", &[3u32]),
            ("99", &[] as &[u32]),
            ("nonexistent", &[] as &[u32]),
        ] {
            assert_eq!(codes.resolve(spec), expected);
        }
    }

    #[test]
    fn test_code_new_dependencies() {
        for (info, expected) in [
            (
                r#"sh [deps:"setup, verify"]"#,
                vec![vec!["setup"], vec!["verify"]],
            ),
            (r#"sh [deps:"setup"]"#, vec![vec!["setup"]]),
            (r#"sh [deps:"build | lint"]"#, vec![vec!["build", "lint"]]),
            (
                r#"sh [deps:"setup, build | lint, test"]"#,
                vec![vec!["setup"], vec!["build", "lint"], vec!["test"]],
            ),
        ] {
            let code = Code::new(1, "echo hi".into(), crate::options::parse(info));
            assert_eq!(code.deps.groups().unwrap(), expected);
        }

        let code = Code::new(1, "echo hi".into(), crate::options::parse("sh"));
        assert!(code.deps.is_empty());
    }
}

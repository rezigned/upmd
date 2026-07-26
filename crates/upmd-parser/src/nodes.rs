use std::collections::{HashMap, HashSet};
use std::ops::Range;

use crate::options;

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

/// Parsed dependency groups from a code block's `deps` attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dependencies {
    Valid(Vec<Vec<String>>),
    Invalid(String),
}

impl Default for Dependencies {
    fn default() -> Self {
        Self::Valid(Vec::new())
    }
}

impl Dependencies {
    pub fn groups(&self) -> Result<&[Vec<String>], &str> {
        match self {
            Self::Valid(groups) => Ok(groups),
            Self::Invalid(error) => Err(error),
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Valid(groups) if groups.is_empty())
    }

    pub fn segments(&self) -> Vec<DepsToken<'_>> {
        match self {
            Self::Valid(groups) if groups.is_empty() => vec![],
            Self::Valid(groups) => {
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
            Self::Invalid(_) => vec![DepsToken::Punct(" [invalid]")],
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
    pub dependencies: Dependencies,
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
        let dependencies =
            match options::parse_dependencies(options.attrs.get("deps").map(String::as_str)) {
                Ok(groups) => Dependencies::Valid(groups),
                Err(error) => Dependencies::Invalid(error),
            };

        Self {
            id,
            name,
            dependencies,
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

/// Resolves a block name or numeric ID.
pub fn resolve_code_block(codes: &[Code], spec: &str) -> Vec<CodeId> {
    match spec.parse::<CodeId>() {
        Ok(id) => codes
            .iter()
            .filter(|code| code.id == id)
            .map(|code| code.id)
            .collect(),
        Err(_) => codes
            .iter()
            .filter(|code| code.name == spec)
            .map(|code| code.id)
            .collect(),
    }
}

/// Resolves dependency names and numeric IDs while preserving their groups.
pub fn resolve_dependencies(
    codes: &[Code],
    dependencies: &Dependencies,
) -> Result<Vec<Vec<CodeId>>, String> {
    let groups = dependencies.groups()?;
    let mut seen = HashSet::new();

    groups
        .iter()
        .map(|group| {
            group
                .iter()
                .map(|dependency| {
                    let matches = resolve_code_block(codes, dependency);
                    let id = match matches.as_slice() {
                        [] => {
                            return Err(format!(
                                "dependency {dependency:?} not found in document"
                            ))
                        }
                        [id] => *id,
                        _ => {
                            return Err(format!(
                                "dependency {dependency:?} is ambiguous ({} matches)",
                                matches.len()
                            ))
                        }
                    };

                    if !seen.insert(id) {
                        return Err(format!(
                            "dependency {dependency:?} refers to block {id}, which is already listed"
                        ));
                    }
                    Ok(id)
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_excerpt() {
        let code = Code {
            content: "line1\nline2\nline3".into(),
            ..Default::default()
        };
        for (lines, expected) in [
            (1, "line1"),
            (2, "line1\nline2"),
            (10, "line1\nline2\nline3"),
        ] {
            assert_eq!(code.excerpt(lines), expected);
        }
        assert_eq!(Code::default().excerpt(5), "");
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
        ];
        for (spec, expected) in [
            ("1", &[1u32] as &[u32]),
            ("2", &[2u32]),
            ("setup", &[2u32]),
            ("build", &[3u32]),
            ("99", &[] as &[u32]),
            ("nonexistent", &[] as &[u32]),
        ] {
            assert_eq!(resolve_code_block(&codes, spec), expected);
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
            let code = Code::new(1, "echo hi".into(), crate::options::parse(info).unwrap());
            assert_eq!(code.dependencies.groups().unwrap(), expected);
        }

        let code = Code::new(1, "echo hi".into(), crate::options::parse("sh").unwrap());
        assert!(code.dependencies.is_empty());
    }

    #[test]
    fn test_resolve_dependencies() {
        let codes = vec![
            Code {
                id: 1,
                name: "setup".into(),
                ..Default::default()
            },
            Code {
                id: 2,
                name: "build".into(),
                ..Default::default()
            },
            Code {
                id: 3,
                name: "test".into(),
                ..Default::default()
            },
            Code {
                id: 10,
                ..Default::default()
            },
            Code {
                id: 20,
                ..Default::default()
            },
        ];
        for (groups, expected) in [
            (vec![vec!["setup".to_string()]], vec![vec![1]]),
            (vec![vec!["test".to_string()]], vec![vec![3]]),
            (
                vec![vec!["10".to_string()], vec!["20".to_string()]],
                vec![vec![10], vec![20]],
            ),
            (vec![vec!["build".to_string()]], vec![vec![2]]),
        ] {
            let dependencies = Dependencies::Valid(groups);
            assert_eq!(
                resolve_dependencies(&codes, &dependencies).unwrap(),
                expected
            );
        }

        let missing = Dependencies::Valid(vec![vec!["missing".to_string()]]);
        assert!(resolve_dependencies(&codes, &missing).is_err());
    }
}

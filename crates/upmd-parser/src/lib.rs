pub mod nodes;
pub mod options;
pub mod parser;

pub use nodes::{resolve_code_block, Code, CodeId, Heading, Node};

/// Completeness of [`Document::nodes`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodesState {
    /// No preview AST nodes were retained.
    NotParsed,
    /// Preview AST nodes cover a bounded source window.
    Partial {
        start_line: usize,
        end_line: usize,
        reason: PartialReason,
    },
    /// Preview AST nodes cover the full document.
    Full,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PartialReason {
    InitialViewport,
    AroundCodeBlock(CodeId),
}

/// The result of parsing a markdown document.
///
/// `codes` and `headings` describe the source document. `nodes` may be full,
/// partial, or absent depending on `nodes_state`.
#[derive(Clone, Debug)]
pub struct Document {
    pub nodes: Vec<Node>,
    pub codes: Vec<Code>,
    pub headings: Vec<Heading>,
    pub nodes_state: NodesState,
}

pub trait Parser {
    fn parse(&self, text: &str) -> Document;
}

pub fn new() -> impl Parser {
    parser::Cmark::new()
}

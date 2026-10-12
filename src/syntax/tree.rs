use std::slice;

use crate::diagnostic::{Diagnostic, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxKind {
    Module,
    Import,
    Binding,
    Assignment,
    Return,
    Function,
    Record,
    Try,
    Handler,
    Cleanup,
    Throw,
    Rethrow,
    If,
    Concurrent,
    For,
    While,
    Break,
    Continue,
    Exit,
    CallStatement,
    Suite,
    Expression,
    BinaryExpression,
    UnaryExpression,
    Comparison,
    Index,
    Slice,
    Attribute,
    MarkedValue,
    CallChain,
    Call,
    Argument,
    Parameter,
    Group,
    List,
    Tuple,
    Set,
    Dictionary,
    RecordLiteral,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxTokenKind {
    Bom,
    Whitespace,
    Comment,
    Newline,
    Word,
    QuotedName,
    Number,
    String,
    Symbol(&'static str),
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyntaxToken {
    pub(crate) kind: SyntaxTokenKind,
    pub(crate) span: Span,
}

impl SyntaxToken {
    pub fn kind(&self) -> SyntaxTokenKind {
        self.kind
    }

    pub fn span(&self) -> Span {
        self.span
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyntaxElement {
    Node(SyntaxNode),
    Token(SyntaxToken),
}

impl SyntaxElement {
    pub fn span(&self) -> Span {
        match self {
            Self::Node(node) => node.span,
            Self::Token(token) => token.span,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxNode {
    kind: SyntaxKind,
    span: Span,
    children: Vec<SyntaxElement>,
}

impl SyntaxNode {
    pub fn kind(&self) -> SyntaxKind {
        self.kind
    }

    pub fn span(&self) -> Span {
        self.span
    }

    pub fn children(&self) -> &[SyntaxElement] {
        &self.children
    }
}

#[derive(Clone, Debug)]
pub struct SyntaxTree {
    source: String,
    root: SyntaxNode,
    diagnostics: Vec<Diagnostic>,
}

impl SyntaxTree {
    pub(crate) fn build(
        source: &str,
        regions: Vec<Region>,
        syntax_tokens: &[SyntaxToken],
        diagnostics: Vec<Diagnostic>,
    ) -> Self {
        let regions = if regions.is_empty() && !diagnostics.is_empty() {
            vec![Region {
                kind: SyntaxKind::Error,
                span: Span::new(0, source.len()),
                children: vec![],
            }]
        } else {
            regions
        };
        let mut cursor = 0;
        let root = node(
            Region {
                kind: SyntaxKind::Module,
                span: Span::new(0, source.len()),
                children: regions,
            },
            syntax_tokens,
            &mut cursor,
        );
        assert_eq!(
            cursor,
            syntax_tokens.len(),
            "구문 트리가 토큰을 모두 담지 못했습니다"
        );
        Self {
            source: source.to_owned(),
            root,
            diagnostics,
        }
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn root(&self) -> &SyntaxNode {
        &self.root
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn has_errors(&self) -> bool {
        !self.diagnostics.is_empty()
    }

    pub fn text(&self, span: Span) -> Option<&str> {
        self.source.get(span.start..span.end)
    }

    pub fn tokens(&self) -> SyntaxTokens<'_> {
        SyntaxTokens {
            stack: vec![self.root.children.iter()],
        }
    }

    pub fn reconstruct(&self) -> String {
        let mut text = String::with_capacity(self.source.len());
        for token in self.tokens() {
            text.push_str(&self.source[token.span.start..token.span.end]);
        }
        text
    }

    pub(crate) fn same_structure(&self, other: &Self) -> bool {
        fn significant(element: &&SyntaxElement) -> bool {
            !matches!(element, SyntaxElement::Token(token)
                if matches!(token.kind, SyntaxTokenKind::Whitespace | SyntaxTokenKind::Newline))
        }
        let mut pending = vec![(&self.root, &other.root)];
        while let Some((left, right)) = pending.pop() {
            if left.kind != right.kind {
                return false;
            }
            let mut left = left.children.iter().filter(significant);
            let mut right = right.children.iter().filter(significant);
            loop {
                match (left.next(), right.next()) {
                    (Some(SyntaxElement::Node(a)), Some(SyntaxElement::Node(b))) => {
                        pending.push((a, b));
                    }
                    (Some(SyntaxElement::Token(a)), Some(SyntaxElement::Token(b)))
                        if a.kind == b.kind && self.text(a.span) == other.text(b.span) => {}
                    (None, None) => break,
                    _ => return false,
                }
            }
        }
        true
    }
}

pub struct SyntaxTokens<'a> {
    stack: Vec<slice::Iter<'a, SyntaxElement>>,
}

impl<'a> Iterator for SyntaxTokens<'a> {
    type Item = &'a SyntaxToken;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.stack.last_mut()?.next() {
                Some(SyntaxElement::Token(token)) => return Some(token),
                Some(SyntaxElement::Node(node)) => self.stack.push(node.children.iter()),
                None => {
                    self.stack.pop();
                }
            }
        }
    }
}

pub(crate) struct Region {
    pub kind: SyntaxKind,
    pub span: Span,
    pub children: Vec<Region>,
}

fn node(region: Region, syntax_tokens: &[SyntaxToken], cursor: &mut usize) -> SyntaxNode {
    fn until(
        children: &mut Vec<SyntaxElement>,
        syntax_tokens: &[SyntaxToken],
        cursor: &mut usize,
        end: usize,
    ) {
        while let Some(&token) = syntax_tokens.get(*cursor).filter(|t| t.span.start < end) {
            assert!(token.span.end <= end, "구문 트리의 경계가 토큰을 가릅니다");
            children.push(SyntaxElement::Token(token));
            *cursor += 1;
        }
    }
    let mut children = Vec::new();
    let mut previous = region.span.start;
    for child in region.children {
        assert!(
            previous <= child.span.start && child.span.end <= region.span.end,
            "구문 트리의 영역이 겹칩니다"
        );
        until(&mut children, syntax_tokens, cursor, child.span.start);
        previous = child.span.end;
        children.push(SyntaxElement::Node(node(child, syntax_tokens, cursor)));
    }
    until(&mut children, syntax_tokens, cursor, region.span.end);
    SyntaxNode {
        kind: region.kind,
        span: region.span,
        children,
    }
}

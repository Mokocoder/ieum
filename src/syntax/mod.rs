pub(crate) mod ast;
pub(crate) mod cursor;
mod format;
mod forms;
mod layout;
pub(crate) mod lexer;
mod parser;
mod tree;

pub use format::format_source;
pub use tree::{
    SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, SyntaxTokenKind, SyntaxTokens, SyntaxTree,
};

use crate::diagnostic::{Diagnostic, Result, Span, code};
use crate::limits::{Limit, MAX_SOURCE_BYTES};
use ast::{Expr, Statement};
use lexer::Kind;
use parser::Recovered;

pub(crate) const EACH: &str = "각각";

pub(crate) struct Parsed {
    pub tree: SyntaxTree,
    pub statements: Vec<Statement>,
    pub categories: Vec<(Span, ast::Category)>,
}

pub(crate) fn source_text(source: &[u8]) -> Result<&str> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(Diagnostic::limit(
            Limit::SourceBytes,
            Span::new(0, source.len()),
        ));
    }
    std::str::from_utf8(source).map_err(|e| {
        let start = e.valid_up_to();
        Diagnostic::new(
            "E-SRC-001",
            Span::new(start, start + e.error_len().unwrap_or(source.len() - start)),
            "UTF-8이 아닌 바이트가 있습니다. 파일을 UTF-8로 저장하세요.",
        )
    })
}

fn parse_text(source: &str, base: usize, editing: bool) -> (Vec<SyntaxToken>, Recovered) {
    let scanned = lexer::scan_lossless(source, base);
    if editing {
        let (tokens, layout_error) = layout::recover(source, base, scanned.tokens);
        let mut parsed = parser::parse_recovering(source, base, tokens);
        parsed.diagnostics.extend(scanned.diagnostic);
        parsed.diagnostics.extend(layout_error);
        return (scanned.syntax_tokens, parsed);
    }
    let tokens = match scanned.diagnostic {
        Some(error) => Err(error),
        None => layout::apply(source, base, scanned.tokens),
    };
    let parsed = match tokens {
        Ok(tokens) => parser::parse_recovering(source, base, tokens),
        Err(error) => Recovered {
            statements: Vec::new(),
            regions: Vec::new(),
            categories: Vec::new(),
            diagnostics: vec![error],
        },
    };
    (scanned.syntax_tokens, parsed)
}

fn parse_source(bytes: &[u8], editing: bool) -> Result<Parsed> {
    let source = source_text(bytes)?;
    let (syntax_tokens, parsed) = parse_text(source, 0, editing);
    Ok(Parsed {
        tree: SyntaxTree::build(source, parsed.regions, &syntax_tokens, parsed.diagnostics),
        statements: parsed.statements,
        categories: parsed.categories,
    })
}

pub(crate) fn parse(bytes: &[u8]) -> Result<Parsed> {
    parse_source(bytes, false)
}

pub(crate) fn parse_editing(bytes: &[u8]) -> Result<Parsed> {
    parse_source(bytes, true)
}

pub(crate) fn parse_at(
    bytes: &[u8],
    base: usize,
) -> std::result::Result<Vec<Statement>, Vec<Diagnostic>> {
    let source = source_text(bytes).map_err(|mut error| {
        error.span = error.span.shift(base);
        vec![error]
    })?;
    let (_, parsed) = parse_text(source, base, false);
    if parsed.diagnostics.is_empty() {
        Ok(parsed.statements)
    } else {
        Err(parsed.diagnostics)
    }
}

pub(crate) fn parse_valid(bytes: &[u8]) -> Result<Parsed> {
    let parsed = parse(bytes)?;
    match parsed.tree.diagnostics().first() {
        Some(error) => Err(error.clone()),
        None => Ok(parsed),
    }
}

pub fn parse_syntax(source: &[u8]) -> Result<SyntaxTree> {
    parse(source).map(|parsed| parsed.tree)
}

pub(crate) fn parse_expression(source: &str, base: usize) -> Option<Expr> {
    let scanned = lexer::scan_lossless(source, base);
    if scanned.diagnostic.is_some() {
        return None;
    }
    parser::parse_expression(source, base, scanned.tokens).ok()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unfinished {
    Delimiter,
    Header,
    Chain,
}

pub(crate) fn unfinished(source: &[u8]) -> Option<Unfinished> {
    let source = source_text(source).ok()?;
    let scanned = lexer::scan_lossless(source, 0);
    if let Some(error) = scanned.diagnostic {
        let block = error.code == code::UNCLOSED_TEXT
            && ["\"\"\"", "'''"]
                .iter()
                .any(|quotes| source[error.span.start..].starts_with(quotes));
        return (error.code == code::UNCLOSED_BRACKET || block).then_some(Unfinished::Delimiter);
    }
    if scanned
        .tokens
        .iter()
        .rev()
        .find(|t| !matches!(t.kind, Kind::Eof | Kind::Newline))
        .is_some_and(|t| t.symbol(":"))
    {
        return Some(Unfinished::Header);
    }
    layout::apply(source, 0, scanned.tokens)
        .is_err_and(|e| e.code == code::OPEN_CONNECTIVE && e.span.start == source.len())
        .then_some(Unfinished::Chain)
}

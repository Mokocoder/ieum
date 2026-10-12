mod chain;
mod clause;
mod expression;
mod particles;
mod statement;

use crate::diagnostic::{Diagnostic, Result, Span, code};
use crate::limits::MAX_DIAGNOSTICS;
use crate::syntax::ast::*;
use crate::syntax::forms::{self, Form};
use crate::syntax::lexer::{Kind, Token, literal};
use crate::syntax::tree::{Region, SyntaxKind};

pub(crate) fn parse_expression(source: &str, base: usize, tokens: Vec<Token>) -> Result<Expr> {
    let mut parser = Parser::new(source, base, tokens);
    let value = parser.expression()?;
    while parser.token().kind == Kind::Newline {
        parser.take();
    }
    parser.expect_kind(Kind::Eof)?;
    Ok(value)
}

pub(crate) struct Recovered {
    pub statements: Vec<Statement>,
    pub regions: Vec<Region>,
    pub categories: Vec<(Span, Category)>,
    pub diagnostics: Vec<Diagnostic>,
}

pub(crate) fn parse_recovering(source: &str, base: usize, tokens: Vec<Token>) -> Recovered {
    let mut parser = Parser::new(source, base, tokens);
    parser.recover = true;
    let statements = parser
        .sequence(false)
        .expect("복구하며 읽으면 오류가 남습니다");
    Recovered {
        statements,
        regions: parser.regions,
        categories: parser.categories,
        diagnostics: parser.diagnostics,
    }
}

#[derive(Clone, Copy)]
struct Mark {
    pos: usize,
    start: usize,
    regions: usize,
    categories: usize,
}

struct Parser<'a> {
    source: &'a str,
    base: usize,
    tokens: Vec<Token>,
    pos: usize,
    recursion: usize,
    regions: Vec<Region>,
    categories: Vec<(Span, Category)>,
    diagnostics: Vec<Diagnostic>,
    recover: bool,
    fatal: bool,
    clause_verb_at: Option<(usize, Form)>,
}

impl Parser<'_> {
    fn new(source: &str, base: usize, tokens: Vec<Token>) -> Parser<'_> {
        Parser {
            source,
            base,
            tokens,
            pos: 0,
            recursion: 0,
            regions: vec![],
            categories: vec![],
            diagnostics: vec![],
            recover: false,
            fatal: false,
            clause_verb_at: None,
        }
    }

    fn mark(&self) -> Mark {
        Mark {
            pos: self.pos,
            start: self.token().span.start,
            regions: self.regions.len(),
            categories: self.categories.len(),
        }
    }

    fn discard(&mut self, mark: Mark) {
        self.regions.truncate(mark.regions);
        self.categories.truncate(mark.categories);
    }

    fn rewind(&mut self, mark: Mark) {
        self.pos = mark.pos;
        self.discard(mark);
    }

    fn note(&mut self, span: Span, category: Category) {
        self.categories.push((span, category));
    }

    fn note_ending(&mut self, ending: &str, category: Category) {
        let end = self.previous().span.end;
        self.note(Span::new(end - ending.len(), end), category);
    }

    fn keyword(&mut self, word: &str, category: Category) -> Result<Token> {
        let token = self.expect_word(word)?;
        self.note(token.span, category);
        Ok(token)
    }

    fn take_keyword(&mut self, category: Category) -> Token {
        let token = self.take();
        self.note(token.span, category);
        token
    }

    fn finish(&mut self, mark: Mark, kind: SyntaxKind) {
        let end = self.previous().span.end.max(mark.start);
        let children = self.regions.split_off(mark.regions);
        self.regions.push(Region {
            kind,
            span: Span::new(mark.start, end),
            children,
        });
    }

    fn node<T>(
        &mut self,
        kind: SyntaxKind,
        parse: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        self.node_of(parse, |_| Some(kind))
    }

    fn node_of<T>(
        &mut self,
        parse: impl FnOnce(&mut Self) -> Result<T>,
        kind: impl FnOnce(&T) -> Option<SyntaxKind>,
    ) -> Result<T> {
        let mark = self.mark();
        let result = parse(self);
        match &result {
            Ok(value) => {
                if let Some(kind) = kind(value) {
                    self.finish(mark, kind);
                }
            }
            Err(_) => self.discard(mark),
        }
        result
    }

    fn text(&self, span: Span) -> &str {
        &self.source[span.start - self.base..span.end - self.base]
    }

    fn token(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn take(&mut self) -> Token {
        let token = self.token().clone();
        if !matches!(token.kind, Kind::Eof) {
            self.pos += 1;
        }
        token
    }

    fn previous(&self) -> &Token {
        &self.tokens[self.pos - 1]
    }

    fn fail(&self, code: &'static str, message: &str) -> Diagnostic {
        Diagnostic::new(code, self.token().span, message)
    }

    fn expect_symbol(&mut self, symbol: &str) -> Result<Token> {
        if !self.token().symbol(symbol) {
            return Err(self.fail("E-SYN-002", &format!("'{symbol}' 기호가 필요합니다.")));
        }
        Ok(self.take())
    }

    fn expect_word(&mut self, word: &str) -> Result<Token> {
        if !self.token().word(word) {
            return Err(self.fail("E-SYN-002", &format!("'{word}' 표기가 필요합니다.")));
        }
        Ok(self.take())
    }

    fn expect_kind(&mut self, kind: Kind) -> Result<Token> {
        if self.token().kind != kind {
            return Err(self.fail(
                "E-SYN-002",
                "문장은 마침표로, 블록 헤더는 콜론으로 끝내세요.",
            ));
        }
        Ok(self.take())
    }

    fn gap(&self, end: usize) -> Result<()> {
        if self.token().span.start <= end {
            return Err(self.fail("E-SYN-004", "어절 사이에 공백이 필요합니다."));
        }
        Ok(())
    }

    fn gap_after_previous(&self) -> Result<()> {
        self.gap(self.previous().span.end)
    }

    fn same_line_gap(&self, end: usize) -> Result<()> {
        self.gap(end)?;
        if self
            .text(Span::new(end, self.token().span.start))
            .contains(['\r', '\n'])
        {
            return Err(self.fail("E-SYN-004", "고정 표기 사이에는 줄바꿈을 넣을 수 없습니다."));
        }
        Ok(())
    }

    fn sequence(&mut self, in_suite: bool) -> Result<Vec<Statement>> {
        let mut statements = Vec::new();
        while !(self.token().kind == Kind::Eof || in_suite && self.token().kind == Kind::Dedent) {
            let start = self.pos;
            let recursion = self.recursion;
            match self.statement() {
                Ok(statement) => statements.push(statement),
                Err(error) => {
                    if !self.recover {
                        return Err(error);
                    }
                    let reported_end = error.span.end;
                    if !self.fatal {
                        if self.diagnostics.len() == MAX_DIAGNOSTICS - 1 {
                            self.diagnostics.push(Diagnostic::new(
                                code::LIMIT,
                                error.span,
                                format!(
                                    "구문 오류가 {MAX_DIAGNOSTICS}개를 넘어 나머지는 보고하지 않습니다."
                                ),
                            ));
                            self.fatal = true;
                        } else {
                            self.fatal = error.code == code::LIMIT;
                            self.diagnostics.push(error);
                        }
                    }
                    if self.fatal {
                        self.pos = self.tokens.len() - 1;
                    } else {
                        self.synchronize(start, in_suite, reported_end);
                    }
                    self.recursion = recursion;
                    let begin = self.tokens[start].span.start;
                    let end = if self.fatal {
                        self.base + self.source.len()
                    } else {
                        self.previous().span.end.max(begin)
                    };
                    self.regions.push(Region {
                        kind: SyntaxKind::Error,
                        span: Span::new(begin, end),
                        children: vec![],
                    });
                    if self.fatal {
                        break;
                    }
                }
            }
        }
        Ok(statements)
    }

    fn synchronize(&mut self, start: usize, in_suite: bool, reported_end: usize) {
        let failed_at = if self.pos > start && self.previous().kind == Kind::Newline {
            self.pos - 1
        } else {
            self.pos
        };
        let reported = self
            .tokens
            .partition_point(|t| t.span.start < reported_end)
            .saturating_sub(1);
        let failed_at = failed_at.max(reported);
        self.pos = start;
        while self.token().kind != Kind::Eof {
            if self.token().kind == Kind::Dedent {
                if !in_suite {
                    self.take();
                }
                return;
            }
            let index = self.pos;
            let token = self.take();
            if token.kind == Kind::Newline && index >= failed_at {
                break;
            }
        }
        if self.token().kind == Kind::Indent {
            let mut depth = 0;
            loop {
                match self.take().kind {
                    Kind::Indent => depth += 1,
                    Kind::Dedent => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    Kind::Eof => break,
                    _ => {}
                }
            }
        }
    }

    fn line_tail(&self) -> Vec<Token> {
        let mut depth = 0usize;
        let mut tail = Vec::new();
        for token in &self.tokens[self.pos..] {
            if depth == 0 && matches!(token.kind, Kind::Newline | Kind::Eof) {
                break;
            }
            if depth == 0 {
                tail.push(token.clone());
            }
            if token.opens() {
                depth += 1;
            } else if token.closes() {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    tail.push(token.clone());
                }
            }
        }
        tail
    }

    fn suite(&mut self) -> Result<Vec<Statement>> {
        self.node(SyntaxKind::Suite, Self::suite_inner)
    }

    fn suite_inner(&mut self) -> Result<Vec<Statement>> {
        self.expect_symbol(":")?;
        self.expect_kind(Kind::Newline)?;
        self.expect_kind(Kind::Indent)?;
        let body = self.sequence(true)?;
        if self.fatal {
            return Err(self
                .diagnostics
                .last()
                .expect("읽기를 멈춘 오류는 진단에 남습니다")
                .clone());
        }
        self.expect_kind(Kind::Dedent)?;
        Ok(body)
    }
}

fn word_ends(token: &Token, suffix: &str) -> bool {
    matches!(&token.kind, Kind::Word(s) if s.ends_with(suffix))
}

fn has_form(token: &Token, form: Form) -> bool {
    matches!(&token.kind, Kind::Word(word) if forms::is_form(word, form))
}

fn word_expr(word: &str, span: Span) -> Expr {
    let kind = match literal(word) {
        Some(value) => ExprKind::Literal(value),
        None => ExprKind::Name(word.to_owned()),
    };
    Expr::leaf(kind, span)
}

fn collect_expr(
    name: String,
    name_span: Span,
    collection: Expr,
    filter: Option<Expr>,
    span: Span,
) -> Result<Expr> {
    let depth = depth([&collection].into_iter().chain(&filter));
    let collect = Collect {
        name,
        name_span,
        collection,
        filter,
        value: None,
        concurrency: Concurrency::Sequential,
    };
    Expr::bounded(ExprKind::Collect(Box::new(collect)), span, depth)
}

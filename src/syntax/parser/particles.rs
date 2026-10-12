use super::{Parser, has_form, word_expr};
use crate::diagnostic::{Diagnostic, Result, Span};
use crate::korean::particle::{OBJECT, correction};
use crate::runtime::value::Constant;
use crate::syntax::ast::*;
use crate::syntax::forms::Form;
use crate::syntax::lexer::{
    Kind, Piece, Token, literal, literal_name_error, normalize, valid_name,
};
use crate::syntax::tree::SyntaxKind;

impl Parser<'_> {
    pub(super) fn marked_name(
        &mut self,
        particles: &[&'static str],
    ) -> Result<(String, Span, &'static str)> {
        let start = self.token().span;
        let (value, particle) = self.marked(particles)?;
        match value.as_name() {
            Some((name, span)) => Ok((name.to_owned(), span, particle)),
            None => Err(literal_name_error(start.through(self.previous().span))),
        }
    }

    pub(super) fn marked(&mut self, particles: &[&'static str]) -> Result<(Expr, &'static str)> {
        self.node(SyntaxKind::MarkedValue, |parser| {
            parser.marked_inner(particles)
        })
    }

    fn marked_inner(&mut self, particles: &[&'static str]) -> Result<(Expr, &'static str)> {
        let start = self.token().span;
        let (mut value, tail) = match self.signed_number()? {
            Some(signed) => signed,
            None if self.sign().is_some() => {
                return Err(self.ungrouped_error(start, particles));
            }
            None => self.postfix(true)?,
        };
        let following_operator = matches!(self.token().kind,
            Kind::Symbol(symbol) if BinOp::from_symbol(symbol).is_some() || CmpOp::from_symbol(symbol).is_some());
        if let Kind::Word(word) = &tail.kind {
            if let Some(particle) = particles
                .iter()
                .filter(|p| word.ends_with(**p))
                .max_by_key(|p| p.len())
                .copied()
            {
                if following_operator && word.len() == particle.len() {
                    return Err(self.ungrouped_error(start, particles));
                }
                let short = particle
                    .strip_prefix('이')
                    .filter(|s| particles.contains(s) && word.len() > particle.len());
                let mut other = short.map(|_| value.clone());
                self.split_tail(&mut value, &tail, particle)?;
                if let (Some(short), Some(mut other)) = (short, other.take()) {
                    self.split_tail(&mut other, &tail, short)?;
                    let span = value.span.through(other.span);
                    let depth = depth([&value, &other]);
                    value = Expr::bounded(
                        ExprKind::Either(Box::new(value), Box::new(other)),
                        span,
                        depth,
                    )?;
                }
                return Ok((value, particle));
            }
        }
        if following_operator {
            return Err(self.ungrouped_error(start, particles));
        }
        if let Kind::Word(word) = &self.token().kind {
            if let Some(&particle) = particles.iter().find(|p| word.as_str() == **p) {
                if self.token().span.start != tail.span.end {
                    return Err(Diagnostic::new(
                        "E-AFFIX-002",
                        start.through(self.token().span),
                        "대상과 조사 사이의 공백을 없애세요.",
                    ));
                }
                let host = match &tail.kind {
                    Kind::Number(host) | Kind::Text(host) | Kind::Quoted(host) => Some(host),
                    Kind::Template(pieces) => match pieces.last() {
                        Some(Piece::Text(host)) => Some(host),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(host) = host {
                    check_particle(host, particle, start.through(self.token().span))?;
                }
                self.take();
                return Ok((value, particle));
            }
        }
        if matches!(self.token().kind, Kind::Word(_)) && self.token().span.start == tail.span.end {
            return Err(Diagnostic::new(
                "E-AFFIX-001",
                start.through(self.token().span),
                "조사가 아닌 글자가 붙었습니다.",
            ));
        }
        Err(Diagnostic::new(
            "E-AFFIX-001",
            start.through(tail.span),
            format!("필요한 접미사: {}", particles.join(" / ")),
        ))
    }

    pub(super) fn signed_number(&mut self) -> Result<Option<(Expr, Token)>> {
        let next = &self.tokens[(self.pos + 1).min(self.tokens.len() - 1)];
        let Some(sign) = self.sign() else {
            return Ok(None);
        };
        if !matches!(next.kind, Kind::Number(_)) || next.span.start != self.token().span.end {
            return Ok(None);
        }
        let start = self.token().span;
        let mark = self.mark();
        let op = self.take();
        let value = self.atom()?;
        let tail = self.previous().clone();
        let span = start.through(value.span);
        let value = Expr::unary(sign, op.span, value, span)?;
        self.finish(mark, SyntaxKind::UnaryExpression);
        Ok(Some((value, tail)))
    }

    pub(super) fn split_tail(&self, value: &mut Expr, token: &Token, particle: &str) -> Result<()> {
        let Kind::Word(word) = &token.kind else {
            return Err(Diagnostic::new(
                "E-AFFIX-001",
                token.span,
                "조사가 없습니다.",
            ));
        };
        let host = &word[..word.len() - particle.len()];
        if host.is_empty() {
            return Err(Diagnostic::new(
                "E-AFFIX-001",
                token.span,
                "조사 앞에 대상이 필요합니다.",
            ));
        }
        if !valid_name(host) {
            return Err(Diagnostic::new(
                "E-NAME-001",
                token.span,
                "조사를 뗀 말을 이름으로 쓸 수 없습니다.",
            ));
        }
        check_particle(host, particle, token.span)?;
        let raw = self.text(token.span);
        let boundary = raw
            .char_indices()
            .rev()
            .map(|(i, _)| i)
            .find(|&i| normalize(&raw[i..]) == particle && normalize(&raw[..i]) == host)
            .ok_or_else(|| {
                Diagnostic::new(
                    "E-AFFIX-001",
                    token.span,
                    "조사가 시작하는 곳을 찾을 수 없습니다.",
                )
            })?;
        let name_span = Span::new(token.span.start, token.span.start + boundary);
        match &mut value.kind {
            ExprKind::Name(_) | ExprKind::Literal(Constant::Bool(_) | Constant::None) => {
                *value = word_expr(host, name_span);
            }
            ExprKind::Attribute(_, name) => {
                if literal(host).is_some() {
                    return Err(Diagnostic::new(
                        "E-NAME-002",
                        token.span,
                        "속성 이름은 리터럴이 될 수 없습니다.",
                    ));
                }
                *name = host.to_owned();
                value.name_span = name_span;
                value.span.end = name_span.end;
            }
            _ => {
                return Err(Diagnostic::new(
                    "E-AFFIX-001",
                    token.span,
                    "이 말 뒤에는 조사를 붙일 수 없습니다. 괄호로 감싸세요.",
                ));
            }
        }
        Ok(())
    }

    fn ungrouped_error(&self, start: Span, particles: &[&str]) -> Diagnostic {
        let mut end = start;
        let mut depth = 0usize;
        for token in &self.tokens[self.pos..] {
            if depth == 0 {
                if matches!(
                    token.kind,
                    Kind::Newline | Kind::Eof | Kind::Symbol("." | ")" | "]" | ",")
                ) {
                    break;
                }
                if let Kind::Word(word) = &token.kind {
                    if word == "바꾼다"
                        || [Form::Finite, Form::Sequence, Form::Pipe]
                            .into_iter()
                            .any(|form| has_form(token, form))
                    {
                        break;
                    }
                }
            }
            end = token.span;
            if matches!(token.kind, Kind::Symbol("(" | "[")) {
                depth += 1;
            } else if matches!(token.kind, Kind::Symbol(")" | "]")) {
                depth = depth.saturating_sub(1);
            } else if depth == 0 {
                if let Kind::Word(word) = &token.kind {
                    if particles.iter().any(|p| word.ends_with(p)) {
                        break;
                    }
                }
            }
        }
        Diagnostic::new(
            "E-AFFIX-003",
            start.through(end),
            "복합 표현식에 조사를 붙일 때는 괄호로 감싸세요.",
        )
    }
}

pub(super) fn check_particle(host: &str, particle: &str, span: Span) -> Result<()> {
    match correction(host, particle) {
        Some(right) => Err(Diagnostic::new(
            "E-AFFIX-004",
            span,
            format!(
                "'{host}' 뒤에는 '{particle}' 대신 '{right}'{} 씁니다.",
                OBJECT.after(right)
            ),
        )
        .with_fix(Span::new(span.end - particle.len(), span.end), right)),
        None => Ok(()),
    }
}

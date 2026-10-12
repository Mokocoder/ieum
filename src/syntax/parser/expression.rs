use super::chain::chain_expr;
use super::clause;
use super::{Parser, collect_expr, has_form, word_ends, word_expr};
use crate::diagnostic::{Diagnostic, Result, Span};
use crate::korean::particle::TOPIC;
use crate::limits::{Limit, MAX_PARSER_RECURSION};
use crate::runtime::value::Constant;
use crate::syntax::ast::*;
use crate::syntax::forms::Form;
use crate::syntax::lexer::{self, Kind, Piece, Token};
use crate::syntax::tree::SyntaxKind;

impl Parser<'_> {
    pub(super) fn expression(&mut self) -> Result<Expr> {
        self.node(SyntaxKind::Expression, |parser| {
            parser.logic(BinOp::Or, Self::conjunction)
        })
    }

    fn conjunction(&mut self) -> Result<Expr> {
        self.logic(BinOp::And, Self::negation)
    }

    fn logic(&mut self, op: BinOp, operand: fn(&mut Self) -> Result<Expr>) -> Result<Expr> {
        let mark = self.mark();
        let mut left = operand(self)?;
        while self.token().word(op.symbol()) {
            self.gap(left.span.end)?;
            let word = self.take();
            self.gap(word.span.end)?;
            let right = operand(self)?;
            left = Expr::binary(op, word.span, left, right)?;
            self.finish(mark, SyntaxKind::BinaryExpression);
        }
        Ok(left)
    }

    fn negation(&mut self) -> Result<Expr> {
        let mark = self.mark();
        let mut value = self.comparison()?;
        while self.token().word(UnaryOp::Not.symbol()) {
            self.gap(value.span.end)?;
            let op = self.take();
            let span = value.span.through(op.span);
            value = Expr::unary(UnaryOp::Not, op.span, value, span)?;
            self.finish(mark, SyntaxKind::UnaryExpression);
        }
        Ok(value)
    }

    fn comparison(&mut self) -> Result<Expr> {
        let mark = self.mark();
        let first = self.sum()?;
        if self.comparison_op().is_none() {
            return Ok(first);
        }
        let start = first.span;
        let mut values = vec![first];
        let mut ops = Vec::new();
        while let Some(op) = self.comparison_op() {
            ops.push((op, self.take().span));
            values.push(self.sum()?);
        }
        let depth = depth(&values);
        let span = start.through(values.last().expect("비교하는 값이 있습니다").span);
        let value = Expr::bounded(ExprKind::Compare(values, ops), span, depth)?;
        self.finish(mark, SyntaxKind::Comparison);
        Ok(value)
    }

    fn comparison_op(&self) -> Option<CmpOp> {
        match self.token().kind {
            Kind::Symbol(symbol) => CmpOp::from_symbol(symbol),
            _ => None,
        }
    }

    pub(super) fn sign(&self) -> Option<UnaryOp> {
        match self.token().kind {
            Kind::Symbol(symbol) => UnaryOp::from_symbol(symbol),
            _ => None,
        }
    }

    fn sum(&mut self) -> Result<Expr> {
        self.binary(&[BinOp::Add, BinOp::Sub], Self::product)
    }

    fn product(&mut self) -> Result<Expr> {
        self.binary(
            &[BinOp::Mul, BinOp::Div, BinOp::FloorDiv, BinOp::Mod],
            Self::unary,
        )
    }

    fn binary(&mut self, ops: &[BinOp], operand: fn(&mut Self) -> Result<Expr>) -> Result<Expr> {
        let mark = self.mark();
        let mut left = operand(self)?;
        while let Kind::Symbol(symbol) = self.token().kind {
            let Some(op) = BinOp::from_symbol(symbol).filter(|op| ops.contains(op)) else {
                break;
            };
            let span = self.take().span;
            let right = operand(self)?;
            left = Expr::binary(op, span, left, right)?;
            self.finish(mark, SyntaxKind::BinaryExpression);
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr> {
        let parse = |parser: &mut Self| {
            parser.recursion += 1;
            if parser.recursion > MAX_PARSER_RECURSION {
                return Err(Diagnostic::limit(
                    Limit::ParserRecursion,
                    parser.token().span,
                ));
            }
            let result = parser.unary_inner();
            parser.recursion -= 1;
            result
        };
        self.node_of(parse, |value| match value.kind {
            ExprKind::Unary(..) => Some(SyntaxKind::UnaryExpression),
            ExprKind::Binary(BinOp::Pow, ..) => Some(SyntaxKind::BinaryExpression),
            _ => None,
        })
    }

    fn unary_inner(&mut self) -> Result<Expr> {
        if let Some(sign) = self.sign() {
            let op = self.take();
            let value = self.unary()?;
            let span = op.span.through(value.span);
            return Expr::unary(sign, op.span, value, span);
        }
        let (mut left, _) = self.postfix(false)?;
        if self.token().symbol("**") {
            let op = self.take();
            let right = self.unary()?;
            left = Expr::binary(BinOp::Pow, op.span, left, right)?;
        }
        Ok(left)
    }

    pub(super) fn postfix(&mut self, marked: bool) -> Result<(Expr, Token)> {
        let mark = self.mark();
        let mut value = self.atom()?;
        let mut tail = self.previous().clone();
        loop {
            if self.token().symbol("[") {
                if self.token().span.start != tail.span.end {
                    if marked {
                        break;
                    }
                    return Err(self.fail("E-SYN-004", "인덱스는 대상 바로 뒤에 붙여 쓰세요."));
                }
                let open = self.take();
                let index = if self.token().symbol(":") {
                    None
                } else {
                    Some(self.expression()?)
                };
                if self.token().symbol(":") {
                    self.take();
                    let stop = if self.token().symbol(":") || self.token().symbol("]") {
                        None
                    } else {
                        Some(self.expression()?)
                    };
                    let step = if self.token().symbol(":") {
                        self.take();
                        if self.token().symbol("]") {
                            None
                        } else {
                            Some(self.expression()?)
                        }
                    } else {
                        None
                    };
                    tail = self.expect_symbol("]")?;
                    let bounds = [index.map(Box::new), stop.map(Box::new), step.map(Box::new)];
                    let depth = depth(
                        std::iter::once(&value).chain(bounds.iter().flatten().map(Box::as_ref)),
                    );
                    let span = value.span.through(tail.span);
                    value = Expr::bounded(
                        ExprKind::Slice(Box::new(value), bounds, open.span.through(tail.span)),
                        span,
                        depth,
                    )?;
                    self.finish(mark, SyntaxKind::Slice);
                    continue;
                }
                let index = index.expect("슬라이스가 아니면 첨자가 있습니다");
                tail = self.expect_symbol("]")?;
                let depth = depth([&value, &index]);
                let span = value.span.through(tail.span);
                value = Expr::bounded(
                    ExprKind::Index(
                        Box::new(value),
                        Box::new(index),
                        open.span.through(tail.span),
                    ),
                    span,
                    depth,
                )?;
                self.finish(mark, SyntaxKind::Index);
                continue;
            }
            let member = matches!(self.token().kind, Kind::Word(_) | Kind::Quoted(_));
            if word_ends(&tail, "의") && member && self.token().span.start > tail.span.end {
                self.split_tail(&mut value, &tail, "의")?;
            } else if self.token().word("의")
                && self.token().span.start == tail.span.end
                && self
                    .tokens
                    .get(self.pos + 1)
                    .is_some_and(|t| matches!(t.kind, Kind::Word(_) | Kind::Quoted(_)))
            {
                let particle = self.take();
                self.gap(particle.span.end)?;
            } else {
                break;
            }
            let name = self.name()?;
            self.note(name.span, Category::Field);
            let spelling = match &name.kind {
                Kind::Word(s) | Kind::Quoted(s) => s.clone(),
                _ => unreachable!("이름은 낱말이나 인용 이름입니다"),
            };
            let span = value.span.through(name.span);
            let depth = depth([&value]);
            value = Expr::bounded(ExprKind::Attribute(Box::new(value), spelling), span, depth)?;
            value.name_span = name.span;
            tail = name;
            self.finish(mark, SyntaxKind::Attribute);
        }
        Ok((value, tail))
    }

    pub(super) fn name(&mut self) -> Result<Token> {
        match &self.token().kind {
            Kind::Word(_) | Kind::Quoted(_) => {
                let span = self.token().span;
                lexer::name(self.text(span), span, false)?;
                Ok(self.take())
            }
            _ => Err(self.fail("E-SYN-001", "이름이 필요합니다.")),
        }
    }

    pub(super) fn atom(&mut self) -> Result<Expr> {
        self.node_of(Self::atom_inner, |value| match value.kind {
            ExprKind::Group(_) | ExprKind::Chain(_) => Some(SyntaxKind::Group),
            ExprKind::List(_) => Some(SyntaxKind::List),
            ExprKind::Tuple(_) => Some(SyntaxKind::Tuple),
            ExprKind::Set(_) => Some(SyntaxKind::Set),
            ExprKind::Dict(_) => Some(SyntaxKind::Dictionary),
            _ => None,
        })
    }

    fn atom_inner(&mut self) -> Result<Expr> {
        let token = self.take();
        let kind = match token.kind {
            Kind::Word(ref word) => return Ok(word_expr(word, token.span)),
            Kind::Quoted(name) => ExprKind::Name(name),
            Kind::Number(number) => ExprKind::Number(number),
            Kind::Text(text) => ExprKind::Literal(Constant::Text(text.into())),
            Kind::Template(pieces) => return self.template(pieces, token.span),
            Kind::Symbol("{") => return self.braces(token.span),
            Kind::Symbol("[") => return self.list(token.span),
            Kind::Symbol("(") => return self.parenthesized(token.span),
            Kind::Symbol(":") => {
                return Err(Diagnostic::new(
                    "E-SYN-009",
                    token.span,
                    "여기에 올 수 없는 식입니다.",
                ));
            }
            _ => {
                return Err(Diagnostic::new(
                    "E-SYN-001",
                    token.span,
                    "값 또는 이름이 필요합니다.",
                ));
            }
        };
        Ok(Expr::leaf(kind, token.span))
    }

    fn template(&mut self, pieces: Vec<Piece>, span: Span) -> Result<Expr> {
        let mut parts = Vec::new();
        for piece in pieces {
            parts.push(match piece {
                Piece::Text(text) => Part::Text(text),
                Piece::Code(code) => {
                    let (code, spec) = split_spec(self.text(code), code)?;
                    let scanned = lexer::scan_lossless(self.text(code), code.start);
                    if let Some(error) = scanned.diagnostic {
                        return Err(error);
                    }
                    let value =
                        super::parse_expression(self.text(code), code.start, scanned.tokens)?;
                    Part::Value(value, spec)
                }
            });
        }
        let depth = depth(parts.iter().filter_map(|part| match part {
            Part::Value(value, _) => Some(value),
            Part::Text(_) => None,
        }));
        Expr::bounded(ExprKind::Template(parts), span, depth)
    }

    fn list(&mut self, open: Span) -> Result<Expr> {
        let mut items = Vec::new();
        if !self.token().symbol("]") {
            loop {
                items.push(self.expression()?);
                if !self.token().symbol(",") {
                    break;
                }
                self.take();
                if self.token().symbol("]") {
                    break;
                }
            }
        }
        let closing = self.expect_symbol("]")?;
        let depth = depth(&items);
        Expr::bounded(ExprKind::List(items), open.through(closing.span), depth)
    }

    fn braces(&mut self, open: Span) -> Result<Expr> {
        let mut entries = Vec::new();
        let mut items = Vec::new();
        let mut is_set = false;
        if !self.token().symbol("}") {
            loop {
                let key = self.expression()?;
                if entries.is_empty() && items.is_empty() {
                    is_set = !self.token().symbol(":");
                }
                if is_set {
                    items.push(key);
                } else {
                    self.expect_symbol(":")?;
                    let value = self.expression()?;
                    entries.push((key, value));
                }
                if !self.token().symbol(",") {
                    break;
                }
                self.take();
                if self.token().symbol("}") {
                    break;
                }
            }
        }
        let closing = self.expect_symbol("}")?;
        let span = open.through(closing.span);
        if is_set {
            let depth = depth(&items);
            return Expr::bounded(ExprKind::Set(items), span, depth);
        }
        let depth = depth(entries.iter().flat_map(|(key, value)| [key, value]));
        Expr::bounded(ExprKind::Dict(entries), span, depth)
    }

    fn parenthesized(&mut self, open: Span) -> Result<Expr> {
        if self.token().symbol(")") {
            let closing = self.take();
            return Ok(Expr::leaf(
                ExprKind::Tuple(Vec::new()),
                open.through(closing.span),
            ));
        }
        let end = self.group_end(open)?;
        let nominal = end >= 3
            && self.tokens[end - 2].word("값")
            && has_form(&self.tokens[end - 3], Form::Nominal);
        if nominal {
            let chain = self.chain(Form::Nominal)?;
            let closing = self.expect_symbol(")")?;
            return chain_expr(chain, open.through(closing.span));
        }
        let among = (self.pos..end).any(|i| self.tokens[i].word("중"));
        let count = self.counts_from(end);
        if (among || count) && end >= 3 && word_ends(&self.tokens[end - 2], "들") {
            let collection = if count {
                let start = self.token().span;
                let (first, last, down) = self.range_bounds()?;
                let depth = depth([&first, &last]);
                let kind = ExprKind::Range(Box::new(first), Box::new(last), down);
                Expr::bounded(kind, start.through(self.previous().span), depth)?
            } else {
                self.expression()?
            };
            self.gap_after_previous()?;
            if !count || self.token().word("중") {
                self.keyword("중", Category::Flow)?;
                self.gap_after_previous()?;
            }
            let filter = self.clause_before(end - 1, "들")?.map(|(test, _)| test);
            let binder = self.take();
            let Kind::Word(word) = &binder.kind else {
                return Err(self.fail("E-NAME-002", "고를 원소 이름이 필요합니다."));
            };
            let name = word.strip_suffix("들").unwrap_or(word).to_owned();
            let closing = self.expect_symbol(")")?;
            return collect_expr(
                name,
                binder.span,
                collection,
                filter,
                open.through(closing.span),
            );
        }
        let down = end >= 3 && self.tokens[end - 2].word("내려가며");
        let last = &self.tokens[end - if down { 3 } else { 2 }];
        if end >= 3 && word_ends(last, "까지") {
            let (first, last, down) = self.range_bounds()?;
            let closing = self.expect_symbol(")")?;
            let depth = depth([&first, &last]);
            let kind = ExprKind::Range(Box::new(first), Box::new(last), down);
            return Expr::bounded(kind, open.through(closing.span), depth);
        }
        if let Some(condition_end) = self.choice_condition_end(end) {
            let test = self.clauses(condition_end + 1, &clause::IF, None)?;
            self.note(self.previous().span, Category::Condition);
            self.gap_after_previous()?;
            let chosen = self.expression()?;
            self.expect_symbol(",")?;
            let word = self.keyword("아니면", Category::Flow)?;
            self.gap(word.span.end)?;
            let other = self.expression()?;
            let closing = self.expect_symbol(")")?;
            let depth = depth([&test, &chosen, &other]);
            return Expr::bounded(
                ExprKind::Choice(Box::new([test, chosen, other])),
                open.through(closing.span),
                depth,
            );
        }
        let whether = end >= 3
            && ([Form::WhetherIs, Form::WhetherDoes]
                .into_iter()
                .any(|form| has_form(&self.tokens[end - 2], form))
                || word_ends(&self.tokens[end - 2], "인지"));
        if whether {
            let test = self.clauses(end - 1, &clause::WHETHER, None)?;
            let closing = self.expect_symbol(")")?;
            let depth = depth([&test]);
            return Expr::bounded(
                ExprKind::Group(Box::new(test)),
                open.through(closing.span),
                depth,
            );
        }
        let record = end >= 4
            && matches!(&self.tokens[end - 2].kind, Kind::Word(_))
            && matches!(&self.tokens[end - 3].kind, Kind::Word(w) if w.ends_with("인"));
        if record {
            let literal = self.node(SyntaxKind::RecordLiteral, Self::record_literal)?;
            let closing = self.expect_symbol(")")?;
            let depth = depth(literal.fields.iter().map(|(_, _, value)| value));
            return Expr::bounded(
                ExprKind::Record(Box::new(literal)),
                open.through(closing.span),
                depth,
            );
        }
        let value = self.expression()?;
        if self.token().symbol(",") {
            let mut items = vec![value];
            while self.token().symbol(",") {
                self.take();
                if self.token().symbol(")") {
                    break;
                }
                items.push(self.expression()?);
            }
            let closing = self.expect_symbol(")")?;
            let depth = depth(&items);
            return Expr::bounded(ExprKind::Tuple(items), open.through(closing.span), depth);
        }
        let closing = self.expect_symbol(")")?;
        let depth = depth([&value]);
        Expr::bounded(
            ExprKind::Group(Box::new(value)),
            open.through(closing.span),
            depth,
        )
    }

    fn group_end(&self, open: Span) -> Result<usize> {
        let mut depth = 1usize;
        let mut end = self.pos;
        while depth != 0 {
            let token = self
                .tokens
                .get(end)
                .filter(|token| token.kind != Kind::Eof)
                .ok_or_else(|| lexer::unclosed_bracket(open))?;
            if token.opens() {
                depth += 1;
            } else if token.closes() {
                depth -= 1;
            }
            end += 1;
        }
        Ok(end)
    }

    fn counts_from(&self, end: usize) -> bool {
        let mut depth = 0usize;
        self.tokens[self.pos..end].iter().any(|token| {
            if token.opens() {
                depth += 1;
            } else if token.closes() {
                depth = depth.saturating_sub(1);
            }
            depth == 0 && word_ends(token, "부터")
        })
    }

    fn choice_condition_end(&self, end: usize) -> Option<usize> {
        let mut depth = 0usize;
        let mut condition_end = None;
        for index in self.pos..end - 1 {
            let token = &self.tokens[index];
            if token.opens() {
                depth += 1;
            } else if token.closes() {
                depth = depth.saturating_sub(1);
            } else if depth == 0 && condition_end.is_none() && word_ends(token, "면") {
                condition_end = Some(index);
            } else if depth == 0 && token.symbol(",") && self.tokens[index + 1].word("아니면") {
                return condition_end;
            }
        }
        None
    }

    pub(super) fn range_bounds(&mut self) -> Result<(Expr, Expr, bool)> {
        let (first, from) = self.marked(&["부터"])?;
        self.note_ending(from, Category::Flow);
        self.gap_after_previous()?;
        let (last, to) = self.marked(&["까지"])?;
        self.note_ending(to, Category::Flow);
        let down = self.token().word("내려가며");
        if down {
            self.gap_after_previous()?;
            self.take_keyword(Category::Flow);
        }
        Ok((first, last, down))
    }

    fn record_literal(&mut self) -> Result<RecordLiteral> {
        let mut fields = Vec::new();
        loop {
            let (name, span, _) = self.marked_name(&["이", "가"])?;
            self.note(span, Category::Field);
            self.gap_after_previous()?;
            let (value, particle) = self.marked(&["이고", "고", "인"])?;
            self.note_ending(particle, Category::Word);
            fields.push((name, span, value));
            self.gap_after_previous()?;
            if particle == "인" {
                break;
            }
        }
        let type_name = self.name()?;
        let Kind::Word(name) = type_name.kind else {
            return Err(Diagnostic::new(
                "E-NAME-002",
                type_name.span,
                "자료형 이름이 필요합니다.",
            ));
        };
        Ok(RecordLiteral {
            type_name: name,
            type_span: type_name.span,
            fields,
        })
    }
}

fn split_spec(code: &str, span: Span) -> Result<(Span, Option<String>)> {
    let mut depth = 0usize;
    let mut colon = None;
    let mut index = 0;
    while let Some(c) = code[index..].chars().next() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => {
                colon = Some(index);
                break;
            }
            '"' | '\'' => {
                index += lexer::text_length(&code[index..]).unwrap_or(code.len() - index);
                continue;
            }
            _ => {}
        }
        index += c.len_utf8();
    }
    let Some(index) = colon else {
        return Ok((span, None));
    };
    let spec = &code[index + 1..];
    if !crate::stdlib::is_spec(spec) {
        return Err(Diagnostic::new(
            "E-FORMAT-001",
            Span::new(span.start + index + 1, span.end),
            format!("'{spec}'{} 쓸 수 없는 서식입니다.", TOPIC.after(spec)),
        ));
    }
    Ok((
        Span::new(span.start, span.start + index),
        Some(spec.to_owned()),
    ))
}

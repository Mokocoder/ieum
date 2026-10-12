use super::chain::chain_expr;
use super::{Mark, Parser, collect_expr, has_form, word_ends};
use crate::diagnostic::{Result, Span};
use crate::runtime::value::Constant;
use crate::syntax::ast::*;
use crate::syntax::forms::{self, Form};
use crate::syntax::lexer::{Kind, Token};
use crate::syntax::tree::SyntaxKind;

pub(super) struct ClauseRule {
    copula: &'static [&'static str],
    negative_copula: &'static str,
    forms: &'static [Form],
    adjective: Form,
    negative_forms: &'static [Form],
    message: &'static str,
}

pub(super) const IF: ClauseRule = ClauseRule {
    copula: &["이면", "면"],
    negative_copula: "아니면",
    forms: &[Form::Conditional],
    adjective: Form::Conditional,
    negative_forms: &[Form::Conditional],
    message: "조건은 '-면' 형태로 끝납니다.",
};

pub(super) const WHILE: ClauseRule = ClauseRule {
    copula: &["인"],
    negative_copula: "아닌",
    forms: &[],
    adjective: Form::Descriptive,
    negative_forms: &[Form::Definition, Form::Descriptive],
    message: "반복 조건은 '-인 동안' 형태로 씁니다.",
};

pub(super) const WHICH: ClauseRule = ClauseRule {
    copula: &["인"],
    negative_copula: "아닌",
    forms: &[Form::Definition, Form::Descriptive],
    adjective: Form::Descriptive,
    negative_forms: &[Form::Definition, Form::Descriptive],
    message: "반복 조건은 '-인', '-한' 또는 '-하는' 형태로 끝납니다.",
};

const AND: ClauseRule = ClauseRule {
    copula: &["이고", "고"],
    negative_copula: "아니고",
    forms: &[Form::Sequence],
    adjective: Form::Sequence,
    negative_forms: &[Form::Sequence],
    message: "'-고'로 잇는 절은 '-이고', '-하고'처럼 끝납니다.",
};

const OR: ClauseRule = ClauseRule {
    copula: &["이거나", "거나"],
    negative_copula: "아니거나",
    forms: &[Form::Or],
    adjective: Form::Or,
    negative_forms: &[Form::Or],
    message: "'-거나'로 잇는 절은 '-이거나', '-하거나'처럼 끝납니다.",
};

pub(super) const WHETHER: ClauseRule = ClauseRule {
    copula: &["인지"],
    negative_copula: "아닌지",
    forms: &[Form::WhetherIs, Form::WhetherDoes],
    adjective: Form::WhetherIs,
    negative_forms: &[Form::WhetherIs, Form::WhetherDoes],
    message: "값으로 쓰는 조건은 '-인지' 또는 '-는지' 형태로 끝납니다.",
};

const NOT: ClauseRule = ClauseRule {
    copula: &["이지"],
    negative_copula: "아니지",
    forms: &[Form::Negative],
    adjective: Form::Negative,
    negative_forms: &[],
    message: "부정은 '-지 않다' 형태로 씁니다.",
};

pub(super) fn range_operator(word: &str, ending: &str) -> Option<CmpOp> {
    match word.strip_suffix(ending)? {
        "이상" => Some(CmpOp::Ge),
        "이하" => Some(CmpOp::Le),
        "초과" => Some(CmpOp::Gt),
        "미만" => Some(CmpOp::Lt),
        _ => None,
    }
}

fn is_literal(value: &Expr) -> bool {
    match &value.kind {
        ExprKind::Literal(_)
        | ExprKind::Number(_)
        | ExprKind::Template(_)
        | ExprKind::List(_)
        | ExprKind::Tuple(_)
        | ExprKind::Set(_)
        | ExprKind::Dict(_)
        | ExprKind::Range(..) => true,
        ExprKind::Unary(UnaryOp::Neg, _, operand) => is_literal(operand),
        _ => false,
    }
}

fn comparison(lemma: &str) -> Option<(CmpOp, &'static [&'static str])> {
    Some(match lemma {
        "크다" | "많다" | "높다" => (CmpOp::Gt, &["보다"]),
        "작다" | "적다" | "낮다" => (CmpOp::Lt, &["보다"]),
        "같다" => (CmpOp::Eq, &["와", "과"]),
        "다르다" => (CmpOp::Ne, &["와", "과"]),
        _ => return None,
    })
}

fn is_range_word(token: &Token) -> bool {
    matches!(&token.kind, Kind::Word(word)
        if [&IF, &WHILE, &AND, &OR].iter().any(|e| range_operator(word, e.copula[0]).is_some()))
}

fn begins_clause(tokens: &[Token]) -> bool {
    match tokens[0].kind {
        Kind::Symbol("-") => tokens
            .get(1)
            .is_some_and(|next| next.span.start == tokens[0].span.end),
        Kind::Symbol(
            "**" | "//" | "==" | "!=" | "<=" | ">=" | "+" | "*" | "/" | "%" | "<" | ">",
        ) => false,
        _ => true,
    }
}

fn said(clause: &Expr) -> Option<Vec<Argument>> {
    match &clause.kind {
        ExprKind::Compare(values, ops) if ops.len() == 1 => Some(vec![Argument {
            role: Role::Subject,
            value: values[0].clone(),
            span: values[0].span,
        }]),
        ExprKind::Chain(chain) => {
            let own = &chain.calls.last()?.arguments;
            let shared = chain
                .implied
                .iter()
                .filter(|implied| own.iter().all(|a| a.role != implied.role));
            Some(own.iter().chain(shared).cloned().collect())
        }
        ExprKind::Either(first, second) => said(first).or_else(|| said(second)),
        ExprKind::Unary(UnaryOp::Not, _, clause) => said(clause),
        _ => None,
    }
}

impl Parser<'_> {
    // 국어에는 -고와 -거나 사이의 우선순위가 없으므로 한 조건에 섞지 않습니다.
    pub(super) fn clauses(
        &mut self,
        end: usize,
        last: &ClauseRule,
        head: Option<Expr>,
    ) -> Result<Expr> {
        let mut test: Option<Expr> = None;
        let mut join: Option<(BinOp, Span)> = None;
        let mut among: Option<Expr> = None;
        let mut earlier: Vec<Argument> = head
            .into_iter()
            .map(|value| Argument {
                role: Role::Subject,
                span: value.span,
                value,
            })
            .collect();
        loop {
            let (stop, rule, op) = self.clause_end(end, last);
            let clause = self.clause(stop, rule, &earlier, &mut among)?;
            if self.pos != stop + 1 {
                return Err(self.fail("E-SYN-006", rule.message));
            }
            if let Some(said) = said(&clause) {
                earlier = said;
            }
            test = Some(match (test, join) {
                (Some(left), Some((op, op_span))) => {
                    let span = left.span.through(clause.span);
                    let depth = depth([&left, &clause]);
                    let kind = ExprKind::Binary(op, op_span, Box::new(left), Box::new(clause));
                    Expr::bounded(kind, span, depth)?
                }
                _ => clause,
            });
            let Some(op) = op else {
                return Ok(test.expect("위에서 정했습니다"));
            };
            if join.is_some_and(|(previous, _)| previous != op) {
                return Err(self.fail(
                    "E-SYN-014",
                    "'-고'와 '-거나'를 한 조건에 섞을 수 없습니다. 한쪽 조건을 먼저 이름에 담으세요.",
                ));
            }
            join = Some((op, self.previous().span));
            self.gap_after_previous()?;
        }
    }

    fn clause_end<'a>(
        &self,
        end: usize,
        last: &'a ClauseRule,
    ) -> (usize, &'a ClauseRule, Option<BinOp>) {
        let mut depth = 0usize;
        for index in self.pos..end.saturating_sub(1) {
            let token = &self.tokens[index];
            if token.opens() {
                depth += 1;
            } else if token.closes() {
                depth = depth.saturating_sub(1);
            } else if let Kind::Word(word) = &token.kind {
                if depth == 0
                    && self.tokens[index + 1].span.start > token.span.end
                    && !is_range_word(&self.tokens[index + 1])
                    && begins_clause(&self.tokens[index + 1..])
                {
                    if word.ends_with("거나") {
                        return (index, &OR, Some(BinOp::Or));
                    }
                    if word.ends_with('고') {
                        return (index, &AND, Some(BinOp::And));
                    }
                }
            }
        }
        (end - 1, last, None)
    }

    fn clause(
        &mut self,
        stop: usize,
        rule: &ClauseRule,
        earlier: &[Argument],
        among: &mut Option<Expr>,
    ) -> Result<Expr> {
        if let Some(positive) = self.negated_at(stop, rule) {
            let clause = self.clause(positive, &NOT, earlier, among)?;
            if self.pos != positive + 1 {
                return Err(self.fail("E-SYN-006", NOT.message));
            }
            self.gap_after_previous()?;
            let not = self.take().span;
            let span = clause.span.through(not);
            return Expr::unary(UnaryOp::Not, not, clause, span);
        }
        if let Some(test) = self.existence(stop, rule, among)? {
            return Ok(test);
        }
        let subject = earlier.iter().find(|a| a.role == Role::Subject);
        let inherited = subject.map(|a| &a.value);
        let last = match &self.tokens[stop].kind {
            Kind::Word(word) => word.clone(),
            _ => String::new(),
        };
        let comparative = self.comparative(stop, rule);
        let range = range_operator(&last, rule.copula[0]).is_some() || comparative.is_some();
        let form = rule
            .forms
            .iter()
            .copied()
            .filter(|_| !range && last != rule.negative_copula)
            .find(|form| has_form(&self.tokens[stop], *form));
        let copula =
            range || last == rule.negative_copula || rule.copula.iter().any(|c| last.ends_with(c));
        if form.is_none() && !copula {
            return Err(self.fail("E-SYN-006", rule.message));
        }
        let begin = self.token().span;
        let predicate = form.map(|form| {
            move |parser: &mut Self| {
                if matches!(form, Form::Definition | Form::Descriptive | Form::Negative) {
                    parser.clause_verb_at = Some((stop, form));
                }
                let chain = parser.chain(form);
                parser.clause_verb_at = None;
                let mut chain = chain?;
                if let Some(call) = chain.calls.last() {
                    chain.implied = earlier
                        .iter()
                        .filter(|e| call.arguments.iter().all(|a| a.role != e.role))
                        .cloned()
                        .collect();
                }
                chain_expr(chain, begin.through(parser.previous().span))
            }
        });
        self.predicate_or_copula(predicate, |parser| {
            if !copula {
                return Err(parser.fail("E-SYN-006", rule.message));
            }
            match parser.subject_comparison(&last, rule, comparative, inherited)? {
                Some(test) => Ok(test),
                None => {
                    let (value, matched) = parser.marked(rule.copula)?;
                    parser.note_ending(matched, Category::Word);
                    Ok(value)
                }
            }
        })
    }

    fn negated_at(&self, stop: usize, rule: &ClauseRule) -> Option<usize> {
        let positive = stop.checked_sub(1).filter(|&i| i > self.pos)?;
        let (Kind::Word(not), Kind::Word(word)) =
            (&self.tokens[stop].kind, &self.tokens[positive].kind)
        else {
            return None;
        };
        let negates = rule
            .negative_forms
            .iter()
            .any(|form| forms::lemmas(not, *form).iter().any(|l| l == "않다"));
        let spaced = self.tokens[stop].span.start > self.tokens[positive].span.end;
        (negates && spaced && forms::is_form(word, Form::Negative)).then_some(positive)
    }

    fn comparative(
        &self,
        stop: usize,
        rule: &ClauseRule,
    ) -> Option<(CmpOp, &'static [&'static str])> {
        let before = stop.checked_sub(1).filter(|&i| i >= self.pos)?;
        let Kind::Word(word) = &self.tokens[stop].kind else {
            return None;
        };
        let (op, particles) = forms::lemmas(word, rule.adjective)
            .iter()
            .find_map(|lemma| comparison(lemma))?;
        let mut depth = 0usize;
        let marked = (self.pos..=before).any(|index| {
            let token = &self.tokens[index];
            if token.opens() {
                depth += 1;
            } else if token.closes() {
                depth = depth.saturating_sub(1);
            } else if let Kind::Word(word) = &token.kind {
                return depth == 0
                    && particles.iter().any(|p| word.ends_with(p))
                    && self.tokens[index + 1].span.start > token.span.end;
            }
            false
        });
        let spaced = self.tokens[stop].span.start > self.tokens[before].span.end;
        (marked && spaced).then_some((op, particles))
    }

    fn existence(
        &mut self,
        stop: usize,
        rule: &ClauseRule,
        among: &mut Option<Expr>,
    ) -> Result<Option<Expr>> {
        let exists = match &self.tokens[stop].kind {
            Kind::Word(word) => rule.forms.iter().find_map(|form| {
                let lemmas = forms::lemmas(word, *form);
                lemmas.iter().find_map(|lemma| match lemma.as_str() {
                    "있다" => Some(true),
                    "없다" => Some(false),
                    _ => None,
                })
            }),
            _ => None,
        };
        let binder = stop.checked_sub(1).map(|i| &self.tokens[i]);
        let particle = match binder.map(|token| &token.kind) {
            Some(Kind::Word(word)) if word.ends_with('이') => "이",
            Some(Kind::Word(word)) if word.ends_with('가') => "가",
            _ => return Ok(None),
        };
        let named = (self.pos..stop).any(|i| self.tokens[i].word("중"));
        let Some(exists) = exists.filter(|_| named || among.is_some()) else {
            return Ok(None);
        };
        let start = self.token().span;
        let collection = if named {
            let collection = self.expression()?;
            self.gap(collection.span.end)?;
            self.keyword("중", Category::Flow)?;
            self.gap_after_previous()?;
            *among = Some(collection.clone());
            collection
        } else {
            among.clone().expect("위에서 확인했습니다")
        };
        let filter = self.clause_before(stop, particle)?.map(|(test, _)| test);
        let binder = self.take();
        let Kind::Word(word) = &binder.kind else {
            return Err(self.fail("E-NAME-002", "원소 이름이 필요합니다."));
        };
        let name = word.strip_suffix(particle).unwrap_or(word).to_owned();
        self.gap(binder.span.end)?;
        let verb = self.take();
        let span = start.through(verb.span);
        let found = collect_expr(name, binder.span, collection, filter, span)?;
        let depth = found.depth;
        let found = Expr::bounded(
            ExprKind::Attribute(Box::new(found), "길이".to_owned()),
            span,
            depth + 1,
        )?;
        let zero = Expr::leaf(ExprKind::Literal(Constant::Int(0.into())), verb.span);
        let op = if exists { CmpOp::Gt } else { CmpOp::Eq };
        let test = Expr::bounded(
            ExprKind::Compare(vec![found, zero], vec![(op, verb.span)]),
            span,
            depth + 2,
        )?;
        Expr::bounded(ExprKind::Group(Box::new(test)), span, depth + 3).map(Some)
    }

    fn predicate_or_copula(
        &mut self,
        predicate: Option<impl Fn(&mut Self) -> Result<Expr>>,
        copula: impl FnOnce(&mut Self) -> Result<Expr>,
    ) -> Result<Expr> {
        let mark = self.mark();
        let mut tried = Vec::new();
        let reading = predicate.as_ref().map(|parse| {
            let result = parse(self);
            let end = self.pos;
            tried = self.categories.split_off(mark.categories);
            self.rewind(mark);
            result.map(|expr| (expr, end))
        });
        match (reading, copula(self)) {
            (Some(Ok((reading, end))), Ok(copula)) if end == self.pos => {
                self.categories.extend(tried);
                let depth = depth([&copula, &reading]);
                let span = reading.span;
                Expr::bounded(
                    ExprKind::Either(Box::new(copula), Box::new(reading)),
                    span,
                    depth,
                )
            }
            (_, Ok(copula)) => Ok(copula),
            (Some(Ok(_)), Err(_)) => {
                self.rewind(mark);
                predicate.expect("위에서 읽었습니다")(self)
            }
            (Some(Err(error)), Err(_)) | (None, Err(error)) => Err(error),
        }
    }

    fn subject_comparison(
        &mut self,
        last: &str,
        rule: &ClauseRule,
        comparative: Option<(CmpOp, &'static [&'static str])>,
        inherited: Option<&Expr>,
    ) -> Result<Option<Expr>> {
        let canonical = rule.copula[0];
        let probe = self.mark();
        let parsed = match self.signed_number() {
            Ok(None) => self.postfix(true),
            other => other.map(|signed| signed.expect("부호 붙은 수를 읽었습니다")),
        };
        let ends = |parser: &Self, particles: &[&str]| match &parsed {
            Ok((_, tail)) => particles.iter().any(|particle| {
                word_ends(tail, particle)
                    || (parser.token().word(particle) && parser.token().span.start == tail.span.end)
            }),
            Err(_) => false,
        };
        let subject = ends(self, &["이", "가"]);
        let scrambled = !subject && comparative.is_some_and(|(_, particles)| ends(self, particles));
        self.rewind(probe);
        let range = range_operator(last, canonical).or(comparative.map(|(op, _)| op));
        let negated = last == rule.negative_copula;
        let copula = rule.copula.iter().any(|e| last.ends_with(e));
        let literal = !subject && copula && inherited.is_some() && {
            let value = self.marked(rule.copula);
            self.rewind(probe);
            value.is_ok_and(|(value, _)| is_literal(&value))
        };
        let inherited = inherited.filter(|_| range.is_some() || negated || literal);
        if (!subject && !scrambled && inherited.is_none())
            || (range.is_none() && !negated && !copula)
        {
            return Ok(None);
        }
        let mark = self.mark();
        if let (true, Some((op, particles))) = (scrambled, comparative) {
            let (right, _) = self.marked(particles)?;
            self.gap_after_previous()?;
            let left = if self.token().word(last) {
                self.inherited_subject(inherited)?
            } else {
                let (left, _) = self.marked(&["이", "가"])?;
                self.gap_after_previous()?;
                left
            };
            let op_span = self.take().span;
            return self.compared(mark, left, op, right, op_span).map(Some);
        }
        let mut left = match (subject, inherited) {
            (true, _) => {
                let (left, _) = self.marked(&["이", "가"])?;
                self.gap_after_previous()?;
                left
            }
            (false, Some(inherited)) => inherited.clone(),
            (false, None) => unreachable!("위에서 반환했습니다"),
        };
        let (op, right, op_span) = if let Some((op, particles)) = comparative {
            let (right, _) = self.marked(particles)?;
            self.gap_after_previous()?;
            (op, right, self.take().span)
        } else if let Some(op) = range {
            let right = self.expression()?;
            if !matches!(&self.token().kind, Kind::Word(word) if range_operator(word, canonical).is_some())
            {
                return Err(self.fail("E-SYN-006", "비교 조건은 '이상이면'처럼 끝맺으세요."));
            }
            self.gap(right.span.end)?;
            (op, right, self.take().span)
        } else if negated {
            let right = if self.token().word(rule.negative_copula) {
                let inherited = self.inherited_subject(inherited)?;
                std::mem::replace(&mut left, inherited)
            } else {
                let (right, _) = self.marked(&["이", "가"])?;
                self.gap_after_previous()?;
                right
            };
            (
                CmpOp::Ne,
                right,
                self.expect_word(rule.negative_copula)?.span,
            )
        } else {
            let (right, matched) = self.marked(rule.copula)?;
            let end = self.previous().span.end;
            (CmpOp::Eq, right, Span::new(end - matched.len(), end))
        };
        self.compared(mark, left, op, right, op_span).map(Some)
    }

    fn inherited_subject(&self, inherited: Option<&Expr>) -> Result<Expr> {
        inherited
            .cloned()
            .ok_or_else(|| self.fail("E-SYN-006", "비교할 주어가 필요합니다."))
    }

    fn compared(
        &mut self,
        mark: Mark,
        left: Expr,
        op: CmpOp,
        right: Expr,
        op_span: Span,
    ) -> Result<Expr> {
        let span = left.span.through(right.span).through(op_span);
        let depth = depth([&left, &right]);
        let value = Expr::bounded(
            ExprKind::Compare(vec![left, right], vec![(op, op_span)]),
            span,
            depth,
        )?;
        self.finish(mark, SyntaxKind::Comparison);
        Ok(value)
    }
}

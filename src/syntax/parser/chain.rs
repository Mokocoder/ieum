use super::Parser;
use super::particles::check_particle;
use crate::diagnostic::{Diagnostic, Result, Span};
use crate::syntax::EACH;
use crate::syntax::ast::*;
use crate::syntax::forms::{self, Form};
use crate::syntax::layout::open_connective;
use crate::syntax::lexer::Kind;
use crate::syntax::tree::SyntaxKind;

impl Parser<'_> {
    pub(super) fn verb(&self) -> Result<Option<(Vec<String>, Form, usize)>> {
        let Kind::Word(word) = &self.token().kind else {
            return Ok(None);
        };
        if forms::ends_with_particle(word)
            || self
                .tokens
                .get(self.pos + 1)
                .is_some_and(|next| next.symbol("[") && next.span.start == self.token().span.end)
        {
            return Ok(None);
        }
        if let Some((_, form)) = self.clause_verb_at.filter(|(at, _)| *at == self.pos) {
            let lemmas = forms::lemmas(word, form);
            if !lemmas.is_empty() {
                return Ok(Some((lemmas, form, 1)));
            }
        }
        if let Some(auxiliary) = self.auxiliary(word) {
            return Ok(Some(auxiliary));
        }
        let following = self.tokens[self.pos + 1..]
            .iter()
            .find(|t| t.kind != Kind::Newline);
        for (form, next_word) in [
            (Form::Definition, Some("법")),
            (Form::Nominal, Some("값")),
            (Form::Finite, None),
            (Form::Sequence, None),
            (Form::Pipe, None),
            (Form::Conditional, None),
            (Form::WhetherIs, None),
            (Form::WhetherDoes, None),
            (Form::Or, None),
        ] {
            if next_word.is_some_and(|s| !following.is_some_and(|t| t.word(s))) {
                continue;
            }
            let lemmas = forms::lemmas(word, form);
            if lemmas.is_empty() {
                continue;
            }
            if next_word.is_some() {
                let following = following.expect("위에서 확인했습니다");
                let gap = self.text(Span::new(self.token().span.end, following.span.start));
                if gap.is_empty() || gap.contains(['\r', '\n']) {
                    return Err(Diagnostic::new(
                        "E-FORM-002",
                        self.token().span.through(following.span),
                        "'법'이나 '값'은 동사 뒤에 같은 줄에서 띄어 쓰세요.",
                    ));
                }
            }
            return Ok(Some((
                lemmas,
                form,
                if next_word.is_some() { 2 } else { 1 },
            )));
        }
        for (tail, form) in [("법", Form::Definition), ("값", Form::Nominal)] {
            if word
                .strip_suffix(tail)
                .is_some_and(|stem| forms::is_form(stem, form))
            {
                return Err(self.fail("E-FORM-002", "'법'이나 '값'은 동사 뒤에 띄어 쓰세요."));
            }
        }
        if let Some(next) = self.tokens.get(self.pos + 1) {
            if matches!(&next.kind, Kind::Word(s) if matches!(s.as_str(), "한다" | "하고" | "해서" | "하는" | "한"))
                && !Role::PARTICLES
                    .iter()
                    .any(|particle| word.ends_with(particle))
            {
                let end = self
                    .tokens
                    .get(self.pos + 2)
                    .filter(|t| {
                        (next.word("하는") && t.word("법")) || (next.word("한") && t.word("값"))
                    })
                    .unwrap_or(next)
                    .span;
                return Err(Diagnostic::new(
                    "E-FORM-002",
                    self.token().span.through(end),
                    "동사의 어근과 활용형을 붙여 쓰세요.",
                ));
            }
        }
        Ok(None)
    }

    fn auxiliary(&self, word: &str) -> Option<(Vec<String>, Form, usize)> {
        let next = self.tokens.get(self.pos + 1)?;
        let Kind::Word(auxiliary) = &next.kind else {
            return None;
        };
        if next.span.start == self.token().span.end
            || !forms::is_form(&format!("{word}서"), Form::Pipe)
        {
            return None;
        }
        let candidates = match self.clause_verb_at {
            Some((at, form)) if at == self.pos + 1 => vec![form],
            _ => vec![
                Form::Finite,
                Form::Sequence,
                Form::Pipe,
                Form::Conditional,
                Form::WhetherIs,
                Form::WhetherDoes,
                Form::Or,
            ],
        };
        candidates.into_iter().find_map(|form| {
            let is = forms::lemmas(auxiliary, form).iter().any(|l| l == "있다");
            let lemmas = forms::lemmas(&format!("{word}{auxiliary}"), form);
            (is && !lemmas.is_empty()).then_some((lemmas, form, 2))
        })
    }

    pub(super) fn chain(&mut self, final_form: Form) -> Result<Chain> {
        self.node(SyntaxKind::CallChain, |parser| {
            parser.chain_inner(final_form)
        })
    }

    fn chain_inner(&mut self, final_form: Form) -> Result<Chain> {
        let mut chain = Chain {
            calls: Vec::new(),
            edges: Vec::new(),
            implied: Vec::new(),
            each: None,
            concurrency: Concurrency::Sequential,
        };
        loop {
            let call_mark = self.mark();
            let mut arguments = Vec::new();
            let (word, lemmas, form, span) = loop {
                if let Some((lemmas, form, count)) = self.verb()? {
                    let mut word = self.text(self.token().span).to_owned();
                    if let Kind::Word(auxiliary) = &self.tokens[self.pos + 1].kind {
                        if count == 2 && !matches!(auxiliary.as_str(), "법" | "값") {
                            word.push_str(auxiliary);
                        }
                    }
                    if lemmas.iter().any(|lemma| lemma == "반환하다") {
                        return Err(self.fail(
                            "E-SYN-012",
                            "반환은 '값을 반환한다.' 또는 '반환한다.'라는 별도 문장으로 쓰세요.",
                        ));
                    }
                    if form != final_form && !matches!(form, Form::Sequence | Form::Pipe) {
                        return Err(
                            self.fail("E-FORM-001", "이 위치에 맞는 동사 활용형이 아닙니다.")
                        );
                    }
                    let start = self.token().span;
                    for _ in 0..count {
                        self.take();
                    }
                    break (word, lemmas, form, start.through(self.previous().span));
                }
                if matches!(
                    self.token().kind,
                    Kind::Newline | Kind::Dedent | Kind::Eof | Kind::Symbol("." | ")")
                ) {
                    return Err(
                        self.fail("E-SYN-005", "문장 끝에 동사가 필요합니다. 예: '출력한다.'")
                    );
                }
                let concurrency = match &self.token().kind {
                    Kind::Word(word) => {
                        Concurrency::from_word(word).map(|concurrency| (word.clone(), concurrency))
                    }
                    _ => None,
                };
                if let Some((word, concurrency)) = concurrency {
                    let token = self.take_keyword(Category::Flow);
                    if chain.each.is_none() {
                        return Err(Diagnostic::new(
                            "E-SYN-016",
                            token.span,
                            format!("문장 안의 '{word}'는 '각각'이 있을 때만 씁니다."),
                        ));
                    }
                    if chain.concurrency != Concurrency::Sequential {
                        return Err(Diagnostic::new(
                            "E-SYN-016",
                            token.span,
                            "'함께'와 '병렬로'는 한 문장에 하나만 씁니다.",
                        ));
                    }
                    chain.concurrency = concurrency;
                    self.gap(token.span.end)?;
                    continue;
                }
                if let Kind::Word(word) = &self.token().kind {
                    if matches!(
                        self.tokens[self.pos + 1].kind,
                        Kind::Newline | Kind::Symbol("." | ")")
                    ) && !Role::PARTICLES.iter().any(|s| word.ends_with(s))
                    {
                        return Err(self.fail("E-FORM-001", "쓸 수 없는 동사 활용형입니다. 문장은 '출력한다.'처럼, 값은 '(정렬한 값)'처럼 씁니다."));
                    }
                }
                let argument_mark = self.mark();
                let start = self.token().span;
                let (value, particle) = match self.each_argument()? {
                    Some((collection, particle)) => {
                        if chain.each.is_some() {
                            return Err(
                                self.fail("E-SYN-015", "'각각'은 한 문장에 한 번만 씁니다.")
                            );
                        }
                        chain.each = Some(Box::new(collection));
                        let span = self.previous().span;
                        (Expr::leaf(ExprKind::Name(EACH.to_owned()), span), particle)
                    }
                    None => self.marked(Role::PARTICLES)?,
                };
                let role =
                    Role::from_particle(particle).expect("Role::PARTICLES에서 읽은 조사입니다");
                arguments.push(Argument {
                    role,
                    value,
                    span: start.through(self.previous().span),
                });
                self.finish(argument_mark, SyntaxKind::Argument);
                self.gap_after_previous()?;
            };
            let callee = if lemmas.iter().any(|lemma| lemma == "호출하다") {
                if !arguments.first().is_some_and(|a| {
                    a.role == Role::Direction
                        && matches!(a.value.kind, ExprKind::Group(_) | ExprKind::Chain(_))
                }) {
                    return Err(Diagnostic::new(
                        "E-SYN-011",
                        span,
                        "호출 대상은 괄호로 감싼 뒤 으로/로를 붙여 먼저 씁니다.",
                    ));
                }
                self.note(span, Category::Word);
                Callee::Invoke(Box::new(arguments.remove(0).value))
            } else {
                let (lemmas, class) = match form {
                    Form::WhetherIs | Form::WhetherDoes => forms::whether_lemmas(&word),
                    _ => (lemmas, form.class()),
                };
                Callee::Verb {
                    name: lemmas[0].clone(),
                    word,
                    lemmas,
                    class,
                }
            };
            chain.calls.push(Call {
                callee,
                span,
                arguments,
            });
            self.finish(call_mark, SyntaxKind::Call);
            if form == final_form {
                return Ok(chain);
            }
            chain.edges.push(if form == Form::Pipe {
                Edge::Pipe
            } else {
                Edge::Sequence
            });
            if matches!(
                self.token().kind,
                Kind::Eof | Kind::Dedent | Kind::Symbol("." | "," | ")")
            ) {
                return Err(open_connective(self.token().span));
            }
            if matches!(self.token().kind, Kind::Newline) {
                self.take();
            } else {
                self.gap(span.end)?;
            }
        }
    }

    fn each_argument(&mut self) -> Result<Option<(Expr, &'static str)>> {
        let mark = self.mark();
        let parsed = self.postfix(true);
        let particle = match (&parsed, &self.token().kind) {
            (Ok((value, _)), Kind::Word(word)) if self.token().span.start > value.span.end => word
                .strip_prefix(EACH)
                .and_then(|particle| Role::PARTICLES.iter().find(|p| **p == particle)),
            _ => None,
        };
        let (Some(&particle), Ok((value, _))) = (particle, parsed) else {
            self.rewind(mark);
            return Ok(None);
        };
        check_particle(EACH, particle, self.token().span)?;
        let word = self.take();
        let start = word.span.start;
        self.note(Span::new(start, start + EACH.len()), Category::Flow);
        Ok(Some((value, particle)))
    }
}

pub(super) fn chain_expr(mut chain: Chain, span: Span) -> Result<Expr> {
    let each = chain.each.take();
    let inner = depth(chain.calls.iter().flat_map(|call| {
        let callee = match &call.callee {
            Callee::Invoke(callee) => Some(callee.as_ref()),
            Callee::Verb { .. } => None,
        };
        call.arguments.iter().map(|a| &a.value).chain(callee)
    }));
    let value = Expr::bounded(ExprKind::Chain(chain), span, inner)?;
    let Some(collection) = each else {
        return Ok(value);
    };
    let depth = depth([collection.as_ref(), &value]);
    let concurrency = match &value.kind {
        ExprKind::Chain(chain) => chain.concurrency,
        _ => Concurrency::Sequential,
    };
    let collect = Collect {
        name: EACH.to_owned(),
        name_span: collection.span,
        collection: *collection,
        filter: None,
        value: Some(value),
        concurrency,
    };
    Expr::bounded(ExprKind::Collect(Box::new(collect)), span, depth)
}

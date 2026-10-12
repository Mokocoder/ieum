use super::{Parser, clause, has_form, word_ends};
use crate::diagnostic::{Diagnostic, Result, Span};
use crate::runtime::value::Constant;
use crate::syntax::EACH;
use crate::syntax::ast::*;
use crate::syntax::forms::{self, Form};
use crate::syntax::lexer::{Kind, Token, literal, literal_name_error};
use crate::syntax::tree::SyntaxKind;

impl Parser<'_> {
    fn starts_binding(&self) -> bool {
        match &self.token().kind {
            Kind::Word(s) => {
                (s.ends_with('은') || s.ends_with('는'))
                    && self.tokens[self.pos + 1].span.start > self.token().span.end
            }
            Kind::Quoted(_) => self
                .tokens
                .get(self.pos + 1)
                .is_some_and(|t| t.word("은") || t.word("는")),
            _ => false,
        }
    }

    pub(super) fn statement(&mut self) -> Result<Statement> {
        self.node_of(Self::statement_inner, |statement| {
            Some(match &statement.kind {
                StatementKind::Import(_) => SyntaxKind::Import,
                StatementKind::Bind { .. } => SyntaxKind::Binding,
                StatementKind::Assign { .. } => SyntaxKind::Assignment,
                StatementKind::Return(_) => SyntaxKind::Return,
                StatementKind::Function(_) => SyntaxKind::Function,
                StatementKind::Record(_) => SyntaxKind::Record,
                StatementKind::Try(_) => SyntaxKind::Try,
                StatementKind::Throw(_) => SyntaxKind::Throw,
                StatementKind::Rethrow => SyntaxKind::Rethrow,
                StatementKind::If { .. } => SyntaxKind::If,
                StatementKind::For { .. } => SyntaxKind::For,
                StatementKind::Concurrent(..) => SyntaxKind::Concurrent,
                StatementKind::While { .. } => SyntaxKind::While,
                StatementKind::Break => SyntaxKind::Break,
                StatementKind::Continue => SyntaxKind::Continue,
                StatementKind::Exit(_) => SyntaxKind::Exit,
                StatementKind::Chain(_) => SyntaxKind::CallStatement,
            })
        })
    }

    fn statement_inner(&mut self) -> Result<Statement> {
        let start = self.token().span;
        let tail = self.line_tail();
        if tail.last().is_some_and(|t| t.symbol(":")) {
            return self.header(&tail);
        }
        if self.token().word("그렇지") {
            return Err(self.misplaced_otherwise());
        }
        let constant = self.constant_keyword(&tail)?;
        misplaced_header(&tail)?;
        let kind = self.statement_kind(&tail, constant)?;
        if matches!(self.token().kind, Kind::Newline | Kind::Eof) {
            return Err(self.fail("E-LAYOUT-006", "문장은 마침표로 끝내세요."));
        }
        let dot = self.expect_symbol(".")?;
        if !matches!(self.token().kind, Kind::Newline) {
            return Err(self.fail("E-LAYOUT-007", "한 줄에는 문장을 하나만 쓰세요."));
        }
        self.take();
        Ok(Statement {
            kind,
            span: start.through(dot.span),
        })
    }

    fn misplaced_otherwise(&self) -> Diagnostic {
        self.fail(
            "E-SYN-008",
            "'그렇지 않으면'과 '그렇지 않고'는 '-면:' 블록 바로 뒤에만 씁니다.",
        )
    }

    fn constant_keyword(&mut self, tail: &[Token]) -> Result<bool> {
        if !self.token().word("상수") {
            return Ok(false);
        }
        let keyword = self.take_keyword(Category::Word);
        self.same_line_gap(keyword.span.end)?;
        if final_word(tail).is_some_and(|t| t.word("가진다")) {
            return Err(Diagnostic::new(
                "E-SYN-012",
                keyword.span,
                "상수는 '이름은 값이다.' 정의에만 붙입니다. 자료형 이름은 원래 바꿀 수 없습니다.",
            ));
        }
        Ok(true)
    }

    fn statement_kind(&mut self, tail: &[Token], constant: bool) -> Result<StatementKind> {
        let ends_with = |word: &str| final_word(tail).is_some_and(|t| t.word(word));
        let control = tail.len() == 3
            && tail[2].symbol(".")
            && ((tail[0].word("반복을") && tail[1].word("중단한다"))
                || (tail[0].word("다음으로") && tail[1].word("진행한다")));
        let rethrow = tail.len() == 4
            && tail[0].word("오류를")
            && tail[1].word("다시")
            && tail[2].word("발생시킨다")
            && tail[3].symbol(".");
        let exits = tail.len() >= 3 && tail[0].word("실행을") && ends_with("끝낸다");
        let returns = tail
            .iter()
            .rev()
            .find(|t| !t.symbol("."))
            .is_some_and(|t| t.word("반환한다"));
        Ok(if !constant && rethrow {
            for _ in 0..2 {
                let word = self.take_keyword(Category::Flow);
                self.same_line_gap(word.span.end)?;
            }
            self.take_keyword(Category::Flow);
            StatementKind::Rethrow
        } else if !constant && self.token().word("오류") {
            let keyword = self.take_keyword(Category::Flow);
            self.same_line_gap(keyword.span.end)?;
            let (value, _) = self.marked(&["을", "를"])?;
            self.gap_after_previous()?;
            self.keyword("발생시킨다", Category::Flow)?;
            StatementKind::Throw(value)
        } else if !constant && matches!(self.token().kind, Kind::Text(_)) && ends_with("가져온다")
        {
            StatementKind::Import(self.import()?)
        } else if !constant && exits {
            let keyword = self.take_keyword(Category::Flow);
            self.same_line_gap(keyword.span.end)?;
            let code = if self.token().word("끝낸다") {
                None
            } else {
                let (code, _) = self.marked(&["으로", "로"])?;
                self.gap_after_previous()?;
                Some(code)
            };
            self.keyword("끝낸다", Category::Flow)?;
            StatementKind::Exit(code)
        } else if control {
            let first = self.take_keyword(Category::Flow);
            self.same_line_gap(first.span.end)?;
            self.take_keyword(Category::Flow);
            if first.word("반복을") {
                StatementKind::Break
            } else {
                StatementKind::Continue
            }
        } else if !constant && self.starts_binding() && ends_with("가진다") {
            StatementKind::Record(self.record_type()?)
        } else if constant || self.starts_binding() {
            self.binding(constant)?
        } else if ends_with("바꾼다") {
            self.assignment()?
        } else if (ends_with("늘린다") || ends_with("줄인다"))
            && tail.iter().any(|t| word_ends(t, "만큼"))
        {
            self.adjustment()?
        } else if returns {
            let value = if self.token().word("반환한다") {
                Expr::leaf(ExprKind::Literal(Constant::None), self.token().span)
            } else {
                let (value, _) = self.marked(&["을", "를"])?;
                self.gap_after_previous()?;
                value
            };
            self.keyword("반환한다", Category::Flow)?;
            StatementKind::Return(value)
        } else if final_word(tail).is_some_and(|t| word_ends(t, "이다")) {
            return Err(self.fail(
                "E-SYN-012",
                "정의는 '이름은 값이다.', 결과는 '값을 반환한다.'로 씁니다.",
            ));
        } else {
            let mut chain = self.chain(Form::Finite)?;
            match chain.each.take() {
                Some(collection) => {
                    let span = collection.span.through(self.previous().span);
                    StatementKind::For {
                        name: EACH.to_owned(),
                        name_span: collection.span,
                        second: None,
                        collection: *collection,
                        last: None,
                        filter: None,
                        concurrency: chain.concurrency,
                        body: vec![Statement {
                            kind: StatementKind::Chain(chain),
                            span,
                        }],
                    }
                }
                None => StatementKind::Chain(chain),
            }
        })
    }

    fn binding(&mut self, constant: bool) -> Result<StatementKind> {
        let (name, name_span, _) = self.marked_name(&["은", "는"])?;
        self.gap_after_previous()?;
        let (value, copula) = self.marked(&["이다", "다"])?;
        self.note_ending(copula, Category::Word);
        Ok(StatementKind::Bind {
            name,
            name_span,
            mutable: !constant,
            value,
        })
    }

    fn assignment(&mut self) -> Result<StatementKind> {
        let (target, target_span) = self.assignment_target()?;
        let (value, _) = self.marked(&["으로", "로"])?;
        self.gap_after_previous()?;
        self.keyword("바꾼다", Category::Word)?;
        Ok(StatementKind::Assign {
            target,
            target_span,
            value,
            adjust: None,
        })
    }

    fn adjustment(&mut self) -> Result<StatementKind> {
        let (target, target_span) = self.assignment_target()?;
        let (value, _) = self.marked(&["만큼"])?;
        self.gap_after_previous()?;
        let adjust = match &self.token().kind {
            Kind::Word(verb) if verb == "늘린다" => BinOp::Add,
            Kind::Word(verb) if verb == "줄인다" => BinOp::Sub,
            _ => return Err(self.fail("E-SYN-002", "'늘린다' 또는 '줄인다' 표기가 필요합니다.")),
        };
        self.take_keyword(Category::Word);
        Ok(StatementKind::Assign {
            target,
            target_span,
            value,
            adjust: Some(adjust),
        })
    }

    fn assignment_target(&mut self) -> Result<(Expr, Span)> {
        let target_start = self.token().span;
        let (target, _) = self.marked(&["을", "를"])?;
        let target_span = target_start.through(self.previous().span);
        if !matches!(
            target.ungrouped().kind,
            ExprKind::Name(_) | ExprKind::Index(..) | ExprKind::Attribute(..)
        ) {
            return Err(Diagnostic::new(
                "E-SYN-010",
                target_start,
                "바꿀 수 있는 것은 이름, 인덱스, 속성뿐입니다.",
            ));
        }
        self.gap_after_previous()?;
        Ok((target, target_span))
    }

    fn header(&mut self, tail: &[Token]) -> Result<Statement> {
        let start = self.token().span;
        let words: Vec<&str> = tail
            .iter()
            .filter_map(|t| {
                if let Kind::Word(s) = &t.kind {
                    Some(s.as_str())
                } else {
                    None
                }
            })
            .collect();
        if words.starts_with(&["그렇지", "않으면"]) || words.starts_with(&["그렇지", "않고"])
        {
            return Err(self.misplaced_otherwise());
        }
        if (tail.len() == 2 && tail[0].word("마지막으로"))
            || matches!(tail, [first, .., word, _] if first.word("오류를") && word.word("받으면"))
        {
            return Err(self.fail(
                "E-SYN-013",
                "오류 처리 블록과 정리 블록은 바로 앞 시도 블록에 이어서 씁니다.",
            ));
        }
        let concurrency = match tail {
            [
                Token {
                    kind: Kind::Word(word),
                    ..
                },
                _,
            ] => Concurrency::from_word(word),
            _ => None,
        };
        let kind = if tail.len() == 2 && tail[0].word("시도한다") {
            self.attempt(start)?
        } else if let Some(concurrency) = concurrency {
            self.take_keyword(Category::Flow);
            StatementKind::Concurrent(concurrency, self.suite()?)
        } else if declares_function(tail) {
            StatementKind::Function(self.definition(Form::Definition)?)
        } else if let Some(form) = [Form::WhetherIs, Form::WhetherDoes]
            .into_iter()
            .find(|form| final_word(tail).is_some_and(|t| has_form(t, *form)))
        {
            StatementKind::Function(self.definition(form)?)
        } else if words.last() == Some(&"동안") {
            self.while_loop(start)?
        } else if words.last().is_some_and(|s| s.ends_with("마다")) {
            self.for_loop(words.iter().any(|s| s.ends_with("부터")))?
        } else if words.last().is_some_and(|s| s.ends_with('면')) {
            self.conditional(start)?
        } else {
            return Err(Diagnostic::new(
                "E-SYN-006",
                tail.last().expect("헤더는 콜론으로 끝납니다").span,
                "알 수 없는 블록 헤더입니다. '-면:', '-인 동안:', '-마다:'처럼 씁니다.",
            ));
        };
        Ok(Statement {
            kind,
            span: start.through(self.previous().span),
        })
    }

    fn attempt(&mut self, start: Span) -> Result<StatementKind> {
        self.take_keyword(Category::Flow);
        let body = self.suite()?;
        let next = self.line_tail();
        let handler = if matches!(next.as_slice(), [first, .., word, colon]
            if first.word("오류를") && word.word("받으면") && colon.symbol(":"))
        {
            Some(self.handler()?)
        } else {
            None
        };
        let next = self.line_tail();
        let (cleanup, cleanup_span) =
            if next.len() == 2 && next[0].word("마지막으로") && next[1].symbol(":") {
                let mark = self.mark();
                let head = self.take_keyword(Category::Flow);
                let body = self.suite()?;
                let span = head.span.through(self.previous().span);
                self.finish(mark, SyntaxKind::Cleanup);
                (body, span)
            } else {
                (Vec::new(), start)
            };
        if handler.is_none() && cleanup.is_empty() {
            return Err(Diagnostic::new(
                "E-SYN-013",
                start,
                "시도 블록에는 오류 처리나 마지막 정리 블록이 필요합니다.",
            ));
        }
        Ok(StatementKind::Try(TryBlock {
            body,
            handler,
            cleanup,
            cleanup_span,
        }))
    }

    fn handler(&mut self) -> Result<Handler> {
        let mark = self.mark();
        let head = self.take_keyword(Category::Flow);
        self.same_line_gap(head.span.end)?;
        let (name, name_span, _) = self.marked_name(&["으로", "로"])?;
        self.same_line_gap(self.previous().span.end)?;
        self.keyword("받으면", Category::Flow)?;
        let body = self.suite()?;
        let span = head.span.through(self.previous().span);
        self.finish(mark, SyntaxKind::Handler);
        Ok(Handler {
            name,
            name_span,
            span,
            body,
        })
    }

    fn while_loop(&mut self, start: Span) -> Result<StatementKind> {
        let end = self.header_end(|token| token.word("동안"));
        let test = self.clauses(end, &clause::WHILE, None)?;
        let test_span = start.through(self.previous().span);
        self.same_line_gap(self.previous().span.end)?;
        self.keyword("동안", Category::Flow)?;
        Ok(StatementKind::While {
            test,
            test_span,
            body: self.suite()?,
        })
    }

    fn for_loop(&mut self, count: bool) -> Result<StatementKind> {
        let (collection, last) = if count {
            let (first, last, down) = self.range_bounds()?;
            self.gap_after_previous()?;
            (first, Some(Box::new((last, down))))
        } else {
            let collection = self.expression()?;
            self.gap(collection.span.end)?;
            (collection, None)
        };
        if !count || self.token().word("중") {
            let among = self.keyword("중", Category::Flow)?;
            self.gap(among.span.end)?;
        }
        let end = self.header_end(|token| token.symbol(":"));
        let pair = (end >= self.pos + 2)
            .then(|| match &self.tokens[end - 2].kind {
                Kind::Word(word) if word.ends_with('과') => Some("과"),
                Kind::Word(word) if word.ends_with('와') => Some("와"),
                _ => None,
            })
            .flatten()
            .filter(|_| word_ends(&self.tokens[end - 1], "마다"));
        let (filter, first) = match pair {
            Some(particle) => {
                let filter = self.clause_before(end - 1, particle)?;
                let (first, first_span, _) = self.marked_name(&["과", "와"])?;
                self.gap_after_previous()?;
                (filter, Some((first, first_span)))
            }
            None => (self.relative_clause()?, None),
        };
        let (name, name_span, each) = self.marked_name(&["마다"])?;
        self.note_ending(each, Category::Flow);
        let (name, name_span, second) = match first {
            Some((first, first_span)) => (first, first_span, Some((name, name_span))),
            None => (name, name_span, None),
        };
        Ok(StatementKind::For {
            name,
            name_span,
            second,
            collection,
            last,
            filter,
            body: self.suite()?,
            concurrency: Concurrency::Sequential,
        })
    }

    fn conditional(&mut self, start: Span) -> Result<StatementKind> {
        let end = self.header_end(|token| token.symbol(":"));
        let test = self.clauses(end, &clause::IF, None)?;
        self.note(self.previous().span, Category::Condition);
        let test_span = start.through(self.previous().span);
        let body = self.suite()?;
        let otherwise = if self.token().word("그렇지") {
            let head = self.take_keyword(Category::Flow);
            self.same_line_gap(head.span.end)?;
            if self.token().word("않고") {
                let and = self.take_keyword(Category::Flow);
                self.same_line_gap(and.span.end)?;
                vec![self.next_condition()?]
            } else {
                self.keyword("않으면", Category::Flow)?;
                self.suite()?
            }
        } else {
            Vec::new()
        };
        Ok(StatementKind::If {
            test,
            test_span,
            body,
            otherwise,
        })
    }

    fn next_condition(&mut self) -> Result<Statement> {
        let start = self.token().span;
        let tail = self.line_tail();
        let header = matches!(tail.as_slice(), [.., last, colon]
            if colon.symbol(":") && word_ends(last, "면"));
        if !header {
            return Err(self.fail("E-SYN-006", "'그렇지 않고' 뒤에는 '-면:' 조건이 옵니다."));
        }
        let kind = self.node(SyntaxKind::If, |parser| parser.conditional(start))?;
        Ok(Statement {
            kind,
            span: start.through(self.previous().span),
        })
    }

    fn record_type(&mut self) -> Result<RecordType> {
        let (name, name_span, _) = self.marked_name(&["은", "는"])?;
        let mut fields = Vec::new();
        loop {
            self.gap_after_previous()?;
            let (field, span, default, particle) = if self.default_follows() {
                let token = self.name()?;
                let field = match token.kind {
                    Kind::Word(word) if literal(&word).is_none() => word,
                    Kind::Quoted(name) => name,
                    _ => return Err(literal_name_error(token.span)),
                };
                let (default, particle) = self.default_value()?;
                (field, token.span, Some(default), particle)
            } else {
                let (field, span, particle) = self.marked_name(&["와", "과", "을", "를"])?;
                (field, span, None, particle.to_owned())
            };
            self.note(span, Category::Field);
            fields.push(Field {
                name: field,
                span,
                default,
            });
            match particle.as_str() {
                "을" | "를" => break,
                "와" | "과" => {}
                _ => {
                    return Err(Diagnostic::new(
                        "E-AFFIX-001",
                        span,
                        "필드는 와/과로 잇고 마지막 필드에 을/를을 붙입니다.",
                    ));
                }
            }
        }
        self.gap_after_previous()?;
        self.keyword("가진다", Category::Word)?;
        Ok(RecordType {
            name,
            name_span,
            fields,
        })
    }

    fn import(&mut self) -> Result<Import> {
        let (path, _) = self.marked(&["에서"])?;
        let ExprKind::Literal(Constant::Text(path_text)) = path.kind else {
            return Err(Diagnostic::new(
                "E-MODULE-001",
                path.span,
                "가져올 파일은 문자열로 씁니다.",
            ));
        };
        let mut names = Vec::new();
        loop {
            self.gap_after_previous()?;
            let (name, span, particle) = self.marked_name(&["와", "과", "을", "를"])?;
            names.push((name, span));
            if matches!(particle, "을" | "를") {
                break;
            }
        }
        self.gap_after_previous()?;
        self.keyword("가져온다", Category::Word)?;
        Ok(Import {
            path: path_text.to_string(),
            path_span: path.span,
            names,
            target: None,
            exports: Default::default(),
        })
    }

    fn header_end(&self, stop: impl Fn(&Token) -> bool) -> usize {
        let mut depth = 0usize;
        for (index, token) in self.tokens.iter().enumerate().skip(self.pos) {
            if token.opens() {
                depth += 1;
            } else if token.closes() {
                depth = depth.saturating_sub(1);
            } else if matches!(token.kind, Kind::Newline | Kind::Eof) || (depth == 0 && stop(token))
            {
                return index;
            }
        }
        self.tokens.len() - 1
    }

    fn relative_clause(&mut self) -> Result<Option<(Expr, Span)>> {
        let end = self.header_end(|token| token.symbol(":"));
        self.clause_before(end, "마다")
    }

    pub(super) fn clause_before(
        &mut self,
        end: usize,
        particle: &str,
    ) -> Result<Option<(Expr, Span)>> {
        let mut binder = end.saturating_sub(1).max(self.pos);
        if binder > self.pos
            && self.tokens[binder].word(particle)
            && matches!(self.tokens[binder - 1].kind, Kind::Quoted(_))
            && self.tokens[binder - 1].span.end == self.tokens[binder].span.start
        {
            binder -= 1;
        }
        if binder == self.pos {
            return Ok(None);
        }
        let head = match &self.tokens[binder].kind {
            Kind::Word(word) => word.strip_suffix(particle).map(str::to_owned),
            Kind::Quoted(name) => Some(name.clone()),
            _ => None,
        };
        let head = head.map(|name| Expr::leaf(ExprKind::Name(name), self.tokens[binder].span));
        let start = self.token().span;
        let test = self.clauses(binder, &clause::WHICH, head)?;
        let span = start.through(self.previous().span);
        self.gap(span.end)?;
        Ok(Some((test, span)))
    }

    fn definition(&mut self, form: Form) -> Result<Function> {
        let mut parameters = Vec::new();
        let mut fixed: Option<(Role, String, Span)> = None;
        let (lemmas, class, name_span, written) = loop {
            if let Some((_, _, count)) = self.verb()?.filter(|(_, found, _)| *found == form) {
                let token = self.token().clone();
                let written = self.text(token.span).to_owned();
                let Kind::Word(word) = &token.kind else {
                    unreachable!("verb()는 낱말만 읽습니다");
                };
                let (lemmas, class) = if form == Form::Definition {
                    (forms::lemmas(word, form), Some(WordClass::Verb))
                } else {
                    forms::whether_lemmas(word)
                };
                if lemmas
                    .iter()
                    .any(|l| matches!(l.as_str(), "호출하다" | "반환하다" | "바꾸다"))
                {
                    return Err(self.fail(
                        "E-SYN-011",
                        "호출하다, 반환하다, 바꾸다는 정해진 문장에 쓰는 말이라 함수 이름으로 쓸 수 없습니다.",
                    ));
                }
                for _ in 0..count {
                    self.take();
                }
                if count == 2 {
                    self.note(self.previous().span, Category::Word);
                }
                break (lemmas, class, token.span, written);
            }
            let marked =
                matches!(&self.token().kind, Kind::Word(w) if forms::ends_with_particle(w));
            if fixed.is_none() && marked {
                let (value, particle) = self.marked(Role::PARTICLES)?;
                let (noun, _) = self.parameter_name(&value)?;
                let role =
                    Role::from_particle(particle).expect("Role::PARTICLES에서 읽은 조사입니다");
                fixed = Some((role, noun.to_owned(), value.span));
                self.gap_after_previous()?;
                continue;
            }
            parameters.push(self.parameter()?);
            if self.token().symbol(",") {
                self.take();
            } else {
                self.gap_after_previous()?;
            }
        };
        let input = parameters
            .iter()
            .position(|p| p.spec.role == Role::Object)
            .or_else(|| (parameters.len() == 1).then_some(0));
        if let Some(index) = input {
            parameters[index].spec.input = true;
        }
        let (lemmas, written, readings) = match &fixed {
            Some((role, noun, _)) => {
                let compounds = lemmas.iter().map(|lemma| format!("{noun}{lemma}"));
                let phrases: Vec<_> = lemmas.iter().map(|l| phrase(noun, *role, l)).collect();
                let written = format!("{noun}{} {written}", role.after(noun));
                let shown = phrases.clone();
                (compounds.chain(phrases).collect(), written, shown)
            }
            None => (lemmas.clone(), written, lemmas),
        };
        let shown = match (readings.as_slice(), form) {
            ([only], _) => only.clone(),
            (_, Form::Definition) => format!("{written} 법"),
            _ => written,
        };
        Ok(Function {
            name: lemmas[0].clone(),
            lemmas,
            shown,
            class,
            name_span,
            fixed: fixed.map(|(_, noun, span)| (noun, span)),
            parameters,
            body: self.suite()?,
        })
    }

    fn parameter_name<'e>(&self, value: &'e Expr) -> Result<(&'e str, Span)> {
        value
            .as_name()
            .ok_or_else(|| self.fail("E-SYN-007", "매개변수 이름이 필요합니다."))
    }

    fn default_follows(&self) -> bool {
        self.tokens
            .get(self.pos + 1)
            .is_some_and(|t| t.symbol("(") && t.span.start == self.token().span.end)
    }

    fn default_value(&mut self) -> Result<(Expr, String)> {
        self.expect_symbol("(")?;
        let keyword = self.keyword("기본", Category::Word)?;
        self.same_line_gap(keyword.span.end)?;
        let default = self.expression()?;
        let close = self.expect_symbol(")")?;
        if !(Role::PARTICLES.iter().any(|p| self.token().word(p))
            && self.token().span.start == close.span.end)
        {
            return Err(self.fail("E-AFFIX-001", "기본값 괄호 뒤에 조사를 붙이세요."));
        }
        let Kind::Word(particle) = self.take().kind else {
            unreachable!("위에서 확인했습니다")
        };
        Ok((default, particle))
    }

    fn parameter(&mut self) -> Result<Parameter> {
        let mark = self.mark();
        let start = self.token().span;
        if !matches!(self.token().kind, Kind::Quoted(_)) {
            return Err(self.fail(
                "E-SYN-007",
                "매개변수 이름을 역따옴표로 감싸세요. 그대로 쓰는 낱말은 하나만 둘 수 있습니다.",
            ));
        }
        let (name, name_span, default, particle) = if self.default_follows() {
            let token = self.name()?;
            let Kind::Quoted(name) = token.kind else {
                unreachable!("위에서 확인했습니다")
            };
            let (default, particle) = self.default_value()?;
            (name, token.span, Some(default), particle)
        } else {
            let (value, particle) = self.marked(Role::PARTICLES)?;
            let (name, name_span) = self.parameter_name(&value)?;
            (name.to_owned(), name_span, None, particle.to_owned())
        };
        let role = Role::from_particle(&particle).expect("Role::PARTICLES에서 읽은 조사입니다");
        let parameter = Parameter {
            name,
            name_span,
            role_span: start.through(self.previous().span),
            spec: ParameterSpec::new(role, Constraint::Any, false),
            default,
        };
        self.finish(mark, SyntaxKind::Parameter);
        Ok(parameter)
    }
}

fn final_word(tail: &[Token]) -> Option<&Token> {
    tail.len().checked_sub(2).map(|index| &tail[index])
}

fn declares_function(tail: &[Token]) -> bool {
    matches!(tail, [.., verb, word, _] if word.word("법") && has_form(verb, Form::Definition))
}

fn misplaced_header(tail: &[Token]) -> Result<()> {
    let [.., verb, word, last] = tail else {
        return Ok(());
    };
    if declares_function(tail) {
        return Err(Diagnostic::new(
            "E-SYN-006",
            last.span,
            "함수 선언은 콜론으로 끝내세요.",
        ));
    }
    if word.word("값이다") && has_form(verb, Form::Nominal) {
        return Err(Diagnostic::new(
            "E-SYN-003",
            verb.span,
            "명사화 호출을 괄호로 감싸세요.",
        ));
    }
    Ok(())
}

pub(crate) mod record;
mod suggestions;

use std::collections::{HashMap, HashSet};

use crate::diagnostic::{Diagnostic, Result, Span, code, internal, type_error};
use crate::korean::particle::{OBJECT, SUBJECT, TOPIC, TOWARD};
use crate::korean::verb::existential;
use crate::runtime::value::Constant;
use crate::stdlib::{self, Native};
use crate::syntax::ast::*;
use suggestions::closest;

#[derive(Debug)]
pub(crate) struct Errors(pub Vec<Diagnostic>);

impl From<Diagnostic> for Errors {
    fn from(diagnostic: Diagnostic) -> Self {
        Self(vec![diagnostic])
    }
}

type Checked = std::result::Result<(), Errors>;

fn both(first: Checked, second: Checked) -> Checked {
    match (first, second) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(errors), Ok(())) | (Ok(()), Err(errors)) => Err(errors),
        (Err(Errors(mut first)), Err(Errors(second))) => {
            first.extend(second);
            Err(Errors(first))
        }
    }
}

#[derive(Clone)]
struct Binding {
    mutable: bool,
    signature: Option<Vec<SignatureParameter>>,
    fields: Option<RecordFields>,
    fixed: &'static str,
    lemmas: Vec<String>,
    class: Option<WordClass>,
    pending: bool,
    definition: Option<Span>,
}

impl Binding {
    fn dynamic(fixed: Option<&'static str>) -> Self {
        Self {
            mutable: fixed.is_none(),
            signature: None,
            fields: None,
            fixed: fixed.unwrap_or("상수"),
            lemmas: Vec::new(),
            class: None,
            pending: false,
            definition: None,
        }
    }

    fn called(mut self, lemmas: Vec<String>, class: Option<WordClass>) -> Self {
        self.lemmas = lemmas;
        self.class = class;
        self
    }
}

type Scope = HashMap<String, Binding>;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum FunctionId {
    Definition(usize),
    Native(Native),
}

fn function_writes(statements: &[Statement]) -> HashSet<FunctionId> {
    fn collect<'a>(
        statements: &'a [Statement],
        scopes: &mut Vec<HashMap<&'a str, Option<FunctionId>>>,
        parameters: Vec<&'a str>,
        writes: &mut HashSet<FunctionId>,
    ) {
        let mut scope: HashMap<_, _> = parameters.into_iter().map(|name| (name, None)).collect();
        let statements = Statement::flattened(statements);
        for statement in &statements {
            if let StatementKind::Import(import) = &statement.kind {
                for (name, _) in &import.names {
                    scope.insert(name, None);
                }
            } else if let Some(declared) = statement.declaration() {
                let function = matches!(statement.kind, StatementKind::Function(_));
                let id = function.then_some(FunctionId::Definition(declared.span.start));
                if let StatementKind::Function(f) = &statement.kind {
                    for lemma in &f.lemmas {
                        scope.insert(lemma, id);
                    }
                }
                scope.insert(declared.name, id);
            }
        }
        scopes.push(scope);
        for statement in statements {
            match &statement.kind {
                StatementKind::Assign { target, .. } => {
                    if let Some((name, _)) = target.as_name() {
                        if let Some(Some(id)) = scopes.iter().rev().find_map(|s| s.get(name)) {
                            writes.insert(*id);
                        }
                    }
                }
                StatementKind::Function(f) => collect(
                    &f.body,
                    scopes,
                    f.parameters.iter().map(|p| p.name.as_str()).collect(),
                    writes,
                ),
                StatementKind::Try(block) => {
                    collect(&block.body, scopes, Vec::new(), writes);
                    if let Some(handler) = &block.handler {
                        collect(&handler.body, scopes, vec![&handler.name], writes);
                    }
                    collect(&block.cleanup, scopes, Vec::new(), writes);
                }
                StatementKind::If {
                    body, otherwise, ..
                } => {
                    collect(body, scopes, Vec::new(), writes);
                    collect(otherwise, scopes, Vec::new(), writes);
                }
                StatementKind::While { body, .. } => collect(body, scopes, Vec::new(), writes),
                StatementKind::For { name, body, .. } => {
                    collect(body, scopes, vec![name], writes);
                }
                _ => {}
            }
        }
        scopes.pop();
    }
    let prelude = Native::ALL
        .map(|native| (native.name(), Some(FunctionId::Native(native))))
        .into();
    let mut writes = HashSet::new();
    collect(statements, &mut vec![prelude], Vec::new(), &mut writes);
    writes
}

pub(crate) fn prepare_program(statements: &mut [Statement]) -> Checked {
    check_placement(statements)?;
    require_origin(statements, false)?;
    prepare_module(statements)
}

fn require_origin(statements: &[Statement], repl: bool) -> Result<()> {
    for statement in statements {
        if matches!(statement.kind, StatementKind::Import(_)) {
            return Err(Diagnostic::new(
                "E-MODULE-007",
                statement.span,
                if repl {
                    "대화형 입력에서는 가져오기를 사용할 수 없습니다. 파일로 저장해 실행하세요."
                } else {
                    "파일 경로가 없어 가져올 파일을 찾을 수 없습니다. 파일로 저장해 검사하거나 실행하세요."
                },
            ));
        }
    }
    Ok(())
}

pub(crate) fn prepare_module(statements: &mut [Statement]) -> Checked {
    let writes = function_writes(statements);
    let mut prelude: Scope = Native::ALL
        .into_iter()
        .map(|native| {
            let binding = Binding {
                signature: (!writes.contains(&FunctionId::Native(native)))
                    .then(|| plain_signature(native.signature())),
                ..Binding::dynamic(None).called(vec![native.name().to_owned()], None)
            };
            (native.name().to_owned(), binding)
        })
        .collect();
    for name in stdlib::CONSTANTS {
        prelude.insert(name.to_owned(), Binding::dynamic(Some("상수")));
    }
    let mut scopes = vec![prelude];
    block(statements, &mut scopes, Vec::new(), &writes, false)
}

#[derive(Clone)]
pub(crate) struct ReplName {
    fixed: Option<&'static str>,
    lemmas: Vec<String>,
    class: Option<WordClass>,
}

pub(crate) type ReplNames = HashMap<String, ReplName>;

pub(crate) fn repl_names() -> ReplNames {
    let mut names: ReplNames = stdlib::prelude()
        .map(|(name, _)| {
            let lemmas = verb_lemmas(name);
            (
                name.to_owned(),
                ReplName {
                    fixed: None,
                    lemmas,
                    class: None,
                },
            )
        })
        .collect();
    for name in stdlib::CONSTANTS {
        names.insert(
            name.to_owned(),
            ReplName {
                fixed: Some("상수"),
                lemmas: Vec::new(),
                class: None,
            },
        );
    }
    names
}

fn repl_scope(names: &ReplNames) -> Scope {
    names
        .iter()
        .map(|(name, known)| {
            let binding = Binding::dynamic(known.fixed).called(known.lemmas.clone(), known.class);
            (name.clone(), binding)
        })
        .collect()
}

pub(crate) fn prepare_repl(
    statements: &mut [Statement],
    previous: &ReplNames,
) -> Result<ReplNames> {
    check_placement(statements)?;
    require_origin(statements, true)?;
    let mut names = previous.clone();
    for Declaration {
        name,
        span,
        fixed,
        lemmas,
        class,
    } in statements.iter().flat_map(Statement::declarations)
    {
        if let Some(what) = previous.get(name).and_then(|known| known.fixed) {
            let topic = TOPIC.after(name);
            return Err(Diagnostic::new(
                "E-BIND-002",
                span,
                format!("{what} '{name}'{topic} 대화형 입력에서도 다시 정의할 수 없습니다."),
            ));
        }
        names.insert(
            name.to_owned(),
            ReplName {
                fixed,
                lemmas,
                class,
            },
        );
    }
    block(
        statements,
        &mut vec![repl_scope(previous)],
        Vec::new(),
        &HashSet::new(),
        true,
    )
    .map_err(|Errors(errors)| {
        errors
            .into_iter()
            .next()
            .expect("오류가 하나 이상 있습니다")
    })?;
    Ok(names)
}

pub(crate) fn prepare_repl_expression(value: &mut Expr, names: &ReplNames) -> Result<()> {
    expression(value, &[repl_scope(names)])
}

pub(crate) fn check_placement(statements: &[Statement]) -> Result<()> {
    placement(statements, false, 0, true)
}

fn placement(statements: &[Statement], function: bool, loops: usize, top: bool) -> Result<()> {
    let mut imports_closed = false;
    for statement in statements {
        let import = matches!(statement.kind, StatementKind::Import(_));
        if !top && import {
            return Err(Diagnostic::new(
                "E-MODULE-004",
                statement.span,
                "가져오기는 파일 최상위에서만 쓸 수 있습니다.",
            ));
        }
        if import && imports_closed {
            return Err(Diagnostic::new(
                "E-MODULE-005",
                statement.span,
                "모듈 가져오기는 다른 문장보다 먼저 모아서 씁니다.",
            ));
        }
        imports_closed |= !import;
        match &statement.kind {
            StatementKind::Return(_) if !function => {
                return Err(Diagnostic::new(
                    "E-CONTEXT-001",
                    statement.span,
                    "반환문은 함수 안에서만 사용할 수 있습니다.",
                ));
            }
            StatementKind::Break | StatementKind::Continue if loops == 0 => {
                return Err(Diagnostic::new(
                    "E-CONTEXT-002",
                    statement.span,
                    "'반복을 중단한다'와 '다음으로 진행한다'는 같은 함수 안의 반복문에서만 씁니다.",
                ));
            }
            StatementKind::Function(f) => placement(&f.body, true, 0, false)?,
            StatementKind::Try(block) => {
                placement(&block.body, function, loops, false)?;
                if let Some(handler) = &block.handler {
                    placement(&handler.body, function, loops, false)?;
                }
                placement(&block.cleanup, function, loops, false)?;
            }
            StatementKind::While { body, .. } | StatementKind::For { body, .. } => {
                placement(body, function, loops + 1, false)?
            }
            StatementKind::If {
                body, otherwise, ..
            } => {
                placement(body, function, loops, false)?;
                placement(otherwise, function, loops, false)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn declare(scope: &mut Scope, name: &str, span: Span, binding: Binding) -> Result<()> {
    if scope.contains_key(name) {
        return Err(Diagnostic::new(
            "E-BIND-001",
            span,
            format!("같은 범위에 '{name}' 이름이 이미 정의되어 있습니다."),
        ));
    }
    scope.insert(
        name.to_owned(),
        Binding {
            definition: Some(span),
            ..binding
        },
    );
    Ok(())
}

fn lookup<'a>(scopes: &'a [Scope], name: &str, span: Span) -> Result<&'a Binding> {
    let binding = scopes
        .iter()
        .rev()
        .find_map(|s| s.get(name))
        .ok_or_else(|| {
            let known = scopes
                .iter()
                .flat_map(|scope| scope.keys().map(String::as_str));
            let word = closest(name, known);
            let diagnostic = suggesting(unresolved(name, span), word);
            match word {
                Some(word) => diagnostic.with_fix(span, word),
                None => diagnostic,
            }
        })?;
    defined(name, binding, span)
}

fn unresolved(name: &str, span: Span) -> Diagnostic {
    Diagnostic::new(
        code::UNRESOLVED,
        span,
        format!("'{name}' 이름을 찾을 수 없습니다."),
    )
}

fn suggesting(diagnostic: Diagnostic, word: Option<&str>) -> Diagnostic {
    match word {
        Some(word) => {
            let object = OBJECT.after(word);
            diagnostic.with_help(format!("혹시 '{word}'{object} 쓰려고 했나요?"))
        }
        None => diagnostic,
    }
}

fn defined<'a>(name: &str, binding: &'a Binding, span: Span) -> Result<&'a Binding> {
    if binding.pending {
        return Err(Diagnostic::new(
            "E-INIT-001",
            span,
            format!(
                "'{name}'{} 정의하기 전에 쓸 수 없습니다.",
                TOPIC.after(name)
            ),
        ));
    }
    Ok(binding)
}

fn lookup_verb<'a>(
    scopes: &'a [Scope],
    word: &str,
    lemmas: &[String],
    class: Option<WordClass>,
    span: Span,
) -> Result<(String, &'a Binding)> {
    let existential = lemmas
        .iter()
        .filter_map(|lemma| lemma.strip_suffix('다'))
        .any(existential);
    for scope in scopes.iter().rev() {
        let (mut found, other): (Vec<_>, Vec<_>) = scope
            .iter()
            .filter(|(_, binding)| binding.lemmas.iter().any(|lemma| lemmas.contains(lemma)))
            .partition(|(_, binding)| {
                existential || class.is_none() || binding.class.is_none_or(|c| Some(c) == class)
            });
        found.sort_by(|a, b| a.0.cmp(b.0));
        match found.as_slice() {
            [] => match other.first() {
                Some((name, binding)) => {
                    return Err(class_mismatch(name, binding.class, word, span));
                }
                None => continue,
            },
            [(name, binding)] => return Ok(((*name).clone(), defined(name, binding, span)?)),
            several => {
                let names: Vec<_> = several.iter().map(|(name, _)| name.as_str()).collect();
                return Err(Diagnostic::new(
                    "E-CALL-009",
                    span,
                    format!(
                        "'{word}'{} 어느 동사인지 알 수 없습니다(후보: {}). 정의 하나의 이름을 바꾸세요.",
                        SUBJECT.after(word),
                        names.join(", ")
                    ),
                ));
            }
        }
    }
    let kind = class_name(class);
    let known = || {
        scopes.iter().flat_map(|scope| {
            scope
                .values()
                .flat_map(|b| b.lemmas.iter().map(String::as_str))
        })
    };
    let suggestion = lemmas.iter().find_map(|lemma| closest(lemma, known()));
    let unknown = Diagnostic::new(
        code::UNRESOLVED,
        span,
        format!("'{word}'에 맞는 {kind}가 정의되어 있지 않습니다."),
    );
    Err(suggesting(unknown, suggestion))
}

fn class_name(class: Option<WordClass>) -> &'static str {
    class.map_or("동사", WordClass::name)
}

fn class_mismatch(name: &str, class: Option<WordClass>, word: &str, span: Span) -> Diagnostic {
    let class = class_name(class);
    Diagnostic::new(
        "E-FORM-003",
        span,
        format!(
            "'{name}'{} {class}로 정의되어 있어 '{word}'처럼 활용할 수 없습니다.",
            TOPIC.after(name)
        ),
    )
}

fn readable(reading: &Expr, scopes: &[Scope]) -> Result<()> {
    if let ExprKind::Chain(chain) = &reading.kind {
        if let Some(Callee::Verb {
            word,
            lemmas,
            class,
            ..
        }) = chain.calls.last().map(|c| &c.callee)
        {
            return lookup_verb(scopes, word, lemmas, *class, reading.span).map(|_| ());
        }
    }
    record::muted(|| expression(&mut reading.clone(), scopes))
}

fn reading_name(reading: &Expr, scopes: &[Scope]) -> String {
    let named = |kind: &str, name: &str| format!("{kind} '{name}'{}", TOWARD.after(name));
    match &reading.kind {
        ExprKind::Name(name) => named("이름", name),
        ExprKind::Chain(chain) => match chain.calls.last().map(|c| &c.callee) {
            Some(Callee::Verb { lemmas, .. }) => named("동사", &lemmas[0]),
            _ => "함수 호출로".to_owned(),
        },
        ExprKind::Compare(..) => "값 비교로".to_owned(),
        ExprKind::Either(..) => {
            let mut chosen = reading.clone();
            match record::muted(|| expression(&mut chosen, scopes)) {
                Ok(()) => reading_name(&chosen, scopes),
                Err(_) => "값으로".to_owned(),
            }
        }
        _ => "값으로".to_owned(),
    }
}

fn block(
    statements: &mut [Statement],
    scopes: &mut Vec<Scope>,
    parameters: Vec<(String, Span)>,
    writes: &HashSet<FunctionId>,
    dynamic: bool,
) -> Checked {
    filtered_block(statements, None, scopes, parameters, writes, dynamic)
}

fn filtered_block(
    statements: &mut [Statement],
    filter: Option<&mut Expr>,
    scopes: &mut Vec<Scope>,
    parameters: Vec<(String, Span)>,
    writes: &HashSet<FunctionId>,
    dynamic: bool,
) -> Checked {
    let mut errors = Vec::new();
    let mut scope = Scope::new();
    let binder = parameters.first().cloned();
    let top = scopes.len() == 1;
    record::enter_block(top, &parameters, statements);
    for (name, span) in parameters {
        record::definition(&name, span, record::Kind::Parameter, &[], None, None);
        errors.extend(declare(&mut scope, &name, span, Binding::dynamic(None)).err());
    }
    for statement in statements.iter_mut() {
        errors.extend(declare_statement(&mut scope, statement, writes, dynamic).err());
    }
    scopes.push(scope);
    if let Some(test) = filter {
        if let Some((name, span)) = binder {
            fill_gap(test, &name, span, scopes);
        }
        errors.extend(expression(test, scopes).err());
    }
    for statement in statements {
        if let Err(Errors(found)) = check_statement(statement, scopes, writes, dynamic) {
            errors.extend(found);
        }
        for declared in statement.declarations() {
            let scope = scopes.last_mut().expect("위에서 넣었습니다");
            if let Some(binding) = scope.get_mut(declared.name) {
                binding.pending = false;
            }
        }
    }
    scopes.pop();
    record::leave_block();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Errors(errors))
    }
}

fn missing<'a>(
    signature: &'a [SignatureParameter],
    arguments: &'a [Argument],
) -> impl Iterator<Item = &'a SignatureParameter> {
    signature
        .iter()
        .filter(|p| !p.spec.optional && arguments.iter().all(|a| a.role != p.spec.role))
}

fn fill_gap(test: &mut Expr, binder: &str, binder_span: Span, scopes: &[Scope]) {
    let chain = match &mut test.kind {
        ExprKind::Either(first, reading) => {
            fill_gap(first, binder, binder_span, scopes);
            return fill_gap(reading, binder, binder_span, scopes);
        }
        ExprKind::Binary(BinOp::And | BinOp::Or, _, left, right) => {
            fill_gap(left, binder, binder_span, scopes);
            return fill_gap(right, binder, binder_span, scopes);
        }
        ExprKind::Unary(UnaryOp::Not, _, clause) | ExprKind::Group(clause) => {
            return fill_gap(clause, binder, binder_span, scopes);
        }
        ExprKind::Compare(values, _) => return owner(&mut values[0], binder, binder_span, scopes),
        ExprKind::Chain(chain) => chain,
        _ => return,
    };
    if let [call] = chain.calls.as_mut_slice() {
        for argument in &mut call.arguments {
            if argument.role == Role::Subject {
                owner(&mut argument.value, binder, binder_span, scopes);
            }
        }
    }
    let [call] = chain.calls.as_mut_slice() else {
        return;
    };
    let Callee::Verb {
        word,
        lemmas,
        class,
        ..
    } = &call.callee
    else {
        return;
    };
    let Ok((_, binding)) = lookup_verb(scopes, word, lemmas, *class, call.span) else {
        return;
    };
    let Some(signature) = &binding.signature else {
        return;
    };
    let gaps: Vec<Role> = missing(signature, &call.arguments)
        .map(|p| p.spec.role)
        .collect();
    if let [role] = gaps[..] {
        call.arguments.push(Argument {
            role,
            value: Expr::leaf(ExprKind::Name(binder.to_owned()), binder_span),
            span: binder_span,
        });
    }
}

fn owner(subject: &mut Expr, binder: &str, binder_span: Span, scopes: &[Scope]) {
    let ExprKind::Name(name) = &subject.kind else {
        return;
    };
    if name == binder || scopes.iter().any(|scope| scope.contains_key(name)) {
        return;
    }
    let head = Expr::leaf(ExprKind::Name(binder.to_owned()), binder_span);
    let kind = ExprKind::Attribute(Box::new(head), name.clone());
    if let Ok(attribute) = Expr::bounded(kind, subject.span, 2) {
        *subject = attribute;
    }
}

fn spelled_out(
    lemmas: &mut Vec<String>,
    arguments: &mut Vec<Argument>,
    class: Option<WordClass>,
    scopes: &[Scope],
) -> Option<Span> {
    for index in 0..arguments.len() {
        let Some((noun, _)) = arguments[index].value.as_name() else {
            continue;
        };
        let role = arguments[index].role;
        let phrases: Vec<String> = lemmas.iter().map(|l| phrase(noun, role, l)).collect();
        if lookup_verb(scopes, noun, &phrases, class, arguments[index].span).is_ok() {
            *lemmas = phrases;
            return Some(arguments.remove(index).value.span);
        }
    }
    None
}

fn declare_statement(
    scope: &mut Scope,
    statement: &mut Statement,
    writes: &HashSet<FunctionId>,
    dynamic: bool,
) -> Result<()> {
    if let StatementKind::Concurrent(_, body) = &mut statement.kind {
        for statement in body {
            declare_statement(scope, statement, writes, dynamic)?;
        }
        return Ok(());
    }
    if let StatementKind::Import(import) = &mut statement.kind {
        for (name, span) in &mut import.names {
            let found = import.exports.get_key_value(name.as_str()).or_else(|| {
                import
                    .exports
                    .iter()
                    .find(|(_, export)| export.lemmas.contains(name))
            });
            let Some((exported, export)) = found else {
                return Err(Diagnostic::new(
                    "E-MODULE-006",
                    *span,
                    format!("모듈 \"{}\"에 '{name}' 이름이 없습니다.", import.path),
                ));
            };
            let binding = Binding {
                signature: export.signature.clone(),
                fields: export.fields.clone(),
                ..Binding::dynamic(Some("가져온 이름")).called(export.lemmas.clone(), export.class)
            };
            *name = exported.clone();
            record::definition(
                name,
                *span,
                record::Kind::Imported,
                &binding.lemmas,
                binding.class,
                binding.signature.as_deref(),
            );
            declare(scope, name, *span, binding)?;
        }
        return Ok(());
    }
    let Some(Declaration {
        name,
        span,
        fixed,
        lemmas,
        class,
    }) = statement.declaration()
    else {
        return Ok(());
    };
    let mut binding = Binding {
        pending: true,
        ..Binding::dynamic(fixed).called(lemmas, class)
    };
    match &statement.kind {
        StatementKind::Function(f)
            if !dynamic && !writes.contains(&FunctionId::Definition(span.start)) =>
        {
            binding.signature = Some(f.parameters.iter().map(Parameter::signature).collect());
        }
        StatementKind::Record(record) => binding.fields = Some(record_fields(record)),
        _ => {}
    }
    if let StatementKind::Function(Function {
        fixed: Some((_, span)),
        ..
    }) = &statement.kind
    {
        record::fixed(*span);
    }
    let kind = match &statement.kind {
        StatementKind::Function(_) => record::Kind::Function,
        StatementKind::Record(_) => record::Kind::Record,
        _ if fixed.is_some() => record::Kind::Constant,
        _ => record::Kind::Variable,
    };
    record::definition(
        name,
        span,
        kind,
        &binding.lemmas,
        binding.class,
        binding.signature.as_deref(),
    );
    declare(scope, name, span, binding)
}

fn check_statement(
    statement: &mut Statement,
    scopes: &mut Vec<Scope>,
    writes: &HashSet<FunctionId>,
    dynamic: bool,
) -> Checked {
    match &mut statement.kind {
        StatementKind::Bind { value, .. }
        | StatementKind::Return(value)
        | StatementKind::Throw(value)
        | StatementKind::Exit(Some(value)) => Ok(expression(value, scopes)?),
        StatementKind::Record(record) => {
            unique_fields(record.fields.iter().map(|f| (f.name.as_str(), f.span)))?;
            for default in record.fields.iter_mut().filter_map(|f| f.default.as_mut()) {
                expression(default, scopes)?;
            }
            Ok(())
        }
        StatementKind::Assign {
            target,
            target_span,
            value,
            ..
        } => Ok(assignment(target, *target_span, value, scopes)?),
        StatementKind::Chain(chain) => Ok(check_chain(chain, scopes)?),
        StatementKind::Function(f) => function(f, scopes, writes, dynamic),
        StatementKind::Try(attempt) => {
            let mut checked = block(&mut attempt.body, scopes, Vec::new(), writes, dynamic);
            if let Some(handler) = &mut attempt.handler {
                let error = vec![(handler.name.clone(), handler.name_span)];
                let handled = block(&mut handler.body, scopes, error, writes, dynamic);
                checked = both(checked, handled);
            }
            let cleanup = block(&mut attempt.cleanup, scopes, Vec::new(), writes, dynamic);
            both(checked, cleanup)
        }
        StatementKind::If {
            test,
            body,
            otherwise,
            ..
        } => {
            let test = expression(test, scopes).map_err(Errors::from);
            let body = both(test, block(body, scopes, Vec::new(), writes, dynamic));
            both(body, block(otherwise, scopes, Vec::new(), writes, dynamic))
        }
        StatementKind::While { test, body, .. } => {
            let test = expression(test, scopes).map_err(Errors::from);
            both(test, block(body, scopes, Vec::new(), writes, dynamic))
        }
        StatementKind::Concurrent(_, body) => {
            let mut checked = Ok(());
            for statement in body {
                let flow = match stays_inside(std::slice::from_ref(statement), false) {
                    Ok(()) => check_statement(statement, scopes, writes, dynamic),
                    Err(error) => Err(error.into()),
                };
                checked = both(checked, flow);
            }
            checked
        }
        StatementKind::For {
            name,
            name_span,
            second,
            collection,
            last,
            filter,
            body,
            concurrency,
        } => {
            if *concurrency != Concurrency::Sequential {
                stays_inside(body, false)?;
            }
            let mut header = expression(collection, scopes).map_err(Errors::from);
            if let Some((last, _)) = last.as_deref_mut() {
                header = both(header, expression(last, scopes).map_err(Errors::from));
            }
            let body = filtered_block(
                body,
                filter.as_mut().map(|(test, _)| test),
                scopes,
                std::iter::once((name.clone(), *name_span))
                    .chain(second.clone())
                    .collect(),
                writes,
                dynamic,
            );
            both(header, body)
        }
        StatementKind::Import(_)
        | StatementKind::Break
        | StatementKind::Continue
        | StatementKind::Exit(None)
        | StatementKind::Rethrow => Ok(()),
    }
}

fn stays_inside(statements: &[Statement], in_loop: bool) -> Result<()> {
    for statement in statements {
        let escapes = match &statement.kind {
            StatementKind::Return(_) => true,
            StatementKind::Break | StatementKind::Continue => !in_loop,
            _ => false,
        };
        if escapes {
            return Err(Diagnostic::new(
                "E-SYN-017",
                statement.span,
                "'함께'나 '병렬로' 블록 안에서는 반환하거나 바깥 반복을 멈출 수 없습니다.",
            ));
        }
        match &statement.kind {
            StatementKind::While { body, .. } | StatementKind::For { body, .. } => {
                stays_inside(body, true)?;
            }
            StatementKind::If {
                body, otherwise, ..
            } => {
                stays_inside(body, in_loop)?;
                stays_inside(otherwise, in_loop)?;
            }
            StatementKind::Try(attempt) => {
                stays_inside(&attempt.body, in_loop)?;
                if let Some(handler) = &attempt.handler {
                    stays_inside(&handler.body, in_loop)?;
                }
                stays_inside(&attempt.cleanup, in_loop)?;
            }
            StatementKind::Concurrent(_, body) => stays_inside(body, in_loop)?,
            _ => {}
        }
    }
    Ok(())
}

fn assignment(
    target: &mut Expr,
    target_span: Span,
    value: &mut Expr,
    scopes: &[Scope],
) -> Result<()> {
    expression(target, scopes)?;
    if let Some((name, span)) = target.as_name() {
        let binding = lookup(scopes, name, span)?;
        if !binding.mutable {
            let what = binding.fixed;
            return Err(Diagnostic::new(
                "E-BIND-002",
                target_span,
                format!("{what} '{name}'의 값은 바꿀 수 없습니다."),
            ));
        }
    }
    expression(value, scopes)
}

fn function(
    f: &mut Function,
    scopes: &mut Vec<Scope>,
    writes: &HashSet<FunctionId>,
    dynamic: bool,
) -> Checked {
    let mut roles = HashSet::new();
    for parameter in &f.parameters {
        if !roles.insert(parameter.spec.role) {
            return Err(Diagnostic::new(
                "E-CALL-001",
                parameter.role_span,
                "매개변수 역할이 중복되었습니다.",
            )
            .into());
        }
    }
    for parameter in &mut f.parameters {
        if let Some(default) = &mut parameter.default {
            expression(default, scopes)?;
        }
    }
    let parameters = f
        .parameters
        .iter()
        .map(|p| (p.name.clone(), p.name_span))
        .collect();
    let mut lifted = Vec::new();
    for (depth, scope) in scopes.iter_mut().enumerate() {
        for (name, binding) in scope.iter_mut().filter(|(_, b)| b.pending) {
            binding.pending = false;
            lifted.push((depth, name.clone()));
        }
    }
    let mut result = block(&mut f.body, scopes, parameters, writes, dynamic);
    for (depth, name) in lifted {
        if let Some(binding) = scopes[depth].get_mut(&name) {
            binding.pending = true;
        }
    }
    if let (Err(Errors(errors)), Some((noun, span))) = (&mut result, &f.fixed) {
        let is_noun = |error: &Diagnostic| {
            error.code == code::UNRESOLVED && error.message == unresolved(noun, error.span).message
        };
        if errors.iter().any(is_noun) {
            errors.retain(|error| !is_noun(error));
            errors.insert(
                0,
                Diagnostic::new(
                    "E-SYN-007",
                    *span,
                    format!("매개변수 이름을 역따옴표로 감싸세요: `{noun}`"),
                ),
            );
        }
    }
    result
}

fn expression(expr: &mut Expr, scopes: &[Scope]) -> Result<()> {
    let span = expr.span;
    match &mut expr.kind {
        ExprKind::Name(name) => {
            if lookup(scopes, name, expr.name_span).is_err() && name.ends_with('다') {
                let lemmas = vec![name.clone()];
                let (found, binding) = lookup_verb(scopes, name, &lemmas, None, expr.name_span)?;
                record::usage(&found, expr.name_span, binding.definition);
                *name = found;
            } else {
                let binding = lookup(scopes, name, expr.name_span)?;
                record::usage(name, expr.name_span, binding.definition);
            }
        }
        ExprKind::Either(first, second) => {
            if let (ExprKind::Attribute(owner, _), ExprKind::Attribute(..)) =
                (&mut first.kind, &second.kind)
            {
                return expression(owner, scopes);
            }
            let chosen = match (readable(first, scopes), readable(second, scopes)) {
                (Ok(()), Ok(())) => {
                    return Err(Diagnostic::new(
                        "E-RESOLVE-002",
                        span,
                        format!(
                            "{}도 {}도 읽힙니다. 이름 하나를 바꾸세요.",
                            reading_name(first, scopes),
                            reading_name(second, scopes)
                        ),
                    ));
                }
                (Ok(()), Err(_)) => first,
                (Err(_), Ok(())) => second,
                (Err(first), Err(second)) => {
                    let unknown = first.code == code::UNRESOLVED;
                    return Err(if unknown { second } else { first });
                }
            };
            expression(chosen, scopes)?;
            let chosen = std::mem::replace(
                chosen.as_mut(),
                Expr::leaf(ExprKind::Tuple(Vec::new()), span),
            );
            *expr = chosen;
        }
        ExprKind::Number(spelling) => {
            expr.kind = ExprKind::Literal(Constant::number(spelling, expr.span)?);
        }
        ExprKind::Record(record) => {
            let binding = lookup(scopes, &record.type_name, record.type_span)?;
            record::usage(&record.type_name, record.type_span, binding.definition);
            unique_fields(
                record
                    .fields
                    .iter()
                    .map(|(name, span, _)| (name.as_str(), *span)),
            )?;
            if let Some(fields) = &binding.fields {
                record_literal(record, fields, span)?;
            }
            for (_, _, value) in &mut record.fields {
                expression(value, scopes)?;
            }
        }
        ExprKind::Unary(op, op_span, inner) => {
            if *op == UnaryOp::Not {
                record::category(*op_span, Category::Word);
            }
            expression(inner, scopes)?
        }
        ExprKind::Attribute(inner, _) | ExprKind::Group(inner) => expression(inner, scopes)?,
        ExprKind::Binary(op, op_span, left, right) => {
            if matches!(op, BinOp::And | BinOp::Or) {
                record::category(*op_span, Category::Word);
            }
            expression(left, scopes)?;
            expression(right, scopes)?;
        }
        ExprKind::Compare(values, ops) => {
            for (op, op_span) in ops.iter() {
                // `X 중 Y가 있으면`의 비교 대상 0은 동사 자리를 그대로 씁니다(clause.rs의 existence).
                match values.last().filter(|zero| zero.span == *op_span) {
                    Some(_) => {
                        let lemma = if *op == CmpOp::Gt { "있다" } else { "없다" };
                        record::usage(lemma, *op_span, None);
                    }
                    None => record::category(*op_span, Category::Word),
                }
            }
            for value in values {
                expression(value, scopes)?;
            }
        }
        ExprKind::Index(left, right, _) | ExprKind::Range(left, right, _) => {
            expression(left, scopes)?;
            expression(right, scopes)?;
        }
        ExprKind::Choice(parts) => {
            for part in parts.iter_mut() {
                expression(part, scopes)?;
            }
        }
        ExprKind::List(values) | ExprKind::Tuple(values) | ExprKind::Set(values) => {
            for value in values {
                expression(value, scopes)?;
            }
        }
        ExprKind::Slice(owner, bounds, _) => {
            expression(owner, scopes)?;
            for bound in bounds.iter_mut().flatten() {
                expression(bound, scopes)?;
            }
        }
        ExprKind::Dict(entries) => {
            for (key, value) in entries {
                expression(key, scopes)?;
                expression(value, scopes)?;
            }
        }
        ExprKind::Collect(collect) => {
            expression(&mut collect.collection, scopes)?;
            let mut scope = Scope::new();
            declare(
                &mut scope,
                &collect.name,
                collect.name_span,
                Binding::dynamic(None),
            )?;
            let mut inner = scopes.to_vec();
            inner.push(scope);
            if let Some(filter) = &mut collect.filter {
                fill_gap(filter, &collect.name, collect.name_span, &inner);
                expression(filter, &inner)?;
            }
            if let Some(value) = &mut collect.value {
                expression(value, &inner)?;
            }
        }
        ExprKind::Template(parts) => {
            for part in parts {
                if let Part::Value(value, _) = part {
                    expression(value, scopes)?;
                }
            }
        }
        ExprKind::Chain(chain) => check_chain(chain, scopes)?,
        ExprKind::Literal(_) => {}
    }
    Ok(())
}

fn check_chain(chain: &mut Chain, scopes: &[Scope]) -> Result<()> {
    let implied = std::mem::take(&mut chain.implied);
    let single = chain.calls.len() == 1;
    for (i, call) in chain.calls.iter_mut().enumerate() {
        let signature = match &mut call.callee {
            Callee::Verb {
                word,
                lemmas,
                name,
                class,
            } => {
                let fixed = spelled_out(lemmas, &mut call.arguments, *class, scopes);
                let (found, binding) = lookup_verb(scopes, word, lemmas, *class, call.span)?;
                record::usage(&found, call.span, binding.definition);
                if let Some(span) = fixed {
                    record::fixed(span);
                }
                *name = found;
                binding.signature.clone()
            }
            Callee::Invoke(value) => {
                expression(value, scopes)?;
                value
                    .as_name()
                    .map(|(name, span)| {
                        lookup(scopes, name, span).map(|binding| binding.signature.clone())
                    })
                    .transpose()?
                    .flatten()
            }
        };
        for shared in implied.iter().filter(|_| single) {
            let needed = signature
                .as_deref()
                .is_some_and(|s| missing(s, &call.arguments).any(|p| p.spec.role == shared.role));
            if needed {
                call.arguments.push(shared.clone());
            }
        }
        let mut roles = HashSet::new();
        for arg in &mut call.arguments {
            expression(&mut arg.value, scopes)?;
            if !roles.insert(arg.role) {
                return Err(Diagnostic::new(
                    "E-CALL-001",
                    arg.span,
                    "같은 역할의 인자를 두 번 지정했습니다.",
                ));
            }
        }
        if let Some(signature) = signature.filter(|_| matches!(call.callee, Callee::Verb { .. })) {
            let arguments: Vec<(Role, Span)> = call
                .arguments
                .iter()
                .map(|argument| (argument.role, argument.span))
                .collect();
            let mapping = signature_map(
                &signature,
                &arguments,
                i > 0 && chain.edges[i - 1] == Edge::Pipe,
                call.span,
            )?;
            for (parameter, origin) in signature.iter().zip(mapping.iter()) {
                let ArgumentOrigin::Argument(index) = origin else {
                    continue;
                };
                let value = &call.arguments[index].value;
                if let ExprKind::Literal(literal) = &value.ungrouped().kind {
                    if !parameter.spec.constraint.accepts_constant(literal) {
                        return Err(type_error(value.span));
                    }
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ArgumentOrigin {
    Argument(usize),
    Pipe,
    Default,
}

const MAX_PARAMETERS: usize = 8;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Mapping {
    origins: [u8; MAX_PARAMETERS],
    len: u8,
}

const PIPE: u8 = u8::MAX - 1;
const DEFAULT: u8 = u8::MAX;

impl Mapping {
    pub fn len(&self) -> usize {
        self.len.into()
    }

    pub fn iter(&self) -> impl Iterator<Item = ArgumentOrigin> + '_ {
        self.origins[..self.len()]
            .iter()
            .map(|&origin| match origin {
                PIPE => ArgumentOrigin::Pipe,
                DEFAULT => ArgumentOrigin::Default,
                index => ArgumentOrigin::Argument(index.into()),
            })
    }

    fn set(&mut self, parameter: usize, origin: ArgumentOrigin) {
        self.origins[parameter] = match origin {
            ArgumentOrigin::Argument(index) => index as u8,
            ArgumentOrigin::Pipe => PIPE,
            ArgumentOrigin::Default => DEFAULT,
        };
    }
}

pub(crate) fn signature_map(
    signature: &[SignatureParameter],
    args: &[(Role, Span)],
    incoming: bool,
    span: Span,
) -> Result<Mapping> {
    if signature.len() > MAX_PARAMETERS || args.len() >= usize::from(PIPE) {
        return Err(internal(span));
    }
    let mut mapping = Mapping {
        origins: [DEFAULT; MAX_PARAMETERS],
        len: signature.len() as u8,
    };
    if incoming {
        let index = signature.iter().position(|p| p.spec.input).ok_or_else(|| {
            Diagnostic::new(
                code::NO_PIPE_TARGET,
                span,
                "이 함수에는 앞 절의 결과를 받을 인자가 없습니다.",
            )
        })?;
        if let Some((_, arg_span)) = args
            .iter()
            .find(|(role, _)| *role == signature[index].spec.role)
        {
            return Err(Diagnostic::new(
                "E-CALL-005",
                *arg_span,
                "앞 절의 결과를 받을 인자를 이미 지정했습니다.",
            ));
        }
        mapping.set(index, ArgumentOrigin::Pipe);
    }
    for (index, (role, arg_span)) in args.iter().enumerate() {
        let parameter = signature
            .iter()
            .position(|p| p.spec.role == *role)
            .ok_or_else(|| {
                Diagnostic::new(
                    "E-CALL-002",
                    *arg_span,
                    format!("이 함수는 '{}' 인자를 받지 않습니다.", role.particle()),
                )
            })?;
        mapping.set(parameter, ArgumentOrigin::Argument(index));
    }
    for (parameter, origin) in signature.iter().zip(mapping.iter()) {
        if matches!(origin, ArgumentOrigin::Default) && !parameter.spec.optional {
            return Err(Diagnostic::new(
                "E-CALL-003",
                span,
                format!(
                    "필수 인자가 빠졌습니다: {}",
                    parameter.name.as_deref().map_or_else(
                        || parameter.spec.role.particle().to_owned(),
                        |n| format!("`{n}`")
                    )
                ),
            ));
        }
    }
    Ok(mapping)
}

fn unique_fields<'a>(fields: impl Iterator<Item = (&'a str, Span)>) -> Result<()> {
    let mut seen = HashSet::new();
    for (name, span) in fields {
        if !seen.insert(name) {
            return Err(Diagnostic::new(
                "E-RECORD-003",
                span,
                format!("'{name}' 필드를 두 번 썼습니다."),
            ));
        }
    }
    Ok(())
}

fn record_literal(record: &RecordLiteral, fields: &RecordFields, span: Span) -> Result<()> {
    if let Some((name, field_span, _)) = record
        .fields
        .iter()
        .find(|(name, _, _)| !fields.iter().any(|(field, _)| field == name))
    {
        return Err(Diagnostic::new(
            "E-RECORD-002",
            *field_span,
            format!("'{}'에는 '{name}' 필드가 없습니다.", record.type_name),
        ));
    }
    if let Some((field, _)) = fields.iter().find(|(field, has_default)| {
        !has_default && !record.fields.iter().any(|(name, _, _)| name == field)
    }) {
        return Err(Diagnostic::new(
            "E-RECORD-001",
            span,
            format!("'{field}' 필드의 값이 필요합니다."),
        ));
    }
    Ok(())
}

fn record_fields(record: &RecordType) -> RecordFields {
    record
        .fields
        .iter()
        .map(|field| (field.name.clone(), field.default.is_some()))
        .collect()
}

// 다시 내보내는 이름이 원래 정보를 유지하도록 가져오기를 먼저 연결해야 합니다.
pub(crate) fn module_exports(statements: &[Statement]) -> Exports {
    let writes = function_writes(statements);
    Statement::flattened(statements)
        .into_iter()
        .flat_map(|statement| match &statement.kind {
            StatementKind::Bind { name, .. } => vec![(
                name.clone(),
                Export {
                    lemmas: verb_lemmas(name),
                    ..Export::default()
                },
            )],
            StatementKind::Record(record) => vec![(
                record.name.clone(),
                Export {
                    fields: Some(record_fields(record)),
                    ..Export::default()
                },
            )],
            StatementKind::Function(f) => {
                let fixed = !writes.contains(&FunctionId::Definition(f.name_span.start));
                let signature =
                    fixed.then(|| f.parameters.iter().map(Parameter::signature).collect());
                vec![(
                    f.name.clone(),
                    Export {
                        signature,
                        fields: None,
                        lemmas: f.lemmas.clone(),
                        class: f.class,
                    },
                )]
            }
            StatementKind::Import(import) => import
                .names
                .iter()
                .map(|(name, _)| {
                    let export = import.exports.get(name).cloned().unwrap_or_default();
                    (name.clone(), export)
                })
                .collect(),
            _ => vec![],
        })
        .collect()
}

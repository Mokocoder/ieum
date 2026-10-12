use std::cell::RefCell;

use crate::diagnostic::Span;
use crate::syntax::ast::{Category, SignatureParameter, Statement, WordClass};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Variable,
    Constant,
    Parameter,
    Function,
    Record,
    Imported,
}

pub(crate) struct Definition {
    pub name: String,
    pub span: Span,
    pub kind: Kind,
    pub scope: Option<Span>,
    pub lemmas: Vec<String>,
    pub adjective: bool,
    pub particles: Vec<&'static str>,
}

pub(crate) struct Usage {
    pub name: String,
    pub span: Span,
    pub definition: Option<Span>,
}

#[derive(Default)]
pub(crate) struct Record {
    pub definitions: Vec<Definition>,
    pub usages: Vec<Usage>,
    pub fixed: Vec<Span>,
    pub categories: Vec<(Span, Category)>,
    blocks: Vec<Option<Span>>,
    muted: usize,
}

thread_local! {
    static RECORD: RefCell<Option<Record>> = const { RefCell::new(None) };
}

fn with(f: impl FnOnce(&mut Record)) {
    RECORD.with(|record| {
        if let Some(record) = record.borrow_mut().as_mut().filter(|r| r.muted == 0) {
            f(record);
        }
    });
}

fn mute(by: isize) {
    RECORD.with(|record| {
        if let Some(record) = record.borrow_mut().as_mut() {
            record.muted = record.muted.saturating_add_signed(by);
        }
    });
}

pub(super) fn muted<T>(run: impl FnOnce() -> T) -> T {
    mute(1);
    let result = run();
    mute(-1);
    result
}

pub(crate) fn recording<T>(run: impl FnOnce() -> T) -> (T, Record) {
    RECORD.with(|record| *record.borrow_mut() = Some(Record::default()));
    let result = run();
    let record = RECORD.with(|record| record.borrow_mut().take());
    (result, record.unwrap_or_default())
}

pub(super) fn enter_block(top: bool, parameters: &[(String, Span)], statements: &[Statement]) {
    with(|record| {
        let scope = (!top).then(|| {
            let start = parameters
                .iter()
                .map(|(_, span)| span.start)
                .chain(statements.first().map(|s| s.span.start))
                .min()
                .unwrap_or_default();
            let end = statements
                .last()
                .map(|s| s.span.end)
                .or(parameters.last().map(|(_, span)| span.end))
                .unwrap_or(start);
            Span::new(start, end)
        });
        record.blocks.push(scope);
    });
}

pub(super) fn leave_block() {
    with(|record| {
        record.blocks.pop();
    });
}

pub(super) fn definition(
    name: &str,
    span: Span,
    kind: Kind,
    lemmas: &[String],
    class: Option<WordClass>,
    signature: Option<&[SignatureParameter]>,
) {
    with(|record| {
        let scope = record.blocks.last().copied().flatten();
        record.definitions.push(Definition {
            name: name.to_owned(),
            span,
            kind,
            scope,
            lemmas: lemmas.to_vec(),
            adjective: class == Some(WordClass::Adjective),
            particles: signature
                .unwrap_or_default()
                .iter()
                .map(|parameter| parameter.spec.role.particle())
                .collect(),
        });
    });
}

pub(super) fn usage(name: &str, span: Span, definition: Option<Span>) {
    with(|record| {
        record.usages.push(Usage {
            name: name.to_owned(),
            span,
            definition,
        });
    });
}

pub(super) fn fixed(span: Span) {
    with(|record| record.fixed.push(span));
}

pub(super) fn category(span: Span, category: Category) {
    with(|record| record.categories.push((span, category)));
}

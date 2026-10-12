use std::rc::Rc;

use num_bigint::BigInt;
use num_traits::FromPrimitive;

use crate::diagnostic::{Diagnostic, Result, Span, type_error};
use crate::limits::{Limit, MAX_ITEMS, MAX_TEXT_BYTES};
use crate::runtime::heap::{Handle, Heap};
use crate::runtime::value::{Constant, Int, Number, Value};

#[inline(never)]
pub(super) fn split(
    input: &str,
    separator: &str,
    heap: &mut Heap,
    roots: impl Iterator<Item = Handle> + Clone,
    span: Span,
) -> Result<Value> {
    let separator = nonempty(separator, span)?;
    pieces(input.split(separator), heap, roots, span)
}

#[inline(never)]
pub(super) fn words(
    input: &str,
    heap: &mut Heap,
    roots: impl Iterator<Item = Handle> + Clone,
    span: Span,
) -> Result<Value> {
    pieces(
        input.split(whitespace).filter(|word| !word.is_empty()),
        heap,
        roots,
        span,
    )
}

#[inline(never)]
pub(super) fn lines(
    input: &str,
    heap: &mut Heap,
    roots: impl Iterator<Item = Handle> + Clone,
    span: Span,
) -> Result<Value> {
    let lines = input
        .split_inclusive('\n')
        .map(|line| match line.strip_suffix('\n') {
            Some(line) => line.strip_suffix('\r').unwrap_or(line),
            None => line,
        });
    pieces(lines, heap, roots, span)
}

fn pieces<'a>(
    parts: impl Iterator<Item = &'a str>,
    heap: &mut Heap,
    roots: impl Iterator<Item = Handle> + Clone,
    span: Span,
) -> Result<Value> {
    let mut texts: Vec<Rc<str>> = Vec::new();
    for part in parts {
        if texts.len() >= MAX_ITEMS {
            return Err(Diagnostic::limit(Limit::Items, span));
        }
        texts.push(part.into());
    }
    heap.text_list(texts, roots, span)
}

#[inline(never)]
pub(super) fn join(items: &Value, separator: &str, heap: &Heap, span: Span) -> Result<String> {
    let (Value::List(handle) | Value::Tuple(handle)) = items else {
        return Err(type_error(span));
    };
    let mut parts = Vec::new();
    for value in heap.list(*handle, span)? {
        parts.push(heap.text(value).ok_or_else(|| type_error(span))?);
    }
    let size = parts
        .iter()
        .try_fold(
            separator.len() * parts.len().saturating_sub(1),
            |size, part| size.checked_add(part.len()),
        )
        .filter(|&size| size <= MAX_TEXT_BYTES)
        .ok_or_else(|| Diagnostic::limit(Limit::Text, span))?;
    let mut result = String::with_capacity(size);
    for (index, part) in parts.iter().enumerate() {
        if index != 0 {
            result.push_str(separator);
        }
        result.push_str(part);
    }
    Ok(result)
}

pub(super) fn trim(input: &str) -> &str {
    input.trim_matches(whitespace)
}

#[inline(never)]
pub(super) fn replace(input: &str, from: &str, to: &str, span: Span) -> Result<String> {
    let from = nonempty(from, span)?;
    let count = input.matches(from).count();
    count
        .checked_mul(to.len())
        .and_then(|added| (input.len() - count * from.len()).checked_add(added))
        .filter(|&size| size <= MAX_TEXT_BYTES)
        .ok_or_else(|| Diagnostic::limit(Limit::Text, span))?;
    Ok(input.replace(from, to))
}

pub(super) fn to_int(value: &Value, heap: &Heap, span: Span) -> Result<Int> {
    if let Some(text) = heap.text(value) {
        return parse_int(text, span);
    }
    match heap.number(value) {
        Some(Number::Int(n)) => Ok(n),
        Some(Number::Float(f)) => BigInt::from_f64(f.trunc())
            .map(Int::from)
            .ok_or_else(|| range_error(span)),
        None => Err(type_error(span)),
    }
}

pub(super) fn to_float(value: &Value, heap: &Heap, span: Span) -> Result<f64> {
    match heap.text(value) {
        Some(text) => parse_float(text, span),
        None => heap.as_float(value, span),
    }
}

fn nonempty(text: &str, span: Span) -> Result<&str> {
    if text.is_empty() {
        Err(Diagnostic::new(
            "E-TEXT-001",
            span,
            "구분자와 찾을 글은 비워 둘 수 없습니다.",
        ))
    } else {
        Ok(text)
    }
}

fn whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | ' ' | '\u{0085}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}')
}

fn conversion_error(input: &str, span: Span) -> Diagnostic {
    let mut shown: String = input.chars().take(40).collect();
    if shown.len() < input.len() {
        shown.push_str("...");
    }
    Diagnostic::new(
        "E-CONVERT-001",
        span,
        format!(
            "'{shown}'{} 십진수로 읽을 수 없습니다.",
            crate::korean::particle::TOPIC.after(&shown)
        ),
    )
}

fn range_error(span: Span) -> Diagnostic {
    Diagnostic::new(
        "E-CONVERT-002",
        span,
        "이 값을 유한한 수로 변환할 수 없습니다.",
    )
}

pub(super) fn parse_int(input: &str, span: Span) -> Result<Int> {
    let input = input.trim_matches(whitespace);
    let negative = input.starts_with('-');
    let digits = input.strip_prefix(['+', '-']).unwrap_or(input);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(conversion_error(input, span));
    }
    let digits = digits.trim_start_matches('0');
    let Constant::Int(value) =
        Constant::number(if digits.is_empty() { "0" } else { digits }, span)?
    else {
        return Err(conversion_error(input, span));
    };
    Ok(if negative { -&value } else { value })
}

pub(super) fn parse_float(input: &str, span: Span) -> Result<f64> {
    let input = input.trim_matches(whitespace);
    let bytes = input.as_bytes();
    let mut index = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let mut digits = 0;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
        digits += 1;
    }
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return Err(conversion_error(input, span));
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        let start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == start {
            return Err(conversion_error(input, span));
        }
    }
    if index != bytes.len() {
        return Err(conversion_error(input, span));
    }
    let value: f64 = input.parse().map_err(|_| conversion_error(input, span))?;
    if !value.is_finite() {
        return Err(range_error(span));
    }
    Ok(value)
}

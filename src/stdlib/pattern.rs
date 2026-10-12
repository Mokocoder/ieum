use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use regex::Regex;

use crate::diagnostic::{Diagnostic, Result, Span};
use crate::limits::{Limit, MAX_TEXT_BYTES};
use crate::runtime::heap::{Handle, Heap};
use crate::runtime::value::Value;

const CACHE_SIZE: usize = 512;

thread_local! {
    static CACHE: RefCell<HashMap<String, Regex>> = RefCell::new(HashMap::new());
}

fn compile(pattern: &str, span: Span) -> Result<Regex> {
    if let Some(regex) = CACHE.with_borrow(|cache| cache.get(pattern).cloned()) {
        return Ok(regex);
    }
    let regex = Regex::new(pattern).map_err(|error| {
        Diagnostic::new(
            "E-REGEX-001",
            span,
            format!("정규식을 읽을 수 없습니다: {error}"),
        )
    })?;
    CACHE.with_borrow_mut(|cache| {
        if cache.len() >= CACHE_SIZE {
            cache.clear();
        }
        cache.insert(pattern.to_owned(), regex.clone());
    });
    Ok(regex)
}

fn groups(regex: &Regex, replacement: &str, span: Span) -> Result<String> {
    let mut out = String::with_capacity(replacement.len());
    let mut rest = replacement;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        rest = &rest[at + 1..];
        if let Some(after) = rest.strip_prefix('$') {
            out.push_str("$$");
            rest = after;
            continue;
        }
        let (name, after) = if let Some(braced) = rest.strip_prefix('{') {
            match braced.find('}') {
                Some(end) => (&braced[..end], &braced[end + 1..]),
                None => ("", rest),
            }
        } else {
            let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            let end = if digits > 0 {
                digits
            } else {
                rest.len()
                    - rest
                        .trim_start_matches(|c: char| c == '_' || c.is_alphanumeric())
                        .len()
            };
            (&rest[..end], &rest[end..])
        };
        let known = match name.parse::<usize>() {
            Ok(index) => index < regex.captures_len(),
            Err(_) => regex.capture_names().flatten().any(|group| group == name),
        };
        if !known {
            return Err(Diagnostic::new(
                "E-REGEX-001",
                span,
                format!(
                    "바꿀 글의 '${name}'에 맞는 묶음이 정규식에 없습니다. '$' 자체는 '$$'로 씁니다."
                ),
            ));
        }
        out.push_str(&format!("${{{name}}}"));
        rest = after;
    }
    out.push_str(rest);
    Ok(out)
}

#[inline(never)]
pub(super) fn matches(text: &str, pattern: &str, span: Span) -> Result<bool> {
    let whole = compile(&format!("^(?:{pattern})$"), span)?;
    Ok(whole.is_match(text))
}

#[inline(never)]
pub(super) fn extract(
    text: &str,
    pattern: &str,
    heap: &mut Heap,
    roots: impl Iterator<Item = Handle> + Clone,
    span: Span,
) -> Result<Value> {
    let found: Vec<Rc<str>> = compile(pattern, span)?
        .find_iter(text)
        .map(|found| found.as_str().into())
        .collect();
    heap.text_list(found, roots, span)
}

#[inline(never)]
pub(super) fn replace(text: &str, pattern: &str, replacement: &str, span: Span) -> Result<String> {
    let regex = compile(pattern, span)?;
    let replacement = groups(&regex, replacement, span)?;
    let replaced = regex.replace_all(text, replacement.as_str());
    if replaced.len() > MAX_TEXT_BYTES {
        return Err(Diagnostic::limit(Limit::Text, span));
    }
    Ok(replaced.into_owned())
}

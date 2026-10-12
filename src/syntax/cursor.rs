use super::lexer;
use crate::diagnostic::Span;
use crate::limits::{MAX_BRACKET_DEPTH, MAX_SOURCE_BYTES};

#[derive(Clone, Debug)]
pub struct CompletionContext {
    pub replacement: Span,
    pub previous: String,
    pub expression: bool,
    pub statement: bool,
    pub quoted: bool,
}

pub fn completion_context(source: &str, at: usize) -> Option<CompletionContext> {
    if source.len() > MAX_SOURCE_BYTES || !source.is_char_boundary(at) {
        return None;
    }
    let region = code_at(source, Span::new(0, source.len()), at, 0)?;
    let before = &source[region.start..at];
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| unicode_ident::is_xid_continue(*c))
        .last()
        .map_or(at, |(index, _)| region.start + index);
    let end = at
        + source[at..region.end]
            .char_indices()
            .take_while(|(_, c)| unicode_ident::is_xid_continue(*c))
            .last()
            .map_or(0, |(index, c)| index + c.len_utf8());
    let quoted = start > region.start && source.as_bytes()[start - 1] == b'`';
    let prefix_end = start - usize::from(quoted);
    let scanned = lexer::scan_lossless(&source[region.start..prefix_end], region.start);
    let mut depth = 0usize;
    for token in &scanned.tokens {
        if token.opens() {
            depth += 1;
        } else if token.closes() {
            depth = depth.saturating_sub(1);
        } else if token.symbol(":") && region.start != 0 && depth == 0 {
            return None;
        }
    }
    let prefix = &source[region.start..prefix_end];
    let line = prefix.rsplit('\n').next().unwrap_or("");
    let previous = line.split_whitespace().next_back().unwrap_or("").to_owned();
    Some(CompletionContext {
        replacement: Span::new(start, end),
        previous,
        expression: region.start != 0 || depth > 0,
        statement: line.trim().is_empty() && region.start == 0 && depth == 0 && !quoted,
        quoted,
    })
}

fn code_at(source: &str, region: Span, at: usize, depth: usize) -> Option<Span> {
    if depth > MAX_BRACKET_DEPTH {
        return None;
    }
    let mut index = region.start;
    while index < at {
        let c = source[index..region.end].chars().next()?;
        match c {
            '#' => {
                let end = source[index..region.end].find('\n').map(|n| index + n);
                match end {
                    Some(end) if end < at => index = end + 1,
                    _ => return None,
                }
            }
            '`' => {
                let rest = &source[index + 1..region.end];
                let end = rest
                    .find(['`', '\r', '\n'])
                    .map_or(region.end, |n| index + 1 + n);
                if at <= end {
                    return Some(region);
                }
                index = end + usize::from(source.as_bytes().get(end) == Some(&b'`'));
            }
            '\'' | '"' => match text_at(source, index, region.end, at, depth)? {
                TextPosition::After(end) => index = end,
                TextPosition::Code(code) => return Some(code),
            },
            _ => index += c.len_utf8(),
        }
    }
    Some(region)
}

enum TextPosition {
    After(usize),
    Code(Span),
}

fn text_at(
    source: &str,
    start: usize,
    end: usize,
    at: usize,
    depth: usize,
) -> Option<TextPosition> {
    let quote = source.as_bytes()[start] as char;
    let triple = source[start..end].starts_with(&quote.to_string().repeat(3));
    let width = if triple { 3 } else { 1 };
    let delimiter = &source[start..start + width];
    let mut index = start + width;
    while index < end && index < at {
        let rest = &source[index..end];
        if rest.starts_with(delimiter) {
            return (index + width <= at).then_some(TextPosition::After(index + width));
        }
        let c = rest.chars().next()?;
        index += c.len_utf8();
        match c {
            '\r' | '\n' if !triple => return Some(TextPosition::After(index)),
            '\\' if quote == '"' => {
                index += source[index..end].chars().next().map_or(0, char::len_utf8);
            }
            '{' if quote == '"' && source[index..end].starts_with('{') => index += 1,
            '{' if quote == '"' => {
                let close = lexer::code_length(&source[index..end]).map(|length| index + length);
                if close.is_none_or(|close| at <= close) {
                    let region = Span::new(index, close.unwrap_or(end));
                    return code_at(source, region, at, depth + 1).map(TextPosition::Code);
                }
                index = close? + 1;
            }
            _ => {}
        }
    }
    None
}

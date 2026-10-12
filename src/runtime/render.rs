use std::collections::HashSet;
use std::io::Write;

use super::heap::{Handle, Heap};
use super::value::Value;
use crate::diagnostic::{Diagnostic, Result, Span, internal};
use crate::limits::{Limit, MAX_ITEMS, MAX_TEXT_BYTES};

/// 깊은 중첩에서도 재귀하지 않도록 아직 쓸 부분을 명시적인 스택에 둡니다.
enum Part<'a> {
    Value(&'a Value, bool),
    List(Handle, usize, bool),
    Dict(Handle, usize, bool),
    Record(Handle, usize),
    Text(&'a str),
    End(Handle, char),
}

struct Rendering<'a> {
    pending: Vec<Part<'a>>,
    active: HashSet<Handle>,
    output: String,
}

impl Rendering<'_> {
    fn open(&mut self, handle: Handle, opening: &[&str]) -> bool {
        if !self.active.insert(handle) {
            self.output.push_str("<순환>");
            return false;
        }
        for text in opening {
            self.output.push_str(text);
        }
        true
    }

    fn separator(&mut self, index: usize) {
        if index != 0 {
            self.output.push_str(", ");
        }
    }
}

impl Heap {
    pub fn render(&self, value: &Value, span: Span) -> Result<String> {
        let mut state = Rendering {
            pending: vec![Part::Value(value, false)],
            active: HashSet::new(),
            output: String::new(),
        };
        while let Some(part) = state.pending.pop() {
            match part {
                Part::Value(value, nested) => self.render_value(value, nested, &mut state, span)?,
                Part::List(handle, i, tuple) => {
                    let items = self.list(handle, span)?;
                    if let Some(value) = items.get(i) {
                        state.separator(i);
                        state.pending.push(Part::List(handle, i + 1, tuple));
                        state.pending.push(Part::Value(value, true));
                    } else if tuple && items.len() == 1 {
                        state.output.push(',');
                    }
                }
                Part::Dict(handle, index, set) => {
                    if let Some((position, entry)) = self.dictionary(handle, span)?.next_from(index)
                    {
                        state.separator(index);
                        state.pending.push(Part::Dict(handle, position + 1, set));
                        if !set {
                            state.pending.push(Part::Value(&entry.value, true));
                            state.pending.push(Part::Text(": "));
                        }
                        state.pending.push(Part::Value(&entry.original, true));
                    }
                }
                Part::Record(handle, index) => {
                    let (record, values) = self.record(handle, span)?;
                    if let Some(value) = values.get(index) {
                        state.separator(index);
                        state.output.push_str(&record.fields[index].name);
                        state.output.push_str(": ");
                        state.pending.push(Part::Record(handle, index + 1));
                        state.pending.push(Part::Value(value, true));
                    }
                }
                Part::Text(text) => state.output.push_str(text),
                Part::End(handle, closing) => {
                    state.active.remove(&handle);
                    state.output.push(closing);
                }
            }
            if state.output.len() > MAX_TEXT_BYTES || state.pending.len() > MAX_ITEMS {
                return Err(Diagnostic::limit(Limit::Text, span));
            }
        }
        Ok(state.output)
    }

    fn render_value<'a>(
        &'a self,
        value: &'a Value,
        nested: bool,
        state: &mut Rendering<'a>,
        span: Span,
    ) -> Result<()> {
        match value {
            Value::List(handle) | Value::Tuple(handle) => {
                self.list(*handle, span)?;
                let tuple = matches!(value, Value::Tuple(_));
                if state.open(*handle, &[if tuple { "(" } else { "[" }]) {
                    state
                        .pending
                        .push(Part::End(*handle, if tuple { ')' } else { ']' }));
                    state.pending.push(Part::List(*handle, 0, tuple));
                }
            }
            Value::Dict(handle) | Value::Set(handle) => {
                let dictionary = self.dictionary(*handle, span)?;
                let set = matches!(value, Value::Set(_));
                if set && dictionary.is_empty() {
                    state.output.push_str("집합()");
                } else if state.open(*handle, &["{"]) {
                    state.pending.push(Part::End(*handle, '}'));
                    state.pending.push(Part::Dict(*handle, 0, set));
                }
            }
            Value::Error(handle) => {
                let diagnostic = &self.error(*handle, span)?.0.diagnostic;
                state.output.push_str("<오류 ");
                state.output.push_str(diagnostic.code);
                state.output.push_str(": ");
                state.output.push_str(&diagnostic.message);
                state.output.push('>');
            }
            Value::Record(handle) => {
                let (record, _) = self.record(*handle, span)?;
                if state.open(*handle, &[&record.name, "("]) {
                    state.pending.push(Part::End(*handle, ')'));
                    state.pending.push(Part::Record(*handle, 0));
                }
            }
            Value::RecordType(handle) => {
                state.output.push_str("<자료형 ");
                state
                    .output
                    .push_str(&self.record_type(*handle, span)?.0.name);
                state.output.push('>');
            }
            Value::Function(handle) => {
                let name = &self.function(*handle, span)?.info.name;
                state.output.push_str(&function_text(name));
            }
            Value::Text(_) => {
                let text = self.text_of(value, span)?;
                if nested {
                    quote(&text, &mut state.output, span)?;
                } else {
                    state.output.push_str(&text);
                }
            }
            Value::Big(_) => {
                let n = self.int(value).ok_or_else(|| internal(span))?;
                state.output.push_str(&n.to_string());
            }
            Value::Int(n) => state.output.push_str(&n.to_string()),
            Value::Float(f) => state.output.push_str(&render_float(*f)),
            Value::Bool(true) => state.output.push('참'),
            Value::Bool(false) => state.output.push_str("거짓"),
            Value::None => state.output.push_str("없음"),
            Value::Native(native) => state.output.push_str(&function_text(native.name())),
        }
        Ok(())
    }
}

fn quote(text: &str, output: &mut String, span: Span) -> Result<()> {
    output.push('"');
    for c in text.chars() {
        match c {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\0'..='\u{1f}' | '\u{7f}'..='\u{9f}' => {
                output.push_str(&format!("\\u{:04x}", c as u32))
            }
            _ => output.push(c),
        }
        if output.len() > MAX_TEXT_BYTES {
            return Err(Diagnostic::limit(Limit::Text, span));
        }
    }
    output.push('"');
    Ok(())
}

pub(crate) fn write_line(
    output: &mut dyn Write,
    heap: &Heap,
    value: &Value,
    span: Span,
) -> Result<()> {
    let mut rendered = heap.render(value, span)?;
    rendered.push('\n');
    write_bytes(output, rendered.as_bytes(), span)
}

pub(super) fn write_bytes(output: &mut dyn Write, bytes: &[u8], span: Span) -> Result<()> {
    output
        .write_all(bytes)
        .map_err(|e| Diagnostic::new("E-IO-001", span, format!("출력하지 못했습니다: {e}")))
}

fn function_text(name: &str) -> String {
    format!("<함수 {name}>")
}

pub(crate) fn render_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f.is_sign_negative() { "-inf" } else { "inf" }.into();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    let scientific = format!("{:e}", f.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap();
    let exponent: i32 = exponent.parse().unwrap();
    let sign = if f.is_sign_negative() { "-" } else { "" };
    if !(-4..16).contains(&exponent) {
        return format!("{sign}{mantissa}e{exponent:+03}");
    }
    let digits = mantissa.replace('.', "");
    let point = exponent + 1;
    if point <= 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-point) as usize))
    } else if point as usize >= digits.len() {
        format!(
            "{sign}{digits}{}.0",
            "0".repeat(point as usize - digits.len())
        )
    } else {
        format!(
            "{sign}{}.{}",
            &digits[..point as usize],
            &digits[point as usize..]
        )
    }
}

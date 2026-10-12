use std::collections::HashSet;
use std::rc::Rc;

use super::text::{parse_float, parse_int};
use crate::diagnostic::{Diagnostic, Result, Span, code, internal};
use crate::korean::particle::SUBJECT;
use crate::limits::{Limit, MAX_DATA_DEPTH, MAX_ITEMS, MAX_TEXT_BYTES};
use crate::runtime::heap::{Handle, Heap};
use crate::runtime::render::render_float;
use crate::runtime::value::{Constant, Value};

enum Node {
    Scalar(Constant),
    List(Vec<Node>),
    Dict(Vec<(Rc<str>, Node)>),
}

fn decode(
    node: Node,
    heap: &mut Heap,
    roots: impl Iterator<Item = Handle> + Clone,
    span: Span,
) -> Result<Value> {
    enum Work {
        Node(Node),
        List(usize),
        Dict(Vec<Rc<str>>),
    }
    let mut pending = vec![Work::Node(node)];
    let mut values: Vec<Value> = Vec::new();
    while let Some(work) = pending.pop() {
        match work {
            Work::Node(Node::Scalar(constant)) => {
                let live = roots.clone().chain(values.iter().filter_map(Value::handle));
                let value = heap.new_constant(&constant, live, span)?;
                values.push(value);
            }
            Work::Node(Node::List(children)) => {
                pending.push(Work::List(children.len()));
                pending.extend(children.into_iter().rev().map(Work::Node));
            }
            Work::Node(Node::Dict(entries)) => {
                let (keys, children): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
                pending.push(Work::Dict(keys));
                pending.extend(children.into_iter().rev().map(Work::Node));
            }
            Work::List(length) => {
                let children = values.split_off(
                    values
                        .len()
                        .checked_sub(length)
                        .ok_or_else(|| internal(span))?,
                );
                let value = heap.new_list(
                    children,
                    roots.clone().chain(values.iter().filter_map(Value::handle)),
                    span,
                )?;
                values.push(value);
            }
            Work::Dict(keys) => {
                let children = values.split_off(
                    values
                        .len()
                        .checked_sub(keys.len())
                        .ok_or_else(|| internal(span))?,
                );
                let value = heap.named_dictionary(
                    keys.into_iter().zip(children).collect(),
                    roots.clone().chain(values.iter().filter_map(Value::handle)),
                    span,
                )?;
                values.push(value);
            }
        }
    }
    if values.len() != 1 {
        return Err(internal(span));
    }
    values.pop().ok_or_else(|| internal(span))
}

fn json_error(span: Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new("E-JSON-001", span, message)
}

enum Stop {
    Syntax(&'static str, usize),
    Limit,
}

const EOF_VALUE: &str = "값이 나오기 전에 입력이 끝났습니다.";
const EOF_LIST: &str = "배열이 닫히기 전에 입력이 끝났습니다.";
const EOF_OBJECT: &str = "객체가 닫히기 전에 입력이 끝났습니다.";
const EOF_STRING: &str = "문자열이 닫히기 전에 입력이 끝났습니다.";
const EXPECTED_COLON: &str = "객체의 키 뒤에는 쌍점(:)이 와야 합니다.";
const EXPECTED_LIST_END: &str = "배열의 값 뒤에는 쉼표나 닫는 대괄호가 와야 합니다.";
const EXPECTED_OBJECT_END: &str = "객체의 값 뒤에는 쉼표나 닫는 중괄호가 와야 합니다.";
const EXPECTED_IDENT: &str = "true, false, null 가운데 하나를 잘못 썼습니다.";
const EXPECTED_VALUE: &str = "값이 와야 할 자리입니다.";
const INVALID_ESCAPE: &str = "잘못된 이스케이프입니다.";
const INVALID_NUMBER: &str = "수의 형식이 잘못되었습니다.";
const CONTROL_CHARACTER: &str =
    "문자열 안에 제어 문자(U+0000부터 U+001F까지)를 그대로 쓸 수 없습니다.";
const KEY_NOT_STRING: &str = "객체의 키는 문자열이어야 합니다.";
const LONE_SURROGATE: &str = "짝이 맞지 않는 서로게이트 이스케이프입니다.";
const TRAILING_CHARACTERS: &str = "값 뒤에 남은 글자가 있습니다.";
const UNFINISHED_SURROGATE: &str = "서로게이트 쌍의 뒷부분이 없습니다.";

enum Frame {
    List(Vec<Node>),
    Object(Vec<(Rc<str>, Node)>, HashSet<String>, String),
}

struct JsonReader<'a> {
    input: &'a str,
    index: usize,
    items: usize,
    span: Span,
    deferred: Option<Diagnostic>,
}

impl JsonReader<'_> {
    fn peek(&self) -> Option<u8> {
        self.input.as_bytes().get(self.index).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.index += 1;
        Some(byte)
    }

    fn whitespace(&mut self) -> Option<u8> {
        while let Some(b' ' | b'\n' | b'\t' | b'\r') = self.peek() {
            self.index += 1;
        }
        self.peek()
    }

    fn error(&self, message: &'static str) -> Stop {
        Stop::Syntax(message, self.index)
    }

    fn peek_error(&self, message: &'static str) -> Stop {
        Stop::Syntax(message, (self.index + 1).min(self.input.len()))
    }

    fn describe(&self, message: &str, index: usize) -> String {
        let before = &self.input.as_bytes()[..index];
        let line_start = before
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1);
        let line = 1 + before[..line_start].iter().filter(|&&b| b == b'\n').count();
        format!("{line}번째 줄 {}번째 바이트: {message}", index - line_start)
    }

    fn defer(&mut self, error: Diagnostic) {
        self.deferred.get_or_insert(error);
    }

    fn document(&mut self) -> std::result::Result<Node, Stop> {
        let node = self.value()?;
        if self.whitespace().is_some() {
            return Err(self.peek_error(TRAILING_CHARACTERS));
        }
        Ok(node)
    }

    fn value(&mut self) -> std::result::Result<Node, Stop> {
        let mut frames = Vec::new();
        loop {
            self.items += 1;
            if self.items > MAX_ITEMS {
                self.defer(Diagnostic::limit(Limit::Items, self.span));
            }
            let Some(byte) = self.whitespace() else {
                return Err(self.peek_error(EOF_VALUE));
            };
            let mut node = match byte {
                b'n' => self.word(b"null", Constant::None)?,
                b't' => self.word(b"true", Constant::Bool(true))?,
                b'f' => self.word(b"false", Constant::Bool(false))?,
                b'-' | b'0'..=b'9' => Node::Scalar(self.number()?),
                b'"' => {
                    self.index += 1;
                    Node::Scalar(Constant::Text(self.string()?.into()))
                }
                b'[' | b'{' => {
                    if frames.len() >= MAX_DATA_DEPTH {
                        return Err(Stop::Limit);
                    }
                    self.index += 1;
                    let (closing, empty) = if byte == b'[' {
                        (b']', Node::List(Vec::new()))
                    } else {
                        (b'}', Node::Dict(Vec::new()))
                    };
                    match self.whitespace() {
                        Some(next) if next == closing => {
                            self.index += 1;
                            empty
                        }
                        None if byte == b'[' => return Err(self.peek_error(EOF_LIST)),
                        _ if byte == b'[' => {
                            frames.push(Frame::List(Vec::new()));
                            continue;
                        }
                        _ => {
                            let key = self.key()?;
                            let names = HashSet::from([key.clone()]);
                            frames.push(Frame::Object(Vec::new(), names, key));
                            continue;
                        }
                    }
                }
                _ => return Err(self.peek_error(EXPECTED_VALUE)),
            };
            loop {
                let Some(mut frame) = frames.pop() else {
                    return Ok(node);
                };
                let keep = self.deferred.is_none();
                let (closing, unexpected, eof) = match &mut frame {
                    Frame::List(values) => {
                        if keep {
                            values.push(node);
                        }
                        (b']', EXPECTED_LIST_END, EOF_LIST)
                    }
                    Frame::Object(entries, _, key) => {
                        if keep {
                            entries.push((std::mem::take(key).into(), node));
                        }
                        (b'}', EXPECTED_OBJECT_END, EOF_OBJECT)
                    }
                };
                match self.whitespace() {
                    Some(b',') => {
                        self.index += 1;
                        if let Frame::Object(_, names, key) = &mut frame {
                            *key = self.key()?;
                            if !names.insert(key.clone()) {
                                let particle = SUBJECT.after(key);
                                let message =
                                    format!("JSON 객체의 키 {key:?}{particle} 중복됩니다.");
                                self.defer(json_error(self.span, message));
                            }
                        }
                        frames.push(frame);
                        break;
                    }
                    Some(byte) if byte == closing => self.index += 1,
                    Some(_) => return Err(self.peek_error(unexpected)),
                    None => return Err(self.peek_error(eof)),
                }
                node = match frame {
                    Frame::List(values) => Node::List(values),
                    Frame::Object(entries, ..) => Node::Dict(entries),
                };
            }
        }
    }

    fn key(&mut self) -> std::result::Result<String, Stop> {
        match self.whitespace() {
            Some(b'"') => self.index += 1,
            Some(_) => return Err(self.peek_error(KEY_NOT_STRING)),
            None => return Err(self.peek_error(EOF_OBJECT)),
        }
        let key = self.string()?;
        match self.whitespace() {
            Some(b':') => self.index += 1,
            Some(_) => return Err(self.peek_error(EXPECTED_COLON)),
            None => return Err(self.peek_error(EOF_OBJECT)),
        }
        Ok(key)
    }

    fn word(&mut self, spelling: &[u8], value: Constant) -> std::result::Result<Node, Stop> {
        self.index += 1;
        for &expected in &spelling[1..] {
            match self.next() {
                None => return Err(self.error(EOF_VALUE)),
                Some(byte) if byte != expected => return Err(self.error(EXPECTED_IDENT)),
                Some(_) => {}
            }
        }
        Ok(Node::Scalar(value))
    }

    fn number(&mut self) -> std::result::Result<Constant, Stop> {
        let start = self.index;
        if self.peek() == Some(b'-') {
            self.index += 1;
        }
        match self.next() {
            Some(b'0') => {
                if let Some(b'0'..=b'9') = self.peek() {
                    return Err(self.peek_error(INVALID_NUMBER));
                }
            }
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(self.error(INVALID_NUMBER)),
        }
        if self.peek() == Some(b'.') {
            self.index += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.peek_error(INVALID_NUMBER));
            }
            self.digits();
        }
        if let Some(b'e' | b'E') = self.peek() {
            self.index += 1;
            if let Some(b'+' | b'-') = self.peek() {
                self.index += 1;
            }
            if !matches!(self.next(), Some(b'0'..=b'9')) {
                return Err(self.error(INVALID_NUMBER));
            }
            self.digits();
        }
        let spelling = &self.input[start..self.index];
        let span = self.span;
        let value = if spelling.contains(['.', 'e', 'E']) {
            parse_float(spelling, span)
                .map(Constant::Float)
                .map_err(|error| {
                    if error.code == code::LIMIT {
                        error
                    } else {
                        json_error(span, "JSON 숫자가 너무 커서 실수로 나타낼 수 없습니다.")
                    }
                })
        } else {
            parse_int(spelling, span).map(Constant::Int)
        };
        Ok(value.unwrap_or_else(|error| {
            self.defer(error);
            Constant::None
        }))
    }

    fn digits(&mut self) {
        while let Some(b'0'..=b'9') = self.peek() {
            self.index += 1;
        }
    }

    fn string(&mut self) -> std::result::Result<String, Stop> {
        let quote = self.index - 1;
        let mut text = String::new();
        let mut unpaired = None;
        loop {
            let start = self.index;
            while let Some(byte) = self.peek() {
                if byte == b'"' || byte == b'\\' || byte < 0x20 {
                    break;
                }
                self.index += 1;
            }
            text.push_str(&self.input[start..self.index]);
            match self.peek() {
                None => return Err(self.error(EOF_STRING)),
                Some(b'"') => {
                    self.index += 1;
                    break;
                }
                Some(b'\\') => {
                    self.index += 1;
                    self.escape(&mut text, &mut unpaired)?;
                }
                Some(_) => return Err(self.error(CONTROL_CHARACTER)),
            }
        }
        if let Some((message, index)) = unpaired {
            let column = index - quote;
            let message = format!("JSON 문자열 {column}번째 바이트: {message}");
            self.defer(json_error(self.span, message));
        }
        Ok(text)
    }

    fn escape(
        &mut self,
        text: &mut String,
        unpaired: &mut Option<(&'static str, usize)>,
    ) -> std::result::Result<(), Stop> {
        let Some(byte) = self.next() else {
            return Err(self.error(EOF_STRING));
        };
        let character = match byte {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let unit = self.hex_escape()?;
                let mut record = |message, index| {
                    unpaired.get_or_insert((message, index));
                };
                match unit {
                    0xDC00..=0xDFFF => record(LONE_SURROGATE, self.index),
                    0xD800..=0xDBFF => {
                        let input = self.input.as_bytes();
                        let rest = &input[self.index..];
                        match rest {
                            [b'\\', b'u', digits @ ..] => match digits.get(..4).and_then(hex) {
                                Some(low @ 0xDC00..=0xDFFF) => {
                                    self.index += 6;
                                    let high = u32::from(unit - 0xD800) << 10;
                                    let scalar = (high | u32::from(low - 0xDC00)) + 0x1_0000;
                                    text.push(char::from_u32(scalar).expect("서로게이트 쌍입니다"));
                                }
                                Some(_) => record(LONE_SURROGATE, self.index + 6),
                                None => {}
                            },
                            [b'\\', _, ..] => record(UNFINISHED_SURROGATE, self.index + 2),
                            [b'\\'] | [] => {}
                            [_, ..] => record(UNFINISHED_SURROGATE, self.index + 1),
                        }
                    }
                    _ => text.push(char::from_u32(unit.into()).expect("서로게이트가 아닙니다")),
                }
                return Ok(());
            }
            _ => return Err(self.error(INVALID_ESCAPE)),
        };
        text.push(character);
        Ok(())
    }

    fn hex_escape(&mut self) -> std::result::Result<u16, Stop> {
        let Some(digits) = self.input.as_bytes().get(self.index..self.index + 4) else {
            self.index = self.input.len();
            return Err(self.error(EOF_STRING));
        };
        self.index += 4;
        hex(digits).ok_or_else(|| self.error(INVALID_ESCAPE))
    }
}

fn hex(digits: &[u8]) -> Option<u16> {
    digits.iter().try_fold(0u16, |unit, &digit| {
        let value = char::from(digit).to_digit(16)?;
        Some(unit << 4 | value as u16)
    })
}

fn nested_too_deeply(input: &str) -> bool {
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    for byte in input.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'[' | b'{' => {
                    depth += 1;
                    if depth > MAX_DATA_DEPTH {
                        return true;
                    }
                }
                b']' | b'}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    false
}

#[inline(never)]
pub(super) fn read_json(
    input: &str,
    heap: &mut Heap,
    roots: impl Iterator<Item = Handle> + Clone,
    span: Span,
) -> Result<Value> {
    if input.len() > MAX_TEXT_BYTES {
        return Err(Diagnostic::limit(Limit::Text, span));
    }
    let mut reader = JsonReader {
        input,
        index: 0,
        items: 0,
        span,
        deferred: None,
    };
    let node = match reader.document() {
        Ok(node) => node,
        Err(Stop::Limit) => return Err(Diagnostic::limit(Limit::Items, span)),
        Err(Stop::Syntax(_, _)) if nested_too_deeply(input) => {
            return Err(Diagnostic::limit(Limit::DataDepth, span));
        }
        Err(Stop::Syntax(message, index)) => {
            let message = reader.describe(message, index);
            return Err(json_error(span, format!("JSON {message}")));
        }
    };
    match reader.deferred {
        Some(error) => Err(error),
        None => decode(node, heap, roots, span),
    }
}

struct TextBuffer {
    text: String,
    span: Span,
}

impl TextBuffer {
    fn new(span: Span) -> Self {
        Self {
            text: String::new(),
            span,
        }
    }

    fn push(&mut self, value: &str) -> Result<()> {
        if self
            .text
            .len()
            .checked_add(value.len())
            .is_none_or(|length| length > MAX_TEXT_BYTES)
        {
            return Err(Diagnostic::limit(Limit::Text, self.span));
        }
        self.text.push_str(value);
        Ok(())
    }

    fn character(&mut self, value: char) -> Result<()> {
        self.push(value.encode_utf8(&mut [0; 4]))
    }

    fn json_string(&mut self, text: &str) -> Result<()> {
        self.push("\"")?;
        for character in text.chars() {
            match character {
                '"' => self.push("\\\"")?,
                '\\' => self.push("\\\\")?,
                '\n' => self.push("\\n")?,
                '\r' => self.push("\\r")?,
                '\t' => self.push("\\t")?,
                '\0'..='\u{1f}' => self.push(&format!("\\u{:04x}", character as u32))?,
                _ => self.character(character)?,
            }
        }
        self.push("\"")
    }
}

#[inline(never)]
pub(super) fn write_json(value: &Value, heap: &Heap, span: Span) -> Result<String> {
    enum Part<'a> {
        Value(&'a Value, usize),
        List(Handle, usize, usize),
        Dict(Handle, usize, usize),
        Record(Handle, usize, usize),
        End(Handle, char),
    }
    let unsupported = || {
        Diagnostic::new(
            "E-JSON-002",
            span,
            "JSON에는 순환 참조, 문자열이 아닌 사전 키, 무한대, NaN, 집합, 함수, 오류 값을 쓸 수 없습니다.",
        )
    };
    let mut pending = vec![Part::Value(value, 0)];
    let mut active = HashSet::new();
    let mut output = TextBuffer::new(span);
    let mut items = 0;
    while let Some(part) = pending.pop() {
        match part {
            Part::Value(value, depth) => {
                items += 1;
                if items > MAX_ITEMS {
                    return Err(Diagnostic::limit(Limit::Items, span));
                }
                match value {
                    Value::None => output.push("null")?,
                    Value::Bool(value) => output.push(if *value { "true" } else { "false" })?,
                    Value::Text(_) => {
                        output.json_string(heap.text(value).ok_or_else(|| internal(span))?)?
                    }
                    Value::Int(n) => output.push(&n.to_string())?,
                    Value::Big(_) => output.push(&heap.render(value, span)?)?,
                    Value::Float(number) if number.is_finite() => {
                        output.push(&render_float(*number))?
                    }
                    Value::List(handle) | Value::Tuple(handle) | Value::Dict(handle) => {
                        if depth >= MAX_DATA_DEPTH {
                            return Err(Diagnostic::limit(Limit::DataDepth, span));
                        }
                        if !active.insert(*handle) {
                            return Err(unsupported());
                        }
                        if matches!(value, Value::List(_) | Value::Tuple(_)) {
                            heap.list(*handle, span)?;
                            output.push("[")?;
                            pending.push(Part::End(*handle, ']'));
                            pending.push(Part::List(*handle, 0, depth + 1));
                        } else {
                            heap.dictionary(*handle, span)?;
                            output.push("{")?;
                            pending.push(Part::End(*handle, '}'));
                            pending.push(Part::Dict(*handle, 0, depth + 1));
                        }
                    }
                    Value::Record(handle) => {
                        if depth >= MAX_DATA_DEPTH {
                            return Err(Diagnostic::limit(Limit::DataDepth, span));
                        }
                        if !active.insert(*handle) {
                            return Err(unsupported());
                        }
                        heap.record(*handle, span)?;
                        output.push("{")?;
                        pending.push(Part::End(*handle, '}'));
                        pending.push(Part::Record(*handle, 0, depth + 1));
                    }
                    _ => return Err(unsupported()),
                }
            }
            Part::Record(handle, index, depth) => {
                let (record, values) = heap.record(handle, span)?;
                if let Some(value) = values.get(index) {
                    if index != 0 {
                        output.push(",")?;
                    }
                    output.json_string(&record.fields[index].name)?;
                    output.push(":")?;
                    pending.push(Part::Record(handle, index + 1, depth));
                    pending.push(Part::Value(value, depth));
                }
            }
            Part::List(handle, index, depth) => {
                if let Some(value) = heap.list(handle, span)?.get(index) {
                    if index != 0 {
                        output.push(",")?;
                    }
                    pending.push(Part::List(handle, index + 1, depth));
                    pending.push(Part::Value(value, depth));
                }
            }
            Part::Dict(handle, index, depth) => {
                if let Some((position, entry)) = heap.dictionary(handle, span)?.next_from(index) {
                    let Some(key) = heap.text(&entry.original) else {
                        return Err(unsupported());
                    };
                    if index != 0 {
                        output.push(",")?;
                    }
                    output.json_string(key)?;
                    output.push(":")?;
                    pending.push(Part::Dict(handle, position + 1, depth));
                    pending.push(Part::Value(&entry.value, depth));
                }
            }
            Part::End(handle, closing) => {
                active.remove(&handle);
                output.character(closing)?;
            }
        }
    }
    Ok(output.text)
}

fn csv_error(span: Span, input: &str, position: usize, message: &str) -> Diagnostic {
    let before = &input[..position];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    Diagnostic::new(
        "E-CSV-001",
        span,
        format!("CSV {line}번째 줄 {column}번째 글자: {message}"),
    )
}

#[inline(never)]
pub(super) fn read_csv(
    input: &str,
    heap: &mut Heap,
    roots: impl Iterator<Item = Handle> + Clone,
    span: Span,
) -> Result<Value> {
    if input.len() > MAX_TEXT_BYTES {
        return Err(Diagnostic::limit(Limit::Text, span));
    }
    let mut rows = Vec::new();
    let mut width = None;
    let mut position = 0;
    let mut items = 1;
    while position < input.len() {
        if let Some(end) = ["\r\n", "\n"]
            .iter()
            .find(|end| input[position..].starts_with(**end))
        {
            position += end.len();
            continue;
        }
        let mut row = Vec::new();
        loop {
            let mut field = String::new();
            if input.as_bytes().get(position) == Some(&b'"') {
                position += 1;
                loop {
                    let Some(character) = input[position..].chars().next() else {
                        return Err(csv_error(
                            span,
                            input,
                            position,
                            "따옴표가 닫히지 않았습니다.",
                        ));
                    };
                    position += character.len_utf8();
                    if character == '"' {
                        if input.as_bytes().get(position) == Some(&b'"') {
                            position += 1;
                            field.push('"');
                        } else {
                            break;
                        }
                    } else {
                        field.push(character);
                    }
                }
                if input
                    .as_bytes()
                    .get(position)
                    .is_some_and(|byte| !matches!(byte, b',' | b'\r' | b'\n'))
                {
                    return Err(csv_error(
                        span,
                        input,
                        position,
                        "닫는 따옴표 뒤에는 쉼표나 줄 끝이 필요합니다.",
                    ));
                }
            } else {
                let start = position;
                while input
                    .as_bytes()
                    .get(position)
                    .is_some_and(|byte| !matches!(byte, b',' | b'\r' | b'\n'))
                {
                    if input.as_bytes()[position] == b'"' {
                        return Err(csv_error(
                            span,
                            input,
                            position,
                            "필드 안의 따옴표는 필드 전체를 감싸고 두 번 써야 합니다.",
                        ));
                    }
                    position += 1;
                }
                field.push_str(&input[start..position]);
            }
            items += 1;
            if items > MAX_ITEMS {
                return Err(Diagnostic::limit(Limit::Items, span));
            }
            row.push(Node::Scalar(Constant::Text(field.into())));
            if input.as_bytes().get(position) == Some(&b',') {
                position += 1;
                continue;
            }
            if input.as_bytes().get(position) == Some(&b'\r') {
                position += 1;
                if input.as_bytes().get(position) != Some(&b'\n') {
                    return Err(csv_error(
                        span,
                        input,
                        position - 1,
                        "행 끝에는 CRLF 또는 LF를 쓰세요.",
                    ));
                }
            }
            if input.as_bytes().get(position) == Some(&b'\n') {
                position += 1;
            }
            break;
        }
        if width.is_some_and(|width| width != row.len()) {
            return Err(csv_error(
                span,
                input,
                position,
                "각 행의 열 수가 다릅니다.",
            ));
        }
        width = Some(row.len());
        items += 1;
        if items > MAX_ITEMS {
            return Err(Diagnostic::limit(Limit::Items, span));
        }
        rows.push(Node::List(row));
    }
    decode(Node::List(rows), heap, roots, span)
}

#[inline(never)]
pub(super) fn write_csv(value: &Value, heap: &Heap, span: Span) -> Result<String> {
    let invalid = || {
        Diagnostic::new(
            "E-CSV-002",
            span,
            "CSV로 바꿀 값은 열 수가 같은 행들의 목록이어야 하고, 칸에는 문자열, 수, 논리값, 없음만 쓸 수 있습니다.",
        )
    };
    let (Value::List(handle) | Value::Tuple(handle)) = value else {
        return Err(invalid());
    };
    let mut output = TextBuffer::new(span);
    let mut width = None;
    let mut items = 1;
    for row in heap.list(*handle, span)? {
        let (Value::List(handle) | Value::Tuple(handle)) = row else {
            return Err(invalid());
        };
        let row = heap.list(*handle, span)?;
        if row.is_empty() || width.is_some_and(|width| width != row.len()) {
            return Err(invalid());
        }
        width = Some(row.len());
        items += 1 + row.len();
        if items > MAX_ITEMS {
            return Err(Diagnostic::limit(Limit::Items, span));
        }
        for (index, field) in row.iter().enumerate() {
            let field = match field {
                Value::Text(_)
                | Value::Int(_)
                | Value::Big(_)
                | Value::Float(_)
                | Value::Bool(_) => heap.render(field, span)?,
                Value::None => String::new(),
                _ => return Err(invalid()),
            };
            let field = field.as_str();
            if index != 0 {
                output.push(",")?;
            }
            if field.is_empty() || field.contains([',', '"', '\r', '\n']) {
                output.push("\"")?;
                for character in field.chars() {
                    if character == '"' {
                        output.push("\"\"")?;
                    } else {
                        output.character(character)?;
                    }
                }
                output.push("\"")?;
            } else {
                output.push(field)?;
            }
        }
        output.push("\r\n")?;
    }
    Ok(output.text)
}

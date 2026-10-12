use unicode_normalization::UnicodeNormalization;

use super::ast::Role;
use super::tree::{SyntaxToken, SyntaxTokenKind};
use crate::diagnostic::{Diagnostic, Result, Span, code};
use crate::korean::particle::TOPIC;
use crate::limits::{Limit, MAX_BRACKET_DEPTH, MAX_TOKENS};
use crate::runtime::value::Constant;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Kind {
    Word(String),
    Quoted(String),
    Number(String),
    Text(String),
    Template(Vec<Piece>),
    Symbol(&'static str),
    Newline,
    Indent,
    Dedent,
    Eof,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Piece {
    Text(String),
    Code(Span),
}

#[derive(Clone, Debug)]
pub(crate) struct Token {
    pub kind: Kind,
    pub span: Span,
}

impl Token {
    pub fn word(&self, expected: &str) -> bool {
        matches!(&self.kind, Kind::Word(s) if s == expected)
    }

    pub fn symbol(&self, expected: &str) -> bool {
        matches!(&self.kind, Kind::Symbol(s) if *s == expected)
    }

    pub fn opens(&self) -> bool {
        matches!(self.kind, Kind::Symbol("(" | "[" | "{"))
    }

    pub fn closes(&self) -> bool {
        matches!(self.kind, Kind::Symbol(")" | "]" | "}"))
    }
}

pub(crate) fn literal_name_error(span: Span) -> Diagnostic {
    Diagnostic::new(
        "E-NAME-002",
        span,
        "리터럴을 이름으로 쓰려면 역따옴표로 감싸세요.",
    )
}

pub(crate) fn unclosed_bracket(span: Span) -> Diagnostic {
    Diagnostic::new(code::UNCLOSED_BRACKET, span, "괄호가 닫히지 않았습니다.")
}

fn unclosed_quoted_name(span: Span) -> Diagnostic {
    Diagnostic::new(
        "E-NAME-003",
        span,
        "비어 있거나 닫히지 않은 인용 이름입니다.",
    )
}

pub(crate) fn normalize(raw: &str) -> String {
    raw.nfc().collect()
}

pub(crate) fn literal(word: &str) -> Option<Constant> {
    match word {
        "참" => Some(Constant::Bool(true)),
        "거짓" => Some(Constant::Bool(false)),
        "없음" => Some(Constant::None),
        _ => None,
    }
}

pub(crate) fn radix(spelling: &str) -> (u32, &str) {
    let radix = match spelling.get(..2) {
        Some("0x" | "0X") => 16,
        Some("0o" | "0O") => 8,
        Some("0b" | "0B") => 2,
        _ => return (10, spelling),
    };
    (radix, &spelling[2..])
}

pub(crate) fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    // 유니코드 15.1의 XID_Continue에 든 Cf 문자는 이 둘뿐입니다.
    let allowed = |c| c != '\u{200c}' && c != '\u{200d}';
    chars
        .next()
        .is_some_and(|c| c == '_' || unicode_ident::is_xid_start(c))
        && chars.all(|c| unicode_ident::is_xid_continue(c) && allowed(c))
}

pub(crate) fn name(raw: &str, span: Span, allow_literal: bool) -> Result<Token> {
    let quoted = raw.starts_with('`');
    let body = if quoted {
        if raw.len() < 3 || !raw.ends_with('`') {
            return Err(unclosed_quoted_name(span));
        }
        &raw[1..raw.len() - 1]
    } else {
        raw
    };
    let name = normalize(body);
    if !valid_name(&name) {
        return Err(Diagnostic::new(
            "E-NAME-001",
            span,
            format!("'{name}'{} 이름으로 쓸 수 없습니다.", TOPIC.after(&name)),
        ));
    }
    if !quoted && !allow_literal && literal(&name).is_some() {
        return Err(literal_name_error(span));
    }
    Ok(Token {
        kind: if quoted {
            Kind::Quoted(name)
        } else {
            Kind::Word(name)
        },
        span,
    })
}

pub(crate) struct Scanned {
    pub tokens: Vec<Token>,
    pub syntax_tokens: Vec<SyntaxToken>,
    pub diagnostic: Option<Diagnostic>,
}

pub(crate) fn scan_lossless(source: &str, base: usize) -> Scanned {
    let mut scanned = scan_local(source);
    if base != 0 {
        for token in &mut scanned.tokens {
            token.span = token.span.shift(base);
            if let Kind::Template(pieces) = &mut token.kind {
                for piece in pieces {
                    if let Piece::Code(span) = piece {
                        *span = span.shift(base);
                    }
                }
            }
        }
        for token in &mut scanned.syntax_tokens {
            token.span = token.span.shift(base);
        }
        if let Some(error) = &mut scanned.diagnostic {
            error.span = error.span.shift(base);
        }
    }
    scanned
}

fn scan_local(source: &str) -> Scanned {
    let result = validate_source(source);
    if let Err(error) = result {
        return Scanned {
            tokens: vec![],
            syntax_tokens: vec![SyntaxToken {
                kind: SyntaxTokenKind::Error,
                span: Span::new(0, source.len()),
            }],
            diagnostic: Some(error),
        };
    }
    let bom = usize::from(source.starts_with('\u{feff}')) * 3;
    let mut scanner = Scanner {
        source,
        pos: bom,
        tokens: Vec::new(),
        brackets: Vec::new(),
        syntax_tokens: Vec::new(),
    };
    if bom != 0 {
        scanner.push_syntax_token(0, SyntaxTokenKind::Bom);
    }
    let diagnostic = scanner.run().err();
    if diagnostic.is_some() {
        let start = scanner.syntax_tokens.last().map_or(0, |t| t.span.end);
        if start < source.len() {
            scanner.syntax_tokens.push(SyntaxToken {
                kind: SyntaxTokenKind::Error,
                span: Span::new(start, source.len()),
            });
        }
    }
    Scanned {
        tokens: scanner.tokens,
        syntax_tokens: scanner.syntax_tokens,
        diagnostic,
    }
}

fn validate_source(source: &str) -> Result<()> {
    for (i, c) in source.char_indices() {
        if (c < ' ' && !matches!(c, '\n' | '\r' | '\t')) || c == '\u{7f}' {
            return Err(Diagnostic::new(
                "E-SRC-003",
                Span::new(i, i + c.len_utf8()),
                "소스에 금지된 제어 문자가 있습니다.",
            ));
        }
        if c == '\r' && source.as_bytes().get(i + 1) != Some(&b'\n') {
            return Err(Diagnostic::new(
                "E-SRC-002",
                Span::new(i, i + 1),
                "줄바꿈은 LF 또는 CRLF를 사용하세요.",
            ));
        }
    }
    Ok(())
}

struct Scanner<'a> {
    source: &'a str,
    pos: usize,
    tokens: Vec<Token>,
    brackets: Vec<(&'static str, Span)>,
    syntax_tokens: Vec<SyntaxToken>,
}

impl Scanner<'_> {
    fn peek(&self) -> Option<char> {
        self.source[self.pos..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn push(&mut self, start: usize, kind: Kind) -> Result<()> {
        if self.tokens.len() >= MAX_TOKENS {
            return Err(Diagnostic::limit(Limit::Tokens, Span::new(start, self.pos)));
        }
        let concrete = match &kind {
            Kind::Word(_) => Some(SyntaxTokenKind::Word),
            Kind::Quoted(_) => Some(SyntaxTokenKind::QuotedName),
            Kind::Number(_) => Some(SyntaxTokenKind::Number),
            Kind::Text(_) | Kind::Template(_) => Some(SyntaxTokenKind::String),
            Kind::Symbol(symbol) => Some(SyntaxTokenKind::Symbol(symbol)),
            Kind::Newline => Some(SyntaxTokenKind::Newline),
            Kind::Eof => None,
            Kind::Indent | Kind::Dedent => unreachable!("레이아웃 토큰은 원문에 없습니다"),
        };
        if let Some(kind) = concrete {
            self.push_syntax_token(start, kind);
        }
        self.tokens.push(Token {
            kind,
            span: Span::new(start, self.pos),
        });
        Ok(())
    }

    fn push_syntax_token(&mut self, start: usize, kind: SyntaxTokenKind) {
        self.syntax_tokens.push(SyntaxToken {
            kind,
            span: Span::new(start, self.pos),
        });
    }

    fn error(&self, code: &'static str, start: usize, message: &str) -> Diagnostic {
        Diagnostic::new(code, Span::new(start, self.pos), message)
    }

    fn unclosed_text(&self, start: usize) -> Diagnostic {
        self.error(code::UNCLOSED_TEXT, start, "문자열이 닫히지 않았습니다.")
    }

    fn run(&mut self) -> Result<()> {
        while let Some(c) = self.peek() {
            let start = self.pos;
            match c {
                ' ' => {
                    while self.peek() == Some(' ') {
                        self.bump();
                    }
                    self.push_syntax_token(start, SyntaxTokenKind::Whitespace);
                }
                '#' => {
                    while !matches!(self.peek(), None | Some('\r' | '\n')) {
                        self.bump();
                    }
                    self.push_syntax_token(start, SyntaxTokenKind::Comment);
                }
                '\r' | '\n' => self.newline(c, start)?,
                c if c.is_whitespace() => {
                    self.bump();
                    return Err(self.error(
                        "E-SRC-004",
                        start,
                        "공백은 스페이스만 씁니다. 탭 같은 공백은 스페이스로 바꾸세요.",
                    ));
                }
                '`' => self.quoted_name(start)?,
                '\'' | '"' => self.string(start, c)?,
                c if c.is_ascii_digit() || (c == '.' && self.digit_follows()) => {
                    self.number(start)?
                }
                '(' | '[' | '{' => self.open(c, start)?,
                ')' | ']' | '}' => self.close(c, start)?,
                _ => self.symbol_or_word(c, start)?,
            }
        }
        if let Some((_, span)) = self.brackets.last() {
            return Err(unclosed_bracket(*span));
        }
        self.push(self.pos, Kind::Eof)?;
        Ok(())
    }

    fn digit_follows(&self) -> bool {
        self.source
            .as_bytes()
            .get(self.pos + 1)
            .is_some_and(u8::is_ascii_digit)
    }

    fn newline(&mut self, c: char, start: usize) -> Result<()> {
        self.bump();
        if c == '\r' {
            self.bump();
        }
        if self.brackets.is_empty() {
            self.push(start, Kind::Newline)?;
        } else {
            self.push_syntax_token(start, SyntaxTokenKind::Newline);
        }
        Ok(())
    }

    fn quoted_name(&mut self, start: usize) -> Result<()> {
        self.bump();
        while !matches!(self.peek(), None | Some('`' | '\r' | '\n')) {
            self.bump();
        }
        if self.peek() != Some('`') {
            return Err(unclosed_quoted_name(Span::new(start, self.pos)));
        }
        self.bump();
        let token = name(
            &self.source[start..self.pos],
            Span::new(start, self.pos),
            true,
        )?;
        self.push(start, token.kind)
    }

    fn open(&mut self, c: char, start: usize) -> Result<()> {
        self.bump();
        if self.brackets.len() >= MAX_BRACKET_DEPTH {
            return Err(Diagnostic::limit(
                Limit::BracketDepth,
                Span::new(start, self.pos),
            ));
        }
        let symbol = match c {
            '(' => "(",
            '[' => "[",
            _ => "{",
        };
        self.brackets.push((symbol, Span::new(start, self.pos)));
        self.push(start, Kind::Symbol(symbol))
    }

    fn close(&mut self, c: char, start: usize) -> Result<()> {
        self.bump();
        let (opening, closing) = match c {
            ')' => ("(", ")"),
            ']' => ("[", "]"),
            _ => ("{", "}"),
        };
        if self
            .brackets
            .pop()
            .is_none_or(|(symbol, _)| symbol != opening)
        {
            return Err(self.error("E-DELIM-001", start, "여는 괄호가 없는 닫는 괄호입니다."));
        }
        self.push(start, Kind::Symbol(closing))
    }

    fn symbol_or_word(&mut self, c: char, start: usize) -> Result<()> {
        const SYMBOLS: [&str; 17] = [
            "**", "//", "==", "!=", "<=", ">=", "+", "-", "*", "/", "%", "<", ">", ".", ",", ":",
            "=",
        ];
        if let Some(symbol) = SYMBOLS
            .into_iter()
            .find(|s| self.source[self.pos..].starts_with(s))
        {
            self.pos += symbol.len();
            return self.push(start, Kind::Symbol(symbol));
        }
        if matches!(c, '!' | ';' | '\\' | '@' | '$' | '&' | '|' | '^' | '~') {
            self.bump();
            return Err(self.error(
                "E-TOKEN-001",
                start,
                &format!("쓸 수 없는 기호입니다: '{c}'"),
            ));
        }
        self.word_end();
        let token = name(
            &self.source[start..self.pos],
            Span::new(start, self.pos),
            true,
        )?;
        self.push(start, token.kind)
    }

    fn word_end(&mut self) {
        while self
            .peek()
            .is_some_and(|c| !c.is_whitespace() && !"#`\"'()[]{}:,.+-*/%<>=!;\\@$&|^~".contains(c))
        {
            self.bump();
        }
    }

    fn digits(&mut self, radix: u32) -> bool {
        let mut any = false;
        let mut previous_digit = false;
        let mut valid = true;
        while let Some(c) = self.peek() {
            if c.is_ascii() && c.is_digit(radix) {
                any = true;
                previous_digit = true;
                self.bump();
            } else if c == '_' {
                valid &= previous_digit;
                previous_digit = false;
                self.bump();
            } else {
                break;
            }
        }
        any && previous_digit && valid
    }

    fn number(&mut self, start: usize) -> Result<()> {
        let leading_dot = self.peek() == Some('.');
        if leading_dot {
            self.bump();
        }
        let (radix, _) = radix(&self.source[self.pos..]);
        if radix != 10 {
            self.pos += 2;
        }
        let mut valid = self.digits(radix) && !leading_dot;
        let mut float = leading_dot;
        if radix == 10 {
            if self.peek() == Some('.') && self.digit_follows() {
                float = true;
                self.bump();
                valid &= self.digits(10);
            }
            if matches!(self.peek(), Some('e' | 'E')) {
                float = true;
                self.bump();
                if matches!(self.peek(), Some('+' | '-')) {
                    self.bump();
                }
                valid &= self.digits(10);
            }
        }
        let number_end = self.pos;
        let number = &self.source[start..number_end];
        if radix == 10 && !float && number.starts_with('0') && number.len() > 1 {
            valid = false;
        }
        self.word_end();
        let tail = normalize(&self.source[number_end..self.pos]);
        let tail = tail.as_str();
        if !tail.is_empty()
            && !Role::PARTICLES.contains(&tail)
            && ![
                "의",
                "이다",
                "이면",
                "인",
                "이고",
                "이거나",
                "인지",
                "다",
                "면",
                "고",
                "거나",
                "부터",
                "까지",
                "보다",
            ]
            .contains(&tail)
        {
            valid = false;
        }
        if !valid {
            return Err(self.error(
                "E-NUM-001",
                start,
                "숫자를 읽을 수 없습니다. 숫자 뒤에 조사가 아닌 글자가 붙었는지 확인하세요.",
            ));
        }
        self.pos = number_end;
        self.push(start, Kind::Number(number.to_owned()))
    }

    fn string(&mut self, start: usize, quote: char) -> Result<()> {
        let unsupported_start = self
            .tokens
            .last()
            .filter(|t| {
                matches!(t.kind, Kind::Text(_) | Kind::Template(_))
                    || (matches!(t.kind, Kind::Word(_)) && t.span.end == start)
            })
            .map(|t| t.span.start);
        let triple = quote.to_string().repeat(3);
        if self.source[start..].starts_with(&triple) {
            let (text, pieces) = self.text_block(start, quote)?;
            return self.finish_text(start, unsupported_start, text, pieces);
        }
        self.bump();
        let mut text = String::new();
        let mut pieces = Vec::new();
        loop {
            let Some(c) = self.bump() else {
                return Err(self.unclosed_text(start));
            };
            if c == quote {
                break;
            }
            if matches!(c, '\r' | '\n') {
                return Err(self.error(
                    code::UNCLOSED_TEXT,
                    start,
                    "문자열 안에 실제 줄바꿈을 넣을 수 없습니다. 여러 줄은 \"\"\"로 씁니다.",
                ));
            }
            if quote == '\'' {
                text.push(c);
            } else {
                self.character(c, start, &mut text, &mut pieces)?;
            }
        }
        self.finish_text(start, unsupported_start, text, pieces)
    }

    fn text_block(&mut self, start: usize, quote: char) -> Result<(String, Vec<Piece>)> {
        let unclosed = |lexer: &Self| {
            lexer.error(
                code::UNCLOSED_TEXT,
                start,
                "여러 줄 문자열이 닫히지 않았습니다.",
            )
        };
        let body = start + 3;
        let close = body + closing(&self.source[body..], quote).ok_or_else(|| unclosed(self))?;
        let raw = &self.source[body..close];
        let first = match raw.find('\n') {
            Some(index) if raw[..index].trim_end_matches('\r').trim().is_empty() => index + 1,
            _ => 0,
        };
        let last_line = raw.rfind('\n').map_or(0, |index| index + 1);
        let (end, indent) = if last_line >= first && raw[last_line..].chars().all(|c| c == ' ') {
            let end = raw[..last_line.saturating_sub(1)]
                .trim_end_matches('\r')
                .len();
            (body + end.max(first), raw.len() - last_line)
        } else {
            (close, 0)
        };
        self.pos = body + first;
        let mut text = String::new();
        let mut pieces = Vec::new();
        let mut line_start = first > 0;
        while self.pos < end {
            if line_start {
                line_start = false;
                let spaces = self.source[self.pos..end]
                    .bytes()
                    .take(indent)
                    .take_while(|&b| b == b' ')
                    .count();
                let blank = self.source[self.pos + spaces..end].starts_with(['\r', '\n'])
                    || self.pos + spaces == end;
                if spaces < indent && !blank {
                    return Err(self.error(
                        "E-STR-005",
                        self.pos,
                        "여러 줄 문자열의 줄은 닫는 따옴표만큼 들여쓰세요.",
                    ));
                }
                self.pos += spaces;
                continue;
            }
            let c = self.bump().expect("끝에 닿기 전입니다");
            match c {
                '\r' if self.peek() == Some('\n') => {}
                '\n' => {
                    text.push('\n');
                    line_start = true;
                }
                c if quote == '\'' => text.push(c),
                c => self.character(c, start, &mut text, &mut pieces)?,
            }
        }
        self.pos = close + 3;
        Ok((text, pieces))
    }

    fn character(
        &mut self,
        c: char,
        start: usize,
        text: &mut String,
        pieces: &mut Vec<Piece>,
    ) -> Result<()> {
        if matches!(c, '{' | '}') && self.peek() == Some(c) {
            self.bump();
            text.push(c);
            return Ok(());
        }
        if c == '}' {
            return Err(self.error(
                "E-STR-004",
                self.pos - 1,
                "'}'를 글자로 쓰려면 '}}'로 씁니다.",
            ));
        }
        if c == '{' {
            if !text.is_empty() {
                pieces.push(Piece::Text(std::mem::take(text)));
            }
            pieces.push(Piece::Code(self.interpolation()?));
            return Ok(());
        }
        if c != '\\' {
            text.push(c);
            return Ok(());
        }
        let escape = self.pos - 1;
        let Some(c) = self.bump() else {
            return Err(self.unclosed_text(start));
        };
        match c {
            '\\' | '\'' | '"' => text.push(c),
            'n' => text.push('\n'),
            'r' => text.push('\r'),
            't' => text.push('\t'),
            'u' | 'U' => {
                let mut scalar = 0;
                for _ in 0..if c == 'u' { 4 } else { 8 } {
                    let digit = self
                        .bump()
                        .filter(char::is_ascii)
                        .and_then(|c| c.to_digit(16));
                    let Some(digit) = digit else {
                        return Err(self.error(
                            "E-STR-002",
                            escape,
                            "Unicode 이스케이프에는 16진 숫자가 필요합니다.",
                        ));
                    };
                    scalar = scalar * 16 + digit;
                }
                text.push(char::from_u32(scalar).ok_or_else(|| {
                    self.error("E-STR-002", escape, "Unicode 문자가 아닌 값입니다.")
                })?);
            }
            _ => {
                return Err(self.error("E-STR-002", escape, r#"쓸 수 없는 이스케이프입니다. 쓸 수 있는 것은 \n \r \t \" \' \\ \u \U입니다."#));
            }
        }
        Ok(())
    }

    fn finish_text(
        &mut self,
        start: usize,
        unsupported_start: Option<usize>,
        mut text: String,
        mut pieces: Vec<Piece>,
    ) -> Result<()> {
        if let Some(start) = unsupported_start {
            return Err(self.error(
                "E-STR-003",
                start,
                "문자열 앞에 접두사를 붙이거나 두 문자열을 나란히 이어 쓸 수 없습니다.",
            ));
        }
        if pieces.is_empty() {
            return self.push(start, Kind::Text(text));
        }
        if !text.is_empty() {
            pieces.push(Piece::Text(std::mem::take(&mut text)));
        }
        self.push(start, Kind::Template(pieces))
    }

    fn interpolation(&mut self) -> Result<Span> {
        let open = self.pos - 1;
        let start = self.pos;
        let Some(length) = code_length(&self.source[start..]) else {
            return Err(self.error(
                "E-STR-004",
                open,
                "'{'를 닫는 '}'가 없습니다. 글자 그대로 쓰려면 '{{'로 씁니다.",
            ));
        };
        self.pos = start + length + 1;
        let span = Span::new(start, start + length);
        if self.source[span.start..span.end].trim().is_empty() {
            return Err(self.error(
                "E-STR-004",
                open,
                "'{' 안에 끼워 넣을 식이 필요합니다. 글자 그대로 쓰려면 '{{'로 씁니다.",
            ));
        }
        Ok(span)
    }
}

fn closing(text: &str, quote: char) -> Option<usize> {
    let triple = [quote; 3].iter().collect::<String>();
    let mut index = 0;
    while let Some(c) = text[index..].chars().next() {
        if text[index..].starts_with(&triple) {
            return Some(index);
        }
        index += c.len_utf8();
        if c == '\\' && quote == '"' {
            index += text[index..].chars().next().map_or(0, char::len_utf8);
        }
    }
    None
}

pub(crate) fn code_length(code: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut index = 0;
    while let Some(c) = code[index..].chars().next() {
        match c {
            '\r' | '\n' => return None,
            '"' | '\'' => {
                index += text_length(&code[index..])?;
                continue;
            }
            '{' => depth += 1,
            '}' if depth == 0 => return Some(index),
            '}' => depth -= 1,
            _ => {}
        }
        index += c.len_utf8();
    }
    None
}

pub(crate) fn text_length(text: &str) -> Option<usize> {
    let quote = text.chars().next()?;
    let mut index = quote.len_utf8();
    while let Some(c) = text[index..].chars().next() {
        index += c.len_utf8();
        match c {
            '\r' | '\n' => return None,
            c if c == quote => return Some(index),
            '\\' if quote == '"' => {
                index += text[index..].chars().next()?.len_utf8();
            }
            '{' if quote == '"' && text[index..].starts_with('{') => index += 1,
            '{' if quote == '"' => index += code_length(&text[index..])? + 1,
            _ => {}
        }
    }
    None
}

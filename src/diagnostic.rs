use std::fmt;
use std::io::{self, Write};

use unicode_width::UnicodeWidthStr;

pub mod code {
    pub const UNRESOLVED: &str = "E-RESOLVE-001";
    pub const INTERNAL: &str = "E-INTERNAL-001";
    pub const LIMIT: &str = "E-LIMIT-001";
    pub const MEMORY: &str = "E-MEMORY-001";
    pub const NO_PIPE_TARGET: &str = "E-CALL-004";
    pub const UNCLOSED_TEXT: &str = "E-STR-001";
    pub const UNCLOSED_BRACKET: &str = "E-DELIM-002";
    pub const OPEN_CONNECTIVE: &str = "E-LAYOUT-005";
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub(crate) fn through(self, other: Self) -> Self {
        Self::new(self.start, other.end)
    }

    pub(crate) fn shift(self, offset: usize) -> Self {
        Self::new(self.start + offset, self.end + offset)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub span: Span,
    pub message: Box<str>,
    pub help: Option<Box<str>>,
    pub fix: Option<Box<Fix>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    pub span: Span,
    pub text: Box<str>,
}

impl Diagnostic {
    pub(crate) fn new(code: &'static str, span: Span, message: impl Into<String>) -> Self {
        Self {
            code,
            span,
            message: message.into().into_boxed_str(),
            help: None,
            fix: None,
        }
    }

    pub(crate) fn is_fatal(&self) -> bool {
        matches!(self.code, code::INTERNAL | code::LIMIT | code::MEMORY)
    }

    pub(crate) fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into().into_boxed_str());
        self
    }

    pub(crate) fn with_fix(mut self, span: Span, text: impl Into<String>) -> Self {
        self.fix = Some(Box::new(Fix {
            span,
            text: text.into().into_boxed_str(),
        }));
        self
    }

    pub(crate) fn limit(limit: crate::limits::Limit, span: Span) -> Self {
        Self::new(code::LIMIT, span, limit.message())
    }

    pub fn location(&self, source: &[u8]) -> (usize, usize) {
        line_column(source, self.span.start)
    }
}

pub(crate) fn line_column(source: &[u8], offset: usize) -> (usize, usize) {
    let end = offset.min(source.len());
    let prefix = String::from_utf8_lossy(&source[..end]);
    let line = prefix.bytes().filter(|&c| c == b'\n').count() + 1;
    let last = prefix.rsplit('\n').next().unwrap_or("");
    let last = if line == 1 {
        last.trim_start_matches('\u{feff}')
    } else {
        last
    };
    (line, last.chars().count() + 1)
}

pub(crate) fn internal(span: Span) -> Diagnostic {
    Diagnostic::new(
        code::INTERNAL,
        span,
        "실행기 내부 상태가 일치하지 않습니다. 실행을 종료합니다.",
    )
}

pub(crate) fn memory(span: Span) -> Diagnostic {
    Diagnostic::new(code::MEMORY, span, "메모리 공간을 확보하지 못했습니다.")
}

pub(crate) fn type_error(span: Span) -> Diagnostic {
    Diagnostic::new(
        "E-TYPE-001",
        span,
        "이 종류의 값에는 이 연산을 쓸 수 없습니다.",
    )
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Diagnostic {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallFrame {
    pub name: String,
    pub span: Span,
    pub clause: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExceptionCause {
    pub diagnostic: Diagnostic,
    pub frames: Box<[CallFrame]>,
    pub omitted_frames: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionError {
    pub diagnostic: Diagnostic,
    pub frames: Box<[CallFrame]>,
    pub omitted_frames: u32,
    pub causes: Box<[ExceptionCause]>,
    pub omitted_causes: u32,
    pub exit_code: Option<u8>,
}

impl From<Diagnostic> for ExecutionError {
    fn from(diagnostic: Diagnostic) -> Self {
        Self {
            diagnostic,
            frames: Box::default(),
            omitted_frames: 0,
            causes: Box::default(),
            omitted_causes: 0,
            exit_code: None,
        }
    }
}

impl ExecutionError {
    pub(crate) fn exit(code: u8, span: Span) -> Self {
        Self {
            exit_code: Some(code),
            ..Diagnostic::new(
                "E-EXIT-001",
                span,
                format!("실행을 끝냈습니다(종료 코드 {code})."),
            )
            .into()
        }
    }

    pub(crate) fn unless_success(self) -> std::result::Result<(), Self> {
        if self.exit_code == Some(0) {
            Ok(())
        } else {
            Err(self)
        }
    }

    pub fn write_to(&self, label: &str, source: &[u8], output: &mut impl Write) -> io::Result<()> {
        self.write_locations(output, |span| {
            let (line, column) = line_column(source, span.start);
            (format!("{label}:{line}:{column}"), snippet(source, span))
        })
    }

    pub(crate) fn write_locations(
        &self,
        output: &mut impl Write,
        location: impl Fn(Span) -> (String, Option<Snippet>),
    ) -> io::Result<()> {
        if self.exit_code.is_some() {
            return Ok(());
        }
        write_record(
            output,
            &location,
            &self.diagnostic,
            &self.frames,
            self.omitted_frames,
        )?;
        for (index, cause) in self.causes.iter().enumerate() {
            writeln!(output, "  앞선 오류 {} (처리 중 새 오류 발생):", index + 1)?;
            write_record(
                output,
                &location,
                &cause.diagnostic,
                &cause.frames,
                cause.omitted_frames,
            )?;
        }
        if self.omitted_causes != 0 {
            writeln!(output, "  더 앞선 오류 {}개 생략", self.omitted_causes)?;
        }
        Ok(())
    }
}

pub(crate) struct Snippet {
    line: usize,
    text: String,
    indent: usize,
    width: usize,
}

pub(crate) fn snippet(source: &[u8], span: Span) -> Option<Snippet> {
    let start = span.start.min(source.len());
    let text = std::str::from_utf8(source).ok()?;
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    let line = text[line_start..line_end].trim_end_matches('\r');
    let before = &text[line_start..start];
    let end = span.end.clamp(start, line_start + line.len());
    let marked = text.get(start..end).unwrap_or("");
    Some(Snippet {
        line: text[..start].matches('\n').count() + 1,
        text: line.trim_start_matches('\u{feff}').replace('\t', " "),
        indent: before.trim_start_matches('\u{feff}').width(),
        width: marked.width().max(1),
    })
}

fn write_record(
    output: &mut impl Write,
    location: &impl Fn(Span) -> (String, Option<Snippet>),
    diagnostic: &Diagnostic,
    frames: &[CallFrame],
    omitted_frames: u32,
) -> io::Result<()> {
    let (place, snippet) = location(diagnostic.span);
    writeln!(output, "{place}: {diagnostic}")?;
    if let Some(snippet) = snippet {
        let number = snippet.line.to_string();
        let gutter = " ".repeat(number.len());
        writeln!(output, "  {number} | {}", snippet.text)?;
        writeln!(
            output,
            "  {gutter} | {}{}",
            " ".repeat(snippet.indent),
            "^".repeat(snippet.width)
        )?;
    }
    if let Some(help) = &diagnostic.help {
        writeln!(output, "  도움말: {help}")?;
    }
    if !frames.is_empty() {
        writeln!(output, "  호출 경로 (가장 안쪽부터):")?;
        for frame in frames {
            write!(output, "    {}: {}", frame.name, location(frame.span).0)?;
            if let Some(clause) = frame.clause {
                write!(output, " (연결 {clause}번째 절)")?;
            }
            writeln!(output)?;
        }
    }
    if omitted_frames != 0 {
        writeln!(output, "    바깥쪽 호출 {}개 생략", omitted_frames)?;
    }
    Ok(())
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.diagnostic.fmt(f)
    }
}

impl std::error::Error for ExecutionError {}

pub(crate) type Result<T> = std::result::Result<T, Diagnostic>;

pub fn display_path(path: &std::path::Path) -> String {
    let text = path.display().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        text
    }
}

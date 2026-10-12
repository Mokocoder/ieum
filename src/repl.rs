use std::io::{self, BufRead, Read, Write};

use crate::check::{self, ReplNames};
use crate::diagnostic::{
    Diagnostic, ExecutionError, Result, Snippet, Span, code, line_column, memory, snippet,
};
use crate::limits::{MAX_REPL_INPUTS, MAX_SOURCE_BYTES};
use crate::runtime::{ReplInput, ReplState};
use crate::stdlib::Reach;
use crate::syntax::ast::{Expr, Statement};
use crate::syntax::{self, Unfinished};
use crate::{FileSystem, Limits};

#[derive(Clone, Copy, Debug, Default)]
pub struct ReplOptions<'a> {
    pub prompts: bool,
    pub limits: Limits,
    pub filesystem: Option<&'a FileSystem>,
    pub network: bool,
    pub system: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReplOutcome {
    pub had_errors: bool,
}

enum Compiled {
    Statements(Vec<Statement>),
    Expression(Expr),
}

struct Entry {
    number: usize,
    span: Span,
}

#[derive(Default)]
struct History {
    text: String,
    entries: Vec<Entry>,
}

impl History {
    fn append(&mut self, source: &str, number: usize) -> Result<()> {
        let separator = usize::from(!source.ends_with('\n'));
        let start = self.text.len();
        if start + source.len() + separator > MAX_SOURCE_BYTES {
            return Err(session_limit(Span::new(0, source.len())));
        }
        self.text
            .try_reserve(source.len() + separator)
            .map_err(|_| memory(Span::new(0, source.len())))?;
        self.entries
            .try_reserve(1)
            .map_err(|_| memory(Span::new(0, source.len())))?;
        self.text.push_str(source);
        self.entries.push(Entry {
            number,
            span: Span::new(start, self.text.len()),
        });
        if separator != 0 {
            self.text.push('\n');
        }
        Ok(())
    }

    fn location(&self, span: Span) -> (String, Option<Snippet>) {
        let Some(entry) = self
            .entries
            .iter()
            .rev()
            .find(|entry| entry.span.start <= span.start && span.start <= entry.span.end)
        else {
            return ("<대화형>:1:1".into(), None);
        };
        let source = &self.text.as_bytes()[entry.span.start..entry.span.end];
        let local = Span::new(span.start - entry.span.start, span.end - entry.span.start);
        let (line, column) = line_column(source, local.start);
        let place = format!("<입력 {}>:{line}:{column}", entry.number);
        (place, snippet(source, local))
    }
}

fn session_limit(span: Span) -> Diagnostic {
    Diagnostic::new(
        code::LIMIT,
        span,
        "대화형 입력 보관 한도를 넘었습니다. :초기화로 새 세션을 시작하세요.",
    )
}

fn compile_input(source: &[u8], base: usize, names: &ReplNames) -> Result<(Compiled, ReplNames)> {
    let mut body = match syntax::parse_at(source, base) {
        Ok(body) => body,
        Err(errors) => {
            let error = errors
                .into_iter()
                .next()
                .expect("구문 분석에 실패하면 오류가 있습니다");
            let expression = syntax::source_text(source)
                .ok()
                .and_then(|text| syntax::parse_expression(text, base));
            let Some(mut expression) = expression else {
                return Err(error);
            };
            check::prepare_repl_expression(&mut expression, names)?;
            return Ok((Compiled::Expression(expression), names.clone()));
        }
    };
    let names = check::prepare_repl(&mut body, names)?;
    Ok((Compiled::Statements(body), names))
}

struct Session {
    state: ReplState,
    names: ReplNames,
    history: History,
    ended: bool,
}

impl Session {
    fn new(options: ReplOptions) -> Result<Self> {
        Ok(Self {
            state: ReplState::new(
                options.limits,
                options.filesystem,
                Reach {
                    network: options.network,
                    system: options.system,
                },
            )?,
            names: check::repl_names(),
            history: History::default(),
            ended: false,
        })
    }

    fn submit(
        &mut self,
        source: &[u8],
        number: usize,
        output: &mut impl Write,
        errors: &mut impl Write,
    ) -> io::Result<bool> {
        let label = format!("<입력 {number}>");
        let base = self.history.text.len();
        let (compiled, names) = match compile_input(source, base, &self.names) {
            Ok(compiled) => compiled,
            Err(mut error) => {
                error.span = Span::new(
                    error.span.start.saturating_sub(base),
                    error.span.end.saturating_sub(base),
                );
                ExecutionError::from(error).write_to(&label, source, errors)?;
                return Ok(false);
            }
        };
        if matches!(&compiled, Compiled::Statements(body) if body.is_empty()) {
            return Ok(true);
        }
        let appended = if self.history.entries.len() >= MAX_REPL_INPUTS {
            Err(session_limit(Span::new(0, source.len())))
        } else {
            let text = std::str::from_utf8(source).expect("컴파일한 입력은 UTF-8입니다");
            self.history.append(text, number)
        };
        if let Err(error) = appended {
            ExecutionError::from(error).write_to(&label, source, errors)?;
            return Ok(false);
        }
        let input = match &compiled {
            Compiled::Statements(body) => ReplInput::Statements(body),
            Compiled::Expression(expr) => ReplInput::Expression(expr),
        };
        self.names = names;
        let result = self.state.run(input, output);
        if let Err(error) = result {
            self.ended = error.diagnostic.code == code::INTERNAL || error.exit_code.is_some();
            error.write_locations(errors, |span| self.history.location(span))?;
            Ok(error.exit_code == Some(0))
        } else {
            Ok(true)
        }
    }
}

fn help(output: &mut impl Write) -> io::Result<()> {
    writeln!(
        output,
        "이음 대화형 실행기\n문장은 마침표로 끝냅니다. 식을 입력하면 값을 바로 보여 줍니다.\n블록은 빈 줄을 입력하면 실행합니다. :취소는 작성 중인 입력을 버립니다.\n:도움말 / :help   도움말\n:초기화 / :reset  변수, 함수, 입력 기록 초기화\n:취소 / :cancel   현재 입력 취소\n:끝 / :quit       종료"
    )
}

fn read_line(input: &mut impl BufRead) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    input
        .by_ref()
        .take(MAX_SOURCE_BYTES as u64 + 1)
        .read_until(b'\n', &mut line)?;
    if line.len() > MAX_SOURCE_BYTES && !line.ends_with(b"\n") {
        loop {
            let bytes = input.fill_buf()?;
            if bytes.is_empty() {
                break;
            }
            let end = bytes.iter().position(|&c| c == b'\n');
            let count = end.map_or(bytes.len(), |index| index + 1);
            input.consume(count);
            if end.is_some() {
                break;
            }
        }
    }
    Ok(line)
}

enum Command {
    Quit,
    Help,
    Reset,
    Cancel,
    Unknown,
}

fn command(line: &[u8]) -> Option<Command> {
    let text = std::str::from_utf8(line)
        .ok()
        .filter(|_| line.len() <= MAX_SOURCE_BYTES)?;
    Some(match text.trim() {
        ":끝" | ":quit" | ":exit" => Command::Quit,
        ":도움말" | ":help" => Command::Help,
        ":초기화" | ":reset" => Command::Reset,
        ":취소" | ":cancel" => Command::Cancel,
        command if command.starts_with(':') => Command::Unknown,
        _ => return None,
    })
}

enum End {
    Quit,
    Reset,
}

fn read_session(
    session: &mut Session,
    input: &mut impl BufRead,
    output: &mut impl Write,
    errors: &mut impl Write,
    prompts: bool,
    outcome: &mut ReplOutcome,
) -> io::Result<End> {
    let mut pending = Vec::new();
    let mut block = false;
    let mut number = 0;
    loop {
        if prompts {
            write!(
                errors,
                "{}",
                if pending.is_empty() { ">>> " } else { "... " }
            )?;
            errors.flush()?;
        }
        let line = read_line(input)?;
        if line.is_empty() {
            if !pending.is_empty() {
                outcome.had_errors |= !session.submit(&pending, number + 1, output, errors)?;
            }
            return Ok(End::Quit);
        }
        if let Some(command) = command(&line) {
            match command {
                Command::Quit => return Ok(End::Quit),
                Command::Help => help(errors)?,
                Command::Reset => {
                    writeln!(errors, "세션을 초기화했습니다.")?;
                    return Ok(End::Reset);
                }
                Command::Cancel => {
                    pending.clear();
                    block = false;
                    writeln!(errors, "현재 입력을 취소했습니다.")?;
                }
                Command::Unknown => {
                    writeln!(
                        errors,
                        "E-REPL-001: 알 수 없는 대화형 명령입니다. :도움말을 참고하세요."
                    )?;
                    outcome.had_errors = true;
                }
            }
            continue;
        }
        let blank = line.iter().all(|c| matches!(c, b' ' | b'\r' | b'\n'));
        if pending.is_empty() && blank {
            continue;
        }
        pending.extend_from_slice(&line);
        if pending.len() <= MAX_SOURCE_BYTES {
            match syntax::unfinished(&pending) {
                Some(Unfinished::Delimiter | Unfinished::Chain) => continue,
                Some(Unfinished::Header) => block = true,
                None => {}
            }
            if block && !blank {
                continue;
            }
        }
        number += 1;
        outcome.had_errors |= !session.submit(&pending, number, output, errors)?;
        if session.ended {
            return Ok(End::Quit);
        }
        pending.clear();
        block = false;
    }
}

pub fn run_repl(
    input: &mut impl BufRead,
    output: &mut impl Write,
    errors: &mut impl Write,
    options: ReplOptions,
) -> io::Result<ReplOutcome> {
    let mut outcome = ReplOutcome::default();
    if options.prompts {
        help(errors)?;
    }
    loop {
        let mut session = match Session::new(options) {
            Ok(session) => session,
            Err(error) => {
                ExecutionError::from(error).write_to("<대화형>", b"", errors)?;
                return Ok(ReplOutcome { had_errors: true });
            }
        };
        let end = read_session(
            &mut session,
            input,
            output,
            errors,
            options.prompts,
            &mut outcome,
        )?;
        if let End::Quit = end {
            output.flush()?;
            errors.flush()?;
            return Ok(outcome);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_bindings_stay_rooted_across_inputs_under_forced_collection() {
        let limits = Limits {
            max_heap_objects: 32,
            max_heap_bytes: 24 * 1024,
            ..Limits::default()
        };
        let options = ReplOptions {
            limits,
            ..Default::default()
        };
        let mut session = Session::new(options).unwrap();
        session.state.stress_gc();
        let mut output = Vec::new();
        for source in [
            "`값들`(기본 ())을 `자료`(기본 {(1,)})와 만드는 법:\n    (`값들`, `자료`)를 반환한다.\n",
            "이전은 만들다이다.\n보관은 (([2],)를 만든 값)이다.\n",
            "`수`(기본 7)를 만드는 법:\n    `수`를 반환한다.\n",
            "`시작`을 카운터만드는 법:\n    현재는 `시작`이다.\n    증가하는 법:\n        현재를 (현재 + 1)로 바꾼다.\n        현재를 반환한다.\n    증가하다를 반환한다.\n카운터는 (0을 카운터만든 값)이다.\n",
            "자료는 (\"{{\\\"가\\\":[7]}}\"를 JSON해석한 값)이다.\n자료[\"자기\"]를 자료로 바꾼다.\n",
            "실패는 없음이다.\n시도한다:\n    오류 \"예전 오류\"를 발생시킨다.\n오류를 문제로 받으면:\n    실패를 문제로 바꾼다.\n",
            "임시는 []이다.\n임시에 임시를 추가한다.\n",
            "보관\n",
            "(만든 값)\n",
            "((이전)으로 호출한 값)\n",
            "((카운터)으로 호출한 값)\n",
            "((카운터)으로 호출한 값)\n",
            "자료\n",
            "실패의 메시지\n",
        ] {
            let mut errors = Vec::new();
            let ok = session.submit(source.as_bytes(), 1, &mut output, &mut errors);
            let errors = String::from_utf8(errors).unwrap();
            assert!(ok.unwrap() && errors.is_empty(), "{source}\n{errors}");
        }
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "(([2],), {(1,)})\n7\n((), {(1,)})\n1\n2\n{\"가\": [7], \"자기\": <순환>}\n예전 오류\n"
        );
    }
}

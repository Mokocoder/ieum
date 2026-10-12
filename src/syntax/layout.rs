use super::forms;
use super::lexer::{Kind, Token};
use crate::diagnostic::{Diagnostic, Result, Span, code};
use crate::limits::{Limit, MAX_BLOCK_DEPTH};

pub(crate) fn apply(source: &str, base: usize, raw: Vec<Token>) -> Result<Vec<Token>> {
    let (tokens, error) = recover(source, base, raw);
    match error {
        Some(error) => Err(error),
        None => Ok(tokens),
    }
}

pub(crate) fn recover(
    source: &str,
    base: usize,
    mut raw: Vec<Token>,
) -> (Vec<Token>, Option<Diagnostic>) {
    let end = base + source.len();
    let eof = Token {
        kind: Kind::Eof,
        span: Span::new(end, end),
    };
    if raw.last().is_none_or(|token| token.kind != Kind::Eof) {
        raw.push(eof.clone());
    }
    let mut output = Vec::new();
    let mut levels = vec![0usize];
    let error = arrange(source, base, raw, &mut output, &mut levels, eof.span).err();
    for _ in &levels[1..] {
        output.push(Token {
            kind: Kind::Dedent,
            span: eof.span,
        });
    }
    output.push(eof);
    (output, error)
}

fn arrange(
    source: &str,
    base: usize,
    raw: Vec<Token>,
    output: &mut Vec<Token>,
    levels: &mut Vec<usize>,
    end: Span,
) -> Result<()> {
    let mut line: Vec<Token> = Vec::new();
    let mut block = false;
    let mut connection = false;
    let mut previous: Option<Token> = None;
    for mut token in raw {
        if matches!(token.kind, Kind::Eof) {
            token.kind = Kind::Newline;
        }
        if !matches!(token.kind, Kind::Newline) {
            line.push(token);
            continue;
        }
        let Some(first) = line.first() else {
            continue;
        };
        let local = first.span.start - base;
        let start = source[..local].rfind('\n').map_or(0, |i| i + 1);
        let prefix = source[start..local].trim_start_matches('\u{feff}');
        let indent = prefix.len();
        let leading = Span::new(first.span.start - indent, first.span.start);
        if indent % 4 != 0 {
            return Err(Diagnostic::new(
                "E-LAYOUT-001",
                leading,
                "들여쓰기는 스페이스 네 칸 단위로 하세요.",
            ));
        }
        let level = *levels
            .last()
            .expect("바깥 들여쓰기 단계는 늘 남아 있습니다");
        if block {
            if indent != level + 4 {
                let width = source[local..]
                    .chars()
                    .next()
                    .expect("토큰은 비어 있지 않습니다")
                    .len_utf8();
                return Err(Diagnostic::new(
                    "E-LAYOUT-003",
                    Span::new(first.span.start, first.span.start + width),
                    "블록 본문은 정확히 네 칸 더 들여써야 합니다.",
                ));
            }
            if levels.len() > MAX_BLOCK_DEPTH {
                return Err(Diagnostic::limit(Limit::BlockDepth, leading));
            }
            levels.push(indent);
            output.push(Token {
                kind: Kind::Indent,
                span: Span::new(first.span.start, first.span.start),
            });
        } else if connection && indent != level {
            return Err(Diagnostic::new(
                "E-LAYOUT-004",
                leading,
                "연결된 절은 같은 깊이에 써야 합니다.",
            ));
        } else if indent > level {
            if let Some(header) = previous.as_ref().filter(|last| !last.symbol(".")) {
                let end = Span::new(header.span.end, header.span.end);
                return Err(Diagnostic::new(
                    "E-LAYOUT-002",
                    end,
                    "블록 헤더 끝에 콜론(:)이 없습니다.",
                )
                .with_help("아랫줄을 들여 쓰려면 이 줄을 콜론으로 끝내세요."));
            }
            return Err(Diagnostic::new(
                "E-LAYOUT-002",
                leading,
                "블록 헤더 없이 들여쓸 수 없습니다.",
            )
            .with_help("들여쓰기를 지우거나, 윗줄을 '크면:'처럼 콜론으로 끝나는 헤더로 쓰세요."));
        } else {
            while levels.last().is_some_and(|&top| indent < top) {
                levels.pop();
                output.push(Token {
                    kind: Kind::Dedent,
                    span: Span::new(first.span.start, first.span.start),
                });
            }
        }
        let last = line.last().expect("빈 줄은 위에서 건너뛰었습니다");
        previous = Some(last.clone());
        block = last.symbol(":");
        connection = matches!(&last.kind, Kind::Word(word) if forms::is_connection(word));
        output.append(&mut line);
        output.push(token);
    }
    if block {
        return Err(Diagnostic::new(
            "E-LAYOUT-003",
            end,
            "블록 본문이 필요합니다.",
        ));
    }
    if connection {
        return Err(open_connective(end));
    }
    Ok(())
}

pub(super) fn open_connective(span: Span) -> Diagnostic {
    Diagnostic::new(
        code::OPEN_CONNECTIVE,
        span,
        "연결 어미 뒤에 다음 절이 필요합니다.",
    )
}

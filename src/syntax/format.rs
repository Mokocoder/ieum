use std::collections::HashSet;

use super::ast::{Callee, Chain, Expr, ExprKind, Statement, StatementKind, UnaryOp};
use super::tree::{SyntaxToken, SyntaxTokenKind};
use crate::diagnostic::{Diagnostic, Result, Span, code};
use crate::limits::{Limit, MAX_SOURCE_BYTES};

#[derive(Default)]
struct Operators {
    binary: HashSet<usize>,
    prefix: HashSet<usize>,
    postfix: HashSet<usize>,
}

fn chain_expressions<'a>(chain: &'a Chain, expressions: &mut Vec<&'a Expr>) {
    for call in &chain.calls {
        if let Callee::Invoke(value) = &call.callee {
            expressions.push(value);
        }
        expressions.extend(call.arguments.iter().map(|a| &a.value));
    }
}

fn operators(statements: &[Statement]) -> Operators {
    let mut result = Operators::default();
    let mut statements: Vec<_> = statements.iter().collect();
    let mut expressions = Vec::new();
    while let Some(statement) = statements.pop() {
        match &statement.kind {
            StatementKind::Bind { value, .. }
            | StatementKind::Return(value)
            | StatementKind::Throw(value)
            | StatementKind::Exit(Some(value)) => expressions.push(value),
            StatementKind::Assign { target, value, .. } => expressions.extend([target, value]),
            StatementKind::Chain(chain) => chain_expressions(chain, &mut expressions),
            StatementKind::Record(record) => {
                expressions.extend(record.fields.iter().filter_map(|f| f.default.as_ref()));
            }
            StatementKind::Function(function) => {
                statements.extend(&function.body);
                expressions.extend(
                    function
                        .parameters
                        .iter()
                        .filter_map(|p| p.default.as_ref()),
                );
            }
            StatementKind::Try(block) => {
                statements.extend(&block.body);
                if let Some(handler) = &block.handler {
                    statements.extend(&handler.body);
                }
                statements.extend(&block.cleanup);
            }
            StatementKind::If {
                test,
                body,
                otherwise,
                ..
            } => {
                expressions.push(test);
                statements.extend(body);
                statements.extend(otherwise);
            }
            StatementKind::While { test, body, .. } => {
                expressions.push(test);
                statements.extend(body);
            }
            StatementKind::For {
                collection,
                last,
                filter,
                body,
                ..
            } => {
                expressions.push(collection);
                expressions.extend(last.as_deref().map(|(last, _)| last));
                expressions.extend(filter.as_ref().map(|(test, _)| test));
                statements.extend(body);
            }
            StatementKind::Concurrent(_, body) => statements.extend(body),
            StatementKind::Import(_)
            | StatementKind::Break
            | StatementKind::Continue
            | StatementKind::Exit(None)
            | StatementKind::Rethrow => {}
        }
    }
    while let Some(expression) = expressions.pop() {
        match &expression.kind {
            ExprKind::Unary(op, span, operand) => {
                if *op == UnaryOp::Not {
                    result.postfix.insert(span.start);
                } else {
                    result.prefix.insert(span.start);
                }
                expressions.push(operand);
            }
            ExprKind::Binary(_, span, left, right) => {
                result.binary.insert(span.start);
                expressions.extend([left.as_ref(), right.as_ref()]);
            }
            ExprKind::Compare(values, ops) => {
                result.binary.extend(ops.iter().map(|(_, span)| span.start));
                expressions.extend(values);
            }
            ExprKind::Group(value) | ExprKind::Attribute(value, _) => expressions.push(value),
            ExprKind::Range(first, last, _) => expressions.extend([first.as_ref(), last.as_ref()]),
            ExprKind::Choice(parts) => expressions.extend(parts.iter()),
            ExprKind::Collect(collect) => {
                expressions.push(&collect.collection);
                expressions.extend(collect.filter.iter().chain(&collect.value));
            }
            ExprKind::Index(value, index, _) => {
                expressions.extend([value.as_ref(), index.as_ref()])
            }
            ExprKind::List(values) | ExprKind::Tuple(values) | ExprKind::Set(values) => {
                expressions.extend(values)
            }
            ExprKind::Slice(owner, bounds, _) => {
                expressions.push(owner);
                expressions.extend(bounds.iter().flatten().map(|b| b.as_ref()));
            }
            ExprKind::Dict(entries) => {
                expressions.extend(entries.iter().flat_map(|(key, value)| [key, value]));
            }
            ExprKind::Chain(chain) => chain_expressions(chain, &mut expressions),
            ExprKind::Record(record) => {
                expressions.extend(record.fields.iter().map(|(_, _, value)| value));
            }
            ExprKind::Either(verb, copula) => expressions.extend([verb.as_ref(), copula.as_ref()]),
            ExprKind::Template(_) => {}
            ExprKind::Name(_) | ExprKind::Number(_) | ExprKind::Literal(_) => {}
        }
    }
    result
}

fn gap<'a>(
    previous: &SyntaxToken,
    current: &SyntaxToken,
    original: &'a str,
    ops: &Operators,
    in_index: bool,
) -> &'a str {
    let slice_colon = in_index
        && (matches!(previous.kind(), SyntaxTokenKind::Symbol(":"))
            || matches!(current.kind(), SyntaxTokenKind::Symbol(":")));
    if slice_colon
        || matches!(
            current.kind(),
            SyntaxTokenKind::Symbol(")" | "]" | "}" | "," | "." | ":")
        )
        || matches!(previous.kind(), SyntaxTokenKind::Symbol("(" | "[" | "{"))
        || ops.prefix.contains(&previous.span().start)
    {
        ""
    } else if (matches!(previous.kind(), SyntaxTokenKind::Symbol(_))
        && ops.binary.contains(&previous.span().start))
        || (matches!(current.kind(), SyntaxTokenKind::Symbol(_))
            && ops.binary.contains(&current.span().start))
        || ops.postfix.contains(&current.span().start)
        || matches!(previous.kind(), SyntaxTokenKind::Symbol("," | ":"))
        || !original.is_empty()
    {
        " "
    } else {
        ""
    }
}

pub fn format_source(source: &[u8]) -> Result<String> {
    let parsed = super::parse_valid(source)?;
    let ops = operators(&parsed.statements);
    let mut output = String::with_capacity(source.len());
    let mut pending = "";
    let mut previous = None;
    let mut line_start = true;
    let mut brackets = Vec::new();
    for token in parsed.tree.tokens() {
        let text = parsed
            .tree
            .text(token.span())
            .expect("토큰은 원문 안에 있습니다");
        match token.kind() {
            SyntaxTokenKind::Bom => output.push_str(text),
            SyntaxTokenKind::Whitespace => pending = text,
            SyntaxTokenKind::Newline => {
                output.push('\n');
                pending = "";
                previous = None;
                line_start = true;
            }
            SyntaxTokenKind::Comment => {
                output.push_str(if line_start { pending } else { "  " });
                output.push_str(text);
                pending = "";
                previous = None;
                line_start = false;
            }
            SyntaxTokenKind::Error => unreachable!("구문 오류가 있는 코드는 정리하지 않습니다"),
            _ => {
                output.push_str(if line_start {
                    pending
                } else if let Some(previous) = previous {
                    gap(
                        previous,
                        token,
                        pending,
                        &ops,
                        brackets.last() == Some(&"["),
                    )
                } else {
                    ""
                });
                output.push_str(text);
                match token.kind() {
                    SyntaxTokenKind::Symbol(open @ ("(" | "[" | "{")) => brackets.push(open),
                    SyntaxTokenKind::Symbol(")" | "]" | "}") => {
                        brackets.pop();
                    }
                    _ => {}
                }
                pending = "";
                previous = Some(token);
                line_start = false;
            }
        }
    }
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    let original_span = Span::new(0, source.len());
    if output.len() > MAX_SOURCE_BYTES {
        return Err(Diagnostic::limit(Limit::SourceBytes, original_span));
    }

    let after = super::parse_valid(output.as_bytes()).map_err(|error| {
        if error.code == code::LIMIT {
            Diagnostic {
                span: original_span,
                ..error
            }
        } else {
            invalid_rewrite(original_span)
        }
    })?;
    if !parsed.tree.same_structure(&after.tree) {
        return Err(invalid_rewrite(original_span));
    }
    Ok(output)
}

fn invalid_rewrite(span: Span) -> Diagnostic {
    Diagnostic::new(
        code::INTERNAL,
        span,
        "정리하면 코드의 구문이 달라지므로 출력하지 않습니다.",
    )
}

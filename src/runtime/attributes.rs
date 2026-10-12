use std::path::Path;

use super::dictionary::{Key, missing_key};
use super::heap::{Handle, Heap};
use super::objects::CollectionKind;
use super::value::{Int, Number, Value};
use crate::diagnostic::{Diagnostic, Result, Span, internal};
use crate::korean::particle::SUBJECT;
use crate::stdlib::{path, path_text};
use crate::syntax::ast::BinOp;

impl Heap {
    pub fn read_attribute(
        &mut self,
        target: &Value,
        name: &str,
        roots: impl Iterator<Item = Handle> + Clone,
        span: Span,
    ) -> Result<Value> {
        let roots = roots.chain(target.handle());
        if let Value::Python(handle) = target {
            return self.python_attribute(*handle, name, roots, span);
        }
        if let Some(text) = self.text(target).cloned() {
            if let Some(part) = path_attribute(&text, name, span) {
                return match part? {
                    Some(part) => self.new_text(part, roots, span),
                    None => Ok(Value::None),
                };
            }
        }
        if let Value::Error(handle) = target {
            let (report, cause, _) = self.error(*handle, span)?;
            let text = match name {
                "코드" => Some(report.diagnostic.code.to_owned()),
                "메시지" => Some(report.diagnostic.message.to_string()),
                _ => None,
            };
            if let Some(text) = text {
                return self.new_text(text, roots, span);
            }
            if name == "원인" {
                return Ok(cause.map_or(Value::None, Value::Error));
            }
        }
        if let Value::Record(handle) = target {
            let index = self.field_index(*handle, name, span)?;
            return Ok(self.record(*handle, span)?.1[index]);
        }
        if let Value::Dict(handle) = target {
            if let Some(view) = self.dictionary_view(*handle, name, roots.clone(), span)? {
                return Ok(view);
            }
            if !is_reserved_dictionary_name(name) {
                return self
                    .dictionary(*handle, span)?
                    .get(&Key::Text(name.into()))
                    .copied()
                    .ok_or_else(|| missing_key(span));
            }
        }
        if let Some(value) = self.numeric_attribute(target, name, roots, span) {
            return value;
        }
        let length = match (target, name) {
            (Value::List(handle) | Value::Tuple(handle), "길이") => {
                self.list(*handle, span)?.len()
            }
            (Value::Dict(handle) | Value::Set(handle), "길이") => {
                self.dictionary(*handle, span)?.len()
            }
            (Value::Text(_), "길이") => self.text_of(target, span)?.chars().count(),
            _ => {
                return Err(Diagnostic::new(
                    "E-ATTR-001",
                    span,
                    format!("'{name}' 속성이 없습니다."),
                ));
            }
        };
        Ok(Value::Int(length as i64))
    }

    fn numeric_attribute(
        &mut self,
        target: &Value,
        name: &str,
        roots: impl Iterator<Item = Handle> + Clone,
        span: Span,
    ) -> Option<Result<Value>> {
        let collection = matches!(target, Value::List(_) | Value::Tuple(_) | Value::Set(_));
        let number = self.number(target);
        Some(match name {
            "합계" | "평균" | "최댓값" | "최솟값" if collection => {
                self.aggregate(target, name, roots, span)
            }
            "절댓값" => match number? {
                Number::Int(n) => self.new_int(n.abs(), roots, span),
                Number::Float(f) => Ok(Value::Float(f.abs())),
            },
            "제곱근" => number?.as_float(span).and_then(|f| {
                if f < 0.0 {
                    Err(Diagnostic::new(
                        "E-VALUE-001",
                        span,
                        "음수에는 제곱근이 없습니다.",
                    ))
                } else {
                    Ok(Value::Float(f.sqrt()))
                }
            }),
            _ => {
                let function = math_function(name)?;
                number?.as_float(span).and_then(|f| {
                    function(f).map(Value::Float).ok_or_else(|| {
                        Diagnostic::new(
                            "E-VALUE-001",
                            span,
                            format!("{f}의 {name}{} 없습니다.", SUBJECT.after(name)),
                        )
                    })
                })
            }
        })
    }

    fn aggregate(
        &mut self,
        target: &Value,
        name: &str,
        roots: impl Iterator<Item = Handle> + Clone,
        span: Span,
    ) -> Result<Value> {
        let items = self.items(target, span)?;
        match name {
            "합계" => {
                let sum = self.total(&items, span)?;
                self.new_number(sum, roots, span)
            }
            "평균" => {
                let sum = self.total(&items, span)?;
                let count = Number::Int(non_empty(&items, name, span)?.len().into());
                let average = sum.arithmetic(BinOp::Div, &count, span)?;
                self.new_number(average, roots, span)
            }
            _ => {
                let items = non_empty(&items, name, span)?;
                let mut best = items[0];
                for item in &items[1..] {
                    let order = self.order(item, &best, span)?;
                    if (name == "최댓값" && order.is_gt()) || (name == "최솟값" && order.is_lt())
                    {
                        best = *item;
                    }
                }
                Ok(best)
            }
        }
    }

    pub fn field_index(&self, record: Handle, name: &str, span: Span) -> Result<usize> {
        self.record(record, span)?
            .0
            .fields
            .iter()
            .position(|field| field.name == name)
            .ok_or_else(|| {
                Diagnostic::new("E-ATTR-001", span, format!("'{name}' 필드가 없습니다."))
            })
    }

    fn dictionary_view(
        &mut self,
        handle: Handle,
        name: &str,
        roots: impl Iterator<Item = Handle> + Clone,
        span: Span,
    ) -> Result<Option<Value>> {
        if !DICTIONARY_VIEWS.contains(&name) {
            return Ok(None);
        }
        let entries: Vec<_> = self
            .dictionary(handle, span)?
            .iter()
            .map(|entry| (entry.original, entry.value))
            .collect();
        let roots = roots.chain(std::iter::once(handle));
        let values = match name {
            "키들" => entries.into_iter().map(|(key, _)| key).collect(),
            "값들" => entries.into_iter().map(|(_, value)| value).collect(),
            _ => {
                let mut rows = Vec::with_capacity(entries.len());
                for (key, value) in entries {
                    let row = self.new_collection(
                        CollectionKind::Tuple,
                        vec![key, value],
                        roots.clone().chain(rows.iter().filter_map(Value::handle)),
                        span,
                    )?;
                    rows.push(row);
                }
                rows
            }
        };
        self.new_list(values, roots, span).map(Some)
    }

    fn total(&self, items: &[Value], span: Span) -> Result<Number> {
        let mut sum = Number::Int(Int::Small(0));
        let mut float: Option<(f64, f64)> = None;
        for item in items {
            let Some(number) = self.number(item) else {
                return Err(Diagnostic::new(
                    "E-TYPE-001",
                    span,
                    format!(
                        "합계는 수끼리만 구합니다. {}{} 있습니다.",
                        item.kind_name(),
                        SUBJECT.after(item.kind_name())
                    ),
                ));
            };
            let (total, compensation) = match (&mut float, &number) {
                (None, Number::Int(_)) => {
                    sum = sum.arithmetic(BinOp::Add, &number, span)?;
                    continue;
                }
                (None, _) => float.insert((sum.as_float(span)?, 0.0)),
                (Some(pair), _) => pair,
            };
            let x = number.as_float(span)?;
            let t = *total + x;
            if total.abs() >= x.abs() {
                *compensation += (*total - t) + x;
            } else {
                *compensation += (x - t) + *total;
            }
            *total = t;
        }
        Ok(match float {
            Some((total, compensation)) if compensation != 0.0 && compensation.is_finite() => {
                Number::Float(total + compensation)
            }
            Some((total, _)) => Number::Float(total),
            None if matches!(sum, Number::Int(_)) => sum,
            None => return Err(internal(span)),
        })
    }
}

const DICTIONARY_VIEWS: [&str; 3] = ["키들", "값들", "항목들"];

pub(super) fn is_reserved_dictionary_name(name: &str) -> bool {
    name == "길이" || DICTIONARY_VIEWS.contains(&name)
}

fn path_attribute(text: &str, name: &str, span: Span) -> Option<Result<Option<String>>> {
    let part = |value: Option<&std::ffi::OsStr>| -> Result<Option<String>> {
        value
            .map(|part| path_text(Path::new(part), span))
            .transpose()
    };
    Some(match name {
        "파일이름" => path(text, true, span).and_then(|p| part(p.file_name())),
        "확장자" => path(text, true, span).and_then(|p| part(p.extension())),
        "상위폴더" => path(text, true, span).and_then(|p| match p.parent() {
            Some(parent) if parent.as_os_str().is_empty() => {
                path_text(Path::new("."), span).map(Some)
            }
            Some(parent) => path_text(parent, span).map(Some),
            None => Ok(None),
        }),
        _ => return None,
    })
}

fn math_function(name: &str) -> Option<fn(f64) -> Option<f64>> {
    let function: fn(f64) -> Option<f64> = match name {
        "사인" => |f| Some(f.sin()),
        "코사인" => |f| Some(f.cos()),
        "탄젠트" => |f| Some(f.tan()),
        "아크사인" => |f| (-1.0..=1.0).contains(&f).then(|| f.asin()),
        "아크코사인" => |f| (-1.0..=1.0).contains(&f).then(|| f.acos()),
        "아크탄젠트" => |f| Some(f.atan()),
        "자연로그" => |f| (f > 0.0).then(|| f.ln()),
        "상용로그" => |f| (f > 0.0).then(|| f.log10()),
        "이진로그" => |f| (f > 0.0).then(|| f.log2()),
        _ => return None,
    };
    Some(function)
}

fn non_empty<'a>(items: &'a [Value], name: &str, span: Span) -> Result<&'a [Value]> {
    if items.is_empty() {
        return Err(Diagnostic::new(
            "E-VALUE-001",
            span,
            format!("빈 모음에는 {name}{} 없습니다.", SUBJECT.after(name)),
        ));
    }
    Ok(items)
}

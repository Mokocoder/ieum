use std::rc::Rc;
use std::time::Duration;

use jiff::fmt::strtime;
use jiff::tz::TimeZone;
use jiff::{Zoned, civil};

use crate::diagnostic::{Diagnostic, Result, Span};
use crate::korean::particle::SUBJECT;
use crate::runtime::dictionary::Key;
use crate::runtime::heap::{Handle, Heap};
use crate::runtime::value::Value;

const WEEKDAYS: [&str; 7] = ["월", "화", "수", "목", "금", "토", "일"];
const FIELDS: [&str; 6] = ["연", "월", "일", "시", "분", "초"];

fn time_error(span: Span, error: impl std::fmt::Display) -> Diagnostic {
    Diagnostic::new(
        "E-TIME-001",
        span,
        format!("시각을 계산하지 못했습니다: {error}"),
    )
}

#[inline(never)]
pub(super) fn now(
    heap: &mut Heap,
    roots: impl Iterator<Item = Handle> + Clone,
    span: Span,
) -> Result<Value> {
    dictionary(heap, &Zoned::now(), roots, span)
}

#[inline(never)]
pub(super) fn parse(
    text: &str,
    spec: &str,
    heap: &mut Heap,
    roots: impl Iterator<Item = Handle> + Clone,
    span: Span,
) -> Result<Value> {
    let parsed = strtime::parse(spec, text).map_err(|e| time_error(span, e))?;
    if parsed.offset().is_some() {
        let instant = parsed.to_timestamp().map_err(|e| time_error(span, e))?;
        return dictionary(heap, &instant.to_zoned(TimeZone::system()), roots, span);
    }
    let datetime = match parsed.to_datetime() {
        Ok(datetime) => datetime,
        Err(_) => parsed
            .to_date()
            .map_err(|e| time_error(span, e))?
            .to_datetime(civil::Time::midnight()),
    };
    let zoned = datetime
        .to_zoned(TimeZone::system())
        .map_err(|e| time_error(span, e))?;
    dictionary(heap, &zoned, roots, span)
}

#[inline(never)]
pub(super) fn format(heap: &Heap, value: &Value, spec: &str, span: Span) -> Result<String> {
    let Value::Dict(handle) = value else {
        return Err(Diagnostic::new(
            "E-TYPE-001",
            span,
            "'%' 서식은 시각에만 쓸 수 있습니다.",
        ));
    };
    let dictionary = heap.dictionary(*handle, span)?;
    let mut parts = [0i64; 6];
    for (part, name) in parts.iter_mut().zip(FIELDS) {
        *part = match dictionary.get(&Key::Text(name.into())) {
            Some(Value::Int(n)) => *n,
            _ => {
                return Err(Diagnostic::new(
                    "E-TYPE-001",
                    span,
                    format!(
                        "시각에는 정수 '{name}'{} 있어야 합니다.",
                        SUBJECT.after(name)
                    ),
                ));
            }
        };
    }
    let narrow = |n: i64| i8::try_from(n).map_err(|e| time_error(span, e));
    let year = i16::try_from(parts[0]).map_err(|e| time_error(span, e))?;
    let datetime = civil::DateTime::new(
        year,
        narrow(parts[1])?,
        narrow(parts[2])?,
        narrow(parts[3])?,
        narrow(parts[4])?,
        narrow(parts[5])?,
        0,
    )
    .map_err(|e| time_error(span, e))?;
    let zoned = datetime
        .to_zoned(TimeZone::system())
        .map_err(|e| time_error(span, e))?;
    strtime::format(spec, &zoned).map_err(|e| time_error(span, e))
}

pub(super) fn seconds(heap: &Heap, seconds: &Value, span: Span) -> Result<Duration> {
    let seconds = heap.as_float(seconds, span)?;
    Duration::try_from_secs_f64(seconds).map_err(|_| {
        Diagnostic::new(
            "E-VALUE-001",
            span,
            "기다릴 시간은 0 이상의 유한한 수입니다.",
        )
    })
}

fn dictionary(
    heap: &mut Heap,
    zoned: &Zoned,
    roots: impl Iterator<Item = Handle> + Clone,
    span: Span,
) -> Result<Value> {
    let int = Value::Int;
    let weekday = WEEKDAYS[usize::try_from(zoned.weekday().to_monday_zero_offset()).unwrap_or(0)];
    let stamp = zoned.timestamp().as_millisecond() as f64 / 1000.0;
    let fields = [
        ("연", int(zoned.year().into())),
        ("월", int(zoned.month().into())),
        ("일", int(zoned.day().into())),
        ("시", int(zoned.hour().into())),
        ("분", int(zoned.minute().into())),
        ("초", int(zoned.second().into())),
        ("타임스탬프", Value::Float(stamp)),
    ];
    let weekday = heap.new_text(weekday, roots.clone(), span)?;
    let mut entries: Vec<(Rc<str>, Value)> = fields
        .into_iter()
        .map(|(name, value)| (Rc::from(name), value))
        .collect();
    entries.insert(6, ("요일".into(), weekday));
    heap.named_dictionary(entries, roots, span)
}

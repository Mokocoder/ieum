use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::time;
use crate::diagnostic::{Diagnostic, Result, Span};
use crate::korean::particle::TOPIC;
use crate::limits::{Limit, MAX_TEXT_BYTES};
use crate::runtime::heap::Heap;
use crate::runtime::render::render_float;
use crate::runtime::value::{Int, Number, Value};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Align {
    Left,
    Right,
    Center,
    AfterSign,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Sign {
    Negative,
    Always,
    Space,
}

#[derive(Debug, PartialEq)]
struct Spec {
    fill: Option<char>,
    align: Option<Align>,
    sign: Option<Sign>,
    positive_zero: bool,
    alternate: bool,
    zero: bool,
    width: Option<usize>,
    grouping: Option<char>,
    precision: Option<usize>,
    kind: Option<char>,
}

fn parse(text: &str) -> Option<Spec> {
    let chars: Vec<char> = text.chars().collect();
    let mut pos = 0;
    let align_of = |c: char| match c {
        '<' => Some(Align::Left),
        '>' => Some(Align::Right),
        '^' => Some(Align::Center),
        '=' => Some(Align::AfterSign),
        _ => None,
    };
    let (mut fill, mut align) = (None, None);
    if let Some(found) = chars.get(1).copied().and_then(align_of) {
        (fill, align) = (Some(chars[0]), Some(found));
        pos = 2;
    } else if let Some(found) = chars.first().copied().and_then(align_of) {
        align = Some(found);
        pos = 1;
    }
    let sign = match chars.get(pos) {
        Some('+') => Some(Sign::Always),
        Some('-') => Some(Sign::Negative),
        Some(' ') => Some(Sign::Space),
        _ => None,
    };
    pos += usize::from(sign.is_some());
    let flag = |pos: &mut usize, c: char| {
        let found = chars.get(*pos) == Some(&c);
        *pos += usize::from(found);
        found
    };
    let positive_zero = flag(&mut pos, 'z');
    let alternate = flag(&mut pos, '#');
    let zero = flag(&mut pos, '0');
    let digits = |pos: &mut usize| {
        let start = *pos;
        while chars.get(*pos).is_some_and(char::is_ascii_digit) {
            *pos += 1;
        }
        (start < *pos).then(|| {
            chars[start..*pos]
                .iter()
                .collect::<String>()
                .parse::<usize>()
        })
    };
    let width = match digits(&mut pos) {
        Some(Ok(width)) => Some(width),
        Some(Err(_)) => return None,
        None => None,
    };
    let grouping = match chars.get(pos) {
        Some(c @ (',' | '_')) => Some(*c),
        _ => None,
    };
    pos += usize::from(grouping.is_some());
    let precision = if flag(&mut pos, '.') {
        Some(digits(&mut pos)?.ok()?)
    } else {
        None
    };
    let kind = match &chars[pos..] {
        [] => None,
        [c @ ('b' | 'd' | 'e' | 'E' | 'f' | 'F' | 'g' | 'G' | 'o' | 's' | 'x' | 'X' | '%')] => {
            Some(*c)
        }
        _ => return None,
    };
    Some(Spec {
        fill,
        align,
        sign,
        positive_zero,
        alternate,
        zero,
        width,
        grouping,
        precision,
        kind,
    })
}

pub(crate) fn is_spec(spec: &str) -> bool {
    !spec.is_empty() && (parse(spec).is_some() || spec.contains('%'))
}

fn unknown_spec_error(spec: &str, span: Span) -> Diagnostic {
    Diagnostic::new(
        "E-FORMAT-001",
        span,
        format!("'{spec}'{} 쓸 수 없는 서식입니다.", TOPIC.after(spec)),
    )
}

pub(crate) fn format(heap: &Heap, value: &Value, spec_text: &str, span: Span) -> Result<String> {
    let Some(spec) = parse(spec_text) else {
        if spec_text.contains('%') {
            return time::format(heap, value, spec_text, span);
        }
        return Err(unknown_spec_error(spec_text, span));
    };
    let invalid = |reason: &str| {
        Diagnostic::new(
            "E-FORMAT-001",
            span,
            format!("서식 '{spec_text}': {reason}"),
        )
    };
    let numeric = spec.sign.is_some()
        || spec.positive_zero
        || spec.alternate
        || spec.zero
        || spec.grouping.is_some()
        || spec.align == Some(Align::AfterSign);
    let parts = match (heap.number(value), heap.text(value)) {
        (Some(n), _) => number(&n, &spec, span).map_err(&invalid)?,
        (_, Some(text)) => {
            if numeric || !matches!(spec.kind, None | Some('s')) {
                return Err(invalid("문자열에는 너비, 정렬, 자릿수만 쓸 수 있습니다."));
            }
            let shown = match spec.precision {
                Some(count) => text.chars().take(count).collect(),
                None => text.to_string(),
            };
            Parts::plain(shown)
        }
        _ if numeric || spec.precision.is_some() || spec.kind.is_some() => {
            return Err(Diagnostic::new(
                "E-TYPE-001",
                span,
                format!("{}에는 수 서식을 쓸 수 없습니다.", value.kind_name()),
            ));
        }
        _ => Parts::plain(heap.render(value, span)?),
    };
    let numeric_value = value.numeric();
    let result = layout(parts, &spec, numeric_value);
    if result.len() > MAX_TEXT_BYTES {
        return Err(Diagnostic::limit(Limit::Text, span));
    }
    Ok(result)
}

struct Parts {
    sign: &'static str,
    prefix: &'static str,
    whole: String,
    rest: String,
    grouping: Option<(char, usize)>,
}

impl Parts {
    fn plain(text: String) -> Self {
        Self {
            sign: "",
            prefix: "",
            whole: text,
            rest: String::new(),
            grouping: None,
        }
    }
}

fn group(digits: &str, grouping: Option<(char, usize)>) -> String {
    let Some((separator, size)) = grouping else {
        return digits.to_owned();
    };
    let count = digits.chars().count();
    let mut out = String::with_capacity(count * 2);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (count - index) % size == 0 {
            out.push(separator);
        }
        out.push(digit);
    }
    out
}

fn number(value: &Number, spec: &Spec, span: Span) -> std::result::Result<Parts, &'static str> {
    match (value, spec.kind) {
        (Number::Int(n), None) if spec.precision.is_none() => integer(n, spec),
        (Number::Int(_), Some('b' | 'o' | 'x' | 'X' | 'd')) if spec.precision.is_some() => {
            Err("정수 서식에는 자릿수를 쓸 수 없습니다.")
        }
        (Number::Int(n), Some('b' | 'o' | 'x' | 'X' | 'd')) => integer(n, spec),
        (_, Some('b' | 'o' | 'x' | 'X' | 'd')) => {
            Err("실수에는 정수 서식(b, o, x, X, d)을 쓸 수 없습니다.")
        }
        (_, Some('s')) => Err("수에는 문자열 서식(s)을 쓸 수 없습니다."),
        (_, None) if spec.alternate => Err("'#'은 형식 문자와 함께 씁니다."),
        _ => {
            let x = value
                .as_float(span)
                .map_err(|_| "정수가 너무 커서 실수로 나타낼 수 없습니다.")?;
            float(x, spec, value)
        }
    }
}

fn sign_of(negative: bool, spec: &Spec) -> &'static str {
    match (negative, spec.sign.unwrap_or(Sign::Negative)) {
        (true, _) => "-",
        (false, Sign::Always) => "+",
        (false, Sign::Space) => " ",
        (false, Sign::Negative) => "",
    }
}

fn integer(n: &Int, spec: &Spec) -> std::result::Result<Parts, &'static str> {
    let (radix, prefix, upper) = match spec.kind {
        Some('b') => (2, "0b", false),
        Some('o') => (8, "0o", false),
        Some('x') => (16, "0x", false),
        Some('X') => (16, "0X", true),
        _ => (10, "", false),
    };
    let size = match (radix, spec.grouping) {
        (10, _) => 3,
        (_, Some(',')) => {
            return Err("2진수, 8진수, 16진수에는 ','를 쓸 수 없습니다. '_'를 쓰세요.");
        }
        _ => 4,
    };
    let mut whole = n.big().magnitude().to_str_radix(radix);
    if upper {
        whole.make_ascii_uppercase();
    }
    Ok(Parts {
        sign: sign_of(n.is_negative(), spec),
        prefix: if spec.alternate { prefix } else { "" },
        whole,
        rest: String::new(),
        grouping: spec.grouping.map(|c| (c, size)),
    })
}

fn float(x: f64, spec: &Spec, value: &Number) -> std::result::Result<Parts, &'static str> {
    let upper = matches!(spec.kind, Some('E' | 'F' | 'G'));
    let magnitude = x.abs();
    let body = if !x.is_finite() {
        let name = if x.is_nan() { "nan" } else { "inf" };
        let mut body = if upper {
            name.to_uppercase()
        } else {
            name.to_owned()
        };
        if spec.kind == Some('%') {
            body.push('%');
        }
        body
    } else {
        match spec.kind {
            Some('f' | 'F') => fixed(magnitude, spec.precision.unwrap_or(6), spec.alternate),
            Some('e' | 'E') => exponent(
                magnitude,
                spec.precision.unwrap_or(6),
                spec.alternate,
                upper,
            ),
            Some('g' | 'G') => general(
                magnitude,
                spec.precision.unwrap_or(6),
                spec.alternate,
                upper,
            ),
            Some('%') => {
                fixed(
                    magnitude * 100.0,
                    spec.precision.unwrap_or(6),
                    spec.alternate,
                ) + "%"
            }
            _ => match spec.precision {
                Some(places) => fixed(magnitude, places, false),
                None => match value {
                    Number::Int(n) => n.big().magnitude().to_string(),
                    Number::Float(_) => render_float(magnitude),
                },
            },
        }
    };
    let rounded_zero = body.chars().all(|c| !c.is_ascii_digit() || c == '0');
    let negative = !x.is_nan() && x.is_sign_negative() && !(spec.positive_zero && rounded_zero);
    let split = body
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(body.len());
    let grouping = if body.contains('e') || body.contains('E') || !x.is_finite() {
        None
    } else {
        spec.grouping.map(|c| (c, 3))
    };
    Ok(Parts {
        sign: sign_of(negative, spec),
        prefix: "",
        whole: body[..split].to_owned(),
        rest: body[split..].to_owned(),
        grouping,
    })
}

fn fixed(x: f64, places: usize, alternate: bool) -> String {
    let mut text = format!("{x:.places$}");
    if alternate && places == 0 {
        text.push('.');
    }
    text
}

fn exponent(x: f64, places: usize, alternate: bool, upper: bool) -> String {
    let text = format!("{x:.places$e}");
    let (mantissa, power) = text.split_once('e').expect("지수 표기입니다");
    let power: i32 = power.parse().expect("지수는 숫자입니다");
    let point = if alternate && places == 0 { "." } else { "" };
    let e = if upper { 'E' } else { 'e' };
    let sign = if power < 0 { '-' } else { '+' };
    format!("{mantissa}{point}{e}{sign}{:02}", power.unsigned_abs())
}

fn general(x: f64, precision: usize, alternate: bool, upper: bool) -> String {
    let precision = precision.max(1);
    let power = if x == 0.0 {
        0
    } else {
        let text = format!("{x:.*e}", precision - 1);
        text.split_once('e')
            .expect("지수 표기입니다")
            .1
            .parse::<i64>()
            .expect("지수는 숫자입니다")
    };
    let mut text = if -4 <= power && power < precision as i64 {
        fixed(x, (precision as i64 - 1 - power) as usize, alternate)
    } else {
        exponent(x, precision - 1, alternate, upper)
    };
    if !alternate {
        let (number, tail) = match text.find(['e', 'E']) {
            Some(at) => text.split_at(at),
            None => (text.as_str(), ""),
        };
        let number = if number.contains('.') {
            number.trim_end_matches('0').trim_end_matches('.')
        } else {
            number
        };
        text = format!("{number}{tail}");
    }
    text
}

fn layout(parts: Parts, spec: &Spec, numeric: bool) -> String {
    let width = spec.width.unwrap_or(0);
    let zero_padding = spec.zero && spec.fill.is_none() && spec.align.is_none();
    let fill = spec.fill.unwrap_or(if zero_padding { '0' } else { ' ' });
    let align = spec.align.unwrap_or(if zero_padding {
        Align::AfterSign
    } else if numeric {
        Align::Right
    } else {
        Align::Left
    });
    let head = format!("{}{}", parts.sign, parts.prefix);
    let mut whole = group(&parts.whole, parts.grouping);
    if align == Align::AfterSign && fill == '0' && parts.grouping.is_some() {
        let mut digits = parts.whole.clone();
        while head.width() + whole.width() + parts.rest.width() < width {
            digits.insert(0, '0');
            whole = group(&digits, parts.grouping);
        }
        return format!("{head}{whole}{}", parts.rest);
    }
    let body = format!("{whole}{}", parts.rest);
    let used = head.width() + body.width();
    let missing = width.saturating_sub(used) / fill.width().unwrap_or(1).max(1);
    let fill_with = |count: usize| std::iter::repeat_n(fill, count).collect::<String>();
    match align {
        Align::Left => format!("{head}{body}{}", fill_with(missing)),
        Align::Right => format!("{}{head}{body}", fill_with(missing)),
        Align::Center => format!(
            "{}{head}{body}{}",
            fill_with(missing / 2),
            fill_with(missing - missing / 2)
        ),
        Align::AfterSign => format!("{head}{}{body}", fill_with(missing)),
    }
}

use std::cmp::Ordering;
use std::rc::Rc;

use num_bigint::{BigInt, BigUint};
use num_integer::Integer;
use num_traits::{One, Signed, ToPrimitive, Zero};

use super::heap::Handle;
pub(crate) use super::integer::Int;
use crate::diagnostic::{Diagnostic, Result, Span, type_error};
use crate::korean::particle::{TOPIC, WITH};
use crate::limits::{Limit, MAX_INT_BITS};
use crate::stdlib::Native;
use crate::syntax::ast::{BinOp, CmpOp};
use crate::syntax::lexer::radix;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Value {
    Int(i64),
    Big(Handle),
    Float(f64),
    Bool(bool),
    Text(Handle),
    None,
    Native(Native),
    Function(Handle),
    List(Handle),
    Tuple(Handle),
    Set(Handle),
    Dict(Handle),
    Error(Handle),
    RecordType(Handle),
    Record(Handle),
    #[cfg_attr(not(feature = "python"), allow(dead_code))]
    Python(Handle),
}

const _: () = assert!(std::mem::size_of::<Value>() == 16);

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Constant {
    Int(Int),
    Float(f64),
    Bool(bool),
    Text(Rc<str>),
    None,
    Native(Native),
}

impl Constant {
    pub fn number(spelling: &str, span: Span) -> Result<Self> {
        let spelling = spelling.replace('_', "");
        let (radix, digits) = radix(&spelling);
        if radix == 10 && spelling.contains(['.', 'e', 'E']) {
            return spelling.parse().map(Self::Float).map_err(|_| {
                Diagnostic::new("E-NUM-001", span, "실수로 읽을 수 없는 표기입니다.")
            });
        }
        let significant = digits.trim_start_matches('0');
        let min_bits = match radix {
            16 => 4,
            8 => 3,
            2 => 1,
            _ => 3,
        };
        if significant.len().saturating_sub(1) as u64 * min_bits > MAX_INT_BITS {
            return Err(Diagnostic::limit(Limit::IntBits, span));
        }
        let value = BigInt::parse_bytes(digits.as_bytes(), radix)
            .ok_or_else(|| Diagnostic::new("E-NUM-001", span, "정수로 읽을 수 없는 표기입니다."))?;
        Ok(Self::Int(bounded(value, span)?.into()))
    }
}

pub(crate) fn operand_error(left: &Value, op: &str, right: &Value, span: Span) -> Diagnostic {
    let (a, b) = (left.kind_name(), right.kind_name());
    let pair = if a == b {
        format!("{a}끼리는")
    } else {
        format!("{a}{} {b}{}", WITH.after(a), TOPIC.after(b))
    };
    let verb = if CmpOp::from_symbol(op).is_some() {
        "비교할"
    } else {
        "계산할"
    };
    Diagnostic::new(
        "E-TYPE-001",
        span,
        format!("{pair} '{op}'로 {verb} 수 없습니다."),
    )
}

pub(crate) fn zero_error(span: Span) -> Diagnostic {
    Diagnostic::new("E-RUNTIME-002", span, "0으로 나눌 수 없습니다.")
}

fn numeric_error(span: Span) -> Diagnostic {
    Diagnostic::new(
        "E-NUMERIC-001",
        span,
        "정수가 너무 커서 실수로 바꿀 수 없습니다.",
    )
}

fn bounded(value: BigInt, span: Span) -> Result<BigInt> {
    if value.bits() > MAX_INT_BITS {
        Err(Diagnostic::limit(Limit::IntBits, span))
    } else {
        Ok(value)
    }
}

fn multiply(a: &BigInt, b: &BigInt, span: Span) -> Result<BigInt> {
    if a.is_zero() || b.is_zero() {
        return Ok(BigInt::zero());
    }
    if a.bits() + b.bits() - 1 > MAX_INT_BITS {
        return Err(Diagnostic::limit(Limit::IntBits, span));
    }
    bounded(a * b, span)
}

impl Value {
    pub(crate) fn handle(&self) -> Option<Handle> {
        match self {
            Self::Big(handle)
            | Self::Text(handle)
            | Self::Function(handle)
            | Self::List(handle)
            | Self::Tuple(handle)
            | Self::Set(handle)
            | Self::Dict(handle)
            | Self::Error(handle)
            | Self::RecordType(handle)
            | Self::Record(handle)
            | Self::Python(handle) => Some(*handle),
            _ => None,
        }
    }

    pub(crate) fn boolean(&self, span: Span) -> Result<bool> {
        match self {
            Self::Bool(b) => Ok(*b),
            _ => Err(Diagnostic::new(
                "E-TYPE-002",
                span,
                "논리 연산에는 참 또는 거짓이 필요합니다.",
            )),
        }
    }

    pub(crate) fn numeric(&self) -> bool {
        matches!(self, Self::Int(_) | Self::Big(_) | Self::Float(_))
    }

    pub(crate) fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    pub(crate) fn kind_name(&self) -> &'static str {
        match self {
            Self::Int(_) | Self::Big(_) => "정수",
            Self::Float(_) => "실수",
            Self::Bool(_) => "논리값",
            Self::Text(_) => "문자열",
            Self::None => "없음",
            Self::Native(_) | Self::Function(_) => "함수",
            Self::List(_) => "목록",
            Self::Tuple(_) => "튜플",
            Self::Set(_) => "집합",
            Self::Dict(_) => "사전",
            Self::Error(_) => "오류",
            Self::RecordType(_) => "자료형",
            Self::Record(_) => "자료형 값",
            Self::Python(_) => "파이썬 값",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Number {
    Int(Int),
    Float(f64),
}

impl Number {
    pub(crate) fn as_float(&self, span: Span) -> Result<f64> {
        match self {
            Self::Int(Int::Small(n)) if n.unsigned_abs() <= 1 << 53 => Ok(*n as f64),
            Self::Int(n) => ratio_float(&n.big(), &BigInt::one(), span),
            Self::Float(f) => Ok(*f),
        }
    }

    pub(crate) fn negate(&self) -> Self {
        match self {
            Self::Int(n) => Self::Int(-n),
            Self::Float(f) => Self::Float(-f),
        }
    }

    pub(crate) fn arithmetic(&self, op: BinOp, right: &Self, span: Span) -> Result<Self> {
        if op == BinOp::Pow {
            return self.power(right, span);
        }
        if let (Self::Int(a), Self::Int(b)) = (self, right) {
            let fast = match op {
                BinOp::Add => a.checked_add(b),
                BinOp::Sub => a.checked_sub(b),
                BinOp::Mul => a.checked_mul(b),
                BinOp::FloorDiv => a.checked_div_mod_floor(b).map(|(q, _)| q),
                BinOp::Mod => a.checked_div_mod_floor(b).map(|(_, r)| r),
                _ => None,
            };
            if let Some(n) = fast {
                return Ok(Self::Int(n));
            }
            if let (BinOp::Div, Int::Small(x), Int::Small(y)) = (op, a, b) {
                if *y != 0 && x.unsigned_abs() <= 1 << 53 && y.unsigned_abs() <= 1 << 53 {
                    return Ok(Self::Float(*x as f64 / *y as f64));
                }
            }
            let (a, b) = (a.big(), b.big());
            let (a, b) = (a.as_ref(), b.as_ref());
            let integer = match op {
                BinOp::Add => bounded(a + b, span)?,
                BinOp::Sub => bounded(a - b, span)?,
                BinOp::Mul => multiply(a, b, span)?,
                BinOp::Div => return Ok(Self::Float(ratio_float(a, b, span)?)),
                BinOp::FloorDiv | BinOp::Mod => {
                    if b.is_zero() {
                        return Err(zero_error(span));
                    }
                    let (q, r) = a.div_mod_floor(b);
                    if op == BinOp::FloorDiv { q } else { r }
                }
                _ => return Err(type_error(span)),
            };
            return Ok(Self::Int(integer.into()));
        }
        let (a, b) = (self.as_float(span)?, right.as_float(span)?);
        if matches!(op, BinOp::Div | BinOp::FloorDiv | BinOp::Mod) && b == 0.0 {
            return Err(zero_error(span));
        }
        Ok(Self::Float(match op {
            BinOp::Add => a + b,
            BinOp::Sub => a - b,
            BinOp::Mul => a * b,
            BinOp::Div => a / b,
            BinOp::FloorDiv => float_div_mod(a, b).0,
            BinOp::Mod => float_div_mod(a, b).1,
            _ => return Err(type_error(span)),
        }))
    }

    fn power(&self, right: &Self, span: Span) -> Result<Self> {
        if let (Self::Int(base), Self::Int(exponent)) = (self, right) {
            let exponent = exponent.big();
            if base.is_zero() && exponent.is_negative() {
                return Err(zero_error(span));
            }
            match integer_power(&base.big(), exponent.magnitude(), span) {
                Ok(power) if exponent.is_negative() => {
                    return Ok(Self::Float(ratio_float(&BigInt::one(), &power, span)?));
                }
                Ok(power) => return Ok(Self::Int(power.into())),
                Err(error) if !exponent.is_negative() => return Err(error),
                Err(_) => {}
            }
        }
        let (base, exponent) = (self.as_float(span)?, right.as_float(span)?);
        float_pow(base, exponent, span).map(Self::Float)
    }

    pub(crate) fn order(&self, other: &Self) -> Option<Ordering> {
        let (n, f, reverse) = match (self, other) {
            (Self::Float(a), Self::Float(b)) => return a.partial_cmp(b),
            (Self::Int(a), Self::Int(b)) => return Some(a.cmp(b)),
            (Self::Int(n), Self::Float(f)) => (n, *f, false),
            (Self::Float(f), Self::Int(n)) => (n, *f, true),
        };
        if f.is_nan() {
            return None;
        }
        let order = if f == f64::INFINITY {
            Ordering::Less
        } else if f == f64::NEG_INFINITY {
            Ordering::Greater
        } else if let Int::Small(small) = n {
            if small.unsigned_abs() <= 1 << 53 {
                (*small as f64).partial_cmp(&f)?
            } else {
                let (p, q) = float_ratio(f);
                (BigInt::from(*small) * q).cmp(&p)
            }
        } else {
            let (p, q) = float_ratio(f);
            (n.big().as_ref() * q).cmp(&p)
        };
        Some(if reverse { order.reverse() } else { order })
    }
}

fn integer_power(base: &BigInt, exponent: &BigUint, span: Span) -> Result<BigInt> {
    if exponent.is_zero() {
        return Ok(BigInt::one());
    }
    if base.is_zero() {
        return Ok(BigInt::zero());
    }
    if base.abs().is_one() {
        return Ok(if base.is_negative() && exponent.is_odd() {
            -BigInt::one()
        } else {
            BigInt::one()
        });
    }
    let Some(mut count) = exponent.to_u64() else {
        return Err(Diagnostic::limit(Limit::IntBits, span));
    };
    if (base.bits() - 1).saturating_mul(count).saturating_add(1) > MAX_INT_BITS {
        return Err(Diagnostic::limit(Limit::IntBits, span));
    }
    let mut result = BigInt::one();
    let mut factor = base.clone();
    while count != 0 {
        if count % 2 == 1 {
            result = multiply(&result, &factor, span)?;
        }
        count /= 2;
        if count != 0 {
            factor = multiply(&factor, &factor, span)?;
        }
    }
    Ok(result)
}

fn ratio_float(n: &BigInt, d: &BigInt, span: Span) -> Result<f64> {
    if d.is_zero() {
        return Err(zero_error(span));
    }
    let sign = u64::from(n.is_negative() != d.is_negative()) << 63;
    let (n, d) = (n.magnitude(), d.magnitude());
    if n.is_zero() {
        return Ok(f64::from_bits(sign));
    }
    let mut exponent = n.bits() as i64 - d.bits() as i64;
    let below = if exponent >= 0 {
        n < &(d << exponent as usize)
    } else {
        &(n << (-exponent) as usize) < d
    };
    if below {
        exponent -= 1;
    }
    if exponent > 1023 {
        return Err(numeric_error(span));
    }
    if exponent < -1075 {
        return Ok(f64::from_bits(sign));
    }
    let mut shift = (exponent - 52).max(-1074);
    let (numerator, denominator) = if shift >= 0 {
        (n.clone(), d << shift as usize)
    } else {
        (n << (-shift) as usize, d.clone())
    };
    let (mut q, r) = numerator.div_rem(&denominator);
    let doubled = r << 1;
    if doubled > denominator || (doubled == denominator && q.is_odd()) {
        q += 1u8;
    }
    let mut significand = q.to_u64().expect("반올림한 가수는 54비트 이하입니다");
    if significand < 1 << 52 {
        return Ok(f64::from_bits(sign | significand));
    }
    if significand == 1 << 53 {
        significand >>= 1;
        shift += 1;
    }
    let exponent = shift + 52;
    if exponent > 1023 {
        return Err(numeric_error(span));
    }
    let bits = sign | (((exponent + 1023) as u64) << 52) | (significand - (1 << 52));
    Ok(f64::from_bits(bits))
}

fn float_ratio(f: f64) -> (BigInt, BigInt) {
    let bits = f.to_bits();
    let raw_exponent = ((bits >> 52) & 0x7ff) as i32;
    let mut significand = bits & ((1 << 52) - 1);
    let exponent = if raw_exponent == 0 {
        -1074
    } else {
        significand |= 1 << 52;
        raw_exponent - 1023 - 52
    };
    let mut n = BigInt::from(significand);
    if f.is_sign_negative() {
        n = -n;
    }
    if exponent >= 0 {
        (n << exponent as usize, BigInt::one())
    } else {
        (n, BigInt::one() << (-exponent) as usize)
    }
}

fn float_pow(x: f64, y: f64, span: Span) -> Result<f64> {
    if x.is_finite() && y.is_finite() {
        if x == 0.0 && y < 0.0 {
            return Err(zero_error(span));
        }
        if x < 0.0 && y.fract() != 0.0 {
            return Err(Diagnostic::new(
                "E-VALUE-001",
                span,
                "음수의 분수 거듭제곱은 실수가 아닙니다.",
            ));
        }
    }
    let result = x.powf(y);
    if result.is_infinite() && x.is_finite() && y.is_finite() {
        return Err(Diagnostic::new(
            "E-NUMERIC-001",
            span,
            "거듭제곱 결과가 너무 커서 실수로 나타낼 수 없습니다.",
        ));
    }
    Ok(result)
}

fn float_div_mod(a: f64, b: f64) -> (f64, f64) {
    if a.is_nan() || b.is_nan() || a.is_infinite() {
        return (f64::NAN, f64::NAN);
    }
    let truncated = a % b;
    let mut quotient = ((a - truncated) / b).round();
    let mut remainder = truncated;
    if remainder == 0.0 {
        remainder = 0.0f64.copysign(b);
    } else if (remainder < 0.0) != (b < 0.0) {
        remainder += b;
        quotient -= 1.0;
    }
    if quotient == 0.0 {
        quotient = 0.0f64.copysign(a / b);
    }
    (quotient, remainder)
}

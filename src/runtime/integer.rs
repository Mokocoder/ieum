use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt;
use std::rc::Rc;

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Int {
    Small(i64),
    Big(Rc<BigInt>),
}

impl Int {
    pub fn big(&self) -> Cow<'_, BigInt> {
        match self {
            Self::Small(n) => Cow::Owned(BigInt::from(*n)),
            Self::Big(n) => Cow::Borrowed(n),
        }
    }

    pub fn is_zero(&self) -> bool {
        matches!(self, Self::Small(0))
    }

    pub fn is_negative(&self) -> bool {
        match self {
            Self::Small(n) => *n < 0,
            Self::Big(n) => n.is_negative(),
        }
    }

    pub fn abs(&self) -> Self {
        match self {
            Self::Small(n) => n
                .checked_abs()
                .map_or_else(|| BigInt::from(*n).abs().into(), Self::Small),
            Self::Big(n) => n.abs().into(),
        }
    }

    pub fn checked_add(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (Self::Small(a), Self::Small(b)) => a.checked_add(*b).map(Self::Small),
            _ => None,
        }
    }

    pub fn checked_sub(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (Self::Small(a), Self::Small(b)) => a.checked_sub(*b).map(Self::Small),
            _ => None,
        }
    }

    pub fn checked_mul(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (Self::Small(a), Self::Small(b)) => a.checked_mul(*b).map(Self::Small),
            _ => None,
        }
    }

    pub fn checked_div_mod_floor(&self, other: &Self) -> Option<(Self, Self)> {
        let (Self::Small(a), Self::Small(b)) = (self, other) else {
            return None;
        };
        if *b == 0 || (*a == i64::MIN && *b == -1) {
            return None;
        }
        let (mut q, mut r) = (a / b, a % b);
        if r != 0 && ((r < 0) != (*b < 0)) {
            q -= 1;
            r += b;
        }
        Some((Self::Small(q), Self::Small(r)))
    }

    pub fn step(&self, down: bool) -> Self {
        let delta = if down { -1 } else { 1 };
        match self {
            Self::Small(n) => n
                .checked_add(delta)
                .map_or_else(|| (BigInt::from(*n) + delta).into(), Self::Small),
            Self::Big(n) => (n.as_ref() + delta).into(),
        }
    }
}

impl From<BigInt> for Int {
    fn from(n: BigInt) -> Self {
        match n.to_i64() {
            Some(n) => Self::Small(n),
            None => Self::Big(Rc::new(n)),
        }
    }
}

impl From<i64> for Int {
    fn from(n: i64) -> Self {
        Self::Small(n)
    }
}

impl From<i32> for Int {
    fn from(n: i32) -> Self {
        Self::Small(n.into())
    }
}

impl From<u8> for Int {
    fn from(n: u8) -> Self {
        Self::Small(n.into())
    }
}

impl From<usize> for Int {
    fn from(n: usize) -> Self {
        i64::try_from(n).map_or_else(|_| BigInt::from(n).into(), Self::Small)
    }
}

impl From<u64> for Int {
    fn from(n: u64) -> Self {
        i64::try_from(n).map_or_else(|_| BigInt::from(n).into(), Self::Small)
    }
}

impl ToPrimitive for Int {
    fn to_i64(&self) -> Option<i64> {
        match self {
            Self::Small(n) => Some(*n),
            Self::Big(_) => None,
        }
    }

    fn to_u64(&self) -> Option<u64> {
        match self {
            Self::Small(n) => u64::try_from(*n).ok(),
            Self::Big(n) => n.to_u64(),
        }
    }
}

impl Ord for Int {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Small(a), Self::Small(b)) => a.cmp(b),
            _ => self.big().cmp(&other.big()),
        }
    }
}

impl PartialOrd for Int {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::ops::Neg for &Int {
    type Output = Int;
    fn neg(self) -> Int {
        match self {
            Int::Small(n) => n
                .checked_neg()
                .map_or_else(|| (-BigInt::from(*n)).into(), Int::Small),
            Int::Big(n) => (-n.as_ref()).into(),
        }
    }
}

impl Zero for Int {
    fn zero() -> Self {
        Self::Small(0)
    }
    fn is_zero(&self) -> bool {
        Int::is_zero(self)
    }
}

impl std::ops::Add for Int {
    type Output = Int;
    fn add(self, other: Int) -> Int {
        self.checked_add(&other)
            .unwrap_or_else(|| (self.big().into_owned() + other.big().as_ref()).into())
    }
}

impl fmt::Display for Int {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Small(n) => n.fmt(f),
            Self::Big(n) => n.fmt(f),
        }
    }
}

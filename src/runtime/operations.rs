use std::cmp::Ordering;
use std::collections::HashSet;
use std::mem::size_of;
use std::rc::Rc;

use num_traits::ToPrimitive;

use super::dictionary::missing_key;
use super::heap::{Handle, Heap, Object};
use super::objects::CollectionKind;
use super::value::{Value, operand_error};
use crate::diagnostic::{Diagnostic, Result, Span, internal, memory, type_error};
use crate::korean::particle::TOPIC;
use crate::limits::{Limit, MAX_ITEMS, MAX_TEXT_BYTES};
use crate::syntax::ast::{BinOp, CmpOp, UnaryOp};

impl Heap {
    pub fn index(
        &mut self,
        target: &Value,
        index: &Value,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        match target {
            Value::List(handle) | Value::Tuple(handle) => {
                let items = self.list(*handle, span)?;
                let position = self.index_position(index, items.len(), span)?;
                Ok(self.list(*handle, span)?[position])
            }
            Value::Text(_) => {
                let text = self.text_of(target, span)?;
                let i = self.index_position(index, text.chars().count(), span)?;
                let c = text.chars().nth(i).ok_or_else(|| internal(span))?;
                self.new_text(c.encode_utf8(&mut [0; 4]) as &str, roots, span)
            }
            Value::Dict(handle) => self
                .dictionary(*handle, span)?
                .get(&self.key(index, span)?)
                .copied()
                .ok_or_else(|| missing_key(span)),
            Value::Python(handle) => self.python_index(*handle, index, roots, span),
            _ => Err(type_error(span)),
        }
    }

    pub fn index_position(&self, index: &Value, length: usize, span: Span) -> Result<usize> {
        let index = match index {
            Value::Int(n) => *n,
            Value::Big(_) => {
                let shown = self.int(index).ok_or_else(|| internal(span))?;
                return Err(index_error(shown, length, span));
            }
            _ => return Err(type_error(span)),
        };
        let i = if index < 0 {
            index.checked_add(length as i64)
        } else {
            Some(index)
        };
        i.and_then(|i| usize::try_from(i).ok())
            .filter(|&n| n < length)
            .ok_or_else(|| index_error(index, length, span))
    }

    pub fn unary(
        &mut self,
        value: &Value,
        op: UnaryOp,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        match (op, value) {
            (UnaryOp::Not, value) => Ok(Value::Bool(!value.boolean(span)?)),
            (UnaryOp::Pos, value) if value.numeric() => Ok(*value),
            (_, Value::Python(handle)) => self.python_unary(*handle, op, roots, span),
            (UnaryOp::Neg, value) => match self.number(value) {
                Some(number) => self.new_number(number.negate(), roots, span),
                None => Err(type_error(span)),
            },
            _ => Err(type_error(span)),
        }
    }

    pub fn binary(
        &mut self,
        left: &Value,
        op: BinOp,
        right: &Value,
        roots: impl Iterator<Item = Handle> + Clone,
        span: Span,
    ) -> Result<Value> {
        let roots = roots.chain(left.handle()).chain(right.handle());
        let (left_text, right_text) = (
            matches!(left, Value::Text(_)),
            matches!(right, Value::Text(_)),
        );
        if op == BinOp::Add && (left_text || right_text) {
            let (a, b) = (self.render(left, span)?, self.render(right, span)?);
            let size = a
                .len()
                .checked_add(b.len())
                .filter(|&n| n <= MAX_TEXT_BYTES)
                .ok_or_else(|| Diagnostic::limit(Limit::Text, span))?;
            let mut joined = String::with_capacity(size);
            joined.push_str(&a);
            joined.push_str(&b);
            return self.new_text(joined, roots, span);
        }
        if matches!(left, Value::Python(_)) || matches!(right, Value::Python(_)) {
            return self.python_binary(left, op, right, roots, span);
        }
        if op == BinOp::Mul && left_text != right_text {
            let (text, count) = if left_text {
                (left, right)
            } else {
                (right, left)
            };
            if let Some(count) = self.int(count) {
                let text = self.text_of(text, span)?;
                if text.is_empty() || count.is_negative() || count.is_zero() {
                    return self.new_text("", roots, span);
                }
                let count = count
                    .to_usize()
                    .ok_or_else(|| Diagnostic::limit(Limit::Text, span))?;
                text.len()
                    .checked_mul(count)
                    .filter(|&n| n <= MAX_TEXT_BYTES)
                    .ok_or_else(|| Diagnostic::limit(Limit::Text, span))?;
                return self.new_text(text.repeat(count), roots, span);
            }
        }
        if let (Value::List(a), Value::List(b), BinOp::Add)
        | (Value::Tuple(a), Value::Tuple(b), BinOp::Add) = (left, right, op)
        {
            let kind = CollectionKind::of(left);
            let (a, b) = (self.list(*a, span)?, self.list(*b, span)?);
            let size = a
                .len()
                .checked_add(b.len())
                .filter(|&n| n <= MAX_ITEMS)
                .ok_or_else(|| Diagnostic::limit(Limit::Items, span))?;
            let mut values = Vec::new();
            values.try_reserve_exact(size).map_err(|_| memory(span))?;
            values.extend_from_slice(a);
            values.extend_from_slice(b);
            return self.new_collection(kind, values, roots, span);
        }
        if op == BinOp::Mul {
            let (sequence, count) = match (left, right) {
                (Value::List(_) | Value::Tuple(_), _) => (left, right),
                _ => (right, left),
            };
            if let (Value::List(handle) | Value::Tuple(handle), Some(count)) =
                (sequence, self.int(count))
            {
                let kind = CollectionKind::of(sequence);
                let items = self.list(*handle, span)?;
                if items.is_empty() || count.is_negative() || count.is_zero() {
                    return self.new_collection(kind, Vec::new(), roots, span);
                }
                let count = count
                    .to_usize()
                    .ok_or_else(|| Diagnostic::limit(Limit::Items, span))?;
                let size = items
                    .len()
                    .checked_mul(count)
                    .filter(|&n| n <= MAX_ITEMS)
                    .ok_or_else(|| Diagnostic::limit(Limit::Items, span))?;
                let bytes = size
                    .checked_mul(size_of::<Value>())
                    .and_then(|n| n.checked_add(size_of::<Object>()))
                    .ok_or_else(|| {
                        Diagnostic::limit(Limit::HeapBytes(self.limits.max_heap_bytes), span)
                    })?;
                self.reserve(1, bytes, 0, roots.clone(), span)?;
                let items = self.list(*handle, span)?;
                let mut values = Vec::new();
                values.try_reserve_exact(size).map_err(|_| memory(span))?;
                for _ in 0..count {
                    values.extend_from_slice(items);
                }
                return self.new_collection(kind, values, roots, span);
            }
        }
        match (self.number(left), self.number(right)) {
            (Some(a), Some(b)) => {
                let result = a.arithmetic(op, &b, span)?;
                self.new_number(result, roots, span)
            }
            _ => Err(operand_error(left, op.symbol(), right, span)),
        }
    }

    fn compare_scalars(&self, left: &Value, op: CmpOp, right: &Value, span: Span) -> Result<bool> {
        if matches!(left, Value::Python(_)) || matches!(right, Value::Python(_)) {
            return self.python_holds(left, op, right, span);
        }
        let order = if let (Some(a), Some(b)) = (self.number(left), self.number(right)) {
            a.order(&b)
        } else if let (Some(a), Some(b)) = (self.text(left), self.text(right)) {
            Some(a.cmp(b))
        } else if matches!(op, CmpOp::Eq | CmpOp::Ne) {
            let equal = match (left, right) {
                (Value::Native(a), Value::Native(b)) => a == b,
                (Value::Bool(a), Value::Bool(b)) => a == b,
                (Value::None, Value::None) => true,
                _ => false,
            };
            return Ok(if op == CmpOp::Eq { equal } else { !equal });
        } else {
            return Err(operand_error(left, op.symbol(), right, span));
        };
        Ok(order.map_or(op == CmpOp::Ne, |order| op.holds(order)))
    }

    pub fn compare(&self, left: &Value, op: CmpOp, right: &Value, span: Span) -> Result<bool> {
        if !matches!(op, CmpOp::Eq | CmpOp::Ne) {
            if matches!((left, right), (Value::Tuple(_), Value::Tuple(_))) {
                self.order(left, left, span)?;
                self.order(right, right, span)?;
                return Ok(op.holds(self.order(left, right, span)?));
            }
            return self.compare_scalars(left, op, right, span);
        }
        let mut pending = vec![(*left, *right)];
        let mut visited = HashSet::new();
        let mut equal = true;
        'pairs: while let Some((a, b)) = pending.pop() {
            match (a, b) {
                (Value::Dict(a), Value::Dict(b)) | (Value::Set(a), Value::Set(b)) => {
                    let (aa, bb) = (self.dictionary(a, span)?, self.dictionary(b, span)?);
                    if aa.len() != bb.len() {
                        equal = false;
                        break;
                    }
                    if !first_visit(&mut visited, (a, b), pending.len() + aa.len(), span)? {
                        continue;
                    }
                    for entry in aa.iter().collect::<Vec<_>>().into_iter().rev() {
                        let Some(value) = bb.get(&entry.key) else {
                            equal = false;
                            break 'pairs;
                        };
                        pending.push((entry.value, *value));
                    }
                }
                (Value::List(a), Value::List(b)) | (Value::Tuple(a), Value::Tuple(b)) => {
                    let (aa, bb) = (self.list(a, span)?, self.list(b, span)?);
                    if aa.len() != bb.len() {
                        equal = false;
                        break;
                    }
                    if !first_visit(&mut visited, (a, b), pending.len() + aa.len(), span)? {
                        continue;
                    }
                    pending.extend(aa.iter().copied().zip(bb.iter().copied()).rev());
                }
                (Value::Record(a), Value::Record(b)) => {
                    let (_, aa) = self.record(a, span)?;
                    let (_, bb) = self.record(b, span)?;
                    if self.record_type_of(a, span)? != self.record_type_of(b, span)? {
                        equal = false;
                        break;
                    }
                    if !first_visit(&mut visited, (a, b), pending.len() + aa.len(), span)? {
                        continue;
                    }
                    pending.extend(aa.iter().copied().zip(bb.iter().copied()).rev());
                }
                (Value::RecordType(a), Value::RecordType(b)) => {
                    if a != b {
                        equal = false;
                        break;
                    }
                }
                (Value::Error(a), Value::Error(b)) => {
                    self.error(a, span)?;
                    self.error(b, span)?;
                    if a != b {
                        equal = false;
                        break;
                    }
                }
                (Value::Function(a), Value::Function(b)) => {
                    self.function(a, span)?;
                    self.function(b, span)?;
                    if a != b {
                        equal = false;
                        break;
                    }
                }
                _ => {
                    if !self.compare_scalars(&a, CmpOp::Eq, &b, span)? {
                        equal = false;
                        break;
                    }
                }
            }
        }
        Ok(if op == CmpOp::Eq { equal } else { !equal })
    }

    pub fn order(&self, left: &Value, right: &Value, span: Span) -> Result<Ordering> {
        enum Work {
            Pair(Value, Value),
            Length(usize, usize),
        }
        let mut work = vec![Work::Pair(*left, *right)];
        let mut visited = 0usize;
        while let Some(next) = work.pop() {
            visited += 1;
            if visited > MAX_ITEMS {
                return Err(Diagnostic::limit(Limit::Items, span));
            }
            let order = match next {
                Work::Length(a, b) => a.cmp(&b),
                Work::Pair(Value::Tuple(a), Value::Tuple(b)) => {
                    let (a, b) = (self.list(a, span)?, self.list(b, span)?);
                    if work.len() + a.len().min(b.len()) > MAX_ITEMS {
                        return Err(Diagnostic::limit(Limit::Items, span));
                    }
                    work.push(Work::Length(a.len(), b.len()));
                    work.extend(a.iter().zip(b).rev().map(|(a, b)| Work::Pair(*a, *b)));
                    continue;
                }
                Work::Pair(a, b) => {
                    let order = match (self.number(&a), self.number(&b)) {
                        (Some(x), Some(y)) => x.order(&y),
                        _ => match (self.text(&a), self.text(&b)) {
                            (Some(x), Some(y)) => Some(x.cmp(y)),
                            _ => return Err(operand_error(&a, "<", &b, span)),
                        },
                    };
                    order.ok_or_else(|| type_error(span))?
                }
            };
            if !order.is_eq() {
                return Ok(order);
            }
        }
        Ok(Ordering::Equal)
    }

    pub fn slice(
        &mut self,
        owner: &Value,
        bounds: [&Value; 3],
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        if let Value::Python(handle) = owner {
            return self.python_slice(*handle, bounds, roots, span);
        }
        let text: Option<Rc<str>> = self.text(owner).cloned();
        let length = match (owner, &text) {
            (Value::List(handle) | Value::Tuple(handle), _) => self.list(*handle, span)?.len(),
            (_, Some(text)) => text.chars().count(),
            _ => return Err(type_error(span)),
        } as isize;
        let integer = |value: &Value| -> Result<Option<isize>> {
            match value {
                Value::None => Ok(None),
                Value::Int(n) => Ok(Some(isize::try_from(*n).unwrap_or(if *n < 0 {
                    isize::MIN
                } else {
                    isize::MAX
                }))),
                Value::Big(_) => Ok(Some(match self.int(value) {
                    Some(n) if n.is_negative() => isize::MIN,
                    _ => isize::MAX,
                })),
                _ => Err(type_error(span)),
            }
        };
        let start = integer(bounds[0])?;
        let stop = integer(bounds[1])?;
        let step = integer(bounds[2])?.unwrap_or(1);
        if step == 0 {
            return Err(Diagnostic::new(
                "E-SLICE-001",
                span,
                "슬라이스의 간격은 0일 수 없습니다.",
            ));
        }
        let step = step.clamp(-length - 1, length + 1);
        let reverse = step < 0;
        let lower = if reverse { -1 } else { 0 };
        let upper = if reverse { length - 1 } else { length };
        let normalize =
            |n: isize| (if n < 0 { n.saturating_add(length) } else { n }).clamp(lower, upper);
        let start = start
            .map(normalize)
            .unwrap_or(if reverse { length - 1 } else { 0 });
        let stop = stop
            .map(normalize)
            .unwrap_or(if reverse { -1 } else { length });
        let selected = |i: isize| {
            if reverse {
                i <= start && i > stop && (start - i) % -step == 0
            } else {
                i >= start && i < stop && (i - start) % step == 0
            }
        };
        if let Some(text) = text {
            let mut result = String::new();
            if reverse {
                for (i, c) in text.chars().rev().enumerate() {
                    if selected(length - 1 - i as isize) {
                        result.push(c);
                    }
                }
            } else {
                for (i, c) in text.chars().enumerate() {
                    if selected(i as isize) {
                        result.push(c);
                    }
                }
            }
            return self.new_text(result, roots, span);
        }
        let handle = owner.handle().ok_or_else(|| internal(span))?;
        let items = self.list(handle, span)?;
        let mut values = Vec::new();
        let mut index = start;
        while if reverse { index > stop } else { index < stop } {
            values.push(items[index as usize]);
            index += step;
        }
        self.new_collection(
            CollectionKind::of(owner),
            values,
            roots.chain(Some(handle)),
            span,
        )
    }
}

fn first_visit(
    visited: &mut HashSet<(Handle, Handle)>,
    pair: (Handle, Handle),
    pending: usize,
    span: Span,
) -> Result<bool> {
    if visited.contains(&pair) {
        return Ok(false);
    }
    if visited.len() >= MAX_ITEMS || pending > MAX_ITEMS {
        return Err(Diagnostic::limit(Limit::Items, span));
    }
    visited.insert(pair);
    Ok(true)
}

pub(super) fn index_error(index: impl std::fmt::Display, length: usize, span: Span) -> Diagnostic {
    let error = Diagnostic::new(
        "E-INDEX-001",
        span,
        format!(
            "인덱스 {index}{} 범위 밖입니다(길이 {length}).",
            TOPIC.after(&index.to_string())
        ),
    );
    match length {
        0 => error.with_help("비어 있어서 인덱스로 꺼낼 원소가 없습니다."),
        1 => error.with_help("쓸 수 있는 인덱스는 0과 -1입니다."),
        _ => error.with_help(format!(
            "쓸 수 있는 인덱스는 0부터 {}까지, 뒤에서 세면 -1부터 -{length}까지입니다.",
            length - 1
        )),
    }
}

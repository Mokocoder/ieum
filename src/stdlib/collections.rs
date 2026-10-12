use super::Native;
use crate::diagnostic::{Diagnostic, Result, Span, memory, type_error};
use crate::limits::{Limit, MAX_DATA_DEPTH, MAX_ITEMS};
use crate::runtime::heap::{Handle, Heap};
use crate::runtime::objects::CollectionKind;
use crate::runtime::value::Value;
use crate::syntax::ast::CmpOp;

pub(super) fn add(
    container: &Value,
    item: &Value,
    heap: &mut Heap,
    roots: impl Iterator<Item = Handle>,
    span: Span,
) -> Result<Value> {
    match container {
        Value::List(list) => heap.append(*list, *item, roots, span)?,
        Value::Set(set) => {
            let key = heap.key(item, span)?;
            if heap.dictionary(*set, span)?.position(&key).is_none() {
                heap.store_key(*set, key, *item, Value::None, roots, span)?;
            }
        }
        _ => return Err(type_error(span)),
    }
    Ok(Value::None)
}

pub(super) fn find(container: &Value, item: &Value, heap: &Heap, span: Span) -> Result<Value> {
    let position = match (container, heap.text(container), heap.text(item)) {
        (_, Some(text), Some(part)) => text
            .find(&**part)
            .map(|index| text[..index].chars().count()),
        (Value::List(handle) | Value::Tuple(handle), ..) => position(*handle, item, heap, span)?,
        _ => return Err(type_error(span)),
    };
    Ok(position.map_or(Value::None, |index| Value::Int(index as i64)))
}

fn position(handle: Handle, item: &Value, heap: &Heap, span: Span) -> Result<Option<usize>> {
    for (index, element) in heap.list(handle, span)?.iter().enumerate() {
        if heap.compare(element, CmpOp::Eq, item, span)? {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

pub(super) fn contains(container: &Value, item: &Value, heap: &Heap, span: Span) -> Result<bool> {
    match (container, heap.text(container), heap.text(item)) {
        (_, Some(text), Some(part)) => Ok(text.contains(&**part)),
        (Value::List(handle) | Value::Tuple(handle), ..) => {
            Ok(position(*handle, item, heap, span)?.is_some())
        }
        (Value::Set(handle) | Value::Dict(handle), ..) => {
            let key = heap.key(item, span)?;
            Ok(heap.dictionary(*handle, span)?.position(&key).is_some())
        }
        _ => Err(type_error(span)),
    }
}

pub(super) fn remove(
    container: &Value,
    item: &Value,
    heap: &mut Heap,
    span: Span,
) -> Result<Value> {
    match container {
        Value::List(handle) => {
            let index = position(*handle, item, heap, span)?.ok_or_else(|| {
                Diagnostic::new("E-INDEX-002", span, "목록에 지울 값이 없습니다.")
            })?;
            heap.remove_index(*handle, &Value::Int(index as i64), span)
        }
        Value::Set(handle) => {
            let key = heap.key(item, span)?;
            let set = heap.dictionary(*handle, span)?;
            let stored = set.position(&key).map(|index| set.entry(index).original);
            heap.remove_key(*handle, &key, span)?;
            Ok(stored.unwrap_or(*item))
        }
        Value::Dict(handle) => {
            let key = heap.key(item, span)?;
            heap.remove_key(*handle, &key, span)
        }
        _ => Err(type_error(span)),
    }
}

#[inline(never)]
pub(super) fn copy(
    value: &Value,
    heap: &mut Heap,
    roots: impl Iterator<Item = Handle>,
    span: Span,
) -> Result<Value> {
    let roots = roots.chain(value.handle());
    match value {
        Value::List(_) => {
            let items = heap.items(value, span)?;
            heap.new_list(items, roots, span)
        }
        Value::Set(_) => {
            let items = heap.items(value, span)?;
            heap.new_collection(CollectionKind::Set, items, roots, span)
        }
        Value::Dict(handle) => {
            let entries = heap
                .dictionary(*handle, span)?
                .iter()
                .map(|entry| (entry.original, entry.value))
                .collect();
            heap.new_dictionary(entries, roots, span)
        }
        Value::Record(handle) => {
            let kind = heap.record_type_of(*handle, span)?;
            let values = heap.record(*handle, span)?.1.to_vec();
            heap.new_record(kind, values, roots, span)
        }
        _ => Err(type_error(span)),
    }
}

#[inline(never)]
pub(super) fn sort(
    value: &Value,
    by: &Value,
    descending: bool,
    heap: &mut Heap,
    roots: impl Iterator<Item = Handle> + Clone,
    span: Span,
) -> Result<Value> {
    let items = heap.snapshot(value, roots.clone(), span)?;
    let mut keys: Vec<Value> = Vec::new();
    keys.try_reserve_exact(items.len())
        .map_err(|_| memory(span))?;
    let name = heap.text(by).cloned();
    for item in &items {
        let held = items.iter().chain(&keys).filter_map(Value::handle);
        let live = roots.clone().chain(held);
        let key = match (by, &name) {
            (Value::None, _) => *item,
            (_, Some(name)) => heap.read_attribute(item, name, live, span)?,
            (Value::Int(_), _) => heap.index(item, by, live, span)?,
            _ => {
                return Err(Diagnostic::new(
                    "E-TYPE-001",
                    span,
                    "정렬 기준은 필드나 키 이름(문자열), 위치(정수) 또는 함수로 씁니다.",
                ));
            }
        };
        keys.push(key);
    }
    let held: Vec<Handle> = items
        .iter()
        .chain(&keys)
        .filter_map(Value::handle)
        .collect();
    sorted(items, keys, descending, heap, roots.chain(held), span)
}

pub(crate) fn sorted(
    items: Vec<Value>,
    keys: Vec<Value>,
    descending: bool,
    heap: &mut Heap,
    roots: impl Iterator<Item = Handle>,
    span: Span,
) -> Result<Value> {
    let mut pairs: Vec<(Value, Value)> = keys.into_iter().zip(items).collect();
    stable_sort(&mut pairs, descending, heap, span)?;
    heap.new_list(
        pairs.into_iter().map(|(_, item)| item).collect(),
        roots,
        span,
    )
}

#[inline(never)]
pub(super) fn set_operation(
    operation: Native,
    left: &Value,
    right: &Value,
    heap: &mut Heap,
    roots: impl Iterator<Item = Handle>,
    span: Span,
) -> Result<Value> {
    let (Value::Set(a), Value::Set(b)) = (left, right) else {
        return Err(type_error(span));
    };
    let (a, b) = (heap.dictionary(*a, span)?, heap.dictionary(*b, span)?);
    if operation == Native::IsSubset {
        let subset = a.iter().all(|entry| b.position(&entry.key).is_some());
        return Ok(Value::Bool(subset));
    }
    let mut values = Vec::new();
    for entry in a.iter() {
        let present = b.position(&entry.key).is_some();
        if operation == Native::Union
            || (operation == Native::Intersection && present)
            || (operation == Native::Difference && !present)
        {
            values.push(entry.original);
        }
    }
    if operation == Native::Union {
        for entry in b.iter() {
            if a.position(&entry.key).is_none() {
                if values.len() >= MAX_ITEMS {
                    return Err(Diagnostic::limit(Limit::Items, span));
                }
                values.push(entry.original);
            }
        }
    }
    heap.new_collection(CollectionKind::Set, values, roots, span)
}

enum Shape {
    Unknown,
    Number,
    Text,
    Tuple(Vec<Shape>),
}

impl Shape {
    fn name(&self) -> &'static str {
        match self {
            Self::Unknown => "값",
            Self::Number => "수",
            Self::Text => "문자열",
            Self::Tuple(_) => "튜플",
        }
    }

    fn add(&mut self, key: &Value, depth: usize, heap: &Heap, span: Span) -> Result<()> {
        if depth > MAX_DATA_DEPTH {
            return Err(Diagnostic::limit(Limit::DataDepth, span));
        }
        let own = match key {
            Value::Float(f) if f.is_nan() => {
                return Err(Diagnostic::new(
                    "E-VALUE-001",
                    span,
                    "NaN은 다른 수와 크기를 비교할 수 없어 정렬할 수 없습니다.",
                ));
            }
            Value::Int(_) | Value::Big(_) | Value::Float(_) => Self::Number,
            Value::Text(_) => Self::Text,
            Value::Tuple(_) => Self::Tuple(Vec::new()),
            other => {
                return Err(Diagnostic::new(
                    "E-TYPE-001",
                    span,
                    format!(
                        "{}은 크기를 비교할 수 없어 정렬할 수 없습니다.",
                        other.kind_name()
                    ),
                ));
            }
        };
        match (&mut *self, own) {
            (Self::Unknown, own) => *self = own,
            (Self::Number, Self::Number)
            | (Self::Text, Self::Text)
            | (Self::Tuple(_), Self::Tuple(_)) => {}
            (shape, own) => {
                return Err(Diagnostic::new(
                    "E-TYPE-001",
                    span,
                    format!(
                        "정렬 기준에 {}와 {}이 섞여 있어 비교할 수 없습니다.",
                        shape.name(),
                        own.name()
                    ),
                ));
            }
        }
        if let (Self::Tuple(positions), Value::Tuple(handle)) = (self, key) {
            let items = heap.list(*handle, span)?;
            if positions.len() < items.len() {
                positions.resize_with(items.len(), || Self::Unknown);
            }
            for (position, item) in positions.iter_mut().zip(items) {
                position.add(item, depth + 1, heap, span)?;
            }
        }
        Ok(())
    }
}

fn stable_sort(
    items: &mut [(Value, Value)],
    descending: bool,
    heap: &Heap,
    span: Span,
) -> Result<()> {
    let mut shape = Shape::Unknown;
    for (key, _) in items.iter() {
        shape.add(key, 0, heap, span)?;
    }
    let mut failure = None;
    items.sort_by(|(a, _), (b, _)| {
        let order = heap.order(a, b, span).unwrap_or_else(|error| {
            failure.get_or_insert(error);
            std::cmp::Ordering::Equal
        });
        if descending { order.reverse() } else { order }
    });
    failure.map_or(Ok(()), Err)
}

#[inline(never)]
pub(super) fn zip(
    left: &Value,
    right: &Value,
    heap: &mut Heap,
    roots: impl Iterator<Item = Handle> + Clone,
    span: Span,
) -> Result<Value> {
    let firsts = heap.snapshot(left, roots.clone(), span)?;
    let held: Vec<Handle> = firsts.iter().filter_map(Value::handle).collect();
    let seconds = heap.snapshot(right, roots.clone().chain(held.iter().copied()), span)?;
    pairs(firsts.into_iter().zip(seconds).collect(), heap, roots, span)
}

#[inline(never)]
pub(super) fn enumerate(
    value: &Value,
    heap: &mut Heap,
    roots: impl Iterator<Item = Handle> + Clone,
    span: Span,
) -> Result<Value> {
    let rows = heap
        .snapshot(value, roots.clone(), span)?
        .into_iter()
        .enumerate()
        .map(|(index, item)| (Value::Int(index as i64 + 1), item))
        .collect();
    pairs(rows, heap, roots, span)
}

fn pairs(
    rows: Vec<(Value, Value)>,
    heap: &mut Heap,
    roots: impl Iterator<Item = Handle> + Clone,
    span: Span,
) -> Result<Value> {
    let held: Vec<Handle> = rows
        .iter()
        .flat_map(|(a, b)| [a.handle(), b.handle()])
        .flatten()
        .collect();
    let mut tuples: Vec<Value> = Vec::new();
    tuples
        .try_reserve_exact(rows.len())
        .map_err(|_| memory(span))?;
    for (first, second) in rows {
        let live = roots
            .clone()
            .chain(held.iter().copied())
            .chain(tuples.iter().filter_map(Value::handle));
        let tuple = heap.new_collection(CollectionKind::Tuple, vec![first, second], live, span)?;
        tuples.push(tuple);
    }
    let live = roots.chain(held.iter().copied());
    heap.new_list(tuples, live, span)
}

pub(super) fn count(container: &Value, item: &Value, heap: &Heap, span: Span) -> Result<Value> {
    let count = match (container, heap.text(container), heap.text(item)) {
        (_, Some(text), Some(part)) => {
            if part.is_empty() {
                return Err(Diagnostic::new(
                    "E-TEXT-001",
                    span,
                    "빈 문자열은 셀 수 없습니다.",
                ));
            }
            text.matches(&**part).count()
        }
        (Value::List(handle) | Value::Tuple(handle), ..) => {
            let mut count = 0;
            for element in heap.list(*handle, span)? {
                if heap.compare(element, CmpOp::Eq, item, span)? {
                    count += 1;
                }
            }
            count
        }
        _ => return Err(type_error(span)),
    };
    Ok(Value::Int(count as i64))
}

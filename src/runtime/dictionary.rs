use std::collections::HashMap;
use std::mem::size_of;
use std::rc::Rc;

use foldhash::fast::RandomState;
use num_bigint::BigInt;

use super::value::Value;
use crate::diagnostic::{Diagnostic, Result, Span, memory};
use crate::limits::{Limit, MAX_ITEMS};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Key {
    Int(i64),
    Big(Rc<BigInt>),
    Float(u64),
    Bool(bool),
    Text(Rc<str>),
    None,
    Tuple(Rc<[Key]>),
}

impl Key {
    fn bytes(&self) -> usize {
        match self {
            Self::Tuple(values) => {
                values.len() * size_of::<Key>() + values.iter().map(Self::bytes).sum::<usize>()
            }
            _ => 0,
        }
    }
}

pub(crate) fn invalid_key(span: Span) -> Diagnostic {
    Diagnostic::new(
        "E-KEY-002",
        span,
        "이 값은 키로 쓸 수 없습니다. 키는 문자열, 정수, 실수(NaN 제외), 논리값, 없음이나 이들로 만든 튜플이어야 합니다.",
    )
}

pub(crate) fn missing_key(span: Span) -> Diagnostic {
    Diagnostic::new(
        "E-KEY-001",
        span,
        "찾는 값이 사전의 키나 집합의 원소에 없습니다.",
    )
}

#[derive(Clone)]
pub(crate) struct Entry {
    pub key: Key,
    pub original: Value,
    pub value: Value,
}

impl Entry {
    pub fn bytes(&self) -> usize {
        self.key.bytes()
    }
}

pub(crate) struct Dictionary {
    slots: Vec<Option<Entry>>,
    live: usize,
    index: HashMap<Key, usize, RandomState>,
    pub version: u64,
}

impl Dictionary {
    pub fn new(values: Vec<(Key, Value, Value)>, span: Span) -> Result<Self> {
        if values.len() > MAX_ITEMS {
            return Err(Diagnostic::limit(Limit::Items, span));
        }
        let mut result = Self::with_capacity(values.len(), span)?;
        for (key, original, value) in values {
            if let Some(index) = result.position(&key) {
                result.entry_mut(index).value = value;
            } else {
                result.push(Entry {
                    key,
                    original,
                    value,
                });
            }
        }
        Ok(result)
    }

    fn with_capacity(capacity: usize, span: Span) -> Result<Self> {
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(capacity)
            .map_err(|_| memory(span))?;
        let mut index = HashMap::with_hasher(RandomState::default());
        index.try_reserve(capacity).map_err(|_| memory(span))?;
        Ok(Self {
            slots,
            live: 0,
            index,
            version: 0,
        })
    }

    pub fn position(&self, key: &Key) -> Option<usize> {
        self.index.get(key).copied()
    }

    pub fn get(&self, key: &Key) -> Option<&Value> {
        self.position(key).map(|index| &self.entry(index).value)
    }

    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entry> + Clone {
        self.slots.iter().flatten()
    }

    pub fn entry(&self, position: usize) -> &Entry {
        self.slots[position]
            .as_ref()
            .expect("위치는 살아 있는 항목을 가리킵니다")
    }

    pub fn entry_mut(&mut self, position: usize) -> &mut Entry {
        self.slots[position]
            .as_mut()
            .expect("위치는 살아 있는 항목을 가리킵니다")
    }

    pub fn next_from(&self, position: usize) -> Option<(usize, &Entry)> {
        let slots = self.slots.get(position..)?;
        let (offset, entry) = slots
            .iter()
            .enumerate()
            .find_map(|(offset, slot)| Some((offset, slot.as_ref()?)))?;
        Some((position + offset, entry))
    }

    pub fn bytes(&self) -> usize {
        self.slots.capacity() * size_of::<Option<Entry>>()
            + self.index.capacity() * (size_of::<Key>() + size_of::<usize>() + 1)
            + self.iter().map(Entry::bytes).sum::<usize>()
    }

    pub fn needs_growth(&self) -> bool {
        self.slots.len() == self.slots.capacity() || self.index.len() == self.index.capacity()
    }

    pub fn grown(&self, span: Span) -> Result<Self> {
        let capacity = (self.live.saturating_mul(2)).clamp(4, MAX_ITEMS);
        let mut result = Self::with_capacity(capacity, span)?;
        result.version = self.version;
        for entry in self.iter() {
            result.push(entry.clone());
        }
        Ok(result)
    }

    pub fn push(&mut self, entry: Entry) {
        debug_assert!(self.slots.len() < self.slots.capacity());
        debug_assert!(self.index.len() < self.index.capacity());
        self.index.insert(entry.key.clone(), self.slots.len());
        self.slots.push(Some(entry));
        self.live += 1;
    }

    pub fn remove(&mut self, position: usize) -> Entry {
        let entry = self.slots[position]
            .take()
            .expect("위치는 살아 있는 항목을 가리킵니다");
        self.index.remove(&entry.key);
        self.live -= 1;
        if self.slots.len() - self.live > self.live.max(8) {
            self.slots.retain(Option::is_some);
            for (position, entry) in self.slots.iter().flatten().enumerate() {
                *self
                    .index
                    .get_mut(&entry.key)
                    .expect("모든 항목이 색인에 있습니다") = position;
            }
        }
        entry
    }
}

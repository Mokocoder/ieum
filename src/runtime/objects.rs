use std::mem::size_of;
use std::rc::Rc;

use num_bigint::BigInt;
use num_traits::FromPrimitive;

use super::code::{FunctionInfo, RecordInfo};
use super::dictionary::{Dictionary, Entry, Key, invalid_key, missing_key};
use super::heap::{Handle, Heap, Object};
use super::operations::index_error;
use super::value::{Constant, Int, Number, Value};
use crate::diagnostic::{
    CallFrame, Diagnostic, ExecutionError, Result, Span, internal, memory, type_error,
};
use crate::limits::{Limit, MAX_ITEMS, MAX_KEY_DEPTH, MAX_TEXT_BYTES};

#[derive(Clone, Copy)]
pub(crate) enum CollectionKind {
    List,
    Tuple,
    Set,
}

impl CollectionKind {
    pub(crate) fn of(value: &Value) -> Self {
        if matches!(value, Value::Tuple(_)) {
            Self::Tuple
        } else {
            Self::List
        }
    }
}

impl Heap {
    pub fn builtins(
        &mut self,
        arguments: &[String],
        system: bool,
        span: Span,
    ) -> Result<[Value; 2]> {
        let texts = arguments.iter().map(|text| Rc::from(text.as_str()));
        let arguments = self.text_list(texts, std::iter::empty(), span)?;
        let roots = arguments.handle().into_iter();
        let variables = self.new_dictionary(Vec::new(), roots.clone(), span)?;
        if system {
            let Value::Dict(handle) = variables else {
                return Err(internal(span));
            };
            let roots = roots.chain([handle]);
            for (name, value) in crate::stdlib::environment() {
                let name = self.new_text(name, roots.clone(), span)?;
                let value = self.new_text(value, roots.clone().chain(name.handle()), span)?;
                let key = self.key(&name, span)?;
                let held = name.handle().into_iter().chain(value.handle());
                self.store_key(handle, key, name, value, roots.clone().chain(held), span)?;
            }
        }
        Ok([arguments, variables])
    }

    pub fn new_constant(
        &mut self,
        constant: &Constant,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        Ok(match constant {
            Constant::Int(n) => return self.new_int(n.clone(), roots, span),
            Constant::Float(f) => Value::Float(*f),
            Constant::Bool(b) => Value::Bool(*b),
            Constant::Text(text) => return self.new_text(text.clone(), roots, span),
            Constant::None => Value::None,
            Constant::Native(native) => Value::Native(*native),
        })
    }

    pub fn new_text(
        &mut self,
        text: impl Into<Rc<str>>,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        let text = text.into();
        if text.len() > MAX_TEXT_BYTES {
            return Err(Diagnostic::limit(Limit::Text, span));
        }
        let bytes = size_of::<Object>() + text.len();
        self.alloc(Object::Text(text), bytes, roots, span)
            .map(Value::Text)
    }

    pub fn text(&self, value: &Value) -> Option<&Rc<str>> {
        let Value::Text(handle) = value else {
            return None;
        };
        match self.object(*handle, Span::default()) {
            Ok(Object::Text(text)) => Some(text),
            _ => None,
        }
    }

    pub fn text_of(&self, value: &Value, span: Span) -> Result<Rc<str>> {
        self.text(value).cloned().ok_or_else(|| type_error(span))
    }

    pub fn text_list(
        &mut self,
        parts: impl IntoIterator<Item = Rc<str>>,
        roots: impl Iterator<Item = Handle> + Clone,
        span: Span,
    ) -> Result<Value> {
        let parts: Vec<Rc<str>> = parts.into_iter().collect();
        if parts.len() > MAX_ITEMS {
            return Err(Diagnostic::limit(Limit::Items, span));
        }
        let list = self.new_list(vec![Value::None; parts.len()], roots.clone(), span)?;
        let Value::List(handle) = list else {
            return Err(internal(span));
        };
        for (index, part) in parts.into_iter().enumerate() {
            let text = self.new_text(part, roots.clone().chain([handle]), span)?;
            self.store_index(handle, index, text, span)?;
        }
        Ok(list)
    }

    pub fn new_int(
        &mut self,
        n: Int,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        match n {
            Int::Small(n) => Ok(Value::Int(n)),
            Int::Big(big) => {
                let bytes = size_of::<Object>() + big.bits().div_ceil(8) as usize;
                self.alloc(Object::Big(big), bytes, roots, span)
                    .map(Value::Big)
            }
        }
    }

    pub fn int(&self, value: &Value) -> Option<Int> {
        match value {
            Value::Int(n) => Some(Int::Small(*n)),
            Value::Big(handle) => match self.object(*handle, Span::default()) {
                Ok(Object::Big(big)) => Some(Int::Big(big.clone())),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn number(&self, value: &Value) -> Option<Number> {
        match value {
            Value::Float(f) => Some(Number::Float(*f)),
            value => self.int(value).map(Number::Int),
        }
    }

    pub fn new_number(
        &mut self,
        number: Number,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        match number {
            Number::Int(n) => self.new_int(n, roots, span),
            Number::Float(f) => Ok(Value::Float(f)),
        }
    }

    pub fn as_float(&self, value: &Value, span: Span) -> Result<f64> {
        self.number(value)
            .ok_or_else(|| type_error(span))?
            .as_float(span)
    }

    pub fn new_cell(
        &mut self,
        value: Option<Value>,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Handle> {
        self.alloc(Object::Cell(value), size_of::<Object>(), roots, span)
    }

    pub fn cell(&self, handle: Handle, span: Span) -> Result<Option<&Value>> {
        match self.object(handle, span)? {
            Object::Cell(value) => Ok(value.as_ref()),
            _ => Err(internal(span)),
        }
    }

    pub fn store_cell(&mut self, handle: Handle, value: Value, span: Span) -> Result<()> {
        let Object::Cell(slot) = self.object_mut(handle, span)? else {
            return Err(internal(span));
        };
        *slot = Some(value);
        Ok(())
    }

    pub fn new_globals(
        &mut self,
        names: &[Rc<str>],
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Handle> {
        let bytes = globals_bytes(names.len());
        let object = Object::Globals {
            names: names.to_vec(),
            values: vec![None; names.len()],
        };
        self.alloc(object, bytes, roots, span)
    }

    pub fn extend_globals(
        &mut self,
        handle: Handle,
        names: &[Rc<str>],
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<()> {
        let Object::Globals { names: known, .. } = self.object(handle, span)? else {
            return Err(internal(span));
        };
        let (added, removed) = (globals_bytes(names.len()), globals_bytes(known.len()));
        let (bytes, object_bytes) = self.resized_bytes(handle, added, removed, roots, span)?;
        let Object::Globals {
            names: known,
            values,
        } = self.object_mut(handle, span)?
        else {
            return Err(internal(span));
        };
        known.extend_from_slice(&names[known.len()..]);
        values.resize(names.len(), None);
        self.commit_bytes(handle, bytes, object_bytes);
        Ok(())
    }

    pub fn global(
        &self,
        handle: Handle,
        index: u32,
        span: Span,
    ) -> Result<std::result::Result<Value, &str>> {
        let Object::Globals { names, values } = self.object(handle, span)? else {
            return Err(internal(span));
        };
        let index = index as usize;
        match values.get(index) {
            Some(Some(value)) => Ok(Ok(*value)),
            Some(None) => Ok(Err(&names[index])),
            None => Err(internal(span)),
        }
    }

    pub fn global_index(&self, handle: Handle, name: &str, span: Span) -> Result<Option<u32>> {
        let Object::Globals { names, .. } = self.object(handle, span)? else {
            return Err(internal(span));
        };
        Ok(names
            .iter()
            .position(|known| &**known == name)
            .map(|i| i as u32))
    }

    pub fn store_global(
        &mut self,
        handle: Handle,
        index: u32,
        value: Value,
        span: Span,
    ) -> Result<()> {
        let Object::Globals { values, .. } = self.object_mut(handle, span)? else {
            return Err(internal(span));
        };
        *values
            .get_mut(index as usize)
            .ok_or_else(|| internal(span))? = Some(value);
        Ok(())
    }

    pub fn function(&self, handle: Handle, span: Span) -> Result<FunctionParts<'_>> {
        match self.object(handle, span)? {
            Object::Function {
                info,
                defaults,
                captures,
                globals,
            } => Ok(FunctionParts {
                info,
                defaults,
                captures,
                globals: *globals,
            }),
            _ => Err(internal(span)),
        }
    }

    pub fn new_function(
        &mut self,
        info: Rc<FunctionInfo>,
        defaults: Vec<Value>,
        captures: Vec<Handle>,
        globals: Handle,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        let bytes = size_of::<Object>()
            + defaults.capacity() * size_of::<Value>()
            + captures.capacity() * size_of::<Handle>();
        let object = Object::Function {
            info,
            defaults,
            captures,
            globals,
        };
        self.alloc(object, bytes, roots, span).map(Value::Function)
    }

    pub fn new_record_type(
        &mut self,
        info: Rc<RecordInfo>,
        defaults: Vec<Value>,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        let bytes = size_of::<Object>() + defaults.capacity() * size_of::<Value>();
        let handle = self.alloc(Object::RecordType { info, defaults }, bytes, roots, span)?;
        Ok(Value::RecordType(handle))
    }

    pub fn record_type(&self, handle: Handle, span: Span) -> Result<(&RecordInfo, &[Value])> {
        match self.object(handle, span)? {
            Object::RecordType { info, defaults } => Ok((info, defaults)),
            _ => Err(internal(span)),
        }
    }

    pub fn new_record(
        &mut self,
        record_type: Handle,
        values: Vec<Value>,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        let bytes = size_of::<Object>() + values.capacity() * size_of::<Value>();
        let object = Object::Record {
            record_type,
            values,
        };
        let handle = self.alloc(object, bytes, roots, span)?;
        Ok(Value::Record(handle))
    }

    pub fn record(&self, handle: Handle, span: Span) -> Result<(&RecordInfo, &[Value])> {
        match self.object(handle, span)? {
            Object::Record {
                record_type,
                values,
            } => Ok((self.record_type(*record_type, span)?.0, values)),
            _ => Err(internal(span)),
        }
    }

    pub fn record_type_of(&self, handle: Handle, span: Span) -> Result<Handle> {
        match self.object(handle, span)? {
            Object::Record { record_type, .. } => Ok(*record_type),
            _ => Err(internal(span)),
        }
    }

    pub fn store_field(
        &mut self,
        handle: Handle,
        index: usize,
        value: Value,
        span: Span,
    ) -> Result<()> {
        let Object::Record { values, .. } = self.object_mut(handle, span)? else {
            return Err(internal(span));
        };
        *values.get_mut(index).ok_or_else(|| internal(span))? = value;
        Ok(())
    }

    pub fn list(&self, handle: Handle, span: Span) -> Result<&[Value]> {
        match self.object(handle, span)? {
            Object::List(values) | Object::Tuple(values) => Ok(values),
            _ => Err(internal(span)),
        }
    }

    fn list_mut(&mut self, handle: Handle, span: Span) -> Result<&mut Vec<Value>> {
        match self.object_mut(handle, span)? {
            Object::List(values) => Ok(values),
            _ => Err(internal(span)),
        }
    }

    pub fn new_list(
        &mut self,
        values: Vec<Value>,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        self.new_collection(CollectionKind::List, values, roots, span)
    }

    pub fn new_collection(
        &mut self,
        kind: CollectionKind,
        values: Vec<Value>,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        if values.len() > MAX_ITEMS {
            return Err(Diagnostic::limit(Limit::Items, span));
        }
        if matches!(kind, CollectionKind::Set) {
            let entries = values
                .into_iter()
                .map(|value| Ok((self.key(&value, span)?, value, Value::None)))
                .collect::<Result<Vec<_>>>()?;
            let set = Dictionary::new(entries, span)?;
            let bytes = size_of::<Object>() + set.bytes();
            return self
                .alloc(Object::Set(Box::new(set)), bytes, roots, span)
                .map(Value::Set);
        }
        let bytes = size_of::<Object>() + values.capacity() * size_of::<Value>();
        match kind {
            CollectionKind::List => self
                .alloc(Object::List(values), bytes, roots, span)
                .map(Value::List),
            CollectionKind::Tuple => self
                .alloc(Object::Tuple(values), bytes, roots, span)
                .map(Value::Tuple),
            CollectionKind::Set => unreachable!(),
        }
    }

    pub fn append(
        &mut self,
        handle: Handle,
        value: Value,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<()> {
        let Object::List(items) = self.object(handle, span)? else {
            return Err(internal(span));
        };
        if items.len() >= MAX_ITEMS {
            return Err(Diagnostic::limit(Limit::Items, span));
        }
        if items.len() < items.capacity() {
            self.list_mut(handle, span)?.push(value);
            return Ok(());
        }
        let capacity = (items.capacity().saturating_mul(2)).clamp(4, MAX_ITEMS);
        let added = (capacity - items.capacity()) * size_of::<Value>();
        let (bytes, object_bytes) =
            self.resized_bytes(handle, added, 0, roots.chain(value.handle()), span)?;
        let items = self.list_mut(handle, span)?;
        items
            .try_reserve_exact(capacity - items.len())
            .map_err(|_| memory(span))?;
        items.push(value);
        self.commit_bytes(handle, bytes, object_bytes);
        Ok(())
    }

    pub fn try_append(&mut self, handle: Handle, value: Value) -> bool {
        match self.object_mut(handle, Span::default()) {
            Ok(Object::List(items)) if items.len() < items.capacity() => {
                items.push(value);
                true
            }
            _ => false,
        }
    }

    pub fn store_index(
        &mut self,
        handle: Handle,
        index: usize,
        value: Value,
        span: Span,
    ) -> Result<()> {
        let items = self.list_mut(handle, span)?;
        let length = items.len();
        *items
            .get_mut(index)
            .ok_or_else(|| index_error(index, length, span))? = value;
        Ok(())
    }

    pub(super) fn fill(
        &mut self,
        handle: Handle,
        values: Vec<Value>,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<()> {
        let added = values.capacity() * size_of::<Value>();
        let held = values.iter().filter_map(Value::handle);
        let (bytes, object_bytes) =
            self.resized_bytes(handle, added, 0, roots.chain(held), span)?;
        match self.object_mut(handle, span)? {
            Object::List(slot)
            | Object::Tuple(slot)
            | Object::Record { values: slot, .. }
            | Object::RecordType { defaults: slot, .. }
            | Object::Function { defaults: slot, .. } => *slot = values,
            _ => return Err(internal(span)),
        }
        self.commit_bytes(handle, bytes, object_bytes);
        Ok(())
    }

    pub fn remove_index(&mut self, handle: Handle, index: &Value, span: Span) -> Result<Value> {
        let length = self.list(handle, span)?.len();
        let index = self.index_position(index, length, span)?;
        Ok(self.list_mut(handle, span)?.remove(index))
    }

    pub fn snapshot(
        &mut self,
        value: &Value,
        roots: impl Iterator<Item = Handle> + Clone,
        span: Span,
    ) -> Result<Vec<Value>> {
        match value {
            Value::Text(_) => {
                let text = self.text_of(value, span)?;
                if text.chars().count() > MAX_ITEMS {
                    return Err(Diagnostic::limit(Limit::Items, span));
                }
                let list = self.text_list(
                    text.chars()
                        .map(|c| Rc::from(c.encode_utf8(&mut [0; 4]) as &str)),
                    roots,
                    span,
                )?;
                let Value::List(handle) = list else {
                    return Err(internal(span));
                };
                Ok(self.list(handle, span)?.to_vec())
            }
            _ => self.items(value, span),
        }
    }

    pub fn items(&self, value: &Value, span: Span) -> Result<Vec<Value>> {
        match value {
            Value::List(handle) | Value::Tuple(handle) => Ok(self.list(*handle, span)?.to_vec()),
            Value::Dict(handle) | Value::Set(handle) => Ok(self
                .dictionary(*handle, span)?
                .iter()
                .map(|entry| entry.original)
                .collect()),
            _ => Err(type_error(span)),
        }
    }

    pub fn dictionary(&self, handle: Handle, span: Span) -> Result<&Dictionary> {
        match self.object(handle, span)? {
            Object::Dict(dictionary) | Object::Set(dictionary) => Ok(dictionary),
            _ => Err(internal(span)),
        }
    }

    fn dictionary_mut(&mut self, handle: Handle, span: Span) -> Result<&mut Dictionary> {
        match self.object_mut(handle, span)? {
            Object::Dict(dictionary) | Object::Set(dictionary) => Ok(dictionary),
            _ => Err(internal(span)),
        }
    }

    pub fn new_dictionary(
        &mut self,
        values: Vec<(Value, Value)>,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        let values = values
            .into_iter()
            .map(|(key, value)| Ok((self.key(&key, span)?, key, value)))
            .collect::<Result<Vec<_>>>()?;
        let dictionary = Dictionary::new(values, span)?;
        let bytes = size_of::<Object>() + dictionary.bytes();
        self.alloc(Object::Dict(Box::new(dictionary)), bytes, roots, span)
            .map(Value::Dict)
    }

    pub fn named_dictionary(
        &mut self,
        entries: Vec<(Rc<str>, Value)>,
        roots: impl Iterator<Item = Handle> + Clone,
        span: Span,
    ) -> Result<Value> {
        let held: Vec<Handle> = entries
            .iter()
            .filter_map(|(_, value)| value.handle())
            .collect();
        let roots = roots.chain(held.iter().copied());
        let dictionary = self.new_dictionary(Vec::new(), roots.clone(), span)?;
        let Value::Dict(handle) = dictionary else {
            return Err(internal(span));
        };
        let roots = roots.chain([handle]);
        for (name, value) in entries {
            let key = self.new_text(name.clone(), roots.clone(), span)?;
            let live = roots.clone().chain(key.handle());
            self.store_key(handle, Key::Text(name), key, value, live, span)?;
        }
        Ok(dictionary)
    }

    fn scalar_key(&self, value: &Value, span: Span) -> Result<Key> {
        Ok(match value {
            Value::Int(n) => Key::Int(*n),
            Value::Big(_) => match self.int(value) {
                Some(Int::Big(big)) => Key::Big(big),
                _ => return Err(internal(span)),
            },
            Value::Float(f) if f.is_nan() => return Err(invalid_key(span)),
            Value::Float(f) if f.is_finite() && f.fract() == 0.0 => {
                match Int::from(BigInt::from_f64(*f).ok_or_else(|| internal(span))?) {
                    Int::Small(n) => Key::Int(n),
                    Int::Big(big) => Key::Big(big),
                }
            }
            Value::Float(f) => Key::Float(f.to_bits()),
            Value::Bool(b) => Key::Bool(*b),
            Value::Text(_) => Key::Text(self.text_of(value, span)?),
            Value::None => Key::None,
            _ => return Err(invalid_key(span)),
        })
    }

    /// 파생된 해시, 같음, 해제가 튜플 키 안으로 재귀하므로 중첩 깊이와 노드 수를 제한합니다.
    pub fn key(&self, value: &Value, span: Span) -> Result<Key> {
        if !matches!(value, Value::Tuple(_)) {
            return self.scalar_key(value, span);
        }
        let mut pending = vec![(*value, 0usize)];
        let mut nodes = 0usize;
        while let Some((value, depth)) = pending.pop() {
            nodes = nodes.saturating_add(1);
            if nodes > MAX_ITEMS + 1 {
                return Err(Diagnostic::limit(Limit::Items, span));
            }
            match value {
                Value::Tuple(handle) => {
                    if depth >= MAX_KEY_DEPTH {
                        return Err(Diagnostic::limit(Limit::KeyDepth, span));
                    }
                    let items = self.list(handle, span)?;
                    if pending.len() + items.len() > MAX_ITEMS {
                        return Err(Diagnostic::limit(Limit::Items, span));
                    }
                    pending.extend(items.iter().rev().map(|item| (*item, depth + 1)));
                }
                value => {
                    self.scalar_key(&value, span)?;
                }
            }
        }
        if nodes.saturating_mul(size_of::<Key>()) > self.limits.max_heap_bytes {
            return Err(Diagnostic::limit(
                Limit::HeapBytes(self.limits.max_heap_bytes),
                span,
            ));
        }
        enum Work {
            Visit(Value),
            Finish(usize),
        }
        let mut pending = vec![Work::Visit(*value)];
        let mut built = Vec::new();
        while let Some(work) = pending.pop() {
            match work {
                Work::Visit(Value::Tuple(handle)) => {
                    let items = self.list(handle, span)?;
                    pending.push(Work::Finish(items.len()));
                    pending.extend(items.iter().rev().map(|v| Work::Visit(*v)));
                }
                Work::Visit(value) => built.push(self.scalar_key(&value, span)?),
                Work::Finish(length) => {
                    let items = built.split_off(built.len() - length);
                    built.push(Key::Tuple(items.into()));
                }
            }
        }
        built.pop().ok_or_else(|| internal(span))
    }

    pub fn store_key(
        &mut self,
        handle: Handle,
        key: Key,
        original: Value,
        value: Value,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<()> {
        let dictionary = self.dictionary(handle, span)?;
        if let Some(position) = dictionary.position(&key) {
            self.dictionary_mut(handle, span)?.entry_mut(position).value = value;
            return Ok(());
        }
        if dictionary.len() >= MAX_ITEMS {
            return Err(Diagnostic::limit(Limit::Items, span));
        }
        let version = dictionary
            .version
            .checked_add(1)
            .ok_or_else(|| Diagnostic::limit(Limit::Counter, span))?;
        let entry = Entry {
            key,
            original,
            value,
        };
        let held = [entry.value.handle(), entry.original.handle()];
        let roots = roots.chain(held.into_iter().flatten());
        if dictionary.needs_growth() {
            let old_bytes = dictionary.bytes();
            let mut prepared = dictionary.grown(span)?;
            prepared.push(entry);
            prepared.version = version;
            let (bytes, object_bytes) =
                self.resized_bytes(handle, prepared.bytes(), old_bytes, roots, span)?;
            *self.dictionary_mut(handle, span)? = prepared;
            self.commit_bytes(handle, bytes, object_bytes);
        } else {
            let added = entry.bytes();
            let (bytes, object_bytes) = self.resized_bytes(handle, added, 0, roots, span)?;
            let dictionary = self.dictionary_mut(handle, span)?;
            dictionary.push(entry);
            dictionary.version = version;
            self.commit_bytes(handle, bytes, object_bytes);
        }
        Ok(())
    }

    pub fn try_store_key(
        &mut self,
        handle: Handle,
        key: Key,
        original: Value,
        value: Value,
    ) -> bool {
        let Ok(Object::Dict(dictionary) | Object::Set(dictionary)) =
            self.object_mut(handle, Span::default())
        else {
            return false;
        };
        if let Some(position) = dictionary.position(&key) {
            dictionary.entry_mut(position).value = value;
            return true;
        }
        let entry = Entry {
            key,
            original,
            value,
        };
        let Some(version) = dictionary.version.checked_add(1) else {
            return false;
        };
        if entry.bytes() != 0 || dictionary.needs_growth() || dictionary.len() >= MAX_ITEMS {
            return false;
        }
        dictionary.push(entry);
        dictionary.version = version;
        true
    }

    pub fn remove_key(&mut self, handle: Handle, key: &Key, span: Span) -> Result<Value> {
        let dictionary = self.dictionary(handle, span)?;
        let position = dictionary.position(key).ok_or_else(|| missing_key(span))?;
        let removed = dictionary.entry(position).bytes();
        let version = dictionary
            .version
            .checked_add(1)
            .ok_or_else(|| Diagnostic::limit(Limit::Counter, span))?;
        let dictionary = self.dictionary_mut(handle, span)?;
        let value = dictionary.remove(position).value;
        dictionary.version = version;
        self.shrink_bytes(handle, removed, span)?;
        Ok(value)
    }

    pub fn new_error(
        &mut self,
        report: Box<ExecutionError>,
        cause: Option<Handle>,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Handle> {
        if !report.causes.is_empty() || report.omitted_causes != 0 {
            return Err(internal(span));
        }
        let depth = if let Some(cause) = cause {
            self.error(cause, span)?
                .2
                .checked_add(1)
                .ok_or_else(|| Diagnostic::limit(Limit::Counter, span))?
        } else {
            1
        };
        let bytes = size_of::<Object>()
            + size_of::<ExecutionError>()
            + report.diagnostic.message.len()
            + report.frames.len() * size_of::<CallFrame>()
            + report
                .frames
                .iter()
                .map(|frame| frame.name.capacity())
                .sum::<usize>();
        self.alloc(
            Object::Error {
                report,
                cause,
                depth,
            },
            bytes,
            roots,
            span,
        )
    }

    pub fn error(
        &self,
        handle: Handle,
        span: Span,
    ) -> Result<(&ExecutionError, Option<Handle>, usize)> {
        match self.object(handle, span)? {
            Object::Error {
                report,
                cause,
                depth,
            } => Ok((report, *cause, *depth)),
            _ => Err(internal(span)),
        }
    }
}

fn globals_bytes(count: usize) -> usize {
    size_of::<Object>() + count * (size_of::<Rc<str>>() + size_of::<Option<Value>>())
}

pub(crate) struct FunctionParts<'a> {
    pub info: &'a Rc<FunctionInfo>,
    pub defaults: &'a [Value],
    pub captures: &'a [Handle],
    pub globals: Handle,
}

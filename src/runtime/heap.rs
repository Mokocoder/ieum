use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};

use num_bigint::BigInt;

use super::code::{FunctionInfo, RecordInfo};
use super::dictionary::Dictionary;
use super::python::Foreign;
use super::value::Value;
use crate::diagnostic::{Diagnostic, ExecutionError, Result, Span, internal, memory};
use crate::limits::{GC_MIN_BYTES, GC_MIN_OBJECTS, Limit, Limits};

static NEXT_HEAP_ID: AtomicU32 = AtomicU32::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Handle {
    slot: u32,
    generation: u32,
}

pub(super) enum Object {
    Text(Rc<str>),
    Big(Rc<BigInt>),
    Cell(Option<Value>),
    Globals {
        names: Vec<Rc<str>>,
        values: Vec<Option<Value>>,
    },
    Function {
        info: Rc<FunctionInfo>,
        defaults: Vec<Value>,
        captures: Vec<Handle>,
        globals: Handle,
    },
    Error {
        report: Box<ExecutionError>,
        cause: Option<Handle>,
        depth: usize,
    },
    List(Vec<Value>),
    Tuple(Vec<Value>),
    Set(Box<Dictionary>),
    Dict(Box<Dictionary>),
    RecordType {
        info: Rc<RecordInfo>,
        defaults: Vec<Value>,
    },
    Record {
        record_type: Handle,
        values: Vec<Value>,
    },
    #[cfg_attr(not(feature = "python"), allow(dead_code))]
    Python(Foreign),
}

impl Object {
    fn width(&self) -> usize {
        match self {
            Self::Text(_) | Self::Big(_) | Self::Cell(_) | Self::Error { .. } | Self::Python(_) => {
                1
            }
            Self::Globals { values, .. } => values.len(),
            Self::Function {
                defaults, captures, ..
            } => defaults.len() + captures.len() + 1,
            Self::RecordType { defaults, .. } => defaults.len(),
            Self::List(values) | Self::Tuple(values) | Self::Record { values, .. } => values.len(),
            Self::Dict(value) | Self::Set(value) => value.len() * 2,
        }
    }

    fn edges(&self) -> impl Iterator<Item = Handle> + Clone + '_ {
        let none: &[Handle] = &[];
        let (owner, handles, values, optional, entries) = match self {
            Self::Text(_) | Self::Big(_) | Self::Python(_) => (None, none, None, None, None),
            Self::Cell(value) => (None, none, None, Some(std::slice::from_ref(value)), None),
            Self::Globals { values, .. } => (None, none, None, Some(&values[..]), None),
            Self::Function {
                defaults,
                captures,
                globals,
                ..
            } => (
                Some(*globals),
                &captures[..],
                Some(&defaults[..]),
                None,
                None,
            ),
            Self::List(values) | Self::Tuple(values) => (None, none, Some(&values[..]), None, None),
            Self::Dict(value) | Self::Set(value) => (None, none, None, None, Some(&**value)),
            Self::Error { cause, .. } => (*cause, none, None, None, None),
            Self::RecordType { defaults, .. } => (None, none, Some(&defaults[..]), None, None),
            Self::Record {
                record_type,
                values,
            } => (Some(*record_type), none, Some(&values[..]), None, None),
        };
        owner
            .into_iter()
            .chain(handles.iter().copied())
            .chain(values.into_iter().flatten().filter_map(Value::handle))
            .chain(
                optional
                    .into_iter()
                    .flatten()
                    .filter_map(|value| value.as_ref().and_then(Value::handle)),
            )
            .chain(
                entries
                    .into_iter()
                    .flat_map(Dictionary::iter)
                    .flat_map(|entry| [entry.original.handle(), entry.value.handle()])
                    .flatten(),
            )
    }
}

struct Slot {
    generation: u32,
    frozen: bool,
    object: Option<Object>,
    bytes: usize,
    next_free: Option<usize>,
}

const _: () = assert!(std::mem::size_of::<Object>() <= 64);

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct HeapStats {
    pub slots: usize,
    pub live: usize,
    pub bytes: usize,
}

/// 할당과 저장은 객체 빌림이 모두 끝난 뒤에 회수를 일으킬 수 있습니다. 넘겨받은
/// 루트 반복자가 실행기의 살아 있는 상태를 나타내고, 입력과 만드는 중인 객체는
/// 여기서 루트로 더합니다. 읽기는 회수를 일으키지 않습니다.
pub(crate) struct Heap {
    id: u32,
    slots: Vec<Slot>,
    free: Option<usize>,
    live: usize,
    bytes: usize,
    pub(super) limits: Limits,
    next_gc_objects: usize,
    next_gc_bytes: usize,
    #[cfg(test)]
    fail_mark_after: Option<usize>,
    #[cfg(test)]
    pub stress_gc: bool,
}

impl Heap {
    pub fn new(limits: Limits) -> Result<Self> {
        let id = NEXT_HEAP_ID
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_mul(0x9E37_79B9);
        Ok(Self {
            id,
            slots: Vec::new(),
            free: None,
            live: 0,
            bytes: 0,
            limits,
            next_gc_objects: GC_MIN_OBJECTS,
            next_gc_bytes: GC_MIN_BYTES,
            #[cfg(test)]
            fail_mark_after: None,
            #[cfg(test)]
            stress_gc: false,
        })
    }

    #[cfg(test)]
    pub fn stats(&self) -> HeapStats {
        HeapStats {
            slots: self.slots.len(),
            live: self.live,
            bytes: self.bytes,
        }
    }

    pub fn should_collect(&self) -> bool {
        self.live >= self.next_gc_objects || self.bytes >= self.next_gc_bytes
    }

    fn mark(
        &self,
        handle: Handle,
        marks: &mut [bool],
        pending: &mut Vec<usize>,
        span: Span,
    ) -> Result<()> {
        self.object(handle, span)?;
        let slot = handle.slot as usize;
        if !marks[slot] {
            marks[slot] = true;
            pending.push(slot);
        }
        Ok(())
    }

    pub fn collect(&mut self, roots: impl Iterator<Item = Handle>, span: Span) -> Result<()> {
        let mut marks = Vec::new();
        marks
            .try_reserve_exact(self.slots.len())
            .map_err(|_| memory(span))?;
        marks.resize(self.slots.len(), false);
        let mut pending = Vec::new();
        pending
            .try_reserve_exact(self.live)
            .map_err(|_| memory(span))?;
        for handle in roots {
            self.mark(handle, &mut marks, &mut pending, span)?;
        }
        #[cfg(test)]
        let mut visited = 0;
        let mut traced = 0usize;
        while let Some(index) = pending.pop() {
            #[cfg(test)]
            {
                if self.fail_mark_after == Some(visited) {
                    return Err(memory(span));
                }
                visited += 1;
            }
            let object = self.slots[index]
                .object
                .as_ref()
                .ok_or_else(|| internal(span))?;
            traced = traced.saturating_add(1 + object.width());
            for handle in object.edges() {
                self.mark(handle, &mut marks, &mut pending, span)?;
            }
        }

        let mut reclaimed = 0usize;
        let mut bytes = 0usize;
        for (slot, &marked) in self.slots.iter().zip(&marks) {
            if slot.object.is_some() && !marked {
                reclaimed += 1;
                bytes = bytes
                    .checked_add(slot.bytes)
                    .ok_or_else(|| internal(span))?;
            }
        }
        let live = self
            .live
            .checked_sub(reclaimed)
            .ok_or_else(|| internal(span))?;
        let remaining_bytes = self
            .bytes
            .checked_sub(bytes)
            .ok_or_else(|| internal(span))?;
        for (index, (slot, marked)) in self.slots.iter_mut().zip(marks).enumerate() {
            if slot.object.is_some() && !marked {
                slot.object = None;
                slot.bytes = 0;
                slot.generation = slot.generation.wrapping_add(1);
                if slot.generation != self.id {
                    slot.next_free = self.free;
                    self.free = Some(index);
                }
            }
        }
        self.live = live;
        self.bytes = remaining_bytes;
        self.next_gc_objects = live.saturating_add(traced.max(GC_MIN_OBJECTS));
        self.next_gc_bytes = remaining_bytes.saturating_mul(2).max(GC_MIN_BYTES);
        Ok(())
    }

    fn check_bytes(&self, added: usize, removed: usize, span: Span) -> Result<usize> {
        self.bytes
            .checked_sub(removed)
            .and_then(|n| n.checked_add(added))
            .filter(|&n| n <= self.limits.max_heap_bytes)
            .ok_or_else(|| Diagnostic::limit(Limit::HeapBytes(self.limits.max_heap_bytes), span))
    }

    pub(super) fn reserve(
        &mut self,
        objects: usize,
        added: usize,
        removed: usize,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<usize> {
        let fits_objects = |live: usize| {
            live.checked_add(objects)
                .is_some_and(|n| n <= self.limits.max_heap_objects)
        };
        let pressure = !fits_objects(self.live) || self.check_bytes(added, removed, span).is_err();
        let collect = self.should_collect() || pressure;
        #[cfg(test)]
        let collect = collect || self.stress_gc;
        if collect {
            self.collect(roots, span)?;
        }
        if self
            .live
            .checked_add(objects)
            .is_none_or(|n| n > self.limits.max_heap_objects)
        {
            return Err(Diagnostic::limit(
                Limit::HeapObjects(self.limits.max_heap_objects),
                span,
            ));
        }
        self.check_bytes(added, removed, span)
    }

    pub(super) fn resized_bytes(
        &mut self,
        handle: Handle,
        added: usize,
        removed: usize,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<(usize, usize)> {
        self.object(handle, span)?;
        // 힙을 키우지 않는 변경은 회수하지 않습니다. 회수는 다음 할당에서 합니다.
        let total = if added <= removed {
            self.bytes
                .checked_sub(removed - added)
                .ok_or_else(|| internal(span))?
        } else {
            self.reserve(0, added, removed, roots.chain([handle]), span)?
        };
        let object = self.slots[handle.slot as usize]
            .bytes
            .checked_sub(removed)
            .and_then(|n| n.checked_add(added))
            .ok_or_else(|| internal(span))?;
        Ok((total, object))
    }

    pub(super) fn shrink_bytes(
        &mut self,
        handle: Handle,
        removed: usize,
        span: Span,
    ) -> Result<()> {
        let slot = &mut self.slots[handle.slot as usize];
        slot.bytes = slot
            .bytes
            .checked_sub(removed)
            .ok_or_else(|| internal(span))?;
        self.bytes = self
            .bytes
            .checked_sub(removed)
            .ok_or_else(|| internal(span))?;
        Ok(())
    }

    pub(super) fn commit_bytes(&mut self, handle: Handle, total: usize, object: usize) {
        self.slots[handle.slot as usize].bytes = object;
        self.bytes = total;
    }

    pub(super) fn alloc(
        &mut self,
        object: Object,
        bytes: usize,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Handle> {
        let total = self.reserve(1, bytes, 0, roots.chain(object.edges()), span)?;
        let index = if let Some(index) = self.free {
            let slot = self.slots.get_mut(index).ok_or_else(|| internal(span))?;
            if slot.object.is_some() {
                return Err(internal(span));
            }
            self.free = slot.next_free.take();
            slot.object = Some(object);
            slot.bytes = bytes;
            slot.frozen = false;
            index
        } else {
            if self.slots.len() >= u32::MAX as usize {
                return Err(Diagnostic::limit(
                    Limit::HeapObjects(self.limits.max_heap_objects),
                    span,
                ));
            }
            self.slots.try_reserve(1).map_err(|_| memory(span))?;
            let index = self.slots.len();
            self.slots.push(Slot {
                generation: self.id,
                frozen: false,
                object: Some(object),
                bytes,
                next_free: None,
            });
            index
        };
        let handle = Handle {
            slot: index as u32,
            generation: self.slots[index].generation,
        };
        self.live += 1;
        self.bytes = total;
        Ok(handle)
    }

    pub(super) fn object(&self, handle: Handle, span: Span) -> Result<&Object> {
        self.slots
            .get(handle.slot as usize)
            .filter(|s| s.generation == handle.generation)
            .and_then(|s| s.object.as_ref())
            .ok_or_else(|| internal(span))
    }

    pub(super) fn object_mut(&mut self, handle: Handle, span: Span) -> Result<&mut Object> {
        let slot = self
            .slots
            .get_mut(handle.slot as usize)
            .filter(|s| s.generation == handle.generation)
            .ok_or_else(|| internal(span))?;
        if slot.frozen {
            return Err(Diagnostic::new(
                "E-PAR-001",
                span,
                "병렬로 실행하는 계산에서는 바깥의 값을 바꿀 수 없습니다.",
            ));
        }
        slot.object.as_mut().ok_or_else(|| internal(span))
    }

    pub(super) fn freeze(&mut self, handle: Handle, span: Span) -> Result<()> {
        self.object(handle, span)?;
        self.slots[handle.slot as usize].frozen = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPAN: Span = Span { start: 7, end: 19 };

    fn list(heap: &mut Heap, values: Vec<Value>, roots: &[Handle]) -> Handle {
        let value = heap.new_list(values, roots.iter().copied(), SPAN).unwrap();
        value.handle().unwrap()
    }

    fn code<T>(result: Result<T>) -> &'static str {
        result.err().unwrap().code
    }

    fn assert_empty_after_rootless_collection(heap: &mut Heap) {
        heap.collect(std::iter::empty(), SPAN).unwrap();
        assert_eq!((heap.stats().live, heap.stats().bytes), (0, 0));
    }

    #[test]
    fn stale_foreign_and_mistyped_handles_are_rejected() {
        let limits = Limits {
            max_heap_objects: 2,
            ..Limits::default()
        };
        let (mut heap, other) = (Heap::new(limits).unwrap(), Heap::new(limits).unwrap());
        let keep = list(&mut heap, vec![], &[]);
        let old = list(&mut heap, vec![], &[keep]);
        let new = list(&mut heap, vec![Value::List(keep)], &[]);
        assert_eq!(
            (new.slot, new.generation),
            (old.slot, old.generation.wrapping_add(1))
        );
        assert_eq!(heap.stats().slots, 2);
        let out_of_range = Handle {
            slot: u32::MAX,
            ..keep
        };
        for (heap, handle) in [(&heap, old), (&heap, out_of_range), (&other, keep)] {
            assert_eq!(code(heap.list(handle, SPAN)), "E-INTERNAL-001");
        }
        assert_eq!(code(heap.function(keep, SPAN)), "E-INTERNAL-001");
        let live = heap.stats().live;
        assert_eq!(
            code(heap.collect([old].into_iter(), SPAN)),
            "E-INTERNAL-001"
        );
        assert_eq!(heap.stats().live, live);
    }

    #[test]
    fn failed_growth_and_collections_leave_prior_state_intact() {
        let only_one =
            |heap: &Heap, list| matches!(heap.list(list, SPAN).unwrap(), [Value::Int(1)]);
        let mut heap = Heap::new(Limits::default()).unwrap();
        let keep = list(&mut heap, vec![Value::Int(1)], &[]);
        heap.limits.max_heap_bytes = heap.bytes;
        let error = heap.append(keep, Value::None, [keep].into_iter(), SPAN);
        assert_eq!(
            error.map_err(|e| (e.code, e.span)),
            Err(("E-LIMIT-001", SPAN))
        );
        assert!(only_one(&heap, keep));

        heap.limits.max_heap_bytes = Limits::default().max_heap_bytes;
        let dead = list(&mut heap, vec![], &[keep]);
        heap.stress_gc = true;
        heap.fail_mark_after = Some(0);
        let error = heap.append(keep, Value::None, [keep].into_iter(), SPAN);
        assert_eq!(
            error.map_err(|e| (e.code, e.span)),
            Err(("E-MEMORY-001", SPAN))
        );
        assert!(only_one(&heap, keep));
        assert!(
            heap.list(dead, SPAN).is_ok(),
            "표시에 실패하면 회수하지 않아야 합니다"
        );
    }

    #[test]
    fn deep_graphs_are_traced_and_dropped_with_a_bounded_rust_stack() {
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let mut heap = Heap::new(Limits::default()).unwrap();
                let mut value = Value::None;
                for _ in 0..20_000 {
                    value = heap
                        .new_list(vec![value], std::iter::empty(), SPAN)
                        .unwrap();
                }
                heap.collect(value.handle().into_iter(), SPAN).unwrap();
                assert_eq!(heap.stats().live, 20_000);
                assert_empty_after_rootless_collection(&mut heap);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

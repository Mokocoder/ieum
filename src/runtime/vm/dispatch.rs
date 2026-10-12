use std::io::Write;
use std::rc::Rc;

use num_traits::{Signed, ToPrimitive};

use super::fast::fast_binary;
use super::{Control, IteratorState, Raised, StoredUnwind, Unwind, Vm, unset};
use crate::diagnostic::{Diagnostic, Result, Span, internal, memory, type_error};
use crate::korean::particle::{SUBJECT, WITH};
use crate::limits::{Limit, MAX_ITEMS, MAX_TEXT_BYTES};
use crate::runtime::attributes::is_reserved_dictionary_name;
use crate::runtime::code::{Code, Op, TemplatePart};
use crate::runtime::dictionary::Key;
use crate::runtime::heap::Handle;
use crate::runtime::objects::CollectionKind;
use crate::runtime::render::write_line;
use crate::runtime::value::{Int, Value};
use crate::stdlib;

fn undefined(span: Span) -> Diagnostic {
    Diagnostic::new("E-INIT-001", span, "정의하기 전에 쓴 이름입니다.")
}

fn count_error(span: Span) -> Diagnostic {
    Diagnostic::new("E-TYPE-001", span, "'부터'와 '까지'에는 정수를 씁니다.")
}

fn adjust_error(span: Span) -> Diagnostic {
    Diagnostic::new(
        "E-TYPE-001",
        span,
        "늘리거나 줄이는 값과 그 양은 수여야 합니다.",
    )
}

impl<W: Write> Vm<'_, W> {
    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    pub(super) fn op(
        &mut self,
        op: Op,
        code: &Rc<Code>,
        at: usize,
        ip: &mut usize,
        locals: usize,
        cells: usize,
        globals: Handle,
        index: usize,
    ) -> Result<Control> {
        macro_rules! span {
            () => {
                code.spans[at]
            };
        }
        match op {
            Op::Nop => {}
            Op::Const(i) => {
                let values = code
                    .constant_values
                    .get()
                    .ok_or_else(|| internal(span!()))?;
                self.push(values[i as usize]);
            }
            Op::LoadFast(slot) => match self.state.thread.data[locals + slot as usize] {
                Some(value) => self.push(value),
                None => return Err(unset(&code.local_names[slot as usize], span!())),
            },
            Op::StoreFast(slot) => {
                let value = self.pop()?;
                self.state.thread.data[locals + slot as usize] = Some(value);
            }
            Op::ClearFast(slot) => self.state.thread.data[locals + slot as usize] = None,
            Op::CheckFast(slot) => {
                if self.state.thread.data[locals + slot as usize].is_none() {
                    return Err(undefined(span!()));
                }
            }
            Op::LoadCell(i) => {
                let cell = self.cell(cells, i, span!())?;
                match self.heap.cell(cell, span!())?.copied() {
                    Some(value) => self.push(value),
                    None => return Err(unset(&code.cell_names[i as usize], span!())),
                }
            }
            Op::StoreCell(i) => {
                let cell = self.cell(cells, i, span!())?;
                let value = self.pop()?;
                self.heap.store_cell(cell, value, span!())?;
            }
            Op::MakeCell(i) => {
                let cell = self.heap.new_cell(None, self.state.roots(), span!())?;
                self.state.thread.cells[cells + i as usize] = Some(cell);
            }
            Op::CheckCell(i) => {
                let cell = self.cell(cells, i, span!())?;
                if self.heap.cell(cell, span!())?.is_none() {
                    return Err(undefined(span!()));
                }
            }
            Op::CellFromFast { cell, slot } => {
                let value = self.state.thread.data[locals + slot as usize].take();
                let handle = self.heap.new_cell(value, self.state.roots(), span!())?;
                self.state.thread.cells[cells + cell as usize] = Some(handle);
            }
            Op::LoadGlobal(i) => match self.heap.global(globals, i, span!())? {
                Ok(value) => self.push(value),
                Err(name) => return Err(unset(name, span!())),
            },
            Op::StoreGlobal(i) => {
                let value = self.pop()?;
                self.heap.store_global(globals, i, value, span!())?;
            }
            Op::CheckGlobal(i) => {
                if self.heap.global(globals, i, span!())?.is_err() {
                    return Err(undefined(span!()));
                }
            }
            Op::LoadBuiltin(i) => {
                let value = self.state.builtins[i as usize];
                self.push(value);
            }
            Op::Name(..) => return Err(internal(span!())),
            Op::Pop => {
                self.pop()?;
            }
            Op::Dup => {
                let value = *self.top()?;
                self.push(value);
            }
            Op::Dup2 => {
                let (owner, index) = (*self.peek(1)?, *self.peek(0)?);
                self.push(owner);
                self.push(index);
            }
            Op::Unary(op) => {
                let operand = self.pop()?;
                let roots = self.state.roots().chain(operand.handle());
                let value = self.heap.unary(&operand, op, roots, span!())?;
                self.push(value);
            }
            Op::Binary(op) => {
                let right = self.pop()?;
                let left = self.pop()?;
                let value = match fast_binary(op, &left, &right) {
                    Some(value) => value,
                    None => self
                        .heap
                        .binary(&left, op, &right, self.state.roots(), span!())?,
                };
                self.push(value);
            }
            Op::CompareJump(op, target) => {
                let right = self.pop()?;
                let left = self.pop()?;
                if !self.heap.compare(&left, op, &right, span!())? {
                    *ip = target as usize;
                }
            }
            Op::Compare(op) => {
                let right = self.pop()?;
                let left = self.pop()?;
                let value = match (&left, &right) {
                    (Value::Int(a), Value::Int(b)) => Value::Bool(op.holds(a.cmp(b))),
                    (Value::Python(_), _) | (_, Value::Python(_)) => {
                        let roots = self
                            .state
                            .roots()
                            .chain(left.handle())
                            .chain(right.handle());
                        self.heap
                            .python_compare(&left, op, &right, roots, span!())?
                    }
                    _ => Value::Bool(self.heap.compare(&left, op, &right, span!())?),
                };
                self.push(value);
            }
            Op::CompareOrFalse(op, target) => {
                let right = self.pop()?;
                let left = self.pop()?;
                if self.heap.compare(&left, op, &right, span!())? {
                    self.push(right);
                } else {
                    self.push(Value::Bool(false));
                    *ip = target as usize;
                }
            }
            Op::ToBool => {
                let value = self.pop()?.boolean(span!())?;
                self.push(Value::Bool(value));
            }
            Op::JumpIfFalseOrPop(target) => {
                if self.top()?.boolean(span!())? {
                    self.pop()?;
                } else {
                    *ip = target as usize;
                }
            }
            Op::JumpIfTrueOrPop(target) => {
                if self.top()?.boolean(span!())? {
                    *ip = target as usize;
                } else {
                    self.pop()?;
                }
            }
            Op::Jump(target) => *ip = target as usize,
            Op::JumpIfFalse(target) => {
                if !self.pop()?.boolean(span!())? {
                    *ip = target as usize;
                }
            }
            Op::Adjust(op) => {
                let amount = self.pop()?;
                let current = self.pop()?;
                let value = match fast_binary(op, &current, &amount) {
                    Some(value) => value,
                    None if current.numeric() && amount.numeric() => {
                        let roots = self
                            .state
                            .roots()
                            .chain(current.handle())
                            .chain(amount.handle());
                        self.heap.binary(&current, op, &amount, roots, span!())?
                    }
                    None => return Err(adjust_error(span!())),
                };
                self.push(value);
            }
            Op::BuildList(count) | Op::BuildTuple(count) | Op::BuildSet(count) => {
                let kind = match op {
                    Op::BuildTuple(_) => CollectionKind::Tuple,
                    Op::BuildSet(_) => CollectionKind::Set,
                    _ => CollectionKind::List,
                };
                let items = self.take(count as usize)?;
                let value = self
                    .heap
                    .new_collection(kind, items, self.state.roots(), span!())?;
                self.push(value);
            }
            Op::BuildDict(count) => {
                let mut values = self.take(2 * count as usize)?.into_iter();
                let mut entries = Vec::with_capacity(count as usize);
                while let (Some(key), Some(value)) = (values.next(), values.next()) {
                    entries.push((key, value));
                }
                let value = self
                    .heap
                    .new_dictionary(entries, self.state.roots(), span!())?;
                self.push(value);
            }
            Op::CheckKey => {
                self.heap.key(self.top()?, span!())?;
            }
            Op::BuildRange(down) => self.build_range(down, span!())?,
            Op::BuildTemplate(i) => self.build_template(&code.templates[i as usize], span!())?,
            Op::BuildRecord(i) => self.build_record(code, i as usize, span!())?,
            Op::Index => {
                let index = self.pop()?;
                let target = self.pop()?;
                let roots = self.state.roots().chain(target.handle());
                let value = self.heap.index(&target, &index, roots, span!())?;
                self.push(value);
            }
            Op::Slice => {
                let bounds = self.take(3)?;
                let owner = self.pop()?;
                let value = self.heap.slice(
                    &owner,
                    [&bounds[0], &bounds[1], &bounds[2]],
                    self.state.roots(),
                    span!(),
                )?;
                self.push(value);
            }
            Op::Attribute(i) => {
                let target = self.pop()?;
                let roots = self.state.roots().chain(target.handle());
                let value =
                    self.heap
                        .read_attribute(&target, &code.names[i as usize], roots, span!())?;
                self.push(value);
            }
            Op::AttributeEither(i) => {
                let (name, other) = &code.either_names[i as usize];
                self.attribute_either(name, other, span!())?;
            }
            Op::PrepareIndex => self.prepare_index(span!())?,
            Op::StoreIndex => {
                let value = self.pop()?;
                let index = self.pop()?;
                let owner = self.pop()?;
                let roots = self.state.roots().chain(owner.handle());
                match owner {
                    Value::Dict(dictionary) => {
                        let key = self.heap.key(&index, span!())?;
                        self.heap
                            .store_key(dictionary, key, index, value, roots, span!())?;
                    }
                    Value::List(list) => {
                        let Value::Int(position) = index else {
                            return Err(internal(span!()));
                        };
                        self.heap
                            .store_index(list, position as usize, value, span!())?;
                    }
                    Value::Python(handle) => {
                        self.heap
                            .python_store_index(handle, &index, &value, span!())?;
                    }
                    _ => return Err(internal(span!())),
                }
            }
            Op::PrepareAttribute(i) => self.prepare_attribute(&code.names[i as usize], span!())?,
            Op::StoreAttribute(i) => {
                let name = &code.names[i as usize];
                let value = self.pop()?;
                let owner = self.pop()?;
                match owner {
                    Value::Record(record) => {
                        let field = self.heap.field_index(record, name, span!())?;
                        self.heap.store_field(record, field, value, span!())?;
                    }
                    Value::Dict(dictionary) => {
                        let held = [Some(dictionary), value.handle()];
                        let roots = self.state.roots().chain(held.into_iter().flatten());
                        let original = self.heap.new_text(name.clone(), roots.clone(), span!())?;
                        let roots = roots.chain(original.handle());
                        let key = Key::Text(name.clone());
                        self.heap
                            .store_key(dictionary, key, original, value, roots, span!())?;
                    }
                    Value::Python(handle) => {
                        self.heap
                            .python_store_attribute(handle, name, &value, span!())?;
                    }
                    _ => return Err(internal(span!())),
                }
            }
            Op::GetIter => {
                let collection = self.pop()?;
                let iterator = self.start_iterator(collection, span!())?;
                self.state.thread.iters.push(iterator);
            }
            Op::ForIter(target) => match self.advance(span!())? {
                Some(value) => self.push(value),
                None => {
                    self.state.thread.iters.pop();
                    *ip = target as usize;
                }
            },
            Op::PopIter => {
                self.state.thread.iters.pop();
            }
            Op::GetCount(down, last) => {
                let end = self.pop()?;
                let iterator = match (self.pop()?, end) {
                    (first, end) if first.numeric() && end.numeric() => {
                        match (self.heap.int(&first), self.heap.int(&end)) {
                            (Some(first), Some(end)) => IteratorState::Count(first, end, down),
                            (Some(_), None) => {
                                return Err(count_error(code.count_end_spans[last as usize]));
                            }
                            _ => return Err(count_error(span!())),
                        }
                    }
                    (first, _) if self.heap.int(&first).is_some() => {
                        return Err(count_error(code.count_end_spans[last as usize]));
                    }
                    _ => return Err(count_error(span!())),
                };
                self.state.thread.iters.push(iterator);
            }
            Op::DictItems => {
                if let Value::Dict(_) = self.top()? {
                    let collection = self.pop()?;
                    let roots = self.state.roots().chain(collection.handle());
                    let items = self
                        .heap
                        .read_attribute(&collection, "항목들", roots, span!())?;
                    self.push(items);
                }
            }
            Op::Unpack2 => {
                let item = self.pop()?;
                let (first, second) = self.pair(&item, span!())?;
                self.push(first);
                self.push(second);
            }
            Op::ListAppend => {
                let value = self.pop()?;
                let Value::List(list) = *self.top()? else {
                    return Err(internal(span!()));
                };
                let roots = self.state.roots().chain(value.handle());
                self.heap.append(list, value, roots, span!())?;
            }
            Op::MakeFunction(i) => {
                let closure = &code.closures[i as usize];
                let captures = closure
                    .captures
                    .iter()
                    .map(|&cell| self.cell(cells, cell, span!()))
                    .collect::<Result<Vec<_>>>()?;
                let defaults = self.take(closure.info.signature.len())?;
                let value = self.heap.new_function(
                    closure.info.clone(),
                    defaults,
                    captures,
                    globals,
                    self.state.roots(),
                    span!(),
                )?;
                self.push(value);
            }
            Op::MakeRecordType(i) => {
                let info = code.record_types[i as usize].clone();
                let defaults = self.take(info.fields.len())?;
                let value =
                    self.heap
                        .new_record_type(info, defaults, self.state.roots(), span!())?;
                self.push(value);
            }
            Op::Call(i) => {
                self.save(index, *ip);
                return self.call(&code.calls[i as usize]);
            }
            Op::Return => {
                let value = self.pop()?;
                return self.leave_frame(value);
            }
            Op::ReturnNone => return self.leave_frame(Value::None),
            Op::Throw => {
                let raised = match self.pop()? {
                    message @ Value::Text(_) => {
                        let message = self.heap.text_of(&message, span!())?;
                        self.raised(Diagnostic::new("E-USER-001", span!(), &*message))
                    }
                    Value::Error(handle) => Raised::Value(handle),
                    _ => return Err(type_error(span!())),
                };
                return Ok(Control::Raise(Unwind::Error(raised)));
            }
            Op::Rethrow => {
                let error = self.active_error().ok_or_else(|| {
                    Diagnostic::new(
                        "E-EXCEPT-001",
                        span!(),
                        "다시 발생시킬 오류가 없습니다. 이 문장은 오류 처리 블록 안에서 씁니다.",
                    )
                })?;
                return Ok(Control::Raise(Unwind::Error(Raised::Value(error))));
            }
            Op::Exit => {
                let status = match self.pop()? {
                    Value::Int(status) => status.to_u8(),
                    _ => None,
                };
                let status = status.ok_or_else(|| {
                    Diagnostic::new(
                        "E-VALUE-001",
                        span!(),
                        "종료 코드는 0부터 255까지의 정수입니다.",
                    )
                })?;
                return Ok(Control::Raise(Unwind::Exit(status)));
            }
            Op::ExitZero => return Ok(Control::Raise(Unwind::Exit(0))),
            Op::Catch => match self.state.thread.unwinds.last() {
                Some(StoredUnwind::Error(handle)) => {
                    let error = Value::Error(*handle);
                    self.push(error);
                }
                Some(&stored) => {
                    self.state.thread.unwinds.pop();
                    return Ok(Control::Raise(stored.into()));
                }
                None => return Err(internal(span!())),
            },
            Op::PopUnwind => {
                self.state
                    .thread
                    .unwinds
                    .pop()
                    .ok_or_else(|| internal(span!()))?;
            }
            Op::Reraise => {
                let stored = self
                    .state
                    .thread
                    .unwinds
                    .pop()
                    .ok_or_else(|| internal(span!()))?;
                return Ok(Control::Raise(stored.into()));
            }
            Op::Together(count) => {
                self.save(index, *ip);
                let mut threads = Vec::with_capacity(count as usize);
                for depth in (0..count as usize).rev() {
                    threads.push(self.new_flow_thread(self.peek(depth)?, Vec::new(), span!())?);
                }
                self.take(count as usize)?;
                return self.spawn_flows(threads, None);
            }
            Op::TogetherEach | Op::TogetherCollect => {
                self.save(index, *ip);
                let closure = self.pop()?;
                let collection = self.pop()?;
                let roots = self.state.roots().chain(closure.handle());
                let items = self.heap.snapshot(&collection, roots, span!())?;
                let results = if op == Op::TogetherCollect {
                    let roots = self
                        .state
                        .roots()
                        .chain(closure.handle())
                        .chain(items.iter().filter_map(Value::handle));
                    let list =
                        self.heap
                            .new_list(vec![Value::None; items.len()], roots, span!())?;
                    list.handle()
                } else {
                    None
                };
                let mut threads = Vec::with_capacity(items.len());
                for item in items {
                    threads.push(self.new_flow_thread(&closure, vec![item], span!())?);
                }
                return self.spawn_flows(threads, results);
            }
            Op::ParallelEach | Op::ParallelCollect => {
                self.save(index, *ip);
                let function = self.pop()?;
                let collection = self.pop()?;
                let roots = self.state.roots().chain(function.handle());
                let items = self.heap.snapshot(&collection, roots, span!())?;
                let collect = op == Op::ParallelCollect;
                return self.start_parallel(function, items, collect, span!());
            }
            Op::ParallelBlock(count) => {
                self.save(index, *ip);
                let tasks = self.take(count as usize)?;
                return self.start_parallel(Value::None, tasks, true, span!());
            }
            Op::Display => {
                let value = self.pop()?;
                write_line(self.output, &self.heap, &value, span!())?;
            }
        }
        Ok(Control::Next)
    }

    fn build_range(&mut self, down: bool, span: Span) -> Result<()> {
        let last = self.pop()?;
        let first = self.pop()?;
        let (Some(first), Some(last)) = (self.heap.int(&first), self.heap.int(&last)) else {
            return Err(count_error(span));
        };
        let (first_big, last_big) = (first.big(), last.big());
        let count = if down {
            first_big.as_ref() - last_big.as_ref()
        } else {
            last_big.as_ref() - first_big.as_ref()
        };
        let count = if count.is_negative() {
            0
        } else {
            (count + 1u8)
                .to_usize()
                .filter(|&n| n <= MAX_ITEMS)
                .ok_or_else(|| Diagnostic::limit(Limit::Items, span))?
        };
        let mut items = Vec::new();
        items.try_reserve_exact(count).map_err(|_| memory(span))?;
        let mut next = first;
        for _ in 0..count {
            let following = next.step(down);
            let value = match std::mem::replace(&mut next, following) {
                Int::Small(n) => Value::Int(n),
                big => {
                    let roots = self
                        .state
                        .roots()
                        .chain(items.iter().filter_map(Value::handle));
                    self.heap.new_int(big, roots, span)?
                }
            };
            items.push(value);
        }
        let held: Vec<Handle> = items.iter().filter_map(Value::handle).collect();
        let list = self
            .heap
            .new_list(items, self.state.roots().chain(held), span)?;
        self.push(list);
        Ok(())
    }

    fn build_template(&mut self, parts: &[TemplatePart], span: Span) -> Result<()> {
        let count = parts
            .iter()
            .filter(|part| matches!(part, TemplatePart::Value(_)))
            .count();
        let mut values = self.take(count)?.into_iter();
        let mut text = String::new();
        for part in parts {
            match part {
                TemplatePart::Text(piece) => text.push_str(piece),
                TemplatePart::Value(spec) => {
                    let value = values.next().ok_or_else(|| internal(span))?;
                    text.push_str(&match spec {
                        Some(spec) => stdlib::format(&self.heap, &value, spec, span)?,
                        None => self.heap.render(&value, span)?,
                    });
                }
            }
            if text.len() > MAX_TEXT_BYTES {
                return Err(Diagnostic::limit(Limit::Text, span));
            }
        }
        let value = self.heap.new_text(text, self.state.roots(), span)?;
        self.push(value);
        Ok(())
    }

    fn build_record(&mut self, code: &Code, index: usize, span: Span) -> Result<()> {
        let literal = &code.record_literals[index];
        let given = self.take(literal.fields.len())?;
        let record_type = self.pop()?;
        let Value::RecordType(record_type) = record_type else {
            return Err(Diagnostic::new(
                "E-TYPE-001",
                literal.type_span,
                format!("'{}' 값은 자료형이 아닙니다.", literal.type_name),
            ));
        };
        let (record, defaults) = self.heap.record_type(record_type, span)?;
        if let Some((name, field_span)) = literal
            .fields
            .iter()
            .find(|(name, _)| !record.fields.iter().any(|f| *f.name == **name))
        {
            return Err(Diagnostic::new(
                "E-RECORD-002",
                *field_span,
                format!("'{}'에는 '{name}' 필드가 없습니다.", record.name),
            ));
        }
        let mut values = Vec::with_capacity(record.fields.len());
        for (index, field) in record.fields.iter().enumerate() {
            let value = match literal
                .fields
                .iter()
                .position(|(name, _)| **name == *field.name)
            {
                Some(position) => given[position],
                None if field.has_default => defaults[index],
                None => {
                    return Err(Diagnostic::new(
                        "E-RECORD-001",
                        span,
                        format!("'{}' 필드의 값이 필요합니다.", field.name),
                    ));
                }
            };
            values.push(value);
        }
        let roots = self
            .state
            .roots()
            .chain([record_type])
            .chain(given.iter().filter_map(Value::handle));
        let value = self.heap.new_record(record_type, values, roots, span)?;
        self.push(value);
        Ok(())
    }

    fn attribute_either(&mut self, name: &str, other: &str, span: Span) -> Result<()> {
        let target = self.pop()?;
        let roots = self.state.roots().chain(target.handle());
        let first = self.heap.read_attribute(&target, name, roots.clone(), span);
        let second = self.heap.read_attribute(&target, other, roots, span);
        let value = match (first, second) {
            (Ok(_), Ok(_)) => {
                return Err(Diagnostic::new(
                    "E-RESOLVE-002",
                    span,
                    format!(
                        "값에 '{name}'{} '{other}'{} 모두 있어 어느 것인지 알 수 없습니다.",
                        WITH.after(name),
                        SUBJECT.after(other)
                    ),
                ));
            }
            (Ok(value), Err(_)) | (Err(_), Ok(value)) => value,
            (Err(error), Err(_)) => return Err(error),
        };
        self.push(value);
        Ok(())
    }

    fn prepare_index(&mut self, span: Span) -> Result<()> {
        let (owner, index) = (self.peek(1)?, self.peek(0)?);
        match owner {
            Value::Dict(dictionary) => {
                self.heap.key(index, span)?;
                self.heap.dictionary(*dictionary, span)?;
            }
            Value::List(list) => {
                let length = self.heap.list(*list, span)?.len();
                let index = self.heap.index_position(index, length, span)?;
                let top = self.state.thread.data.last_mut();
                *top.ok_or_else(|| internal(span))? = Some(Value::Int(index as i64));
            }
            Value::Text(_) | Value::Tuple(_) => {
                return Err(Diagnostic::new(
                    "E-ASSIGN-001",
                    span,
                    "문자열과 튜플의 원소는 바꿀 수 없습니다.",
                ));
            }
            Value::Python(_) => {}
            _ => return Err(type_error(span)),
        }
        Ok(())
    }

    fn prepare_attribute(&mut self, name: &str, span: Span) -> Result<()> {
        let target = *self.top()?;
        match target {
            Value::Record(record) => {
                self.heap.field_index(record, name, span)?;
                Ok(())
            }
            Value::Dict(_) if !is_reserved_dictionary_name(name) => Ok(()),
            Value::Python(_) => Ok(()),
            target => {
                self.heap
                    .read_attribute(&target, name, self.state.roots(), span)?;
                Err(Diagnostic::new(
                    "E-ASSIGN-001",
                    span,
                    "이 속성은 읽기 전용입니다.",
                ))
            }
        }
    }

    fn pair(&self, item: &Value, span: Span) -> Result<(Value, Value)> {
        if let Value::List(handle) | Value::Tuple(handle) = item {
            if let [first, second] = self.heap.list(*handle, span)? {
                return Ok((*first, *second));
            }
        }
        Err(Diagnostic::new(
            "E-ITER-002",
            span,
            "이름 두 개로 받으려면 원소가 둘씩 묶여 있어야 합니다.",
        ))
    }

    fn start_iterator(&mut self, collection: Value, span: Span) -> Result<IteratorState> {
        Ok(match collection {
            Value::List(handle) | Value::Tuple(handle) => IteratorState::List(handle, 0),
            Value::Dict(handle) | Value::Set(handle) => {
                IteratorState::Dict(handle, 0, self.heap.dictionary(handle, span)?.version)
            }
            Value::Text(handle) => {
                IteratorState::Text(handle, self.heap.text_of(&collection, span)?, 0)
            }
            Value::Python(handle) => {
                let roots = self.state.roots().chain([handle]);
                IteratorState::Python(self.heap.python_iterator(handle, roots, span)?)
            }
            _ => return Err(type_error(span)),
        })
    }

    fn advance(&mut self, span: Span) -> Result<Option<Value>> {
        enum Next {
            Value(Value),
            Int(Int),
            Character(char),
        }
        let iterator = self
            .state
            .thread
            .iters
            .last_mut()
            .ok_or_else(|| internal(span))?;
        let next = match iterator {
            IteratorState::List(handle, index) => {
                let item = self.heap.list(*handle, span)?.get(*index).copied();
                *index += 1;
                item.map(Next::Value)
            }
            IteratorState::Count(next, last, down) => {
                let within = if *down {
                    *next >= *last
                } else {
                    *next <= *last
                };
                within.then(|| {
                    let following = next.step(*down);
                    match std::mem::replace(next, following) {
                        Int::Small(n) => Next::Value(Value::Int(n)),
                        big => Next::Int(big),
                    }
                })
            }
            IteratorState::Text(_, text, index) => {
                let next = text[*index..].chars().next();
                next.map(|c| {
                    *index += c.len_utf8();
                    Next::Character(c)
                })
            }
            IteratorState::Dict(handle, index, version) => {
                let dictionary = self.heap.dictionary(*handle, span)?;
                if dictionary.version != *version {
                    return Err(Diagnostic::new(
                        "E-ITER-001",
                        span,
                        "사전이나 집합을 반복하는 동안 원소를 넣거나 지울 수 없습니다.",
                    ));
                }
                dictionary.next_from(*index).map(|(position, entry)| {
                    *index = position + 1;
                    Next::Value(entry.original)
                })
            }
            IteratorState::Python(handle) => {
                let handle = *handle;
                return self.heap.python_next(handle, self.state.roots(), span);
            }
        };
        Ok(match next {
            None => None,
            Some(Next::Value(value)) => Some(value),
            Some(Next::Int(n)) => Some(self.heap.new_int(n, self.state.roots(), span)?),
            Some(Next::Character(c)) => {
                let c = c.encode_utf8(&mut [0; 4]).to_owned();
                Some(self.heap.new_text(c, self.state.roots(), span)?)
            }
        })
    }
}

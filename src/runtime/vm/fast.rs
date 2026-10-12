use super::{CodeFrame, Frame, IteratorState, Thread, Trace, TraceName};
use crate::check::ArgumentOrigin;
use crate::diagnostic::Span;
use crate::limits::Limits;
use crate::runtime::code::{CallCache, CallSite, Callee, Op};
use crate::runtime::heap::Heap;
use crate::runtime::value::{Int, Value};
use crate::stdlib::Native;
use crate::syntax::ast::BinOp;

fn fast_native(
    native: Native,
    site: &CallSite,
    cache: &CallCache,
    data: &[Option<Value>],
    heap: &mut Heap,
) -> Option<Value> {
    if !matches!(native, Native::Add | Native::Contains | Native::Missing) || site.incoming {
        return None;
    }
    let base = data.len().checked_sub(site.arguments.len() + 1)?;
    let argument = |parameter: usize| match cache.mapping.iter().nth(parameter)? {
        ArgumentOrigin::Argument(index) => data.get(base + 1 + index).copied().flatten(),
        _ => None,
    };
    let (first, second) = (argument(0)?, argument(1)?);
    match (native, first) {
        (Native::Add, Value::List(list)) => heap.try_append(list, second).then_some(Value::None),
        (Native::Contains | Native::Missing, Value::Dict(handle) | Value::Set(handle)) => {
            let key = heap.key(&second, Span::default()).ok()?;
            let found = heap
                .dictionary(handle, Span::default())
                .ok()?
                .position(&key)
                .is_some();
            Some(Value::Bool(found == (native == Native::Contains)))
        }
        _ => None,
    }
}

#[inline(always)]
pub(super) fn fast_binary(op: BinOp, left: &Value, right: &Value) -> Option<Value> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => {
            let (a, b) = (*a, *b);
            Some(Value::Int(match op {
                BinOp::Add => a.checked_add(b)?,
                BinOp::Sub => a.checked_sub(b)?,
                BinOp::Mul => a.checked_mul(b)?,
                BinOp::FloorDiv | BinOp::Mod => {
                    let (quotient, remainder) =
                        Int::Small(a).checked_div_mod_floor(&Int::Small(b))?;
                    let pick = if op == BinOp::FloorDiv {
                        quotient
                    } else {
                        remainder
                    };
                    match pick {
                        Int::Small(n) => n,
                        Int::Big(_) => return None,
                    }
                }
                _ => return None,
            }))
        }
        (Value::Float(a), Value::Float(b)) => Some(Value::Float(match op {
            BinOp::Add => a + b,
            BinOp::Sub => a - b,
            BinOp::Mul => a * b,
            BinOp::Div if *b != 0.0 => a / b,
            _ => return None,
        })),
        _ => None,
    }
}

#[inline(never)]
pub(super) fn fast_path(thread: &mut Thread, heap: &mut Heap, steps: &mut u64, limits: Limits) {
    let Some(Frame::Code(frame)) = thread.frames.last() else {
        return;
    };
    let mut code = frame.code.clone();
    let (mut ip, mut locals, mut cells, mut globals) =
        (frame.ip, frame.locals, frame.cells, frame.globals);
    let mut count = *steps;
    while count < limits.max_steps {
        let Some(constants) = code.constant_values.get() else {
            break;
        };
        let data = &mut thread.data;
        match code.ops[ip] {
            Op::LoadFast(slot) => match data[locals + slot as usize] {
                Some(value) => data.push(Some(value)),
                None => break,
            },
            Op::StoreFast(slot) => match data.pop() {
                Some(Some(value)) => data[locals + slot as usize] = Some(value),
                popped => {
                    data.extend(popped);
                    break;
                }
            },
            Op::Const(i) => data.push(Some(constants[i as usize])),
            Op::Pop => {
                if data.pop().is_none() {
                    break;
                }
            }
            Op::LoadGlobal(i) => match heap.global(globals, i, Span::default()) {
                Ok(Ok(value)) => data.push(Some(value)),
                _ => break,
            },
            Op::StoreGlobal(i) => {
                let Some(Some(value)) = data.last().copied() else {
                    break;
                };
                if heap
                    .store_global(globals, i, value, Span::default())
                    .is_err()
                {
                    break;
                }
                data.pop();
            }
            Op::LoadCell(i) => {
                let Some(Some(cell)) = thread.cells.get(cells + i as usize).copied() else {
                    break;
                };
                match heap.cell(cell, Span::default()) {
                    Ok(Some(value)) => data.push(Some(*value)),
                    _ => break,
                }
            }
            Op::Dup2 => {
                let length = data.len();
                let (Some(Some(owner)), Some(Some(index))) = (
                    data.get(length.wrapping_sub(2)),
                    data.get(length.wrapping_sub(1)),
                ) else {
                    break;
                };
                let pair = [Some(*owner), Some(*index)];
                data.extend(pair);
            }
            Op::Index => {
                let length = data.len();
                let (Some(Some(target)), Some(Some(index))) = (
                    data.get(length.wrapping_sub(2)),
                    data.get(length.wrapping_sub(1)),
                ) else {
                    break;
                };
                let item = match (target, index) {
                    (Value::List(handle) | Value::Tuple(handle), Value::Int(_)) => {
                        let Ok(items) = heap.list(*handle, Span::default()) else {
                            break;
                        };
                        match heap.index_position(index, items.len(), Span::default()) {
                            Ok(position) => items[position],
                            Err(_) => break,
                        }
                    }
                    (Value::Dict(handle), key) => {
                        let (Ok(key), Ok(dictionary)) = (
                            heap.key(key, Span::default()),
                            heap.dictionary(*handle, Span::default()),
                        ) else {
                            break;
                        };
                        match dictionary.get(&key) {
                            Some(value) => *value,
                            None => break,
                        }
                    }
                    _ => break,
                };
                data.truncate(length - 2);
                data.push(Some(item));
            }
            Op::PrepareIndex => {
                let length = data.len();
                let (Some(Some(owner)), Some(Some(index))) = (
                    data.get(length.wrapping_sub(2)),
                    data.get(length.wrapping_sub(1)),
                ) else {
                    break;
                };
                match owner {
                    Value::List(handle) => {
                        let Ok(items) = heap.list(*handle, Span::default()) else {
                            break;
                        };
                        let Ok(position) = heap.index_position(index, items.len(), Span::default())
                        else {
                            break;
                        };
                        data[length - 1] = Some(Value::Int(position as i64));
                    }
                    Value::Dict(_) => {
                        if heap.key(index, Span::default()).is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            Op::StoreIndex => {
                let length = data.len();
                let (Some(Some(owner)), Some(Some(index)), Some(Some(value))) = (
                    data.get(length.wrapping_sub(3)),
                    data.get(length.wrapping_sub(2)),
                    data.get(length.wrapping_sub(1)),
                ) else {
                    break;
                };
                let (owner, index, value) = (*owner, *index, *value);
                let stored = match (owner, index) {
                    (Value::List(handle), Value::Int(position)) => heap
                        .store_index(handle, position as usize, value, Span::default())
                        .is_ok(),
                    (Value::Dict(handle), _) => match heap.key(&index, Span::default()) {
                        Ok(key) => heap.try_store_key(handle, key, index, value),
                        Err(_) => false,
                    },
                    _ => false,
                };
                if !stored {
                    break;
                }
                data.truncate(length - 3);
            }
            Op::Jump(target) => {
                ip = target as usize;
                count += 1;
                continue;
            }
            Op::JumpIfFalse(target) => match data.last() {
                Some(Some(Value::Bool(holds))) => {
                    let holds = *holds;
                    data.pop();
                    if !holds {
                        ip = target as usize;
                        count += 1;
                        continue;
                    }
                }
                _ => break,
            },
            Op::Compare(op) | Op::CompareJump(op, _) => {
                let length = data.len();
                let (Some(Some(Value::Int(a))), Some(Some(Value::Int(b)))) = (
                    data.get(length.wrapping_sub(2)),
                    data.get(length.wrapping_sub(1)),
                ) else {
                    break;
                };
                let holds = op.holds(a.cmp(b));
                data.truncate(length - 2);
                if let Op::CompareJump(_, target) = code.ops[ip] {
                    if !holds {
                        ip = target as usize;
                        count += 1;
                        continue;
                    }
                } else {
                    data.push(Some(Value::Bool(holds)));
                }
            }
            Op::Binary(op) | Op::Adjust(op) => {
                let length = data.len();
                let (Some(Some(left)), Some(Some(right))) = (
                    data.get(length.wrapping_sub(2)),
                    data.get(length.wrapping_sub(1)),
                ) else {
                    break;
                };
                let Some(value) = fast_binary(op, left, right) else {
                    break;
                };
                data[length - 2] = Some(value);
                data.pop();
            }
            Op::ForIter(target) => {
                let next = match thread.iters.last_mut() {
                    Some(IteratorState::Count(next, last, down)) => {
                        let Int::Small(n) = *next else { break };
                        let within = if *down {
                            *next >= *last
                        } else {
                            *next <= *last
                        };
                        if within {
                            *next = next.step(*down);
                            Some(Value::Int(n))
                        } else {
                            None
                        }
                    }
                    Some(IteratorState::List(handle, index)) => {
                        let Ok(items) = heap.list(*handle, Span::default()) else {
                            break;
                        };
                        let item = items.get(*index).copied();
                        *index += 1;
                        item
                    }
                    _ => break,
                };
                match next {
                    Some(value) => data.push(Some(value)),
                    None => {
                        thread.iters.pop();
                        ip = target as usize;
                        count += 1;
                        continue;
                    }
                }
            }
            Op::Call(i) => {
                let site = &code.calls[i as usize];
                let Some(cache) = site.cache.get() else { break };
                if let Callee::Native(native) = cache.callee {
                    let Some(value) = fast_native(native, site, &cache, data, heap) else {
                        break;
                    };
                    let base = data.len() - site.arguments.len() - 1;
                    data.truncate(base - usize::from(site.incoming));
                    data.push(Some(value));
                    ip += 1;
                    count += 1;
                    continue;
                }
                let base = match data.len().checked_sub(site.arguments.len() + 1) {
                    Some(base) if cache.exact && !cache.checked && !site.incoming => base,
                    _ => break,
                };
                let Some(Value::Function(handle)) = data[base] else {
                    break;
                };
                let Ok(parts) = heap.function(handle, site.span) else {
                    break;
                };
                if cache.callee != Callee::Function(parts.info.version)
                    || thread.call_depth >= limits.max_call_depth
                {
                    break;
                }
                thread.trace.push(Trace {
                    name: TraceName::Function(parts.info.name.clone()),
                    span: site.span,
                    clause: site.clause,
                });
                let callee = parts.info.code.clone();
                if let Some(Frame::Code(caller)) = thread.frames.last_mut() {
                    caller.ip = ip + 1;
                }
                locals = base + 1;
                data.resize(locals + callee.locals as usize, None);
                cells = thread.cells.len();
                thread.cells.resize(cells + callee.cells as usize, None);
                thread
                    .cells
                    .extend(parts.captures.iter().map(|&cell| Some(cell)));
                globals = parts.globals;
                thread.frames.push(Frame::Code(CodeFrame {
                    code: callee.clone(),
                    ip: 0,
                    locals,
                    cells,
                    base,
                    iters: thread.iters.len(),
                    unwinds: thread.unwinds.len(),
                    trace: thread.trace.len() - 1,
                    call: true,
                    globals,
                }));
                thread.call_depth += 1;
                code = callee;
                ip = 0;
                count += 1;
                continue;
            }
            op @ (Op::Return | Op::ReturnNone) => {
                let depth = thread.frames.len();
                let (Some(Frame::Code(frame)), Some(Frame::Code(_))) = (
                    thread.frames.get(depth.wrapping_sub(1)),
                    thread.frames.get(depth.wrapping_sub(2)),
                ) else {
                    break;
                };
                if !frame.call {
                    break;
                }
                let value = if op == Op::Return {
                    match data.last() {
                        Some(Some(value)) => *value,
                        _ => break,
                    }
                } else {
                    Value::None
                };
                let Some(Frame::Code(frame)) = thread.frames.pop() else {
                    break;
                };
                data.truncate(frame.base);
                data.push(Some(value));
                thread.cells.truncate(frame.cells);
                thread.iters.truncate(frame.iters);
                thread.unwinds.truncate(frame.unwinds);
                thread.trace.truncate(frame.trace);
                thread.call_depth -= 1;
                let Some(Frame::Code(caller)) = thread.frames.last() else {
                    break;
                };
                code = caller.code.clone();
                (ip, locals, cells, globals) =
                    (caller.ip, caller.locals, caller.cells, caller.globals);
                count += 1;
                continue;
            }
            _ => break,
        }
        ip += 1;
        count += 1;
    }
    if let Some(Frame::Code(frame)) = thread.frames.last_mut() {
        frame.ip = ip;
    }
    *steps = count;
}

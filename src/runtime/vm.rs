mod calls;
mod dispatch;
mod fast;
mod flow;
mod unwind;

use std::collections::hash_map::Values;
use std::io::Write;
use std::rc::Rc;

use super::code::{Code, FunctionInfo};
use super::heap::{Handle, Heap};
use super::transfer::{self, Package};
use super::value::{Int, Value};
use crate::diagnostic::{CallFrame, Diagnostic, ExecutionError, Result, Span, internal};
use crate::limits::{Limit, MAX_TRACE_FRAMES};
use crate::stdlib::{FileSystem, Input, Native, Random, Reach};
use crate::syntax::ast::SignatureParameter;
use fast::fast_path;
use flow::{Flows, Group, MAIN, Parked};

pub(super) use flow::Delivery;

#[derive(Clone)]
pub(super) struct Access<'a> {
    pub(super) filesystem: Option<FileSystem>,
    pub(super) input: Option<&'a dyn Input>,
    pub(super) reach: Reach,
}

impl<'a> Access<'a> {
    pub(super) fn of(options: &crate::Options<'a>) -> Self {
        Self {
            filesystem: options.filesystem.cloned(),
            input: options.input,
            reach: Reach {
                network: options.network,
                system: options.system,
            },
        }
    }
}

pub(super) enum IteratorState {
    List(Handle, usize),
    Dict(Handle, usize, u64),
    Text(Handle, Rc<str>, usize),
    Count(Int, Int, bool),
}

impl IteratorState {
    fn handle(&self) -> Option<Handle> {
        match self {
            Self::List(handle, _) | Self::Dict(handle, ..) | Self::Text(handle, ..) => {
                Some(*handle)
            }
            Self::Count(..) => None,
        }
    }
}

pub(super) enum Raised {
    Pending {
        report: Box<ExecutionError>,
        cause: Option<Handle>,
    },
    Value(Handle),
}

impl Raised {
    fn root(&self) -> Option<Handle> {
        match self {
            Self::Pending { cause, .. } => *cause,
            Self::Value(handle) => Some(*handle),
        }
    }
}

pub(super) enum Unwind {
    Error(Raised),
    Exit(u8),
    Cancel,
}

#[derive(Clone, Copy)]
enum StoredUnwind {
    Error(Handle),
    Exit(u8),
    Cancel,
}

impl From<StoredUnwind> for Unwind {
    fn from(stored: StoredUnwind) -> Self {
        match stored {
            StoredUnwind::Error(handle) => Self::Error(Raised::Value(handle)),
            StoredUnwind::Exit(code) => Self::Exit(code),
            StoredUnwind::Cancel => Self::Cancel,
        }
    }
}

struct Trace {
    name: TraceName,
    span: Span,
    clause: Option<usize>,
}

struct CodeFrame {
    code: Rc<Code>,
    ip: usize,
    locals: usize,
    cells: usize,
    base: usize,
    iters: usize,
    unwinds: usize,
    trace: usize,
    call: bool,
    globals: Handle,
}

struct SortFrame {
    items: Handle,
    function: Value,
    keys: Vec<Value>,
    descending: bool,
    span: Span,
    base: usize,
    trace: usize,
}

enum Frame {
    Code(CodeFrame),
    Sort(SortFrame),
}

#[derive(Default)]
struct Thread {
    frames: Vec<Frame>,
    data: Vec<Option<Value>>,
    cells: Vec<Option<Handle>>,
    iters: Vec<IteratorState>,
    unwinds: Vec<StoredUnwind>,
    trace: Vec<Trace>,
    call_depth: usize,
    awaiting: Option<usize>,
}

impl Thread {
    #[inline]
    fn push_frame(
        &mut self,
        code: Rc<Code>,
        locals: usize,
        base: usize,
        captures: &[Handle],
        globals: Handle,
        call: bool,
    ) {
        self.data.resize(locals + code.locals as usize, None);
        let cells = self.cells.len();
        self.cells.resize(cells + code.cells as usize, None);
        self.cells.extend(captures.iter().map(|&cell| Some(cell)));
        self.frames.push(Frame::Code(CodeFrame {
            code,
            ip: 0,
            locals,
            cells,
            base,
            iters: self.iters.len(),
            unwinds: self.unwinds.len(),
            trace: self.trace.len() - usize::from(call),
            call,
            globals,
        }));
        if call {
            self.call_depth += 1;
        }
    }

    #[inline]
    fn release_frame(&mut self, frame: &CodeFrame) {
        self.data.truncate(frame.base);
        self.cells.truncate(frame.cells);
        self.iters.truncate(frame.iters);
        self.unwinds.truncate(frame.unwinds);
        self.trace.truncate(frame.trace);
        if frame.call {
            self.call_depth -= 1;
        }
    }

    #[inline]
    fn release_sort(&mut self, sort: &SortFrame) {
        self.data.truncate(sort.base);
        self.trace.truncate(sort.trace);
    }
}

#[derive(Clone)]
struct ThreadRoots<'a> {
    thread: &'a Thread,
    part: u8,
    index: usize,
    inner: usize,
}

impl<'a> ThreadRoots<'a> {
    fn new(thread: &'a Thread) -> Self {
        Self {
            thread,
            part: 0,
            index: 0,
            inner: 0,
        }
    }
}

impl Iterator for ThreadRoots<'_> {
    type Item = Handle;

    fn next(&mut self) -> Option<Handle> {
        let thread = self.thread;
        loop {
            let i = self.index;
            self.index += 1;
            let found = match self.part {
                0 => match thread.frames.get(i) {
                    Some(Frame::Code(frame)) => Some(frame.globals),
                    Some(Frame::Sort(sort)) => {
                        self.index = i;
                        let found = match self.inner {
                            0 => Some(sort.items),
                            1 => sort.function.handle(),
                            k => match sort.keys.get(k - 2) {
                                Some(key) => key.handle(),
                                None => {
                                    self.inner = 0;
                                    self.index = i + 1;
                                    continue;
                                }
                            },
                        };
                        self.inner += 1;
                        found
                    }
                    None => {
                        (self.part, self.index) = (1, 0);
                        continue;
                    }
                },
                1 => match thread.data.get(i) {
                    Some(value) => value.as_ref().and_then(Value::handle),
                    None => {
                        (self.part, self.index) = (2, 0);
                        continue;
                    }
                },
                2 => match thread.cells.get(i) {
                    Some(cell) => *cell,
                    None => {
                        (self.part, self.index) = (3, 0);
                        continue;
                    }
                },
                3 => match thread.iters.get(i) {
                    Some(iterator) => iterator.handle(),
                    None => {
                        (self.part, self.index) = (4, 0);
                        continue;
                    }
                },
                _ => match thread.unwinds.get(i) {
                    Some(StoredUnwind::Error(handle)) => Some(*handle),
                    Some(_) => None,
                    None => return None,
                },
            };
            if found.is_some() {
                return found;
            }
        }
    }
}

struct State {
    thread: Thread,
    flows: Flows,
    builtins: [Value; 2],
    held: Vec<Handle>,
}

impl State {
    fn roots(&self) -> Roots<'_> {
        Roots {
            state: self,
            thread: Some(ThreadRoots::new(&self.thread)),
            parked: self.flows.parked.values(),
            groups: self.flows.groups.values(),
            group: [None, None],
            index: 0,
        }
    }
}

#[derive(Clone)]
struct Roots<'a> {
    state: &'a State,
    thread: Option<ThreadRoots<'a>>,
    parked: Values<'a, usize, Parked>,
    groups: Values<'a, usize, Group>,
    group: [Option<Handle>; 2],
    index: usize,
}

impl Iterator for Roots<'_> {
    type Item = Handle;

    fn next(&mut self) -> Option<Handle> {
        loop {
            if let Some(thread) = &mut self.thread {
                match thread.next() {
                    Some(handle) => return Some(handle),
                    None => {
                        self.thread = self
                            .parked
                            .next()
                            .map(|parked| ThreadRoots::new(&parked.thread));
                        if self.thread.is_some() {
                            continue;
                        }
                    }
                }
            }
            if let Some(handle) = self.group.iter_mut().find_map(Option::take) {
                return Some(handle);
            }
            if let Some(group) = self.groups.next() {
                self.group = [group.failed.as_ref().and_then(Raised::root), group.results];
                continue;
            }
            let state = self.state;
            let i = self.index;
            self.index += 1;
            match i.checked_sub(state.builtins.len()) {
                None => {
                    if let Some(handle) = state.builtins[i].handle() {
                        return Some(handle);
                    }
                }
                Some(j) => return state.held.get(j).copied(),
            }
        }
    }
}

enum Control {
    Next,
    Reload,
    Raise(Unwind),
    Park,
    Finished(Value),
}

enum Ending {
    Returned(Value),
    Unwound(Unwind),
}

enum TraceName {
    Function(Rc<str>),
    Native(Native),
}

impl TraceName {
    fn as_str(&self) -> &str {
        match self {
            Self::Function(name) => name,
            Self::Native(native) => native.name(),
        }
    }
}

enum Target {
    Function(Handle, Rc<FunctionInfo>),
    Native(Native),
}

impl Target {
    fn name(&self) -> TraceName {
        match self {
            Self::Function(_, info) => TraceName::Function(info.name.clone()),
            Self::Native(native) => TraceName::Native(*native),
        }
    }

    fn signature(&self) -> &[SignatureParameter] {
        match self {
            Self::Function(_, info) => &info.signature,
            Self::Native(native) => native.plain_signature(),
        }
    }
}

pub(super) struct Vm<'w, W> {
    pub(super) heap: Heap,
    state: State,
    output: &'w mut W,
    access: Access<'w>,
    pub(super) random: Random,
    pub(super) steps: u64,
    arguments: Vec<Value>,
}

pub(super) fn unset(name: &str, span: Span) -> Diagnostic {
    Diagnostic::new(
        "E-INIT-001",
        span,
        format!("'{name}'의 값을 정의하기 전에 썼습니다."),
    )
}

impl<'w, W: Write> Vm<'w, W> {
    pub(super) fn new(
        heap: Heap,
        builtins: [Value; 2],
        output: &'w mut W,
        access: Access<'w>,
        random: Random,
    ) -> Self {
        Self {
            heap,
            state: State {
                thread: Thread::default(),
                flows: Flows::new(),
                builtins,
                held: Vec::new(),
            },
            output,
            access,
            random,
            steps: 0,
            arguments: Vec::new(),
        }
    }

    pub(super) fn hold(&mut self, handle: Handle) {
        self.state.held.push(handle);
    }

    pub(super) fn globals(&mut self, names: &[Rc<str>], span: Span) -> Result<Handle> {
        let handle = self.heap.new_globals(names, self.state.roots(), span)?;
        self.state.held.push(handle);
        Ok(handle)
    }

    #[cfg(test)]
    pub(super) fn roots(&self) -> impl Iterator<Item = Handle> + Clone + '_ {
        self.state.roots()
    }

    #[cfg(test)]
    pub(super) fn release(&mut self) {
        self.state.builtins = [Value::None, Value::None];
        self.state.held.clear();
    }

    pub(super) fn start(&mut self, code: Rc<Code>, globals: Handle) -> Result<()> {
        // 상수를 만드는 동안 프레임이 전역을 루트로 붙듭니다.
        let constants = code.clone();
        let thread = &mut self.state.thread;
        *thread = Thread::default();
        thread.push_frame(code, 0, 0, &[], globals, false);
        self.state.flows.current = MAIN;
        self.state.flows.group = None;
        self.state.flows.running = true;
        self.load_constants(&constants)
    }

    fn load_constants(&mut self, code: &Rc<Code>) -> Result<()> {
        let mut pending = vec![code.clone()];
        while let Some(code) = pending.pop() {
            if code.constant_values.get().is_none() {
                let mut values = Vec::with_capacity(code.constants.len());
                for constant in &code.constants {
                    let value =
                        self.heap
                            .new_constant(constant, self.state.roots(), Span::default())?;
                    self.state.held.extend(value.handle());
                    values.push(value);
                }
                code.constant_values
                    .set(values.into())
                    .map_err(|_| internal(Span::default()))?;
            }
            pending.extend(code.closures.iter().map(|c| c.info.code.clone()));
        }
        Ok(())
    }

    pub(super) fn held(&self) -> &[Handle] {
        &self.state.held
    }

    fn report(&self, diagnostic: Diagnostic) -> ExecutionError {
        let trace = &self.state.thread.trace;
        ExecutionError {
            diagnostic,
            frames: trace
                .iter()
                .rev()
                .take(MAX_TRACE_FRAMES)
                .map(|call| CallFrame {
                    name: call.name.as_str().to_owned(),
                    span: call.span,
                    clause: call.clause,
                })
                .collect(),
            omitted_frames: u32::try_from(trace.len().saturating_sub(MAX_TRACE_FRAMES))
                .unwrap_or(u32::MAX),
            causes: Box::default(),
            omitted_causes: 0,
            exit_code: None,
        }
    }

    pub(super) fn run(&mut self) -> std::result::Result<(), ExecutionError> {
        self.execute().map(drop)
    }

    pub(super) fn execute(&mut self) -> std::result::Result<Value, ExecutionError> {
        loop {
            let control = if self.state.flows.running {
                self.run_ops()
            } else {
                match self.switch() {
                    Ok(Some(resume)) => self.resume(resume),
                    Ok(None) => break,
                    Err(error) => Err(error),
                }
            };
            let ending = match control {
                Ok(Control::Next | Control::Reload | Control::Park) => continue,
                Ok(Control::Finished(value)) => Ending::Returned(value),
                Ok(Control::Raise(unwind)) => match self.unwind(unwind) {
                    Ok(None) => continue,
                    Ok(Some(unwind)) => Ending::Unwound(unwind),
                    Err(error) => return Err(self.report(error)),
                },
                Err(error) if error.is_fatal() => return Err(self.report(error)),
                Err(error) => {
                    let raised = self.raised(error);
                    match self.unwind(Unwind::Error(raised)) {
                        Ok(None) => continue,
                        Ok(Some(unwind)) => Ending::Unwound(unwind),
                        Err(error) => return Err(self.report(error)),
                    }
                }
            };
            match self.end_flow(ending) {
                Ok(None) => {}
                Ok(Some(result)) => return result,
                Err(error) => return Err(self.report(error)),
            }
        }
        self.flush_output().map(|()| Value::None)
    }

    fn flush_output(&mut self) -> std::result::Result<(), ExecutionError> {
        self.output.flush().map_err(|e| {
            self.report(Diagnostic::new(
                "E-IO-001",
                Span::default(),
                format!("출력을 마무리하지 못했습니다: {e}"),
            ))
        })
    }

    fn end_flow(
        &mut self,
        ending: Ending,
    ) -> Result<Option<std::result::Result<Value, ExecutionError>>> {
        let main = self.state.flows.current == MAIN;
        match ending {
            Ending::Returned(value) if main => {
                self.state.flows.running = false;
                Ok(Some(self.flush_output().map(|()| value)))
            }
            Ending::Returned(value) => {
                self.finish_flow(value)?;
                Ok(None)
            }
            Ending::Unwound(Unwind::Exit(code)) => {
                Ok(Some(Err(ExecutionError::exit(code, Span::default()))))
            }
            Ending::Unwound(Unwind::Error(raised)) if main => {
                Ok(Some(Err(self.raised_report(raised)?)))
            }
            Ending::Unwound(Unwind::Error(raised)) => {
                self.fail_flow(raised)?;
                Ok(None)
            }
            Ending::Unwound(Unwind::Cancel) => {
                let key = self.stop_flow()?;
                self.leave_group(key)?;
                Ok(None)
            }
        }
    }

    fn frame(&self) -> Result<&CodeFrame> {
        match self.state.thread.frames.last() {
            Some(Frame::Code(frame)) => Ok(frame),
            _ => Err(internal(Span::default())),
        }
    }

    fn run_ops(&mut self) -> Result<Control> {
        let limits = self.heap.limits;
        loop {
            fast_path(
                &mut self.state.thread,
                &mut self.heap,
                &mut self.steps,
                limits,
            );
            let (code, at, locals, cells, globals) = {
                let frame = self.frame()?;
                (
                    frame.code.clone(),
                    frame.ip,
                    frame.locals,
                    frame.cells,
                    frame.globals,
                )
            };
            let index = self.state.thread.frames.len() - 1;
            if self.steps >= limits.max_steps {
                return Err(Diagnostic::limit(
                    Limit::Steps(limits.max_steps),
                    code.spans[at],
                ));
            }
            self.steps += 1;
            let mut ip = at + 1;
            match self.op(
                code.ops[at],
                &code,
                at,
                &mut ip,
                locals,
                cells,
                globals,
                index,
            ) {
                Ok(Control::Next) => self.save(index, ip),
                Ok(Control::Reload) => {}
                Ok(control) => {
                    if matches!(control, Control::Raise(_)) {
                        self.save(index, ip);
                    }
                    return Ok(control);
                }
                Err(error) => {
                    self.save(index, ip);
                    return Err(error);
                }
            }
        }
    }

    fn save(&mut self, index: usize, ip: usize) {
        if let Some(Frame::Code(frame)) = self.state.thread.frames.get_mut(index) {
            frame.ip = ip;
        }
    }

    fn push(&mut self, value: Value) {
        self.state.thread.data.push(Some(value));
    }

    fn pop(&mut self) -> Result<Value> {
        self.state
            .thread
            .data
            .pop()
            .flatten()
            .ok_or_else(|| internal(Span::default()))
    }

    fn top(&self) -> Result<&Value> {
        self.state
            .thread
            .data
            .last()
            .and_then(Option::as_ref)
            .ok_or_else(|| internal(Span::default()))
    }

    fn take(&mut self, count: usize) -> Result<Vec<Value>> {
        let data = &mut self.state.thread.data;
        let start = data
            .len()
            .checked_sub(count)
            .ok_or_else(|| internal(Span::default()))?;
        data.drain(start..)
            .map(|value| value.ok_or_else(|| internal(Span::default())))
            .collect()
    }

    fn peek(&self, depth: usize) -> Result<&Value> {
        let data = &self.state.thread.data;
        data.len()
            .checked_sub(depth + 1)
            .and_then(|at| data[at].as_ref())
            .ok_or_else(|| internal(Span::default()))
    }

    fn cell(&self, cells: usize, index: u32, span: Span) -> Result<Handle> {
        self.state
            .thread
            .cells
            .get(cells + index as usize)
            .copied()
            .flatten()
            .ok_or_else(|| internal(span))
    }

    /// 돌려준 값은 아무 곳에도 붙잡혀 있지 않습니다.
    pub(super) fn unpack(&mut self, package: &Package, frozen: bool) -> Result<Vec<Value>> {
        let span = Span::default();
        let imported = transfer::import(&mut self.heap, package, self.state.roots(), frozen, span)?;
        let count = imported.values.len();
        self.state
            .thread
            .data
            .extend(imported.values.into_iter().map(Some));
        for code in &imported.codes {
            self.load_constants(code)?;
        }
        self.take(count)
    }

    pub(super) fn hold_value(&mut self, value: Value) {
        self.state.held.extend(value.handle());
    }

    pub(super) fn set_builtins(&mut self, builtins: [Value; 2]) {
        self.state.builtins = builtins;
    }

    pub(super) fn begin_call(
        &mut self,
        function: &Value,
        arguments: Vec<Value>,
        call_depth: usize,
    ) -> Result<()> {
        let mut thread = self.new_flow_thread(function, arguments, Span::default())?;
        thread.call_depth = call_depth;
        self.state.thread = thread;
        self.state.flows = Flows::new();
        Ok(())
    }

    pub(super) fn output(&mut self) -> &mut W {
        self.output
    }
}

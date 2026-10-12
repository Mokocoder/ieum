mod attributes;
mod code;
mod compile;
pub(crate) mod dictionary;
pub(crate) mod heap;
mod integer;
pub(crate) mod objects;
mod operations;
mod parallel;
mod python;
pub(crate) mod render;
#[cfg(test)]
mod tests;
mod transfer;
pub(crate) mod value;
mod vm;

use std::io::Write;

use crate::Options;
use crate::check::prepare_program;
use crate::diagnostic::{Diagnostic, ExecutionError, Result, Span, internal};
use crate::limits::{Limits, MAX_DIAGNOSTICS};
use crate::stdlib::{FileSystem, Random, Reach};
use crate::syntax::ast::{Expr, Statement, StatementKind};
use compile::{Globals, compile_display, compile_module};
use heap::{Handle, Heap};
use value::Value;
use vm::{Access, Vm, unset};

pub(crate) struct Program {
    statements: Vec<Statement>,
}

impl Program {
    pub fn prepare(mut statements: Vec<Statement>) -> std::result::Result<Self, Vec<Diagnostic>> {
        match prepare_program(&mut statements) {
            Ok(()) => Ok(Self { statements }),
            Err(crate::check::Errors(mut errors)) => {
                errors.sort_by_key(|error| error.span.start);
                errors.truncate(MAX_DIAGNOSTICS);
                Err(errors)
            }
        }
    }

    pub fn disassemble(&self, line: &dyn Fn(Span) -> usize) -> Result<String> {
        let code = compile_module(&self.statements, &mut Globals::default())?;
        let mut out = String::new();
        code.disassemble("<모듈>", line, &mut out);
        Ok(out)
    }

    pub fn run(
        &self,
        output: &mut impl Write,
        options: &Options,
    ) -> std::result::Result<(), ExecutionError> {
        self.vm(Heap::new(options.limits)?, output, options)?.run()
    }

    fn vm<'w, W: Write>(
        &self,
        mut heap: Heap,
        output: &'w mut W,
        options: &Options<'w>,
    ) -> Result<Vm<'w, W>> {
        let mut globals = Globals::default();
        let code = compile_module(&self.statements, &mut globals)?;
        let span = Span::default();
        let builtins = heap.builtins(options.arguments, options.system, span)?;
        let held = builtins.iter().filter_map(Value::handle);
        let handle = heap.new_globals(&globals.names, held, span)?;
        let random = Random::new(options.seed);
        let mut vm = Vm::new(heap, builtins, output, Access::of(options), random);
        vm.start(code, handle)?;
        Ok(vm)
    }
}

pub(crate) struct LinkedModule {
    pub span: Span,
    pub body: Vec<Statement>,
}

pub(crate) fn run_modules(
    modules: &[LinkedModule],
    order: &[usize],
    output: &mut impl Write,
    options: &Options,
) -> std::result::Result<(), ExecutionError> {
    let heap = Heap::new(options.limits)?;
    run_modules_with_heap(modules, order, output, options, heap).map(|_| ())
}

pub(crate) fn run_modules_with_heap(
    modules: &[LinkedModule],
    order: &[usize],
    output: &mut impl Write,
    options: &Options,
    mut heap: Heap,
) -> std::result::Result<Heap, ExecutionError> {
    let builtins = heap.builtins(options.arguments, options.system, Span::default())?;
    let random = Random::new(options.seed);
    let mut vm = Vm::new(heap, builtins, output, Access::of(options), random);
    let mut linked: Vec<Option<Handle>> = vec![None; modules.len()];
    for &id in order {
        let module = &modules[id];
        let mut names = Globals::default();
        let code = compile_module(&module.body, &mut names)?;
        let globals = vm.globals(&names.names, module.span)?;
        for statement in &module.body {
            let StatementKind::Import(import) = &statement.kind else {
                continue;
            };
            let target = import
                .target
                .and_then(|target| linked[target])
                .ok_or_else(|| internal(import.path_span))?;
            for (name, span) in &import.names {
                let value = imported(&vm.heap, target, name, *span)?;
                let index = names.get(name).ok_or_else(|| internal(*span))?;
                vm.heap.store_global(globals, index, value, *span)?;
            }
        }
        vm.start(code, globals)?;
        vm.run()?;
        linked[id] = Some(globals);
    }
    Ok(vm.heap)
}

fn imported(heap: &Heap, globals: Handle, name: &str, span: Span) -> Result<Value> {
    let index = heap
        .global_index(globals, name, span)?
        .ok_or_else(|| internal(span))?;
    heap.global(globals, index, span)?
        .map_err(|name| unset(name, span))
}

pub(crate) enum ReplInput<'a> {
    Statements(&'a [Statement]),
    Expression(&'a Expr),
}

pub(crate) struct ReplState {
    heap: Option<Heap>,
    globals: Handle,
    names: Globals,
    builtins: [Value; 2],
    /// 앞선 입력의 상수는 그 안의 함수가 아직 실행될 수 있으므로 붙잡아 둡니다.
    held: Vec<Handle>,
    filesystem: Option<FileSystem>,
    reach: Reach,
}

impl ReplState {
    pub fn new(limits: Limits, filesystem: Option<&FileSystem>, reach: Reach) -> Result<Self> {
        let mut heap = Heap::new(limits)?;
        let span = Span::default();
        let builtins = heap.builtins(&[], reach.system, span)?;
        let held = builtins.iter().filter_map(Value::handle);
        let globals = heap.new_globals(&[], held, span)?;
        Ok(Self {
            heap: Some(heap),
            globals,
            names: Globals::default(),
            builtins,
            held: Vec::new(),
            filesystem: filesystem.cloned(),
            reach,
        })
    }

    #[cfg(test)]
    pub(crate) fn stress_gc(&mut self) {
        self.heap.as_mut().unwrap().stress_gc = true;
    }

    pub fn run(
        &mut self,
        input: ReplInput<'_>,
        output: &mut impl Write,
    ) -> std::result::Result<(), ExecutionError> {
        let code = match input {
            ReplInput::Statements(body) => compile_module(body, &mut self.names)?,
            ReplInput::Expression(expr) => compile_display(expr, &mut self.names)?,
        };
        let mut heap = self.heap.take().ok_or_else(|| internal(Span::default()))?;
        let held = self
            .builtins
            .iter()
            .filter_map(Value::handle)
            .chain(self.held.iter().copied());
        let extended = heap.extend_globals(self.globals, &self.names.names, held, Span::default());
        if let Err(error) = extended {
            self.heap = Some(heap);
            return Err(error.into());
        }
        let access = Access {
            filesystem: self.filesystem.clone(),
            input: None,
            reach: self.reach,
        };
        let builtins = self.builtins;
        let mut vm = Vm::new(heap, builtins, output, access, Random::new(None));
        vm.hold(self.globals);
        for &handle in &self.held {
            vm.hold(handle);
        }
        let kept = vm.held().len();
        let result = vm
            .start(code, self.globals)
            .map_err(Into::into)
            .and_then(|()| vm.run());
        self.held.extend_from_slice(&vm.held()[kept..]);
        self.heap = Some(vm.heap);
        result
    }
}

fn spawn(span: Span, work: impl FnOnce() + Send + 'static) -> Result<()> {
    std::thread::Builder::new()
        .spawn(work)
        .map(drop)
        .map_err(|error| {
            Diagnostic::new(
                "E-WORK-001",
                span,
                format!("작업 스레드를 시작하지 못했습니다: {error}"),
            )
        })
}

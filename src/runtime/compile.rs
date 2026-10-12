use std::cell::{Cell, OnceCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::code::{
    CallSite, Closure, Code, FunctionInfo, Handler, NameOp, Op, RecordField, RecordInfo,
    RecordLiteral, TemplatePart,
};
use super::value::{Constant, Int};
use crate::diagnostic::{Result, Span, internal};
use crate::stdlib::{self, Native};
use crate::syntax::ast::*;

#[derive(Default)]
pub(crate) struct Globals {
    pub names: Vec<Rc<str>>,
    index: HashMap<Rc<str>, u32>,
}

impl Globals {
    pub fn get(&self, name: &str) -> Option<u32> {
        self.index.get(name).copied()
    }

    fn add(&mut self, name: &str) -> u32 {
        if let Some(index) = self.get(name) {
            return index;
        }
        let name: Rc<str> = name.into();
        let index = self.names.len() as u32;
        self.names.push(name.clone());
        self.index.insert(name, index);
        index
    }
}

pub(crate) fn compile_module(statements: &[Statement], globals: &mut Globals) -> Result<Rc<Code>> {
    let mut compiler = Compiler::new(globals);
    compiler.units.push(Unit::default());
    compiler.block(statements, Binders::None, BlockEntry::Global)?;
    Ok(compiler.finish()?.0)
}

pub(crate) fn compile_display(expr: &Expr, globals: &mut Globals) -> Result<Rc<Code>> {
    let mut compiler = Compiler::new(globals);
    compiler.units.push(Unit::default());
    compiler.expr(expr)?;
    compiler.emit(Op::Display, expr.span);
    Ok(compiler.finish()?.0)
}

struct Decl {
    name: Rc<str>,
    unit: usize,
    slot: u32,
    global: Option<u32>,
    captured: bool,
}

#[derive(Default)]
struct Scope<'a> {
    names: HashMap<&'a str, usize>,
}

enum Resolved {
    Decl(usize),
    Global(u32),
    Constant(Constant),
    Builtin(u32),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockEntry {
    Global,
    Body,
    Nested,
}

enum Binders<'s, 'a> {
    None,
    Stack(&'s [(&'a str, Span)]),
    Parameters(&'s [(&'a str, Span)]),
}

enum Block<'a> {
    Loop {
        top: u32,
        end: u32,
        iterator: bool,
    },
    Try {
        cleanup: Option<&'a [Statement]>,
        handler: u32,
    },
    Handler {
        cleanup: Option<&'a [Statement]>,
        handler: Option<u32>,
    },
    Cleanup,
    Value,
}

struct HandlerDef {
    target: u32,
    depth: u32,
    iters: u32,
    unwinds: u32,
}

#[derive(Default)]
struct Unit<'a> {
    ops: Vec<Op>,
    spans: Vec<Span>,
    handler_of: Vec<Option<u32>>,
    handlers: Vec<HandlerDef>,
    labels: Vec<Option<u32>>,
    blocks: Vec<Block<'a>>,
    handler: Option<u32>,
    iters: u32,
    unwinds: u32,
    locals: Vec<Rc<str>>,
    owned: Vec<usize>,
    frees: Vec<usize>,
    constants: Vec<Constant>,
    constant_index: HashMap<ConstantKey, u32>,
    names: Vec<Rc<str>>,
    either_names: Vec<(Rc<str>, Rc<str>)>,
    closures: Vec<(Rc<FunctionInfo>, Vec<usize>)>,
    record_types: Vec<Rc<RecordInfo>>,
    calls: Vec<CallSite>,
    templates: Vec<Box<[TemplatePart]>>,
    record_literals: Vec<RecordLiteral>,
    count_end_spans: Vec<Span>,
}

impl Unit<'_> {
    fn current_handler(&self) -> Option<u32> {
        self.blocks.iter().rev().find_map(|block| match block {
            Block::Try { handler, .. } => Some(*handler),
            Block::Handler { handler, .. } => *handler,
            _ => None,
        })
    }
}

struct Compiler<'a, 'g> {
    globals: &'g mut Globals,
    decls: Vec<Decl>,
    scopes: Vec<Scope<'a>>,
    units: Vec<Unit<'a>>,
}

impl<'a, 'g> Compiler<'a, 'g> {
    fn new(globals: &'g mut Globals) -> Self {
        Self {
            globals,
            decls: Vec::new(),
            scopes: Vec::new(),
            units: Vec::new(),
        }
    }

    fn unit(&mut self) -> &mut Unit<'a> {
        self.units.last_mut().expect("컴파일 중인 단위가 있습니다")
    }

    fn emit(&mut self, op: Op, span: Span) {
        let unit = self.unit();
        unit.ops.push(op);
        unit.spans.push(span);
        let handler = unit.handler;
        unit.handler_of.push(handler);
    }

    fn label(&mut self) -> u32 {
        push_index(&mut self.unit().labels, None)
    }

    fn bind(&mut self, label: u32) {
        let unit = self.unit();
        unit.labels[label as usize] = Some(unit.ops.len() as u32);
    }

    fn push_block(&mut self, block: Block<'a>) {
        let unit = self.unit();
        unit.blocks.push(block);
        unit.handler = unit.current_handler();
    }

    fn pop_block(&mut self) -> Option<Block<'a>> {
        let unit = self.unit();
        let block = unit.blocks.pop();
        unit.handler = unit.current_handler();
        block
    }

    fn add_handler(&mut self, target: u32) -> u32 {
        let unit = self.unit();
        let (iters, unwinds) = (unit.iters, unit.unwinds);
        let depth = unit
            .blocks
            .iter()
            .filter(|block| matches!(block, Block::Value))
            .count() as u32;
        push_index(
            &mut unit.handlers,
            HandlerDef {
                target,
                depth,
                iters,
                unwinds,
            },
        )
    }

    fn emit_constant(&mut self, value: Constant, span: Span) {
        let unit = self.unit();
        let next = unit.constants.len() as u32;
        let index = *unit
            .constant_index
            .entry(ConstantKey::of(&value))
            .or_insert(next);
        if index == next {
            unit.constants.push(value);
        }
        self.emit(Op::Const(index), span);
    }

    fn name_index(&mut self, name: &str) -> u32 {
        let unit = self.unit();
        match unit.names.iter().position(|known| &**known == name) {
            Some(index) => index as u32,
            None => push_index(&mut unit.names, name.into()),
        }
    }

    fn declare(&mut self, name: &'a str, global: bool) -> usize {
        let depth = self.units.len() - 1;
        let global = global.then(|| self.globals.add(name));
        let unit = self.unit();
        let slot = unit.locals.len() as u32;
        if global.is_none() {
            unit.locals.push(name.into());
        }
        let id = self.decls.len();
        self.unit().owned.push(id);
        self.decls.push(Decl {
            name: name.into(),
            unit: depth,
            slot,
            global,
            captured: false,
        });
        self.scopes
            .last_mut()
            .expect("열린 범위가 있습니다")
            .names
            .insert(name, id);
        id
    }

    fn resolve(&self, name: &str, span: Span) -> Result<Resolved> {
        if let Some(id) = self.scopes.iter().rev().find_map(|s| s.names.get(name)) {
            return Ok(Resolved::Decl(*id));
        }
        if let Some(index) = self.globals.get(name) {
            return Ok(Resolved::Global(index));
        }
        if let Some(native) = Native::ALL.into_iter().find(|n| n.name() == name) {
            return Ok(Resolved::Constant(Constant::Native(native)));
        }
        if let Some((_, value)) = stdlib::numbers().into_iter().find(|(n, _)| *n == name) {
            return Ok(Resolved::Constant(value));
        }
        match name {
            stdlib::ARGUMENTS => Ok(Resolved::Builtin(0)),
            stdlib::ENVIRONMENT => Ok(Resolved::Builtin(1)),
            _ => Err(internal(span)),
        }
    }

    fn reference(&mut self, id: usize) {
        let owner = self.decls[id].unit;
        let current = self.units.len() - 1;
        if self.decls[id].global.is_some() || owner == current {
            return;
        }
        self.decls[id].captured = true;
        for unit in &mut self.units[owner + 1..] {
            if !unit.frees.contains(&id) {
                unit.frees.push(id);
            }
        }
    }

    fn name_op(&mut self, kind: NameOp, name: &str, span: Span) -> Result<()> {
        let global = |index| match kind {
            NameOp::Load => Some(Op::LoadGlobal(index)),
            NameOp::Store => Some(Op::StoreGlobal(index)),
            NameOp::Check => Some(Op::CheckGlobal(index)),
            _ => None,
        };
        let op = match self.resolve(name, span)? {
            Resolved::Decl(id) => match self.decls[id].global {
                Some(index) => global(index),
                None => {
                    self.reference(id);
                    Some(Op::Name(kind, id as u32))
                }
            },
            Resolved::Global(index) => global(index),
            Resolved::Constant(value) if kind == NameOp::Load => {
                self.emit_constant(value, span);
                None
            }
            Resolved::Builtin(index) if kind == NameOp::Load => Some(Op::LoadBuiltin(index)),
            _ => return Err(internal(span)),
        };
        if let Some(op) = op {
            self.emit(op, span);
        }
        Ok(())
    }

    fn decl_op(&mut self, kind: NameOp, id: usize, span: Span) {
        match self.decls[id].global {
            Some(index) if kind == NameOp::Store => self.emit(Op::StoreGlobal(index), span),
            Some(index) if kind == NameOp::Load => self.emit(Op::LoadGlobal(index), span),
            Some(_) => {}
            None => self.emit(Op::Name(kind, id as u32), span),
        }
    }

    fn closure(
        &mut self,
        name: Rc<str>,
        signature: Box<[SignatureParameter]>,
        build: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<u32> {
        self.units.push(Unit::default());
        build(self)?;
        let (code, frees) = self.finish()?;
        let function = Rc::new(FunctionInfo {
            name,
            signature,
            code,
            version: next_version(),
        });
        Ok(push_index(&mut self.unit().closures, (function, frees)))
    }

    fn finish(&mut self) -> Result<(Rc<Code>, Vec<usize>)> {
        self.emit(Op::ReturnNone, Span::default());
        let unit = self.units.pop().expect("컴파일 중인 단위가 있습니다");
        let (cells, cell_names, own_cells) = self.cell_slots(&unit);
        let mut ops = unit.ops;
        self.resolve_names(&mut ops, &cells);
        let closures = closure_captures(unit.closures, &cells)?;
        fuse_compare_jumps(&mut ops, &unit.labels);
        let (labels, kept) = relocate_labels(&ops, &unit.labels)?;
        let (ops, spans, handler_of) = remove_nops(ops, unit.spans, unit.handler_of, &labels, kept);
        let code = Code {
            ops: ops.into(),
            spans: spans.into(),
            handlers: handler_table(&handler_of, &unit.handlers, &labels).into(),
            constants: unit.constants.into(),
            constant_values: OnceCell::new(),
            names: unit.names.into(),
            either_names: unit.either_names.into(),
            locals: unit.locals.len() as u32,
            local_names: unit.locals.into(),
            cells: own_cells,
            cell_names: cell_names.into(),
            closures,
            record_types: unit.record_types.into(),
            calls: unit.calls.into(),
            templates: unit.templates.into(),
            record_literals: unit.record_literals.into(),
            count_end_spans: unit.count_end_spans.into(),
        };
        Ok((Rc::new(code), unit.frees))
    }

    fn cell_slots(&self, unit: &Unit) -> (HashMap<usize, u32>, Vec<Rc<str>>, u32) {
        let mut cells: HashMap<usize, u32> = HashMap::new();
        let mut cell_names = Vec::new();
        for &id in &unit.owned {
            let decl = &self.decls[id];
            if decl.captured && decl.global.is_none() {
                cells.insert(id, cell_names.len() as u32);
                cell_names.push(decl.name.clone());
            }
        }
        let own_cells = cell_names.len() as u32;
        for &id in &unit.frees {
            cells.insert(id, cell_names.len() as u32);
            cell_names.push(self.decls[id].name.clone());
        }
        (cells, cell_names, own_cells)
    }

    fn resolve_names(&self, ops: &mut [Op], cells: &HashMap<usize, u32>) {
        for op in ops {
            let Op::Name(kind, id) = *op else { continue };
            let slot = self.decls[id as usize].slot;
            *op = match (cells.get(&(id as usize)).copied(), kind) {
                (Some(cell), NameOp::Load) => Op::LoadCell(cell),
                (Some(cell), NameOp::Store) => Op::StoreCell(cell),
                (Some(cell), NameOp::Check) => Op::CheckCell(cell),
                (Some(cell), NameOp::Parameter) => Op::CellFromFast {
                    cell: cell as u16,
                    slot: slot as u16,
                },
                (Some(cell), _) => Op::MakeCell(cell),
                (None, NameOp::Load) => Op::LoadFast(slot),
                (None, NameOp::Store) => Op::StoreFast(slot),
                (None, NameOp::Check) => Op::CheckFast(slot),
                (None, NameOp::Clear) => Op::ClearFast(slot),
                (None, _) => Op::Nop,
            };
        }
    }

    fn block(
        &mut self,
        statements: &'a [Statement],
        binders: Binders<'_, 'a>,
        entry: BlockEntry,
    ) -> Result<()> {
        self.filtered_block(statements, binders, entry, None)
    }

    fn filtered_block(
        &mut self,
        statements: &'a [Statement],
        binders: Binders<'_, 'a>,
        entry: BlockEntry,
        filter: Option<(&'a Expr, Span, u32)>,
    ) -> Result<()> {
        self.scopes.push(Scope::default());
        let global = entry == BlockEntry::Global;
        let clear = if entry == BlockEntry::Nested {
            NameOp::Clear
        } else {
            NameOp::ClearFirst
        };
        match binders {
            Binders::None => {}
            Binders::Parameters(names) => {
                for &(name, span) in names {
                    let id = self.declare(name, false);
                    self.decl_op(NameOp::Parameter, id, span);
                }
            }
            Binders::Stack(names) => {
                let ids: Vec<(usize, Span)> = names
                    .iter()
                    .map(|&(name, span)| (self.declare(name, false), span))
                    .collect();
                for &(id, span) in &ids {
                    self.decl_op(NameOp::ClearBound, id, span);
                }
                for &(id, span) in ids.iter().rev() {
                    self.decl_op(NameOp::Store, id, span);
                }
            }
        }
        for statement in statements {
            if let StatementKind::Import(import) = &statement.kind {
                for (name, _) in &import.names {
                    self.declare(name, global);
                }
                continue;
            }
            for declared in statement.declarations() {
                let id = self.declare(declared.name, global);
                if !global {
                    self.decl_op(clear, id, declared.span);
                }
            }
        }
        if let Some((test, span, skip)) = filter {
            self.expr(test)?;
            self.emit(Op::JumpIfFalse(skip), span);
        }
        for statement in statements {
            self.statement(statement)?;
        }
        self.scopes.pop();
        Ok(())
    }

    fn statement(&mut self, statement: &'a Statement) -> Result<()> {
        let span = statement.span;
        match &statement.kind {
            StatementKind::Import(_) => {}
            StatementKind::Bind {
                name,
                name_span,
                value,
                ..
            } => {
                self.expr(value)?;
                self.name_op(NameOp::Store, name, *name_span)?;
            }
            StatementKind::Assign {
                target,
                target_span,
                value,
                adjust,
            } => self.assign(target, *target_span, value, *adjust)?,
            StatementKind::Chain(chain) => {
                self.chain(chain)?;
                self.emit(Op::Pop, span);
            }
            StatementKind::Function(function) => self.function(function)?,
            StatementKind::Record(record) => {
                for field in &record.fields {
                    self.default(field.default.as_ref(), field.span)?;
                }
                let info = Rc::new(RecordInfo {
                    name: record.name.clone(),
                    fields: record
                        .fields
                        .iter()
                        .map(|field| RecordField {
                            name: field.name.clone(),
                            has_default: field.default.is_some(),
                        })
                        .collect(),
                });
                let index = push_index(&mut self.unit().record_types, info);
                self.emit(Op::MakeRecordType(index), record.name_span);
                self.name_op(NameOp::Store, &record.name, record.name_span)?;
            }
            StatementKind::Try(attempt) => self.attempt(attempt)?,
            StatementKind::Throw(value) => {
                self.expr(value)?;
                self.emit(Op::Throw, value.span);
            }
            StatementKind::Rethrow => self.emit(Op::Rethrow, span),
            StatementKind::If {
                test,
                test_span,
                body,
                otherwise,
            } => {
                self.expr(test)?;
                let other = self.label();
                self.emit(Op::JumpIfFalse(other), *test_span);
                self.block(body, Binders::None, BlockEntry::Nested)?;
                if otherwise.is_empty() {
                    self.bind(other);
                } else {
                    let end = self.label();
                    self.emit(Op::Jump(end), span);
                    self.bind(other);
                    self.block(otherwise, Binders::None, BlockEntry::Nested)?;
                    self.bind(end);
                }
            }
            StatementKind::While {
                test,
                test_span,
                body,
            } => {
                let (top, end) = (self.label(), self.label());
                self.bind(top);
                self.expr(test)?;
                self.emit(Op::JumpIfFalse(end), *test_span);
                self.push_block(Block::Loop {
                    top,
                    end,
                    iterator: false,
                });
                self.block(body, Binders::None, BlockEntry::Nested)?;
                self.pop_block();
                self.emit(Op::Jump(top), span);
                self.bind(end);
            }
            StatementKind::For {
                name,
                name_span,
                second,
                collection,
                last,
                filter,
                body,
                concurrency,
            } => {
                if let Some((label, op)) = match concurrency {
                    Concurrency::Sequential => None,
                    Concurrency::Together => Some(("함께", Op::TogetherEach)),
                    Concurrency::Parallel => Some(("병렬로", Op::ParallelEach)),
                } {
                    self.expr(collection)?;
                    let names = [(name.as_str(), *name_span)];
                    let flow = self.closure(label.into(), Box::default(), |compiler| {
                        compiler.block(body, Binders::Parameters(&names), BlockEntry::Body)
                    })?;
                    self.emit(Op::MakeFunction(flow), span);
                    self.emit(op, span);
                    return Ok(());
                }
                self.expr(collection)?;
                if let Some((last, down)) = last.as_deref() {
                    self.expr(last)?;
                    let index = push_index(&mut self.unit().count_end_spans, last.span);
                    self.emit(Op::GetCount(*down, index), collection.span);
                } else {
                    if second.is_some() {
                        self.emit(Op::DictItems, span);
                    }
                    self.emit(Op::GetIter, span);
                }
                let (top, end) = (self.label(), self.label());
                self.bind(top);
                self.emit(Op::ForIter(end), span);
                let mut names = vec![(name.as_str(), *name_span)];
                if let Some((second, second_span)) = second {
                    self.emit(Op::Unpack2, *second_span);
                    names.push((second.as_str(), *second_span));
                }
                self.unit().iters += 1;
                self.push_block(Block::Loop {
                    top,
                    end,
                    iterator: true,
                });
                let filter = filter.as_ref().map(|(test, span)| (test, *span, top));
                self.filtered_block(body, Binders::Stack(&names), BlockEntry::Nested, filter)?;
                self.pop_block();
                self.unit().iters -= 1;
                self.emit(Op::Jump(top), span);
                self.bind(end);
            }
            StatementKind::Concurrent(Concurrency::Parallel, body) => {
                self.parallel_block(body, span)?;
            }
            StatementKind::Concurrent(_, body) => {
                for statement in body {
                    let flow = self.closure("함께".into(), Box::default(), |compiler| {
                        compiler.statement(statement)
                    })?;
                    self.emit(Op::MakeFunction(flow), statement.span);
                }
                self.emit(Op::Together(body.len() as u32), span);
            }
            StatementKind::Return(value) => {
                self.expr(value)?;
                self.leave(false, true, span)?;
                self.emit(Op::Return, span);
            }
            StatementKind::Break => {
                let Some((_, end, iterator)) = self.leave(true, false, span)? else {
                    return Err(internal(span));
                };
                if iterator {
                    self.emit(Op::PopIter, span);
                }
                self.emit(Op::Jump(end), span);
            }
            StatementKind::Continue => {
                let Some((top, _, _)) = self.leave(true, false, span)? else {
                    return Err(internal(span));
                };
                self.emit(Op::Jump(top), span);
            }
            StatementKind::Exit(None) => self.emit(Op::ExitZero, span),
            StatementKind::Exit(Some(code)) => {
                self.expr(code)?;
                self.emit(Op::Exit, code.span);
            }
        }
        Ok(())
    }

    fn leave(
        &mut self,
        to_loop: bool,
        value: bool,
        span: Span,
    ) -> Result<Option<(u32, u32, bool)>> {
        let Some(block) = self.pop_block() else {
            return Ok(None);
        };
        let mut found = None;
        match &block {
            Block::Loop { top, end, iterator } if to_loop => found = Some((*top, *end, *iterator)),
            Block::Loop { .. } => {}
            Block::Try { cleanup, .. } => self.inline_cleanup(*cleanup, value)?,
            Block::Handler { cleanup, .. } => {
                self.emit(Op::PopUnwind, span);
                self.inline_cleanup(*cleanup, value)?;
            }
            Block::Cleanup => self.emit(Op::PopUnwind, span),
            Block::Value if !value => self.emit(Op::Pop, span),
            Block::Value => {}
        }
        if found.is_none() {
            found = self.leave(to_loop, value, span)?;
        }
        self.push_block(block);
        Ok(found)
    }

    fn inline_cleanup(&mut self, cleanup: Option<&'a [Statement]>, value: bool) -> Result<()> {
        let Some(cleanup) = cleanup else {
            return Ok(());
        };
        if value {
            self.push_block(Block::Value);
        }
        self.block(cleanup, Binders::None, BlockEntry::Nested)?;
        if value {
            self.pop_block();
        }
        Ok(())
    }

    fn attempt(&mut self, attempt: &'a TryBlock) -> Result<()> {
        let cleanup = (!attempt.cleanup.is_empty()).then_some(&attempt.cleanup[..]);
        let cleanup_label = cleanup.map(|_| self.label());
        let cleanup_handler = cleanup_label.map(|label| self.add_handler(label));
        let handler_label = attempt.handler.as_ref().map(|_| self.label());
        let body_handler = handler_label
            .map(|label| self.add_handler(label))
            .or(cleanup_handler);
        let Some(body_handler) = body_handler else {
            return self.block(&attempt.body, Binders::None, BlockEntry::Nested);
        };
        self.push_block(Block::Try {
            cleanup,
            handler: body_handler,
        });
        self.block(&attempt.body, Binders::None, BlockEntry::Nested)?;
        self.pop_block();
        let after = self.label();
        if let (Some(handler), Some(label)) = (&attempt.handler, handler_label) {
            self.emit(Op::Jump(after), handler.span);
            self.bind(label);
            self.push_block(Block::Handler {
                cleanup,
                handler: cleanup_handler,
            });
            self.emit(Op::Catch, handler.span);
            self.unit().unwinds += 1;
            let names = [(handler.name.as_str(), handler.name_span)];
            self.block(&handler.body, Binders::Stack(&names), BlockEntry::Nested)?;
            self.emit(Op::PopUnwind, handler.span);
            self.unit().unwinds -= 1;
            self.pop_block();
        }
        self.bind(after);
        if let (Some(cleanup), Some(label)) = (cleanup, cleanup_label) {
            let end = self.label();
            self.block(cleanup, Binders::None, BlockEntry::Nested)?;
            self.emit(Op::Jump(end), attempt.cleanup_span);
            self.bind(label);
            self.push_block(Block::Cleanup);
            self.unit().unwinds += 1;
            self.block(cleanup, Binders::None, BlockEntry::Nested)?;
            self.unit().unwinds -= 1;
            self.pop_block();
            self.emit(Op::Reraise, attempt.cleanup_span);
            self.bind(end);
        }
        Ok(())
    }

    fn default(&mut self, default: Option<&'a Expr>, span: Span) -> Result<()> {
        match default {
            Some(value) => self.expr(value),
            None => {
                self.emit_constant(Constant::None, span);
                Ok(())
            }
        }
    }

    fn function(&mut self, function: &'a Function) -> Result<()> {
        for parameter in &function.parameters {
            self.default(parameter.default.as_ref(), parameter.name_span)?;
        }
        let names: Vec<(&str, Span)> = function
            .parameters
            .iter()
            .map(|p| (p.name.as_str(), p.name_span))
            .collect();
        let signature = function
            .parameters
            .iter()
            .map(Parameter::signature)
            .collect();
        let closure = self.closure(function.shown.as_str().into(), signature, |compiler| {
            compiler.block(
                &function.body,
                Binders::Parameters(&names),
                BlockEntry::Body,
            )
        })?;
        self.emit(Op::MakeFunction(closure), function.name_span);
        self.name_op(NameOp::Store, &function.name, function.name_span)
    }

    fn assign(
        &mut self,
        target: &'a Expr,
        target_span: Span,
        value: &'a Expr,
        adjust: Option<BinOp>,
    ) -> Result<()> {
        let target = target.ungrouped();
        match &target.kind {
            ExprKind::Name(name) => {
                if adjust.is_none() {
                    self.name_op(NameOp::Check, name, target_span)?;
                }
                if let Some(op) = adjust {
                    self.name_op(NameOp::Load, name, target_span)?;
                    self.expr(value)?;
                    self.emit(Op::Adjust(op), target_span);
                } else {
                    self.expr(value)?;
                }
                self.name_op(NameOp::Store, name, target_span)
            }
            ExprKind::Index(owner, index, span) => {
                self.expr(owner)?;
                self.expr(index)?;
                self.emit(Op::PrepareIndex, *span);
                if let Some(op) = adjust {
                    self.emit(Op::Dup2, *span);
                    self.emit(Op::Index, *span);
                    self.expr(value)?;
                    self.emit(Op::Adjust(op), *span);
                } else {
                    self.expr(value)?;
                }
                self.emit(Op::StoreIndex, *span);
                Ok(())
            }
            ExprKind::Attribute(owner, name) => {
                let span = target.name_span;
                self.expr(owner)?;
                let name = self.name_index(name);
                self.emit(Op::PrepareAttribute(name), span);
                if let Some(op) = adjust {
                    self.emit(Op::Dup, span);
                    self.emit(Op::Attribute(name), span);
                    self.expr(value)?;
                    self.emit(Op::Adjust(op), span);
                } else {
                    self.expr(value)?;
                }
                self.emit(Op::StoreAttribute(name), span);
                Ok(())
            }
            _ => Err(internal(target_span)),
        }
    }

    fn chain(&mut self, chain: &'a Chain) -> Result<()> {
        for (index, call) in chain.calls.iter().enumerate() {
            let incoming = index > 0 && chain.edges[index - 1] == Edge::Pipe;
            if index > 0 && !incoming {
                self.emit(Op::Pop, call.span);
            }
            match &call.callee {
                Callee::Verb { name, .. } => self.name_op(NameOp::Load, name, call.span)?,
                Callee::Invoke(value) => self.expr(value)?,
            }
            for argument in &call.arguments {
                self.expr(&argument.value)?;
            }
            let site = CallSite {
                span: call.span,
                clause: (chain.calls.len() > 1).then_some(index + 1),
                arguments: call
                    .arguments
                    .iter()
                    .map(|argument| (argument.role, argument.span))
                    .collect(),
                values: call.arguments.iter().map(|a| a.value.span).collect(),
                cache: Cell::new(None),
                incoming,
            };
            let site = push_index(&mut self.unit().calls, site);
            self.emit(Op::Call(site), call.span);
        }
        Ok(())
    }

    fn expr(&mut self, expr: &'a Expr) -> Result<()> {
        let span = expr.span;
        match &expr.kind {
            ExprKind::Literal(value) => self.emit_constant(value.clone(), span),
            ExprKind::Name(name) => self.name_op(NameOp::Load, name, expr.name_span)?,
            ExprKind::Group(inner) => self.expr(inner)?,
            ExprKind::Unary(op, op_span, inner) => {
                self.expr(inner)?;
                self.emit(Op::Unary(*op), *op_span);
            }
            ExprKind::Binary(op @ (BinOp::And | BinOp::Or), op_span, left, right) => {
                self.expr(left)?;
                let end = self.label();
                let jump = if *op == BinOp::And {
                    Op::JumpIfFalseOrPop(end)
                } else {
                    Op::JumpIfTrueOrPop(end)
                };
                self.emit(jump, *op_span);
                self.expr(right)?;
                self.emit(Op::ToBool, *op_span);
                self.bind(end);
            }
            ExprKind::Binary(op, op_span, left, right) => {
                self.expr(left)?;
                self.expr(right)?;
                self.emit(Op::Binary(*op), *op_span);
            }
            ExprKind::Compare(values, ops) => {
                self.expr(&values[0])?;
                let end = (ops.len() > 1).then(|| self.label());
                for (index, (value, (op, op_span))) in values[1..].iter().zip(ops).enumerate() {
                    self.expr(value)?;
                    match end {
                        Some(end) if index + 1 < ops.len() => {
                            self.emit(Op::CompareOrFalse(*op, end), *op_span);
                        }
                        _ => self.emit(Op::Compare(*op), *op_span),
                    }
                }
                if let Some(end) = end {
                    self.bind(end);
                }
            }
            ExprKind::Choice(parts) => {
                let [test, chosen, other] = &**parts;
                self.expr(test)?;
                let (otherwise, end) = (self.label(), self.label());
                self.emit(Op::JumpIfFalse(otherwise), test.span);
                self.expr(chosen)?;
                self.emit(Op::Jump(end), span);
                self.bind(otherwise);
                self.expr(other)?;
                self.bind(end);
            }
            ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => {
                let set = matches!(expr.kind, ExprKind::Set(_));
                for item in items {
                    self.expr(item)?;
                    if set {
                        self.emit(Op::CheckKey, item.span);
                    }
                }
                let count = items.len() as u32;
                self.emit(
                    match expr.kind {
                        ExprKind::Tuple(_) => Op::BuildTuple(count),
                        ExprKind::Set(_) => Op::BuildSet(count),
                        _ => Op::BuildList(count),
                    },
                    span,
                );
            }
            ExprKind::Dict(entries) => {
                for (key, value) in entries {
                    self.expr(key)?;
                    self.emit(Op::CheckKey, key.span);
                    self.expr(value)?;
                }
                self.emit(Op::BuildDict(entries.len() as u32), span);
            }
            ExprKind::Index(owner, index, op_span) => {
                self.expr(owner)?;
                self.expr(index)?;
                self.emit(Op::Index, *op_span);
            }
            ExprKind::Slice(owner, bounds, op_span) => {
                self.expr(owner)?;
                for bound in bounds {
                    match bound {
                        Some(bound) => self.expr(bound)?,
                        None => self.emit_constant(Constant::None, *op_span),
                    }
                }
                self.emit(Op::Slice, *op_span);
            }
            ExprKind::Attribute(owner, name) => {
                self.expr(owner)?;
                let name = self.name_index(name);
                self.emit(Op::Attribute(name), expr.name_span);
            }
            ExprKind::Either(first, second) => {
                let (ExprKind::Attribute(owner, name), ExprKind::Attribute(_, other)) =
                    (&first.kind, &second.kind)
                else {
                    return Err(internal(span));
                };
                self.expr(owner)?;
                let names = (name.as_str().into(), other.as_str().into());
                let index = push_index(&mut self.unit().either_names, names);
                self.emit(Op::AttributeEither(index), second.name_span);
            }
            ExprKind::Chain(chain) => self.chain(chain)?,
            ExprKind::Record(literal) => {
                self.name_op(NameOp::Load, &literal.type_name, literal.type_span)?;
                for (_, _, value) in &literal.fields {
                    self.expr(value)?;
                }
                let literal = RecordLiteral {
                    type_name: literal.type_name.as_str().into(),
                    type_span: literal.type_span,
                    fields: literal
                        .fields
                        .iter()
                        .map(|(name, span, _)| (name.as_str().into(), *span))
                        .collect(),
                };
                let index = push_index(&mut self.unit().record_literals, literal);
                self.emit(Op::BuildRecord(index), span);
            }
            ExprKind::Collect(collect) => self.collect(collect, span)?,
            ExprKind::Range(first, last, down) => {
                self.expr(first)?;
                self.expr(last)?;
                self.emit(Op::BuildRange(*down), span);
            }
            ExprKind::Template(parts) => {
                let mut pieces = Vec::with_capacity(parts.len());
                for part in parts {
                    match part {
                        Part::Text(text) => pieces.push(TemplatePart::Text(text.as_str().into())),
                        Part::Value(value, spec) => {
                            self.expr(value)?;
                            pieces.push(TemplatePart::Value(spec.as_deref().map(Box::from)));
                        }
                    }
                }
                let index = push_index(&mut self.unit().templates, pieces.into());
                self.emit(Op::BuildTemplate(index), span);
            }
            ExprKind::Number(_) => return Err(internal(span)),
        }
        Ok(())
    }

    fn parallel_block(&mut self, body: &'a [Statement], span: Span) -> Result<()> {
        let mut defined = Vec::with_capacity(body.len());
        for statement in body {
            let names: Vec<(&'a str, Span)> = statement
                .declarations()
                .into_iter()
                .map(|declared| (declared.name, declared.span))
                .collect();
            let task = self.closure("병렬로".into(), Box::default(), |compiler| {
                compiler.scopes.push(Scope::default());
                for &(name, _) in &names {
                    compiler.declare(name, false);
                }
                compiler.statement(statement)?;
                for &(name, name_span) in &names {
                    compiler.name_op(NameOp::Load, name, name_span)?;
                }
                compiler.emit(Op::BuildTuple(names.len() as u32), statement.span);
                compiler.emit(Op::Return, statement.span);
                compiler.scopes.pop();
                Ok(())
            })?;
            self.emit(Op::MakeFunction(task), statement.span);
            defined.push(names);
        }
        self.emit(Op::ParallelBlock(body.len() as u32), span);
        for (task, names) in defined.iter().enumerate() {
            for (position, &(name, name_span)) in names.iter().enumerate() {
                self.emit(Op::Dup, name_span);
                self.emit_constant(Constant::Int(Int::Small(task as i64)), name_span);
                self.emit(Op::Index, name_span);
                self.emit_constant(Constant::Int(Int::Small(position as i64)), name_span);
                self.emit(Op::Index, name_span);
                self.name_op(NameOp::Store, name, name_span)?;
            }
        }
        self.emit(Op::Pop, span);
        Ok(())
    }

    fn collect(&mut self, collect: &'a Collect, span: Span) -> Result<()> {
        self.expr(&collect.collection)?;
        let names = [(collect.name.as_str(), collect.name_span)];
        if let Some((label, op)) = match collect.concurrency {
            Concurrency::Sequential => None,
            Concurrency::Together => Some(("함께", Op::TogetherCollect)),
            Concurrency::Parallel => Some(("병렬로", Op::ParallelCollect)),
        } {
            let (Some(value), None) = (&collect.value, &collect.filter) else {
                return Err(internal(span));
            };
            let flow = self.closure(label.into(), Box::default(), |compiler| {
                compiler.scopes.push(Scope::default());
                let id = compiler.declare(&collect.name, false);
                compiler.decl_op(NameOp::Parameter, id, collect.name_span);
                compiler.expr(value)?;
                compiler.emit(Op::Return, value.span);
                compiler.scopes.pop();
                Ok(())
            })?;
            self.emit(Op::MakeFunction(flow), span);
            self.emit(op, collect.collection.span);
            return Ok(());
        }
        self.emit(Op::GetIter, collect.collection.span);
        self.emit(Op::BuildList(0), span);
        self.unit().iters += 1;
        self.scopes.push(Scope::default());
        let (top, end) = (self.label(), self.label());
        self.bind(top);
        self.emit(Op::ForIter(end), collect.name_span);
        let id = self.declare(names[0].0, false);
        self.decl_op(NameOp::ClearBound, id, collect.name_span);
        self.decl_op(NameOp::Store, id, collect.name_span);
        if let Some(filter) = &collect.filter {
            self.expr(filter)?;
            self.emit(Op::JumpIfFalse(top), filter.span);
        }
        let put = match &collect.value {
            Some(value) => {
                self.expr(value)?;
                value.span
            }
            None => {
                self.decl_op(NameOp::Load, id, collect.name_span);
                collect.name_span
            }
        };
        self.emit(Op::ListAppend, put);
        self.emit(Op::Jump(top), span);
        self.bind(end);
        self.scopes.pop();
        self.unit().iters -= 1;
        Ok(())
    }
}

fn push_index<T>(items: &mut Vec<T>, item: T) -> u32 {
    items.push(item);
    items.len() as u32 - 1
}

fn closure_captures(
    closures: Vec<(Rc<FunctionInfo>, Vec<usize>)>,
    cells: &HashMap<usize, u32>,
) -> Result<Box<[Closure]>> {
    closures
        .into_iter()
        .map(|(info, frees)| {
            let captures = frees
                .iter()
                .map(|id| {
                    cells
                        .get(id)
                        .copied()
                        .ok_or_else(|| internal(Span::default()))
                })
                .collect::<Result<_>>()?;
            Ok(Closure { info, captures })
        })
        .collect()
}

fn fuse_compare_jumps(ops: &mut [Op], labels: &[Option<u32>]) {
    let mut targets = vec![false; ops.len() + 1];
    for label in labels.iter().flatten() {
        targets[*label as usize] = true;
    }
    for at in 1..ops.len() {
        if let (Op::Compare(compare), Op::JumpIfFalse(label)) = (ops[at - 1], ops[at]) {
            if !targets[at] {
                ops[at - 1] = Op::CompareJump(compare, label);
                ops[at] = Op::Nop;
            }
        }
    }
}

fn relocate_labels(ops: &[Op], labels: &[Option<u32>]) -> Result<(Vec<u32>, usize)> {
    let mut position = Vec::with_capacity(ops.len() + 1);
    let mut kept = 0u32;
    for op in ops {
        position.push(kept);
        if *op != Op::Nop {
            kept += 1;
        }
    }
    position.push(kept);
    let labels: Vec<u32> = labels
        .iter()
        .map(|label| label.map(|at| position[at as usize]))
        .collect::<Option<_>>()
        .ok_or_else(|| internal(Span::default()))?;
    Ok((labels, kept as usize))
}

fn remove_nops(
    ops: Vec<Op>,
    spans: Vec<Span>,
    handler_of: Vec<Option<u32>>,
    labels: &[u32],
    kept: usize,
) -> (Vec<Op>, Vec<Span>, Vec<Option<u32>>) {
    let mut kept_ops = Vec::with_capacity(kept);
    let mut kept_spans = Vec::with_capacity(kept);
    let mut kept_handlers = Vec::with_capacity(kept);
    for ((op, span), handler) in ops.into_iter().zip(spans).zip(handler_of) {
        let op = match op {
            Op::Nop => continue,
            Op::Jump(l) => Op::Jump(labels[l as usize]),
            Op::JumpIfFalse(l) => Op::JumpIfFalse(labels[l as usize]),
            Op::JumpIfFalseOrPop(l) => Op::JumpIfFalseOrPop(labels[l as usize]),
            Op::JumpIfTrueOrPop(l) => Op::JumpIfTrueOrPop(labels[l as usize]),
            Op::CompareOrFalse(c, l) => Op::CompareOrFalse(c, labels[l as usize]),
            Op::CompareJump(c, l) => Op::CompareJump(c, labels[l as usize]),
            Op::ForIter(l) => Op::ForIter(labels[l as usize]),
            op => op,
        };
        kept_ops.push(op);
        kept_spans.push(span);
        kept_handlers.push(handler);
    }
    (kept_ops, kept_spans, kept_handlers)
}

fn handler_table(
    handler_of: &[Option<u32>],
    definitions: &[HandlerDef],
    labels: &[u32],
) -> Vec<Handler> {
    let mut handlers: Vec<Handler> = Vec::new();
    for (at, handler) in handler_of.iter().enumerate() {
        let Some(id) = handler else { continue };
        let definition = &definitions[*id as usize];
        let target = labels[definition.target as usize];
        match handlers.last_mut() {
            Some(last) if last.end == at as u32 && last.target == target => last.end += 1,
            _ => handlers.push(Handler {
                start: at as u32,
                end: at as u32 + 1,
                target,
                depth: definition.depth,
                iters: definition.iters,
                unwinds: definition.unwinds,
            }),
        }
    }
    handlers
}

fn next_version() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[derive(PartialEq, Eq, Hash)]
enum ConstantKey {
    Int(Int),
    Float(u64),
    Bool(bool),
    Text(Rc<str>),
    None,
    Native(Native),
}

impl ConstantKey {
    fn of(value: &Constant) -> Self {
        match value {
            Constant::Int(n) => Self::Int(n.clone()),
            Constant::Float(f) => Self::Float(f.to_bits()),
            Constant::Bool(b) => Self::Bool(*b),
            Constant::Text(text) => Self::Text(text.clone()),
            Constant::None => Self::None,
            Constant::Native(native) => Self::Native(*native),
        }
    }
}

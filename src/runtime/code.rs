use std::cell::{Cell, OnceCell};
use std::rc::Rc;

use super::value::{Constant, Value};
use crate::check::Mapping;
use crate::diagnostic::Span;
use crate::stdlib::Native;
use crate::syntax::ast::{BinOp, CmpOp, Role, SignatureParameter, UnaryOp};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Op {
    Nop,
    Const(u32),
    LoadFast(u32),
    StoreFast(u32),
    ClearFast(u32),
    CheckFast(u32),
    LoadCell(u32),
    StoreCell(u32),
    MakeCell(u32),
    CheckCell(u32),
    CellFromFast { cell: u16, slot: u16 },
    LoadGlobal(u32),
    StoreGlobal(u32),
    CheckGlobal(u32),
    LoadBuiltin(u32),
    Name(NameOp, u32),
    Pop,
    Dup,
    Dup2,
    Unary(UnaryOp),
    Binary(BinOp),
    Compare(CmpOp),
    CompareJump(CmpOp, u32),
    CompareOrFalse(CmpOp, u32),
    ToBool,
    JumpIfFalseOrPop(u32),
    JumpIfTrueOrPop(u32),
    Jump(u32),
    JumpIfFalse(u32),
    Adjust(BinOp),
    BuildList(u32),
    BuildTuple(u32),
    BuildSet(u32),
    BuildDict(u32),
    CheckKey,
    BuildRange(bool),
    BuildTemplate(u32),
    BuildRecord(u32),
    Index,
    Slice,
    Attribute(u32),
    AttributeEither(u32),
    PrepareIndex,
    StoreIndex,
    PrepareAttribute(u32),
    StoreAttribute(u32),
    GetIter,
    ForIter(u32),
    PopIter,
    GetCount(bool, u32),
    DictItems,
    Unpack2,
    ListAppend,
    MakeFunction(u32),
    MakeRecordType(u32),
    Call(u32),
    Return,
    ReturnNone,
    Throw,
    Rethrow,
    Exit,
    ExitZero,
    Catch,
    PopUnwind,
    Reraise,
    Together(u32),
    TogetherEach,
    TogetherCollect,
    ParallelEach,
    ParallelCollect,
    ParallelBlock(u32),
    Display,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NameOp {
    Load,
    Store,
    Check,
    Clear,
    ClearFirst,
    ClearBound,
    Parameter,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Handler {
    pub start: u32,
    pub end: u32,
    pub target: u32,
    pub depth: u32,
    pub iters: u32,
    pub unwinds: u32,
}

#[derive(Debug)]
pub(crate) struct CallSite {
    pub span: Span,
    pub clause: Option<usize>,
    pub arguments: Box<[(Role, Span)]>,
    pub values: Box<[Span]>,
    pub incoming: bool,
    pub cache: Cell<Option<CallCache>>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CallCache {
    pub callee: Callee,
    pub mapping: Mapping,
    pub exact: bool,
    pub checked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Callee {
    Function(u64),
    Native(Native),
}

#[derive(Clone, Debug)]
pub(crate) enum TemplatePart {
    Text(Box<str>),
    Value(Option<Box<str>>),
}

#[derive(Debug)]
pub(crate) struct RecordLiteral {
    pub type_name: Rc<str>,
    pub type_span: Span,
    pub fields: Box<[(Rc<str>, Span)]>,
}

#[derive(Clone, Debug)]
pub(crate) struct RecordInfo {
    pub name: String,
    pub fields: Vec<RecordField>,
}

#[derive(Clone, Debug)]
pub(crate) struct RecordField {
    pub name: String,
    pub has_default: bool,
}

#[derive(Debug)]
pub(crate) struct FunctionInfo {
    pub name: Rc<str>,
    pub signature: Box<[SignatureParameter]>,
    pub code: Rc<Code>,
    /// 정의마다 달라서 캐시가 함수를 붙들지 않고도 구별할 수 있습니다.
    pub version: u64,
}

#[derive(Debug)]
pub(crate) struct Closure {
    pub info: Rc<FunctionInfo>,
    pub captures: Box<[u32]>,
}

fn constant_text(value: &Constant) -> String {
    match value {
        Constant::Int(n) => n.to_string(),
        Constant::Float(f) => super::render::render_float(*f),
        Constant::Bool(true) => "참".to_owned(),
        Constant::Bool(false) => "거짓".to_owned(),
        Constant::Text(text) => format!("{text:?}"),
        Constant::None => "없음".to_owned(),
        Constant::Native(native) => native.name().to_owned(),
    }
}

#[derive(Debug, Default)]
pub(crate) struct Code {
    pub ops: Box<[Op]>,
    pub spans: Box<[Span]>,
    pub handlers: Box<[Handler]>,
    pub constants: Box<[Constant]>,
    pub constant_values: OnceCell<Box<[Value]>>,
    pub names: Box<[Rc<str>]>,
    pub either_names: Box<[(Rc<str>, Rc<str>)]>,
    pub locals: u32,
    pub local_names: Box<[Rc<str>]>,
    pub cells: u32,
    pub cell_names: Box<[Rc<str>]>,
    pub closures: Box<[Closure]>,
    pub record_types: Box<[Rc<RecordInfo>]>,
    pub calls: Box<[CallSite]>,
    pub templates: Box<[Box<[TemplatePart]>]>,
    pub record_literals: Box<[RecordLiteral]>,
    pub count_end_spans: Box<[Span]>,
}

impl Code {
    pub fn disassemble(&self, name: &str, line: &dyn Fn(Span) -> usize, out: &mut String) {
        use std::fmt::Write;
        let _ = writeln!(out, "{name}:");
        let mut last = 0;
        for (at, (op, span)) in self.ops.iter().zip(&self.spans).enumerate() {
            let here = line(*span);
            let shown = if here == last {
                String::new()
            } else {
                here.to_string()
            };
            last = here;
            let detail = match *op {
                Op::Const(i) => format!("  ({})", constant_text(&self.constants[i as usize])),
                Op::LoadFast(i) | Op::StoreFast(i) | Op::ClearFast(i) | Op::CheckFast(i) => {
                    format!("  ({})", self.local_names[i as usize])
                }
                Op::LoadCell(i) | Op::StoreCell(i) | Op::MakeCell(i) | Op::CheckCell(i) => {
                    format!("  ({})", self.cell_names[i as usize])
                }
                Op::Attribute(i) | Op::PrepareAttribute(i) | Op::StoreAttribute(i) => {
                    format!("  ({})", self.names[i as usize])
                }
                Op::MakeFunction(i) => format!("  ({})", self.closures[i as usize].info.name),
                _ => String::new(),
            };
            let _ = writeln!(out, "{shown:>5} {at:>5}  {op:?}{detail}");
        }
        for handler in &self.handlers {
            let _ = writeln!(
                out,
                "  처리: {}..{} -> {} (값 {}, 반복자 {}, 되감기 {})",
                handler.start,
                handler.end,
                handler.target,
                handler.depth,
                handler.iters,
                handler.unwinds
            );
        }
        for closure in &self.closures {
            out.push('\n');
            closure.info.code.disassemble(&closure.info.name, line, out);
        }
    }

    pub fn handler(&self, at: usize) -> Option<&Handler> {
        let at = at as u32;
        let index = self.handlers.partition_point(|h| h.end <= at);
        self.handlers.get(index).filter(|h| h.start <= at)
    }
}

use std::cell::{Cell, OnceCell};
use std::collections::HashMap;
use std::rc::Rc;

use num_bigint::BigInt;

use super::code::{
    CallSite, Closure, Code, FunctionInfo, Handler, Op, RecordInfo, RecordLiteral, TemplatePart,
};
use super::heap::{Handle, Heap, Object};
use super::objects::CollectionKind;
use super::value::{Constant, Int, Value};
use crate::diagnostic::{Diagnostic, ExecutionError, Result, Span, internal};
use crate::stdlib::Native;
use crate::syntax::ast::{Role, SignatureParameter};

#[derive(Clone)]
enum Portable {
    Int(i64),
    Big(Box<BigInt>),
    Float(f64),
    Bool(bool),
    Text(Box<str>),
    None,
    Native(Native),
    Object(u32),
}

enum Shape {
    Cell(Option<Portable>),
    Globals {
        names: Box<[Box<str>]>,
        values: Vec<Option<Portable>>,
    },
    Function {
        info: u32,
        defaults: Vec<Portable>,
        captures: Vec<u32>,
        globals: u32,
    },
    Error {
        report: Box<ExecutionError>,
        cause: Option<u32>,
    },
    List(Vec<Portable>),
    Tuple(Vec<Portable>),
    Set(Vec<Portable>),
    Dict(Vec<(Portable, Portable)>),
    RecordType {
        info: u32,
        defaults: Vec<Portable>,
    },
    Record {
        record_type: u32,
        values: Vec<Portable>,
    },
}

struct FunctionShape {
    name: Box<str>,
    signature: Box<[SignatureParameter]>,
    code: u32,
    version: u64,
}

struct CallShape {
    span: Span,
    clause: Option<usize>,
    arguments: Box<[(Role, Span)]>,
    values: Box<[Span]>,
    incoming: bool,
}

struct LiteralShape {
    type_name: Box<str>,
    type_span: Span,
    fields: Box<[(Box<str>, Span)]>,
}

struct CodeShape {
    ops: Box<[Op]>,
    spans: Box<[Span]>,
    handlers: Box<[Handler]>,
    constants: Box<[Portable]>,
    names: Box<[Box<str>]>,
    either_names: Box<[(Box<str>, Box<str>)]>,
    locals: u32,
    local_names: Box<[Box<str>]>,
    cells: u32,
    cell_names: Box<[Box<str>]>,
    closures: Box<[(u32, Box<[u32]>)]>,
    record_types: Box<[u32]>,
    calls: Box<[CallShape]>,
    templates: Box<[Box<[TemplatePart]>]>,
    record_literals: Box<[LiteralShape]>,
    count_end_spans: Box<[Span]>,
}

pub(super) struct Package {
    roots: Vec<Portable>,
    objects: Vec<Shape>,
    functions: Vec<FunctionShape>,
    codes: Vec<CodeShape>,
    records: Vec<RecordInfo>,
}

fn boxed_names(names: &[Rc<str>]) -> Box<[Box<str>]> {
    names.iter().map(|name| Box::from(&**name)).collect()
}

fn shared_names(names: &[Box<str>]) -> Box<[Rc<str>]> {
    names.iter().map(|name| Rc::from(&**name)).collect()
}

pub(super) fn export(heap: &Heap, roots: &[Value], span: Span) -> Result<Package> {
    let mut exporter = Exporter {
        heap,
        span,
        package: Package {
            roots: Vec::with_capacity(roots.len()),
            objects: Vec::new(),
            functions: Vec::new(),
            codes: Vec::new(),
            records: Vec::new(),
        },
        objects: HashMap::new(),
        pending: Vec::new(),
        functions: HashMap::new(),
        codes: HashMap::new(),
        records: HashMap::new(),
        used: HashMap::new(),
    };
    for root in roots {
        let portable = exporter.value(root)?;
        exporter.package.roots.push(portable);
    }
    while let Some(handle) = exporter.pending.pop() {
        exporter.shape(handle)?;
    }
    Ok(exporter.package)
}

struct Exporter<'h> {
    heap: &'h Heap,
    span: Span,
    package: Package,
    objects: HashMap<Handle, u32>,
    pending: Vec<Handle>,
    functions: HashMap<*const FunctionInfo, u32>,
    codes: HashMap<*const Code, u32>,
    records: HashMap<*const RecordInfo, u32>,
    used: HashMap<*const Code, Rc<[u32]>>,
}

impl Exporter<'_> {
    fn value(&mut self, value: &Value) -> Result<Portable> {
        Ok(match *value {
            Value::Int(n) => Portable::Int(n),
            Value::Big(_) => match self.heap.int(value) {
                Some(Int::Big(big)) => Portable::Big(Box::new((*big).clone())),
                _ => return Err(internal(self.span)),
            },
            Value::Float(f) => Portable::Float(f),
            Value::Bool(b) => Portable::Bool(b),
            Value::Text(_) => Portable::Text(Box::from(&*self.heap.text_of(value, self.span)?)),
            Value::None => Portable::None,
            Value::Native(native) => Portable::Native(native),
            Value::Function(handle)
            | Value::List(handle)
            | Value::Tuple(handle)
            | Value::Set(handle)
            | Value::Dict(handle)
            | Value::Error(handle)
            | Value::RecordType(handle)
            | Value::Record(handle) => Portable::Object(self.object(handle)?),
            Value::Python(_) => {
                return Err(Diagnostic::new(
                    "E-PAR-002",
                    self.span,
                    "파이썬 값은 병렬 작업에 넘길 수 없습니다.",
                ));
            }
        })
    }

    fn values(&mut self, values: &[Value]) -> Result<Vec<Portable>> {
        values.iter().map(|value| self.value(value)).collect()
    }

    fn object(&mut self, handle: Handle) -> Result<u32> {
        if let Some(&index) = self.objects.get(&handle) {
            return Ok(index);
        }
        let index = self.package.objects.len() as u32;
        let shape = match self.heap.object(handle, self.span)? {
            Object::Globals { names, .. } => Shape::Globals {
                names: boxed_names(names),
                values: vec![None; names.len()],
            },
            _ => {
                self.pending.push(handle);
                Shape::Cell(None)
            }
        };
        self.package.objects.push(shape);
        self.objects.insert(handle, index);
        Ok(index)
    }

    fn shape(&mut self, handle: Handle) -> Result<()> {
        let heap = self.heap;
        let index = self.objects[&handle];
        let shape = match heap.object(handle, self.span)? {
            Object::Cell(value) => Shape::Cell(value.as_ref().map(|v| self.value(v)).transpose()?),
            Object::Function {
                info,
                defaults,
                captures,
                globals,
            } => {
                let info_index = self.function(info);
                let globals_index = self.object(*globals)?;
                for &slot in self.used(&info.code).iter() {
                    self.global(*globals, globals_index, slot)?;
                }
                Shape::Function {
                    info: info_index,
                    defaults: self.values(defaults)?,
                    captures: captures
                        .iter()
                        .map(|&cell| self.object(cell))
                        .collect::<Result<_>>()?,
                    globals: globals_index,
                }
            }
            Object::Error { report, cause, .. } => Shape::Error {
                report: report.clone(),
                cause: cause.map(|cause| self.object(cause)).transpose()?,
            },
            Object::List(values) => Shape::List(self.values(values)?),
            Object::Tuple(values) => Shape::Tuple(self.values(values)?),
            Object::Set(set) => Shape::Set(
                set.iter()
                    .map(|entry| self.value(&entry.original))
                    .collect::<Result<_>>()?,
            ),
            Object::Dict(dictionary) => Shape::Dict(
                dictionary
                    .iter()
                    .map(|entry| Ok((self.value(&entry.original)?, self.value(&entry.value)?)))
                    .collect::<Result<_>>()?,
            ),
            Object::RecordType { info, defaults } => Shape::RecordType {
                info: self.record(info),
                defaults: self.values(defaults)?,
            },
            Object::Record {
                record_type,
                values,
            } => Shape::Record {
                record_type: self.object(*record_type)?,
                values: self.values(values)?,
            },
            Object::Globals { .. } | Object::Text(_) | Object::Big(_) | Object::Python(_) => {
                return Err(internal(self.span));
            }
        };
        self.package.objects[index as usize] = shape;
        Ok(())
    }

    fn global(&mut self, globals: Handle, index: u32, slot: u32) -> Result<()> {
        let Shape::Globals { values, .. } = &self.package.objects[index as usize] else {
            return Err(internal(self.span));
        };
        if values.get(slot as usize).is_none_or(Option::is_some) {
            return Ok(());
        }
        if let Ok(value) = self.heap.global(globals, slot, self.span)? {
            let portable = self.value(&value)?;
            if let Shape::Globals { values, .. } = &mut self.package.objects[index as usize] {
                values[slot as usize] = Some(portable);
            }
        }
        Ok(())
    }

    fn used(&mut self, code: &Rc<Code>) -> Rc<[u32]> {
        if let Some(used) = self.used.get(&Rc::as_ptr(code)) {
            return used.clone();
        }
        let mut used: Vec<u32> = code
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::LoadGlobal(i) | Op::StoreGlobal(i) | Op::CheckGlobal(i) => Some(*i),
                _ => None,
            })
            .collect();
        for closure in &code.closures {
            used.extend(self.used(&closure.info.code).iter());
        }
        used.sort_unstable();
        used.dedup();
        let used: Rc<[u32]> = used.into();
        self.used.insert(Rc::as_ptr(code), used.clone());
        used
    }

    fn function(&mut self, info: &Rc<FunctionInfo>) -> u32 {
        if let Some(&index) = self.functions.get(&Rc::as_ptr(info)) {
            return index;
        }
        let code = self.code(&info.code);
        let index = self.package.functions.len() as u32;
        self.package.functions.push(FunctionShape {
            name: Box::from(&*info.name),
            signature: info.signature.clone(),
            code,
            version: info.version,
        });
        self.functions.insert(Rc::as_ptr(info), index);
        index
    }

    fn record(&mut self, info: &Rc<RecordInfo>) -> u32 {
        if let Some(&index) = self.records.get(&Rc::as_ptr(info)) {
            return index;
        }
        let index = self.package.records.len() as u32;
        self.package.records.push((**info).clone());
        self.records.insert(Rc::as_ptr(info), index);
        index
    }

    fn code(&mut self, code: &Rc<Code>) -> u32 {
        if let Some(&index) = self.codes.get(&Rc::as_ptr(code)) {
            return index;
        }
        let closures = code
            .closures
            .iter()
            .map(|closure| (self.function(&closure.info), closure.captures.clone()))
            .collect();
        let record_types = code
            .record_types
            .iter()
            .map(|info| self.record(info))
            .collect();
        let shape = CodeShape {
            ops: code.ops.clone(),
            spans: code.spans.clone(),
            handlers: code.handlers.clone(),
            constants: code.constants.iter().map(portable_constant).collect(),
            names: boxed_names(&code.names),
            either_names: code
                .either_names
                .iter()
                .map(|(a, b)| (Box::from(&**a), Box::from(&**b)))
                .collect(),
            locals: code.locals,
            local_names: boxed_names(&code.local_names),
            cells: code.cells,
            cell_names: boxed_names(&code.cell_names),
            closures,
            record_types,
            calls: code
                .calls
                .iter()
                .map(|call| CallShape {
                    span: call.span,
                    clause: call.clause,
                    arguments: call.arguments.clone(),
                    values: call.values.clone(),
                    incoming: call.incoming,
                })
                .collect(),
            templates: code.templates.clone(),
            record_literals: code
                .record_literals
                .iter()
                .map(|literal| LiteralShape {
                    type_name: Box::from(&*literal.type_name),
                    type_span: literal.type_span,
                    fields: literal
                        .fields
                        .iter()
                        .map(|(name, span)| (Box::from(&**name), *span))
                        .collect(),
                })
                .collect(),
            count_end_spans: code.count_end_spans.clone(),
        };
        let index = self.package.codes.len() as u32;
        self.package.codes.push(shape);
        self.codes.insert(Rc::as_ptr(code), index);
        index
    }
}

fn portable_constant(constant: &Constant) -> Portable {
    match constant {
        Constant::Int(Int::Small(n)) => Portable::Int(*n),
        Constant::Int(Int::Big(big)) => Portable::Big(Box::new((**big).clone())),
        Constant::Float(f) => Portable::Float(*f),
        Constant::Bool(b) => Portable::Bool(*b),
        Constant::Text(text) => Portable::Text(Box::from(&**text)),
        Constant::None => Portable::None,
        Constant::Native(native) => Portable::Native(*native),
    }
}

pub(super) struct Imported {
    pub values: Vec<Value>,
    pub codes: Vec<Rc<Code>>,
}

pub(super) fn import(
    heap: &mut Heap,
    package: &Package,
    roots: impl Iterator<Item = Handle> + Clone,
    frozen: bool,
    span: Span,
) -> Result<Imported> {
    let mut importer = Importer {
        package,
        span,
        records: package
            .records
            .iter()
            .map(|info| Rc::new(info.clone()))
            .collect(),
        functions: vec![None; package.functions.len()],
        codes: vec![None; package.codes.len()],
        handles: vec![None; package.objects.len()],
    };
    importer.shells(heap, roots.clone())?;
    importer.fill(heap, roots.clone())?;
    let handles: Vec<Handle> = importer.handles.iter().flatten().copied().collect();
    if frozen {
        for &handle in &handles {
            heap.freeze(handle, span)?;
        }
    }
    let mut values = Vec::with_capacity(package.roots.len());
    for root in &package.roots {
        let live = roots
            .clone()
            .chain(handles.iter().copied())
            .chain(values.iter().filter_map(Value::handle));
        let value = importer.value(heap, root, live)?;
        values.push(value);
    }
    let codes = (0..package.codes.len())
        .map(|index| importer.code(index as u32))
        .collect::<Result<_>>()?;
    Ok(Imported { values, codes })
}

struct Importer<'p> {
    package: &'p Package,
    span: Span,
    records: Vec<Rc<RecordInfo>>,
    functions: Vec<Option<Rc<FunctionInfo>>>,
    codes: Vec<Option<Rc<Code>>>,
    handles: Vec<Option<Handle>>,
}

impl Importer<'_> {
    fn handle(&self, index: u32) -> Result<Handle> {
        self.handles
            .get(index as usize)
            .copied()
            .flatten()
            .ok_or_else(|| internal(self.span))
    }

    fn record(&self, index: u32) -> Result<Rc<RecordInfo>> {
        self.records
            .get(index as usize)
            .cloned()
            .ok_or_else(|| internal(self.span))
    }

    fn function(&mut self, index: u32) -> Result<Rc<FunctionInfo>> {
        if let Some(Some(info)) = self.functions.get(index as usize) {
            return Ok(info.clone());
        }
        let package = self.package;
        let shape = package
            .functions
            .get(index as usize)
            .ok_or_else(|| internal(self.span))?;
        let info = Rc::new(FunctionInfo {
            name: Rc::from(&*shape.name),
            signature: shape.signature.clone(),
            code: self.code(shape.code)?,
            version: shape.version,
        });
        self.functions[index as usize] = Some(info.clone());
        Ok(info)
    }

    fn code(&mut self, index: u32) -> Result<Rc<Code>> {
        if let Some(Some(code)) = self.codes.get(index as usize) {
            return Ok(code.clone());
        }
        let package = self.package;
        let shape = package
            .codes
            .get(index as usize)
            .ok_or_else(|| internal(self.span))?;
        let closures = shape
            .closures
            .iter()
            .map(|(function, captures)| {
                Ok(Closure {
                    info: self.function(*function)?,
                    captures: captures.clone(),
                })
            })
            .collect::<Result<_>>()?;
        let code = Rc::new(Code {
            ops: shape.ops.clone(),
            spans: shape.spans.clone(),
            handlers: shape.handlers.clone(),
            constants: shape
                .constants
                .iter()
                .map(|portable| self.imported_constant(portable))
                .collect::<Result<_>>()?,
            constant_values: OnceCell::new(),
            names: shared_names(&shape.names),
            either_names: shape
                .either_names
                .iter()
                .map(|(a, b)| (Rc::from(&**a), Rc::from(&**b)))
                .collect(),
            locals: shape.locals,
            local_names: shared_names(&shape.local_names),
            cells: shape.cells,
            cell_names: shared_names(&shape.cell_names),
            closures,
            record_types: shape
                .record_types
                .iter()
                .map(|&index| self.record(index))
                .collect::<Result<_>>()?,
            calls: shape
                .calls
                .iter()
                .map(|call| CallSite {
                    span: call.span,
                    clause: call.clause,
                    arguments: call.arguments.clone(),
                    values: call.values.clone(),
                    incoming: call.incoming,
                    cache: Cell::new(None),
                })
                .collect(),
            templates: shape.templates.clone(),
            record_literals: shape
                .record_literals
                .iter()
                .map(|literal| RecordLiteral {
                    type_name: Rc::from(&*literal.type_name),
                    type_span: literal.type_span,
                    fields: literal
                        .fields
                        .iter()
                        .map(|(name, span)| (Rc::from(&**name), *span))
                        .collect(),
                })
                .collect(),
            count_end_spans: shape.count_end_spans.clone(),
        });
        self.codes[index as usize] = Some(code.clone());
        Ok(code)
    }

    fn imported_constant(&self, portable: &Portable) -> Result<Constant> {
        Ok(match portable {
            Portable::Int(n) => Constant::Int(Int::Small(*n)),
            Portable::Big(big) => Constant::Int(Int::Big(Rc::new((**big).clone()))),
            Portable::Float(f) => Constant::Float(*f),
            Portable::Bool(b) => Constant::Bool(*b),
            Portable::Text(text) => Constant::Text(Rc::from(&**text)),
            Portable::None => Constant::None,
            Portable::Native(native) => Constant::Native(*native),
            Portable::Object(_) => return Err(internal(self.span)),
        })
    }

    fn value(
        &self,
        heap: &mut Heap,
        portable: &Portable,
        roots: impl Iterator<Item = Handle>,
    ) -> Result<Value> {
        let span = self.span;
        Ok(match portable {
            Portable::Int(n) => Value::Int(*n),
            Portable::Big(big) => heap.new_int(Int::Big(Rc::new((**big).clone())), roots, span)?,
            Portable::Float(f) => Value::Float(*f),
            Portable::Bool(b) => Value::Bool(*b),
            Portable::Text(text) => heap.new_text(&**text, roots, span)?,
            Portable::None => Value::None,
            Portable::Native(native) => Value::Native(*native),
            Portable::Object(index) => {
                let handle = self.handle(*index)?;
                match &self.package.objects[*index as usize] {
                    Shape::Function { .. } => Value::Function(handle),
                    Shape::Error { .. } => Value::Error(handle),
                    Shape::List(_) => Value::List(handle),
                    Shape::Tuple(_) => Value::Tuple(handle),
                    Shape::Set(_) => Value::Set(handle),
                    Shape::Dict(_) => Value::Dict(handle),
                    Shape::RecordType { .. } => Value::RecordType(handle),
                    Shape::Record { .. } => Value::Record(handle),
                    Shape::Cell(_) | Shape::Globals { .. } => return Err(internal(span)),
                }
            }
        })
    }

    /// 객체끼리 서로 가리킬 수 있으므로 담는 객체는 빈 껍데기로 먼저 만들고 나중에 채웁니다.
    fn shells(
        &mut self,
        heap: &mut Heap,
        roots: impl Iterator<Item = Handle> + Clone,
    ) -> Result<()> {
        let package = self.package;
        let span = self.span;
        for (index, shape) in package.objects.iter().enumerate() {
            let live = roots.clone().chain(self.handles.iter().flatten().copied());
            let handle = match shape {
                Shape::Cell(_) => heap.new_cell(None, live, span)?,
                Shape::Globals { names, .. } => {
                    heap.new_globals(&shared_names(names), live, span)?
                }
                Shape::List(_) => shell(heap.new_list(Vec::new(), live, span)?, span)?,
                Shape::Tuple(_) => {
                    let tuple = heap.new_collection(CollectionKind::Tuple, Vec::new(), live, span);
                    shell(tuple?, span)?
                }
                Shape::Set(_) => {
                    let set = heap.new_collection(CollectionKind::Set, Vec::new(), live, span);
                    shell(set?, span)?
                }
                Shape::Dict(_) => shell(heap.new_dictionary(Vec::new(), live, span)?, span)?,
                Shape::RecordType { info, .. } => {
                    let record_type =
                        heap.new_record_type(self.record(*info)?, Vec::new(), live, span);
                    shell(record_type?, span)?
                }
                Shape::Function { .. } | Shape::Record { .. } | Shape::Error { .. } => continue,
            };
            self.handles[index] = Some(handle);
        }
        for index in 0..package.objects.len() {
            self.error(heap, index as u32, roots.clone())?;
        }
        for (index, shape) in package.objects.iter().enumerate() {
            let handle = match shape {
                Shape::Function {
                    info,
                    captures,
                    globals,
                    ..
                } => {
                    let info = self.function(*info)?;
                    let captures = captures
                        .iter()
                        .map(|&cell| self.handle(cell))
                        .collect::<Result<_>>()?;
                    let globals = self.handle(*globals)?;
                    let live = roots.clone().chain(self.handles.iter().flatten().copied());
                    let function =
                        heap.new_function(info, Vec::new(), captures, globals, live, span);
                    shell(function?, span)?
                }
                Shape::Record { record_type, .. } => {
                    let live = roots.clone().chain(self.handles.iter().flatten().copied());
                    shell(
                        heap.new_record(self.handle(*record_type)?, Vec::new(), live, span)?,
                        span,
                    )?
                }
                _ => continue,
            };
            self.handles[index] = Some(handle);
        }
        Ok(())
    }

    fn error(
        &mut self,
        heap: &mut Heap,
        index: u32,
        roots: impl Iterator<Item = Handle> + Clone,
    ) -> Result<()> {
        let mut chain = Vec::new();
        let mut next = Some(index);
        while let Some(index) = next {
            let Shape::Error { cause, .. } = &self.package.objects[index as usize] else {
                break;
            };
            if self.handles[index as usize].is_some() {
                break;
            }
            chain.push(index);
            next = *cause;
        }
        for &index in chain.iter().rev() {
            let Shape::Error { report, cause } = &self.package.objects[index as usize] else {
                return Err(internal(self.span));
            };
            let cause = cause.map(|cause| self.handle(cause)).transpose()?;
            let live = roots.clone().chain(self.handles.iter().flatten().copied());
            let handle = heap.new_error(report.clone(), cause, live, self.span)?;
            self.handles[index as usize] = Some(handle);
        }
        Ok(())
    }

    fn fill(&mut self, heap: &mut Heap, roots: impl Iterator<Item = Handle> + Clone) -> Result<()> {
        let package = self.package;
        let span = self.span;
        // 사전의 키가 될 튜플을 먼저 채웁니다.
        for keyed in [false, true] {
            for (index, shape) in package.objects.iter().enumerate() {
                let handle = self.handle(index as u32)?;
                let held = || roots.clone().chain(self.handles.iter().flatten().copied());
                match (keyed, shape) {
                    (false, Shape::Cell(Some(value))) => {
                        let value = self.value(heap, value, held())?;
                        heap.store_cell(handle, value, span)?;
                    }
                    (false, Shape::Globals { values, .. }) => {
                        for (slot, value) in values.iter().enumerate() {
                            if let Some(value) = value {
                                let value = self.value(heap, value, held())?;
                                heap.store_global(handle, slot as u32, value, span)?;
                            }
                        }
                    }
                    (
                        false,
                        Shape::List(values)
                        | Shape::Tuple(values)
                        | Shape::Function {
                            defaults: values, ..
                        }
                        | Shape::RecordType {
                            defaults: values, ..
                        }
                        | Shape::Record { values, .. },
                    ) => {
                        let values = self.values(heap, values, held())?;
                        heap.fill(handle, values, held(), span)?;
                    }
                    (true, Shape::Set(items)) => {
                        for item in items {
                            let item = self.value(heap, item, held())?;
                            let key = heap.key(&item, span)?;
                            let live = held().chain(item.handle());
                            heap.store_key(handle, key, item, Value::None, live, span)?;
                        }
                    }
                    (true, Shape::Dict(entries)) => {
                        for (key, value) in entries {
                            let key = self.value(heap, key, held())?;
                            let value = self.value(heap, value, held().chain(key.handle()))?;
                            let canonical = heap.key(&key, span)?;
                            let live = held().chain(key.handle()).chain(value.handle());
                            heap.store_key(handle, canonical, key, value, live, span)?;
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn values(
        &self,
        heap: &mut Heap,
        portables: &[Portable],
        roots: impl Iterator<Item = Handle> + Clone,
    ) -> Result<Vec<Value>> {
        let mut values = Vec::with_capacity(portables.len());
        for portable in portables {
            let live = roots.clone().chain(values.iter().filter_map(Value::handle));
            let value = self.value(heap, portable, live)?;
            values.push(value);
        }
        Ok(values)
    }
}

fn shell(value: Value, span: Span) -> Result<Handle> {
    value.handle().ok_or_else(|| internal(span))
}

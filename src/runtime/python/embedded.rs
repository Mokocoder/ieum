use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::Once;

use num_bigint::BigInt;
use pyo3::basic::CompareOp;
use pyo3::exceptions::PyImportError;
use pyo3::prelude::*;
use pyo3::types::{
    PyBool, PyDict, PyFloat, PyInt, PyIterator, PyList, PySet, PySlice, PyString, PyTuple,
};
use pyo3::{IntoPyObjectExt, PyTypeInfo};

use crate::diagnostic::{Diagnostic, Result, Span, display_path, internal};
use crate::korean::particle::TOPIC;
use crate::limits::{Limit, MAX_DATA_DEPTH, MAX_INT_BITS, MAX_ITEMS};
use crate::runtime::heap::{Handle, Heap, Object};
use crate::runtime::value::Value;
use crate::syntax::ast::{BinOp, CmpOp, UnaryOp};

pub(crate) struct Foreign(Py<PyAny>);

trait Raised<T> {
    fn raised(self, span: Span) -> Result<T>;
}

impl<T, E: Into<PyErr>> Raised<T> for std::result::Result<T, E> {
    fn raised(self, span: Span) -> Result<T> {
        self.map_err(|error| {
            let error = error.into();
            Python::attach(|py| {
                let kind = error.get_type(py).name().map(|name| name.to_string());
                let message = error.value(py).str().map(|text| text.to_string());
                let text = match (kind, message) {
                    (Ok(kind), Ok(message)) if !message.is_empty() => format!("{kind}: {message}"),
                    (Ok(kind), _) => kind,
                    (Err(_), _) => "파이썬 오류".to_owned(),
                };
                Diagnostic::new("E-PY-002", span, text)
            })
        })
    }
}

fn arguments_error(span: Span) -> Diagnostic {
    Diagnostic::new(
        "E-PY-004",
        span,
        "파이썬 함수에는 위치 인자를 을/를 자리의 목록이나 튜플로, 이름 붙은 인자를 와/과 자리의 사전으로 넘깁니다.",
    )
}

fn attach<T>(span: Span, work: impl FnOnce(Python<'_>) -> Result<T>) -> Result<T> {
    static PREPARED: Once = Once::new();
    Python::attach(|py| {
        let mut prepared = Ok(());
        PREPARED.call_once(|| prepared = prepare(py));
        prepared.raised(span)?;
        work(py)
    })
}

fn prepare(py: Python<'_>) -> PyResult<()> {
    let sys = py.import("sys")?;
    let output = PyDict::new(py);
    output.set_item("encoding", "utf-8")?;
    output.set_item(
        "newline", "
",
    )?;
    for name in ["stdout", "stderr"] {
        let stream = sys.getattr(name)?;
        if stream.hasattr("reconfigure")? {
            stream.call_method("reconfigure", (), Some(&output))?;
        }
    }
    if let Some(environment) = std::env::var_os("VIRTUAL_ENV") {
        let base = PyDict::new(py);
        base.set_item("base", &environment)?;
        base.set_item("platbase", &environment)?;
        let options = PyDict::new(py);
        options.set_item("vars", base)?;
        let packages =
            py.import("sysconfig")?
                .call_method("get_path", ("purelib",), Some(&options))?;
        py.import("site")?.call_method1("addsitedir", (packages,))?;
    }
    Ok(())
}

fn flush(py: Python<'_>) -> PyResult<()> {
    let sys = py.import("sys")?;
    for name in ["stdout", "stderr"] {
        let stream = sys.getattr(name)?;
        if !stream.is_none() {
            stream.call_method0("flush")?;
        }
    }
    Ok(())
}

fn load<'py>(py: Python<'py>, file: &Path) -> PyResult<Bound<'py, PyAny>> {
    let file = PathBuf::from(display_path(file));
    let (Some(folder), Some(name)) = (file.parent(), file.file_stem()) else {
        return Err(PyImportError::new_err(format!(
            "'{}'에서 모듈 이름을 알 수 없습니다.",
            file.display()
        )));
    };
    let folder = folder.as_os_str();
    let search = py.import("sys")?.getattr("path")?;
    if !search.contains(folder)? {
        search.call_method1("insert", (0, folder))?;
    }
    let module = py
        .import("importlib")?
        .call_method1("import_module", (name,))?;
    let loaded = module.getattr_opt("__file__")?;
    let same = match loaded {
        Some(loaded) if !loaded.is_none() => py
            .import("os.path")?
            .call_method1("samefile", (loaded, &file))?
            .is_truthy()?,
        _ => false,
    };
    if !same {
        return Err(PyImportError::new_err(format!(
            "'{}' 모듈은 이미 다른 곳에서 가져왔습니다.",
            name.to_string_lossy()
        )));
    }
    Ok(module)
}

enum Plain {
    Value(Value),
    Big(BigInt),
    Text(String),
}

fn plain(object: &Bound<'_, PyAny>, span: Span) -> Result<Option<Plain>> {
    Ok(Some(if object.is_none() {
        Plain::Value(Value::None)
    } else if let Ok(truth) = object.cast::<PyBool>() {
        Plain::Value(Value::Bool(truth.is_true()))
    } else if object.is_instance_of::<PyInt>() {
        match object.extract::<i64>() {
            Ok(n) => Plain::Value(Value::Int(n)),
            Err(_) => Plain::Big(object.extract().raised(span)?),
        }
    } else if object.is_instance_of::<PyFloat>() {
        Plain::Value(Value::Float(object.extract().raised(span)?))
    } else if let Ok(text) = object.cast::<PyString>() {
        Plain::Text(text.to_str().raised(span)?.to_owned())
    } else {
        return Ok(None);
    }))
}

fn numpy_scalar(object: &Bound<'_, PyAny>) -> PyResult<bool> {
    let modules = object.py().import("sys")?.getattr("modules")?;
    match modules.cast::<PyDict>()?.get_item("numpy")? {
        Some(numpy) => object.is_instance(&numpy.getattr("generic")?),
        None => Ok(false),
    }
}

fn scalar(object: &Bound<'_, PyAny>, span: Span) -> Result<Option<Plain>> {
    match plain(object, span)? {
        Some(found) => Ok(Some(found)),
        None if numpy_scalar(object).raised(span)? => {
            plain(&object.call_method0("item").raised(span)?, span)
        }
        None => Ok(None),
    }
}

impl Heap {
    fn foreign<'py>(
        &self,
        py: Python<'py>,
        handle: Handle,
        span: Span,
    ) -> Result<Bound<'py, PyAny>> {
        match self.object(handle, span)? {
            Object::Python(Foreign(object)) => Ok(object.bind(py).clone()),
            _ => Err(internal(span)),
        }
    }

    fn value_of(
        &mut self,
        object: Bound<'_, PyAny>,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        match scalar(&object, span)? {
            Some(Plain::Value(value)) => Ok(value),
            Some(Plain::Big(big)) if big.bits() > MAX_INT_BITS => {
                Err(Diagnostic::limit(Limit::IntBits, span))
            }
            Some(Plain::Big(big)) => self.new_int(big.into(), roots, span),
            Some(Plain::Text(text)) => self.new_text(text, roots, span),
            None => {
                let object = Object::Python(Foreign(object.unbind()));
                self.alloc(object, size_of::<Object>(), roots, span)
                    .map(Value::Python)
            }
        }
    }

    fn python_of<'py>(
        &self,
        py: Python<'py>,
        value: &Value,
        depth: usize,
        span: Span,
    ) -> Result<Bound<'py, PyAny>> {
        if depth > MAX_DATA_DEPTH {
            return Err(Diagnostic::limit(Limit::DataDepth, span));
        }
        let all = |values: &[Value]| {
            values
                .iter()
                .map(|value| self.python_of(py, value, depth + 1, span))
                .collect::<Result<Vec<_>>>()
        };
        Ok(match value {
            Value::Int(n) => n.into_bound_py_any(py).raised(span)?,
            Value::Big(_) => {
                let n = self.int(value).ok_or_else(|| internal(span))?;
                n.big().as_ref().into_bound_py_any(py).raised(span)?
            }
            Value::Float(f) => PyFloat::new(py, *f).into_any(),
            Value::Bool(b) => PyBool::new(py, *b).to_owned().into_any(),
            Value::Text(_) => PyString::new(py, &self.text_of(value, span)?).into_any(),
            Value::None => py.None().into_bound(py),
            Value::List(handle) => PyList::new(py, all(self.list(*handle, span)?)?)
                .raised(span)?
                .into_any(),
            Value::Tuple(handle) => PyTuple::new(py, all(self.list(*handle, span)?)?)
                .raised(span)?
                .into_any(),
            Value::Set(_) => PySet::new(py, all(&self.items(value, span)?)?)
                .raised(span)?
                .into_any(),
            Value::Dict(handle) => {
                let dictionary = PyDict::new(py);
                for entry in self.dictionary(*handle, span)?.iter() {
                    let key = self.python_of(py, &entry.original, depth + 1, span)?;
                    let value = self.python_of(py, &entry.value, depth + 1, span)?;
                    dictionary.set_item(key, value).raised(span)?;
                }
                dictionary.into_any()
            }
            Value::Python(handle) => self.foreign(py, *handle, span)?,
            Value::Native(_)
            | Value::Function(_)
            | Value::Error(_)
            | Value::RecordType(_)
            | Value::Record(_) => {
                let kind = value.kind_name();
                return Err(Diagnostic::new(
                    "E-PY-005",
                    span,
                    format!("{kind}{} 파이썬에 넘길 수 없습니다.", TOPIC.after(kind)),
                ));
            }
        })
    }

    pub fn python_import(
        &mut self,
        name: &str,
        file: Option<&Path>,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        attach(span, |py| {
            let module = match file {
                Some(file) => load(py, file),
                None => py.import(name).map(Bound::into_any),
            };
            flush(py).raised(span)?;
            self.value_of(module.raised(span)?, roots, span)
        })
    }

    pub fn python_call(
        &mut self,
        callee: Handle,
        positional: Option<&Value>,
        keywords: Option<&Value>,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        attach(span, |py| {
            let function = self.foreign(py, callee, span)?;
            let positional = match positional {
                None => Vec::new(),
                Some(Value::List(handle) | Value::Tuple(handle)) => self
                    .list(*handle, span)?
                    .iter()
                    .map(|value| self.python_of(py, value, 1, span))
                    .collect::<Result<_>>()?,
                Some(_) => return Err(arguments_error(span)),
            };
            let keywords = match keywords {
                None => None,
                Some(Value::Dict(handle)) => {
                    let named = PyDict::new(py);
                    for entry in self.dictionary(*handle, span)?.iter() {
                        let name = self
                            .text(&entry.original)
                            .ok_or_else(|| arguments_error(span))?;
                        let value = self.python_of(py, &entry.value, 1, span)?;
                        named.set_item(&**name, value).raised(span)?;
                    }
                    Some(named)
                }
                Some(_) => return Err(arguments_error(span)),
            };
            let positional = PyTuple::new(py, positional).raised(span)?;
            let result = function.call(positional, keywords.as_ref());
            flush(py).raised(span)?;
            self.value_of(result.raised(span)?, roots, span)
        })
    }

    pub fn python_attribute(
        &mut self,
        handle: Handle,
        name: &str,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        attach(span, |py| {
            let object = self.foreign(py, handle, span)?;
            if name == "길이" {
                let length = object.len().raised(span)?;
                return Ok(Value::Int(length as i64));
            }
            self.value_of(object.getattr(name).raised(span)?, roots, span)
        })
    }

    pub fn python_store_attribute(
        &self,
        handle: Handle,
        name: &str,
        value: &Value,
        span: Span,
    ) -> Result<()> {
        attach(span, |py| {
            let object = self.foreign(py, handle, span)?;
            let value = self.python_of(py, value, 0, span)?;
            object.setattr(name, value).raised(span)
        })
    }

    pub fn python_index(
        &mut self,
        handle: Handle,
        index: &Value,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        attach(span, |py| {
            let object = self.foreign(py, handle, span)?;
            let index = self.python_of(py, index, 0, span)?;
            self.value_of(object.get_item(index).raised(span)?, roots, span)
        })
    }

    pub fn python_store_index(
        &self,
        handle: Handle,
        index: &Value,
        value: &Value,
        span: Span,
    ) -> Result<()> {
        attach(span, |py| {
            let object = self.foreign(py, handle, span)?;
            let index = self.python_of(py, index, 0, span)?;
            let value = self.python_of(py, value, 0, span)?;
            object.set_item(index, value).raised(span)
        })
    }

    pub fn python_slice(
        &mut self,
        handle: Handle,
        bounds: [&Value; 3],
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        attach(span, |py| {
            let object = self.foreign(py, handle, span)?;
            let [start, stop, step] = bounds.map(|bound| self.python_of(py, bound, 0, span));
            let slice = PySlice::type_object(py)
                .call1((start?, stop?, step?))
                .raised(span)?;
            self.value_of(object.get_item(slice).raised(span)?, roots, span)
        })
    }

    pub fn python_unary(
        &mut self,
        handle: Handle,
        op: UnaryOp,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        attach(span, |py| {
            let object = self.foreign(py, handle, span)?;
            let result = match op {
                UnaryOp::Neg => object.neg(),
                UnaryOp::Pos => object.pos(),
                UnaryOp::Not => return Err(internal(span)),
            };
            self.value_of(result.raised(span)?, roots, span)
        })
    }

    pub fn python_binary(
        &mut self,
        left: &Value,
        op: BinOp,
        right: &Value,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        attach(span, |py| {
            let left = self.python_of(py, left, 0, span)?;
            let right = self.python_of(py, right, 0, span)?;
            let result = match op {
                BinOp::Add => left.add(right),
                BinOp::Sub => left.sub(right),
                BinOp::Mul => left.mul(right),
                BinOp::Div => left.div(right),
                BinOp::FloorDiv => left.floor_div(right),
                BinOp::Mod => left.rem(right),
                BinOp::Pow => left.pow(right, py.None()),
                BinOp::And | BinOp::Or => return Err(internal(span)),
            };
            self.value_of(result.raised(span)?, roots, span)
        })
    }

    fn rich_compare<'py>(
        &self,
        py: Python<'py>,
        left: &Value,
        op: CmpOp,
        right: &Value,
        span: Span,
    ) -> Result<Bound<'py, PyAny>> {
        let left = self.python_of(py, left, 0, span)?;
        let right = self.python_of(py, right, 0, span)?;
        let op = match op {
            CmpOp::Eq => CompareOp::Eq,
            CmpOp::Ne => CompareOp::Ne,
            CmpOp::Lt => CompareOp::Lt,
            CmpOp::Le => CompareOp::Le,
            CmpOp::Gt => CompareOp::Gt,
            CmpOp::Ge => CompareOp::Ge,
        };
        left.rich_compare(right, op).raised(span)
    }

    pub fn python_compare(
        &mut self,
        left: &Value,
        op: CmpOp,
        right: &Value,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        attach(span, |py| {
            let result = self.rich_compare(py, left, op, right, span)?;
            self.value_of(result, roots, span)
        })
    }

    pub fn python_holds(&self, left: &Value, op: CmpOp, right: &Value, span: Span) -> Result<bool> {
        attach(span, |py| {
            let result = self.rich_compare(py, left, op, right, span)?;
            match scalar(&result, span)? {
                Some(Plain::Value(Value::Bool(holds))) => Ok(holds),
                _ => Err(Diagnostic::new(
                    "E-TYPE-002",
                    span,
                    "비교 결과가 참 또는 거짓이 아닙니다.",
                )),
            }
        })
    }

    pub fn python_iterator(
        &mut self,
        handle: Handle,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Handle> {
        attach(span, |py| {
            let iterator = self.foreign(py, handle, span)?.try_iter().raised(span)?;
            let object = Object::Python(Foreign(iterator.into_any().unbind()));
            self.alloc(object, size_of::<Object>(), roots, span)
        })
    }

    pub fn python_next(
        &mut self,
        iterator: Handle,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Option<Value>> {
        attach(span, |py| {
            let iterator = self.foreign(py, iterator, span)?;
            let mut iterator = iterator
                .cast_into::<PyIterator>()
                .map_err(|_| internal(span))?;
            match iterator.next() {
                Some(item) => self.value_of(item.raised(span)?, roots, span).map(Some),
                None => Ok(None),
            }
        })
    }

    pub fn python_items(
        &mut self,
        handle: Handle,
        roots: impl Iterator<Item = Handle> + Clone,
        span: Span,
    ) -> Result<Vec<Value>> {
        attach(span, |py| {
            let iterator = self.foreign(py, handle, span)?.try_iter().raised(span)?;
            let mut items: Vec<Value> = Vec::new();
            for item in iterator {
                if items.len() >= MAX_ITEMS {
                    return Err(Diagnostic::limit(Limit::Items, span));
                }
                let held = items.iter().filter_map(Value::handle);
                let value = self.value_of(item.raised(span)?, roots.clone().chain(held), span)?;
                items.push(value);
            }
            Ok(items)
        })
    }

    pub fn python_number(
        &mut self,
        handle: Handle,
        float: bool,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        attach(span, |py| {
            let object = self.foreign(py, handle, span)?;
            let number = if float {
                PyFloat::type_object(py).call1((object,))
            } else {
                PyInt::type_object(py).call1((object,))
            };
            self.value_of(number.raised(span)?, roots, span)
        })
    }

    pub fn python_text(&self, handle: Handle, nested: bool, span: Span) -> Result<String> {
        attach(span, |py| {
            let object = self.foreign(py, handle, span)?;
            let text = if nested { object.repr() } else { object.str() };
            Ok(text.raised(span)?.to_str().raised(span)?.to_owned())
        })
    }
}

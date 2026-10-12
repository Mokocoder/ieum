use std::path::Path;

use crate::diagnostic::{Diagnostic, Result, Span};
use crate::runtime::heap::{Handle, Heap};
use crate::runtime::value::Value;
use crate::syntax::ast::{BinOp, CmpOp, UnaryOp};

pub(crate) enum Foreign {}

fn absent<T>(span: Span) -> Result<T> {
    Err(Diagnostic::new(
        "E-PY-001",
        span,
        "이 실행기는 파이썬 연동 없이 빌드했습니다.",
    ))
}

impl Heap {
    pub fn python_import(
        &mut self,
        _: &str,
        _: Option<&Path>,
        _: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        absent(span)
    }

    pub fn python_call(
        &mut self,
        _: Handle,
        _: Option<&Value>,
        _: Option<&Value>,
        _: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        absent(span)
    }

    pub fn python_attribute(
        &mut self,
        _: Handle,
        _: &str,
        _: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        absent(span)
    }

    pub fn python_store_attribute(&self, _: Handle, _: &str, _: &Value, span: Span) -> Result<()> {
        absent(span)
    }

    pub fn python_index(
        &mut self,
        _: Handle,
        _: &Value,
        _: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        absent(span)
    }

    pub fn python_store_index(&self, _: Handle, _: &Value, _: &Value, span: Span) -> Result<()> {
        absent(span)
    }

    pub fn python_slice(
        &mut self,
        _: Handle,
        _: [&Value; 3],
        _: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        absent(span)
    }

    pub fn python_unary(
        &mut self,
        _: Handle,
        _: UnaryOp,
        _: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        absent(span)
    }

    pub fn python_binary(
        &mut self,
        _: &Value,
        _: BinOp,
        _: &Value,
        _: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        absent(span)
    }

    pub fn python_compare(
        &mut self,
        _: &Value,
        _: CmpOp,
        _: &Value,
        _: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        absent(span)
    }

    pub fn python_holds(&self, _: &Value, _: CmpOp, _: &Value, span: Span) -> Result<bool> {
        absent(span)
    }

    pub fn python_iterator(
        &mut self,
        _: Handle,
        _: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Handle> {
        absent(span)
    }

    pub fn python_next(
        &mut self,
        _: Handle,
        _: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Option<Value>> {
        absent(span)
    }

    pub fn python_items(
        &mut self,
        _: Handle,
        _: impl Iterator<Item = Handle> + Clone,
        span: Span,
    ) -> Result<Vec<Value>> {
        absent(span)
    }

    pub fn python_number(
        &mut self,
        _: Handle,
        _: bool,
        _: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        absent(span)
    }

    pub fn python_text(&self, _: Handle, _: bool, span: Span) -> Result<String> {
        absent(span)
    }
}

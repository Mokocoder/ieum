use std::io::Write;

use super::flow::MAIN;
use super::{Control, Frame, SortFrame, Target, Trace, TraceName, Vm};
use crate::check::{ArgumentOrigin, Mapping, signature_map};
use crate::diagnostic::{Diagnostic, Result, Span, code, internal, type_error};
use crate::limits::Limit;
use crate::runtime::code::{CallCache, CallSite, Callee};
use crate::runtime::heap::Handle;
use crate::runtime::value::Value;
use crate::stdlib::{self, Native};
use crate::syntax::ast::{Constraint, SignatureParameter};

fn not_callable(span: Span) -> Diagnostic {
    Diagnostic::new("E-CALL-007", span, "호출할 수 있는 함수 값이 아닙니다.")
}

impl<W: Write> Vm<'_, W> {
    pub(super) fn call(&mut self, site: &CallSite) -> Result<Control> {
        let span = site.span;
        let length = self.state.thread.data.len();
        let base = length
            .checked_sub(site.arguments.len() + 1)
            .ok_or_else(|| internal(span))?;
        let return_to = if site.incoming {
            base.checked_sub(1).ok_or_else(|| internal(span))?
        } else {
            base
        };
        match self.state.thread.data[base] {
            Some(Value::Function(handle)) => self.call_function(site, handle, base, return_to),
            Some(Value::Native(native)) => self.call_native(site, native, base, return_to),
            _ => Err(not_callable(span)),
        }
    }

    fn cache(
        site: &CallSite,
        callee: Callee,
        signature: &[SignatureParameter],
    ) -> Result<CallCache> {
        if let Some(cache) = site.cache.get().filter(|cache| cache.callee == callee) {
            return Ok(cache);
        }
        let mapping = signature_map(signature, &site.arguments, site.incoming, site.span)?;
        let exact = mapping.len() == site.arguments.len()
            && mapping.iter().enumerate().all(
                |(index, origin)| matches!(origin, ArgumentOrigin::Argument(at) if at == index),
            );
        let checked = signature
            .iter()
            .any(|parameter| parameter.spec.constraint != Constraint::Any);
        let cache = CallCache {
            callee,
            mapping,
            exact,
            checked,
        };
        site.cache.set(Some(cache));
        Ok(cache)
    }

    fn written<'s>(
        data: &'s [Option<Value>],
        site: &CallSite,
        origin: ArgumentOrigin,
        base: usize,
        return_to: usize,
    ) -> Option<(&'s Value, Span)> {
        match origin {
            ArgumentOrigin::Argument(index) => {
                Some((data[base + 1 + index].as_ref()?, site.values[index]))
            }
            ArgumentOrigin::Pipe => Some((data[return_to].as_ref()?, site.span)),
            ArgumentOrigin::Default => None,
        }
    }

    fn check_arguments(
        data: &[Option<Value>],
        site: &CallSite,
        signature: &[SignatureParameter],
        mapping: &Mapping,
        base: usize,
        return_to: usize,
    ) -> Result<()> {
        for (parameter, origin) in signature.iter().zip(mapping.iter()) {
            if let Some((value, span)) = Self::written(data, site, origin, base, return_to) {
                if !parameter.spec.constraint.accepts(value) {
                    return Err(type_error(span));
                }
            }
        }
        Ok(())
    }

    fn call_function(
        &mut self,
        site: &CallSite,
        handle: Handle,
        base: usize,
        return_to: usize,
    ) -> Result<Control> {
        let span = site.span;
        let parts = self.heap.function(handle, span)?;
        let info = parts.info;
        let thread = &mut self.state.thread;
        thread.trace.push(Trace {
            name: TraceName::Function(info.name.clone()),
            span,
            clause: site.clause,
        });
        let cache = Self::cache(site, Callee::Function(info.version), &info.signature)?;
        if cache.checked {
            let data = &thread.data;
            Self::check_arguments(data, site, &info.signature, &cache.mapping, base, return_to)?;
        }
        let limit = self.heap.limits.max_call_depth;
        if thread.call_depth >= limit {
            return Err(Diagnostic::limit(Limit::CallDepth(limit), span));
        }
        let code = &info.code;
        let locals = if cache.exact {
            base + 1
        } else {
            let start = thread.data.len();
            thread.data.reserve(code.locals as usize);
            for (index, origin) in cache.mapping.iter().enumerate() {
                let value = match Self::written(&thread.data, site, origin, base, return_to) {
                    Some((value, _)) => *value,
                    None => parts
                        .defaults
                        .get(index)
                        .cloned()
                        .ok_or_else(|| internal(span))?,
                };
                thread.data.push(Some(value));
            }
            start
        };
        thread.push_frame(
            code.clone(),
            locals,
            return_to,
            parts.captures,
            parts.globals,
            true,
        );
        Ok(Control::Reload)
    }

    fn call_native(
        &mut self,
        site: &CallSite,
        native: Native,
        base: usize,
        return_to: usize,
    ) -> Result<Control> {
        let depth = self.state.thread.trace.len();
        let result = self.run_native(site, native, base, return_to);
        if result.is_err() && self.state.thread.trace.len() == depth {
            self.trace_native(site, native);
        }
        result
    }

    fn trace_native(&mut self, site: &CallSite, native: Native) {
        self.state.thread.trace.push(Trace {
            name: TraceName::Native(native),
            span: site.span,
            clause: site.clause,
        });
    }

    fn run_native(
        &mut self,
        site: &CallSite,
        native: Native,
        base: usize,
        return_to: usize,
    ) -> Result<Control> {
        let span = site.span;
        let thread = &mut self.state.thread;
        let signature = native.plain_signature();
        let cache = Self::cache(site, Callee::Native(native), signature)?;
        Self::check_arguments(
            &thread.data,
            site,
            signature,
            &cache.mapping,
            base,
            return_to,
        )?;
        let mut arguments = std::mem::take(&mut self.arguments);
        arguments.clear();
        for origin in cache.mapping.iter() {
            arguments.push(
                match Self::written(&thread.data, site, origin, base, return_to) {
                    Some((value, _)) => *value,
                    None => Value::None,
                },
            );
        }
        let result = if matches!(native, Native::Sort | Native::SortDown)
            && matches!(arguments[1], Value::Function(_) | Value::Native(_))
        {
            self.trace_native(site, native);
            self.start_sort(&arguments, native == Native::SortDown, return_to, span)
        } else {
            self.apply_native(site, native, &arguments, return_to)
        };
        arguments.clear();
        self.arguments = arguments;
        result
    }

    fn apply_native(
        &mut self,
        site: &CallSite,
        native: Native,
        arguments: &[Value],
        return_to: usize,
    ) -> Result<Control> {
        let span = site.span;
        if self.state.flows.current != MAIN {
            let filesystem = self.access.filesystem.as_ref();
            let job = native.job(arguments, self.access.reach, filesystem, &self.heap, span)?;
            if let Some(job) = job {
                self.trace_native(site, native);
                self.await_job(job, return_to, span)?;
                return Ok(Control::Park);
            }
        }
        let result = self.invoke_native(native, arguments, span)?;
        let thread = &mut self.state.thread;
        thread.data.truncate(return_to);
        thread.data.push(Some(result));
        Ok(Control::Next)
    }

    #[inline]
    fn invoke_native(&mut self, native: Native, arguments: &[Value], span: Span) -> Result<Value> {
        native.invoke(
            arguments,
            stdlib::Host {
                filesystem: self.access.filesystem.as_ref(),
                input: self.access.input,
                output: &mut *self.output,
                random: &mut self.random,
                reach: self.access.reach,
            },
            &mut self.heap,
            self.state
                .roots()
                .chain(arguments.iter().filter_map(Value::handle)),
            span,
        )
    }

    fn target(&self, callee: &Value, span: Span) -> Result<Target> {
        match callee {
            Value::Function(handle) => {
                let info = self.heap.function(*handle, span)?.info;
                Ok(Target::Function(*handle, info.clone()))
            }
            Value::Native(native) => Ok(Target::Native(*native)),
            _ => Err(not_callable(span)),
        }
    }

    fn enter(&mut self, handle: Handle, locals: usize, return_to: usize, span: Span) -> Result<()> {
        let limit = self.heap.limits.max_call_depth;
        if self.state.thread.call_depth >= limit {
            return Err(Diagnostic::limit(Limit::CallDepth(limit), span));
        }
        let parts = self.heap.function(handle, span)?;
        let code = parts.info.code.clone();
        self.state
            .thread
            .push_frame(code, locals, return_to, parts.captures, parts.globals, true);
        Ok(())
    }

    pub(super) fn leave_frame(&mut self, value: Value) -> Result<Control> {
        let thread = &mut self.state.thread;
        let Some(Frame::Code(frame)) = thread.frames.pop() else {
            return Err(internal(Span::default()));
        };
        thread.release_frame(&frame);
        match thread.frames.last() {
            Some(Frame::Code(_)) => {
                thread.data.push(Some(value));
                Ok(Control::Reload)
            }
            Some(Frame::Sort(_)) => self.sort_step(Some(value)),
            None => Ok(Control::Finished(value)),
        }
    }

    fn start_sort(
        &mut self,
        arguments: &[Value],
        descending: bool,
        return_to: usize,
        span: Span,
    ) -> Result<Control> {
        let roots = self
            .state
            .roots()
            .chain(arguments.iter().filter_map(Value::handle));
        let items = self.heap.snapshot(&arguments[0], roots, span)?;
        let roots = self
            .state
            .roots()
            .chain(arguments.iter().filter_map(Value::handle));
        let Value::List(items) = self.heap.new_list(items, roots, span)? else {
            return Err(internal(span));
        };
        let thread = &mut self.state.thread;
        thread.frames.push(Frame::Sort(SortFrame {
            items,
            function: arguments[1],
            keys: Vec::new(),
            descending,
            span,
            base: return_to,
            trace: thread.trace.len() - 1,
        }));
        self.sort_step(None)
    }

    fn sort_step(&mut self, mut key: Option<Value>) -> Result<Control> {
        loop {
            let Some(Frame::Sort(sort)) = self.state.thread.frames.last_mut() else {
                return Err(internal(Span::default()));
            };
            let span = sort.span;
            if let Some(key) = key.take() {
                sort.keys.push(key);
            }
            let next = sort.keys.len();
            let (items, function) = (sort.items, sort.function);
            if let Some(item) = self.heap.list(items, span)?.get(next).cloned() {
                let computed = self.call_key(function, item, span).map_err(|error| {
                    if error.code == code::NO_PIPE_TARGET {
                        Diagnostic::new(
                            code::NO_PIPE_TARGET,
                            span,
                            "정렬 기준 함수는 원소를 을/를 자리의 인자로 받아야 합니다.",
                        )
                    } else {
                        error
                    }
                })?;
                match computed {
                    Some(value) => key = Some(value),
                    None => return Ok(Control::Reload),
                }
                continue;
            }
            let Some(Frame::Sort(sort)) = self.state.thread.frames.last() else {
                return Err(internal(span));
            };
            let items = self.heap.list(sort.items, span)?.to_vec();
            let keys = sort.keys.clone();
            let descending = sort.descending;
            let sorted = stdlib::sorted(
                items,
                keys,
                descending,
                &mut self.heap,
                self.state.roots(),
                span,
            )?;
            let thread = &mut self.state.thread;
            let Some(Frame::Sort(sort)) = thread.frames.pop() else {
                return Err(internal(span));
            };
            thread.release_sort(&sort);
            thread.data.push(Some(sorted));
            return Ok(Control::Reload);
        }
    }

    fn call_key(&mut self, callee: Value, input: Value, span: Span) -> Result<Option<Value>> {
        let target = self.target(&callee, span)?;
        self.state.thread.trace.push(Trace {
            name: target.name(),
            span,
            clause: None,
        });
        let signature = target.signature();
        let mapping = signature_map(signature, &[], true, span)?;
        let function = match target {
            Target::Function(handle, _) => Some(handle),
            Target::Native(_) => None,
        };
        let accepted = signature
            .iter()
            .zip(mapping.iter())
            .all(|(parameter, origin)| {
                !matches!(origin, ArgumentOrigin::Pipe) || parameter.spec.constraint.accepts(&input)
            });
        if !accepted {
            return Err(type_error(span));
        }
        let mut arguments = Vec::with_capacity(mapping.len());
        for (index, origin) in mapping.iter().enumerate() {
            arguments.push(match (origin, function) {
                (ArgumentOrigin::Pipe, _) => input,
                (_, Some(handle)) => self
                    .heap
                    .function(handle, span)?
                    .defaults
                    .get(index)
                    .cloned()
                    .ok_or_else(|| internal(span))?,
                (_, None) => Value::None,
            });
        }
        match (function, callee) {
            (Some(handle), _) => {
                let start = self.state.thread.data.len();
                self.state
                    .thread
                    .data
                    .extend(arguments.into_iter().map(Some));
                if let Err(error) = self.enter(handle, start, start, span) {
                    self.state.thread.data.truncate(start);
                    return Err(error);
                }
                Ok(None)
            }
            (None, Value::Native(native)) => {
                let result = self.invoke_native(native, &arguments, span)?;
                self.state.thread.trace.pop();
                Ok(Some(result))
            }
            _ => Err(internal(span)),
        }
    }
}

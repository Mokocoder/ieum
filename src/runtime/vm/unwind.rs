use std::io::Write;

use super::{Frame, Raised, StoredUnwind, Unwind, Vm};
use crate::diagnostic::{Diagnostic, ExceptionCause, ExecutionError, Result, Span};
use crate::limits::{MAX_ERROR_CAUSES, MAX_TRACE_FRAMES};
use crate::runtime::heap::Handle;

impl<W: Write> Vm<'_, W> {
    pub(super) fn active_error(&self) -> Option<Handle> {
        self.state
            .thread
            .unwinds
            .iter()
            .rev()
            .find_map(|stored| match stored {
                StoredUnwind::Error(handle) => Some(*handle),
                _ => None,
            })
    }

    pub(super) fn raised(&self, diagnostic: Diagnostic) -> Raised {
        Raised::Pending {
            report: Box::new(self.report(diagnostic)),
            cause: self.active_error(),
        }
    }

    fn store_unwind(&mut self, unwind: Unwind, span: Span) -> Result<StoredUnwind> {
        Ok(match unwind {
            Unwind::Error(Raised::Value(error)) => {
                self.heap.error(error, span)?;
                StoredUnwind::Error(error)
            }
            Unwind::Error(Raised::Pending { report, cause }) => StoredUnwind::Error(
                self.heap
                    .new_error(report, cause, self.state.roots(), span)?,
            ),
            Unwind::Exit(code) => StoredUnwind::Exit(code),
            Unwind::Cancel => StoredUnwind::Cancel,
        })
    }

    pub(super) fn raised_report(&self, raised: Raised) -> Result<ExecutionError> {
        let span = Span::default();
        let (mut report, mut cause) = match raised {
            Raised::Pending { report, cause } => (*report, cause),
            Raised::Value(handle) => {
                let (report, cause, _) = self.heap.error(handle, span)?;
                (report.clone(), cause)
            }
        };
        let mut causes = Vec::new();
        while let Some(handle) = cause {
            let (earlier, next, depth) = self.heap.error(handle, span)?;
            if causes.len() == MAX_ERROR_CAUSES {
                report.omitted_causes = u32::try_from(depth).unwrap_or(u32::MAX);
                break;
            }
            causes.push(ExceptionCause {
                diagnostic: earlier.diagnostic.clone(),
                frames: earlier.frames.clone(),
                omitted_frames: earlier.omitted_frames,
            });
            cause = next;
        }
        report.causes = causes.into_boxed_slice();
        Ok(report)
    }

    pub(super) fn unwind(&mut self, unwind: Unwind) -> Result<Option<Unwind>> {
        loop {
            let thread = &mut self.state.thread;
            let frame = match thread.frames.last_mut() {
                None => return Ok(Some(unwind)),
                Some(Frame::Sort(_)) => {
                    if let Some(Frame::Sort(sort)) = thread.frames.pop() {
                        thread.release_sort(&sort);
                    }
                    continue;
                }
                Some(Frame::Code(frame)) => frame,
            };
            let handler = frame
                .ip
                .checked_sub(1)
                .and_then(|at| frame.code.handler(at))
                .copied();
            let Some(handler) = handler else {
                let Some(Frame::Code(frame)) = thread.frames.pop() else {
                    unreachable!("맨 위 프레임은 코드 프레임입니다")
                };
                thread.release_frame(&frame);
                continue;
            };
            frame.ip = handler.target as usize;
            let span = frame.code.spans[handler.target as usize];
            let (stack, iters, unwinds, trace) = (
                frame.locals + frame.code.locals as usize + handler.depth as usize,
                frame.iters + handler.iters as usize,
                frame.unwinds + handler.unwinds as usize,
                frame.trace + usize::from(frame.call),
            );
            thread.data.truncate(stack);
            thread.iters.truncate(iters);
            thread.unwinds.truncate(unwinds);
            thread.trace.truncate(trace);
            let stored = self.store_unwind(unwind, span)?;
            self.state.thread.unwinds.push(stored);
            return Ok(None);
        }
    }

    pub(super) fn adopt_error(&mut self, mut error: ExecutionError) -> Result<Raised> {
        let span = Span::default();
        let outer = self.report(error.diagnostic.clone());
        let mut frames = std::mem::take(&mut error.frames).into_vec();
        frames.extend(outer.frames);
        let mut omitted = error.omitted_frames + outer.omitted_frames;
        if frames.len() > MAX_TRACE_FRAMES {
            omitted = omitted
                .saturating_add(u32::try_from(frames.len() - MAX_TRACE_FRAMES).unwrap_or(u32::MAX));
            frames.truncate(MAX_TRACE_FRAMES);
        }
        error.frames = frames.into();
        error.omitted_frames = omitted;
        error.omitted_causes = 0;
        let causes = std::mem::take(&mut error.causes);
        let mut cause = if causes.is_empty() {
            self.active_error()
        } else {
            None
        };
        for earlier in causes.iter().rev() {
            let report = Box::new(ExecutionError {
                diagnostic: earlier.diagnostic.clone(),
                frames: earlier.frames.clone(),
                omitted_frames: earlier.omitted_frames,
                causes: Box::default(),
                omitted_causes: 0,
                exit_code: None,
            });
            let roots = self.state.roots().chain(cause);
            cause = Some(self.heap.new_error(report, cause, roots, span)?);
        }
        Ok(Raised::Pending {
            report: Box::new(error),
            cause,
        })
    }
}

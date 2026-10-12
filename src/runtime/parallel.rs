use std::cell::Cell;
use std::collections::VecDeque;
use std::num::NonZero;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};

use super::heap::Heap;
use super::spawn;
use super::transfer::{self, Package};
use super::value::Value;
use super::vm::{Access, Delivery, Vm};
use crate::diagnostic::{Diagnostic, ExecutionError, Result, Span, internal};
use crate::limits::Limits;
use crate::stdlib::{FileSystem, Random, Reach};

thread_local! {
    static IN_WORKER: Cell<bool> = const { Cell::new(false) };
}

pub(super) struct Request {
    pub shared: Package,
    pub items: Vec<Package>,
    pub collect: bool,
    pub limits: Limits,
    pub steps: u64,
    pub call_depth: usize,
    pub seed: u64,
    pub filesystem: Option<FileSystem>,
    pub reach: Reach,
    #[cfg(test)]
    pub stress_gc: bool,
}

pub(super) enum Outcome {
    Finished {
        results: Option<Vec<Package>>,
        steps: u64,
    },
    Failed {
        error: Box<ExecutionError>,
        steps: u64,
    },
}

pub(super) struct ItemDone {
    output: Vec<u8>,
    result: std::result::Result<Option<Package>, Box<ExecutionError>>,
}

pub(super) struct ParallelCall {
    id: u64,
    request: Request,
    next: AtomicUsize,
    failed: AtomicUsize,
    stopped: AtomicBool,
    steps: AtomicU64,
    flow: usize,
    sender: Sender<(usize, Delivery)>,
}

impl ParallelCall {
    pub fn new(request: Request, flow: usize, sender: Sender<(usize, Delivery)>) -> Arc<Self> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        Arc::new(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            request,
            next: AtomicUsize::new(0),
            failed: AtomicUsize::new(usize::MAX),
            stopped: AtomicBool::new(false),
            steps: AtomicU64::new(0),
            flow,
            sender,
        })
    }

    fn claim(&self) -> Option<usize> {
        if self.stopped.load(Ordering::SeqCst) {
            return None;
        }
        let index = self.next.fetch_add(1, Ordering::SeqCst);
        (index < self.request.items.len() && index <= self.failed.load(Ordering::SeqCst))
            .then_some(index)
    }

    fn send(&self, index: usize, done: ItemDone) -> bool {
        if done.result.is_err() {
            self.failed.fetch_min(index, Ordering::SeqCst);
        }
        let delivery = Delivery::Item(self.id, index, done);
        self.sender.send((self.flow, delivery)).is_ok()
    }
}

// 작업 스레드 안의 `병렬로`는 풀을 기다리다 서로 막히지 않도록 따로 스레드 하나에서 차례로 계산합니다.
pub(super) fn start(call: Arc<ParallelCall>, span: Span) -> Result<()> {
    let size = if IN_WORKER.get() { 0 } else { pool_size() };
    if size == 0 {
        return spawn(span, move || {
            IN_WORKER.set(true);
            work(&call);
        });
    }
    let mut queue = POOL.queue.lock().unwrap_or_else(PoisonError::into_inner);
    for _ in 0..size.min(call.request.items.len()) {
        queue.push_back(call.clone());
    }
    drop(queue);
    POOL.wake.notify_all();
    Ok(())
}

struct Pool {
    queue: Mutex<VecDeque<Arc<ParallelCall>>>,
    wake: Condvar,
}

static POOL: Pool = Pool {
    queue: Mutex::new(VecDeque::new()),
    wake: Condvar::new(),
};

fn pool_size() -> usize {
    static SIZE: OnceLock<usize> = OnceLock::new();
    *SIZE.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(1, NonZero::get);
        (0..cores)
            .filter(|_| {
                std::thread::Builder::new()
                    .name("이음 작업".into())
                    .spawn(serve)
                    .is_ok()
            })
            .count()
    })
}

fn serve() {
    IN_WORKER.set(true);
    loop {
        let call = {
            let mut queue = POOL.queue.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                match queue.pop_front() {
                    Some(call) => break call,
                    None => {
                        queue = POOL
                            .wake
                            .wait(queue)
                            .unwrap_or_else(PoisonError::into_inner);
                    }
                }
            }
        };
        work(&call);
    }
}

fn work(call: &ParallelCall) {
    let request = &call.request;
    let Some(mut index) = call.claim() else {
        return;
    };
    let mut output = Vec::new();
    let access = Access {
        filesystem: request.filesystem.clone(),
        input: None,
        reach: request.reach,
    };
    let prepared = catch_unwind(AssertUnwindSafe(|| prepare(request, &mut output, access)));
    let (mut vm, function) = match prepared {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(error)) => return send_failure(call, index, error),
        Err(_) => return send_failure(call, index, internal(Span::default())),
    };
    loop {
        let start = request
            .steps
            .saturating_add(call.steps.load(Ordering::SeqCst));
        vm.steps = start;
        vm.random = Random::for_item(request.seed, index);
        let computed = catch_unwind(AssertUnwindSafe(|| {
            run_item(&mut vm, &function, &request.items[index], request)
        }));
        let Ok(result) = computed else {
            return send_failure(call, index, internal(Span::default()));
        };
        call.steps.fetch_add(vm.steps - start, Ordering::SeqCst);
        let output = std::mem::take(vm.output());
        if !call.send(index, ItemDone { output, result }) {
            return;
        }
        match call.claim() {
            Some(claimed) => index = claimed,
            None => return,
        }
    }
}

fn send_failure(call: &ParallelCall, index: usize, error: Diagnostic) {
    let done = ItemDone {
        output: Vec::new(),
        result: Err(boxed_error(error)),
    };
    call.send(index, done);
}

fn boxed_error(error: Diagnostic) -> Box<ExecutionError> {
    Box::new(error.into())
}

fn prepare<'w>(
    request: &Request,
    output: &'w mut Vec<u8>,
    access: Access<'w>,
) -> Result<(Vm<'w, Vec<u8>>, Value)> {
    let heap = Heap::new(request.limits)?;
    #[cfg(test)]
    let heap = {
        let mut heap = heap;
        heap.stress_gc = request.stress_gc;
        heap
    };
    let mut vm = Vm::new(heap, [Value::None; 2], output, access, Random::new(Some(0)));
    let [function, arguments, environment] = vm.unpack(&request.shared, true)?[..] else {
        return Err(internal(Span::default()));
    };
    vm.hold_value(function);
    vm.set_builtins([arguments, environment]);
    Ok((vm, function))
}

fn run_item(
    vm: &mut Vm<'_, Vec<u8>>,
    function: &Value,
    item: &Package,
    request: &Request,
) -> std::result::Result<Option<Package>, Box<ExecutionError>> {
    let [value] = vm.unpack(item, true).map_err(boxed_error)?[..] else {
        return Err(boxed_error(internal(Span::default())));
    };
    match function {
        Value::None => vm.begin_call(&value, Vec::new(), request.call_depth),
        function => vm.begin_call(function, vec![value], request.call_depth),
    }
    .map_err(boxed_error)?;
    let value = vm.execute().map_err(Box::new)?;
    if !request.collect {
        return Ok(None);
    }
    let package = transfer::export(&vm.heap, &[value], Span::default()).map_err(boxed_error)?;
    Ok(Some(package))
}

pub(super) struct Gathering {
    call: Arc<ParallelCall>,
    done: Vec<Option<ItemDone>>,
    emitted: usize,
    results: Vec<Package>,
}

impl Drop for Gathering {
    fn drop(&mut self) {
        self.call.stopped.store(true, Ordering::SeqCst);
    }
}

impl Gathering {
    pub fn new(call: Arc<ParallelCall>) -> Self {
        let count = call.request.items.len();
        Self {
            call,
            done: (0..count).map(|_| None).collect(),
            emitted: 0,
            results: Vec::new(),
        }
    }

    pub fn gathers(&self, call: u64) -> bool {
        self.call.id == call
    }

    pub fn accept(
        &mut self,
        index: usize,
        done: ItemDone,
        print: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<Option<Outcome>> {
        *self
            .done
            .get_mut(index)
            .ok_or_else(|| internal(Span::default()))? = Some(done);
        while let Some(item) = self.done.get_mut(self.emitted).and_then(Option::take) {
            if !item.output.is_empty() {
                print(&item.output)?;
            }
            self.emitted += 1;
            match item.result {
                Ok(result) => self.results.extend(result),
                Err(error) => {
                    let steps = self.call.steps.load(Ordering::SeqCst);
                    return Ok(Some(Outcome::Failed { error, steps }));
                }
            }
        }
        if self.emitted < self.done.len() {
            return Ok(None);
        }
        let results = std::mem::take(&mut self.results);
        Ok(Some(Outcome::Finished {
            results: self.call.request.collect.then_some(results),
            steps: self.call.steps.load(Ordering::SeqCst),
        }))
    }
}

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, Sender, channel};

use super::{Control, Raised, Thread, Unwind, Vm};
use crate::diagnostic::{Result, Span, internal};
use crate::runtime::heap::Handle;
use crate::runtime::parallel::{self, Gathering, ItemDone, Outcome, ParallelCall};
use crate::runtime::render::write_bytes;
use crate::runtime::spawn;
use crate::runtime::transfer;
use crate::runtime::value::Value;
use crate::stdlib::{Job, JobValue};

pub(super) const MAIN: usize = 0;

pub(super) enum Resume {
    Start,
    Continue,
    Delivered(Delivery),
    Joined(usize),
    Cancel,
}

pub(in crate::runtime) enum Delivery {
    Job(Result<JobValue>),
    Item(u64, usize, ItemDone),
    Parallel(Outcome),
}

pub(super) struct Parked {
    pub(super) thread: Thread,
    group: Option<usize>,
    slot: usize,
    resume: Resume,
}

pub(super) struct Group {
    parent: usize,
    children: Vec<usize>,
    remaining: usize,
    pub(super) failed: Option<Raised>,
    cancelled: bool,
    pub(super) results: Option<Handle>,
}

pub(super) struct Flows {
    pub(super) parked: HashMap<usize, Parked>,
    ready: VecDeque<usize>,
    pub(super) groups: HashMap<usize, Group>,
    waiting: HashSet<usize>,
    gatherings: HashMap<usize, Gathering>,
    pub(super) current: usize,
    pub(super) group: Option<usize>,
    slot: usize,
    pub(super) running: bool,
    next: usize,
    sender: Sender<(usize, Delivery)>,
    receiver: Receiver<(usize, Delivery)>,
}

impl Flows {
    pub(super) fn new() -> Self {
        let (sender, receiver) = channel();
        Self {
            parked: HashMap::new(),
            ready: VecDeque::new(),
            groups: HashMap::new(),
            waiting: HashSet::new(),
            gatherings: HashMap::new(),
            current: MAIN,
            group: None,
            slot: 0,
            running: true,
            next: MAIN + 1,
            sender,
            receiver,
        }
    }

    fn next_id(&mut self) -> usize {
        let id = self.next;
        self.next += 1;
        id
    }

    fn cancel(&mut self, id: usize) {
        let started = self
            .groups
            .iter()
            .find(|(_, group)| group.parent == id && group.remaining > 0)
            .map(|(&key, _)| key);
        if let Some(key) = started {
            let group = self.groups.get_mut(&key).expect("위에서 찾았습니다");
            group.cancelled = true;
            for child in group.children.clone() {
                self.cancel(child);
            }
            return;
        }
        let Some(parked) = self.parked.get_mut(&id) else {
            return;
        };
        parked.resume = Resume::Cancel;
        self.gatherings.remove(&id);
        if self.waiting.remove(&id) || !self.ready.contains(&id) {
            self.ready.push_back(id);
        }
    }
}

fn start_job(job: Job, flow: usize, sender: Sender<(usize, Delivery)>, span: Span) -> Result<()> {
    spawn(span, move || {
        let result = catch_unwind(AssertUnwindSafe(job)).unwrap_or_else(|_| Err(internal(span)));
        let _ = sender.send((flow, Delivery::Job(result)));
    })
}

impl<W: Write> Vm<'_, W> {
    pub(super) fn start_parallel(
        &mut self,
        function: Value,
        items: Vec<Value>,
        collect: bool,
        span: Span,
    ) -> Result<Control> {
        if items.is_empty() {
            if collect {
                let list = self.heap.new_list(Vec::new(), self.state.roots(), span)?;
                self.push(list);
            }
            return Ok(Control::Next);
        }
        let [arguments, environment] = self.state.builtins;
        let request = parallel::Request {
            shared: transfer::export(&self.heap, &[function, arguments, environment], span)?,
            items: items
                .iter()
                .map(|item| transfer::export(&self.heap, std::slice::from_ref(item), span))
                .collect::<Result<_>>()?,
            collect,
            limits: self.heap.limits,
            steps: self.steps,
            call_depth: self.state.thread.call_depth,
            seed: self.random.split(),
            filesystem: self.access.filesystem.clone(),
            reach: self.access.reach,
            #[cfg(test)]
            stress_gc: self.heap.stress_gc,
        };
        let flows = &mut self.state.flows;
        let id = flows.current;
        let call = ParallelCall::new(request, id, flows.sender.clone());
        parallel::start(call.clone(), span)?;
        flows.gatherings.insert(id, Gathering::new(call.clone()));
        self.state.flows.waiting.insert(id);
        self.state.thread.awaiting = Some(self.state.thread.data.len());
        self.park(Resume::Continue);
        Ok(Control::Park)
    }

    fn finish_parallel(&mut self, outcome: Outcome) -> Result<Control> {
        let span = Span::default();
        match outcome {
            Outcome::Finished { results, steps } => {
                self.steps = self.steps.saturating_add(steps);
                let Some(results) = results else {
                    return Ok(Control::Next);
                };
                for package in &results {
                    let [value] = self.unpack(package, false)?[..] else {
                        return Err(internal(span));
                    };
                    self.push(value);
                }
                let values = self.take(results.len())?;
                let list = self.heap.new_list(values, self.state.roots(), span)?;
                self.push(list);
                Ok(Control::Next)
            }
            Outcome::Failed { error, steps } => {
                self.steps = self.steps.saturating_add(steps);
                if let Some(code) = error.exit_code {
                    return Ok(Control::Raise(Unwind::Exit(code)));
                }
                if error.diagnostic.is_fatal() {
                    return Err(error.diagnostic);
                }
                let raised = self.adopt_error(*error)?;
                Ok(Control::Raise(Unwind::Error(raised)))
            }
        }
    }

    pub(super) fn new_flow_thread(
        &self,
        closure: &Value,
        arguments: Vec<Value>,
        span: Span,
    ) -> Result<Thread> {
        let Value::Function(handle) = closure else {
            return Err(internal(span));
        };
        let parts = self.heap.function(*handle, span)?;
        let mut thread = Thread {
            call_depth: self.state.thread.call_depth,
            ..Thread::default()
        };
        thread.data.extend(arguments.into_iter().map(Some));
        thread.push_frame(
            parts.info.code.clone(),
            0,
            0,
            parts.captures,
            parts.globals,
            false,
        );
        Ok(thread)
    }

    pub(super) fn spawn_flows(
        &mut self,
        threads: Vec<Thread>,
        results: Option<Handle>,
    ) -> Result<Control> {
        if threads.is_empty() {
            if let Some(results) = results {
                self.push(Value::List(results));
            }
            return Ok(Control::Reload);
        }
        let flows = &mut self.state.flows;
        let key = flows.next_id();
        let mut children = Vec::with_capacity(threads.len());
        for (slot, thread) in threads.into_iter().enumerate() {
            let id = flows.next_id();
            flows.parked.insert(
                id,
                Parked {
                    thread,
                    group: Some(key),
                    slot,
                    resume: Resume::Start,
                },
            );
            flows.ready.push_back(id);
            children.push(id);
        }
        let remaining = children.len();
        flows.groups.insert(
            key,
            Group {
                parent: flows.current,
                children,
                remaining,
                failed: None,
                cancelled: false,
                results,
            },
        );
        self.park(Resume::Joined(key));
        Ok(Control::Park)
    }

    fn park(&mut self, resume: Resume) {
        let flows = &mut self.state.flows;
        let parked = Parked {
            thread: std::mem::take(&mut self.state.thread),
            group: flows.group,
            slot: flows.slot,
            resume,
        };
        flows.parked.insert(flows.current, parked);
        flows.running = false;
    }

    pub(super) fn await_job(&mut self, job: Job, return_to: usize, span: Span) -> Result<()> {
        let sender = self.state.flows.sender.clone();
        let id = self.state.flows.current;
        start_job(job, id, sender, span)?;
        self.state.flows.waiting.insert(id);
        self.state.thread.awaiting = Some(return_to);
        self.park(Resume::Continue);
        Ok(())
    }

    pub(super) fn switch(&mut self) -> Result<Option<Resume>> {
        loop {
            let flows = &mut self.state.flows;
            if let Some(id) = flows.ready.pop_front() {
                let Some(parked) = flows.parked.remove(&id) else {
                    continue;
                };
                flows.current = id;
                flows.group = parked.group;
                flows.slot = parked.slot;
                flows.running = true;
                self.state.thread = parked.thread;
                return Ok(Some(parked.resume));
            }
            if flows.waiting.is_empty() {
                if flows.parked.contains_key(&MAIN) {
                    return Err(internal(Span::default()));
                }
                return Ok(None);
            }
            let (id, delivery) = flows
                .receiver
                .recv()
                .map_err(|_| internal(Span::default()))?;
            let delivery = match delivery {
                Delivery::Item(call, index, done) => {
                    let Some(gathering) = flows
                        .gatherings
                        .get_mut(&id)
                        .filter(|gathering| gathering.gathers(call))
                    else {
                        continue;
                    };
                    let output = &mut *self.output;
                    let mut print = |bytes: &[u8]| write_bytes(output, bytes, Span::default());
                    match gathering.accept(index, done, &mut print)? {
                        Some(outcome) => {
                            flows.gatherings.remove(&id);
                            Delivery::Parallel(outcome)
                        }
                        None => continue,
                    }
                }
                delivery => delivery,
            };
            if flows.waiting.remove(&id) {
                if let Some(parked) = flows.parked.get_mut(&id) {
                    parked.resume = Resume::Delivered(delivery);
                    flows.ready.push_back(id);
                }
            }
        }
    }

    pub(super) fn resume(&mut self, resume: Resume) -> Result<Control> {
        match resume {
            Resume::Start | Resume::Continue => Ok(Control::Next),
            Resume::Delivered(delivery) => {
                let thread = &mut self.state.thread;
                let base = thread
                    .awaiting
                    .take()
                    .ok_or_else(|| internal(Span::default()))?;
                thread.data.truncate(base);
                match delivery {
                    Delivery::Job(outcome) => {
                        thread.trace.pop();
                        let roots = self.state.roots();
                        let value = outcome?.into_value(&mut self.heap, roots, Span::default())?;
                        self.push(value);
                        Ok(Control::Next)
                    }
                    Delivery::Parallel(outcome) => self.finish_parallel(outcome),
                    Delivery::Item(..) => Err(internal(Span::default())),
                }
            }
            Resume::Joined(key) => {
                let group = self
                    .state
                    .flows
                    .groups
                    .remove(&key)
                    .ok_or_else(|| internal(Span::default()))?;
                if let Some(raised) = group.failed {
                    return Ok(Control::Raise(Unwind::Error(raised)));
                }
                if group.cancelled {
                    return Ok(Control::Raise(Unwind::Cancel));
                }
                if let Some(results) = group.results {
                    self.push(Value::List(results));
                }
                Ok(Control::Next)
            }
            Resume::Cancel => {
                self.state.thread.awaiting = None;
                Ok(Control::Raise(Unwind::Cancel))
            }
        }
    }

    pub(super) fn stop_flow(&mut self) -> Result<usize> {
        let key = self
            .state
            .flows
            .group
            .ok_or_else(|| internal(Span::default()))?;
        self.state.thread = Thread::default();
        self.state.flows.running = false;
        Ok(key)
    }

    pub(super) fn finish_flow(&mut self, value: Value) -> Result<()> {
        let key = self.stop_flow()?;
        let results = self
            .state
            .flows
            .groups
            .get(&key)
            .ok_or_else(|| internal(Span::default()))?
            .results;
        if let Some(results) = results {
            self.heap
                .store_index(results, self.state.flows.slot, value, Span::default())?;
        }
        self.leave_group(key)
    }

    pub(super) fn leave_group(&mut self, key: usize) -> Result<()> {
        let group = self
            .state
            .flows
            .groups
            .get_mut(&key)
            .ok_or_else(|| internal(Span::default()))?;
        group.remaining -= 1;
        if group.remaining == 0 {
            let parent = group.parent;
            self.state.flows.ready.push_back(parent);
        }
        Ok(())
    }

    pub(super) fn fail_flow(&mut self, raised: Raised) -> Result<()> {
        let key = self.stop_flow()?;
        let flows = &mut self.state.flows;
        let group = flows
            .groups
            .get_mut(&key)
            .ok_or_else(|| internal(Span::default()))?;
        group.failed.get_or_insert(raised);
        let current = flows.current;
        let others: Vec<usize> = group
            .children
            .iter()
            .copied()
            .filter(|&id| id != current)
            .collect();
        for id in others {
            flows.cancel(id);
        }
        self.leave_group(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    #[test]
    fn a_failed_job_delivers_an_error() {
        let (sender, receiver) = channel();
        start_job(
            Box::new(|| panic!("작업 실패를 확인합니다")),
            0,
            sender,
            Span::new(3, 6),
        )
        .unwrap();
        let (id, delivery) = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(id, 0);
        let Delivery::Job(Err(error)) = delivery else {
            panic!("작업 오류가 필요합니다")
        };
        assert_eq!(
            (error.code, error.span),
            ("E-INTERNAL-001", Span::new(3, 6))
        );
    }
}

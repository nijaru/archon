//! Agent dispatch: one worker thread per registered machine owns the
//! [`AgentClient`] and performs every round trip off the caller's lock.
//! The controller enqueues jobs and absorbs completions later, so slow or
//! dead agents cannot stall other clients or agents. Completions carry the
//! session and fence captured at send time; the kernel endpoints stay the
//! authority that rejects stale generations.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use archon_kernel::{BindingId, LeaseId, NodeId};

use crate::protocol::{AgentRequest, AgentResponse};
use crate::service::AgentClient;

/// A controller-side answer from an agent.
pub type AgentReply = Result<AgentResponse, String>;

/// Which lifecycle step produced a completion, keeping inflight keys
/// distinct when one binding sees several effects in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EffectPhase {
    Prepare,
    Activate,
    Release,
    Fence,
}

/// Identifies an outstanding asynchronous agent call.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tag {
    Binding {
        binding: BindingId,
        session: u64,
        fence: u64,
        phase: EffectPhase,
    },
    Status {
        lease: LeaseId,
        machine: NodeId,
    },
    Probe {
        machine: NodeId,
    },
}

/// How a worker delivers one answer.
pub enum Delivery {
    /// Hand the answer to a waiting caller (Hello, Logs).
    Direct(Sender<AgentReply>),
    /// Post the answer to the shared inbox for absorption.
    Inbox(Tag),
}

/// One unit of agent work.
pub struct Job {
    pub request: AgentRequest,
    pub delivery: Delivery,
}

/// Completed agent answers waiting for the controller to absorb them.
#[derive(Default)]
pub struct Inbox {
    queue: Mutex<Vec<(Tag, AgentReply)>>,
    signal: Condvar,
}

impl Inbox {
    pub(crate) fn post(&self, tag: Tag, reply: AgentReply) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.push((tag, reply));
        }
        self.signal.notify_all();
    }

    pub(crate) fn take(&self) -> Vec<(Tag, AgentReply)> {
        self.queue
            .lock()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default()
    }

    /// Block until a completion arrives or the timeout passes. Safe to call
    /// without the controller lock: it never touches controller state.
    pub fn wait_timeout(&self, timeout: Duration) {
        if let Ok(queue) = self.queue.lock()
            && queue.is_empty()
        {
            let _ = self.signal.wait_timeout(queue, timeout);
        }
    }
}

/// Cloneable handle for sending work to one machine's worker.
#[derive(Clone)]
pub struct AgentHandle {
    tx: Sender<Job>,
}

impl AgentHandle {
    pub fn send(&self, job: Job) -> bool {
        self.tx.send(job).is_ok()
    }
}

/// Move `client` onto a dedicated thread; every call it receives is a full
/// request/response round trip performed outside any controller lock.
pub fn spawn_worker(mut client: Box<dyn AgentClient>, inbox: Arc<Inbox>) -> AgentHandle {
    let (tx, rx) = channel::<Job>();
    std::thread::spawn(move || {
        while let Ok(job) = rx.recv() {
            let reply = client.call(job.request);
            match job.delivery {
                Delivery::Direct(sender) => {
                    let _ = sender.send(reply);
                }
                Delivery::Inbox(tag) => inbox.post(tag, reply),
            }
        }
    });
    AgentHandle { tx }
}

/// One-shot query helper for callers outside any lock: send a job whose
/// answer comes back directly.
pub(crate) fn direct_job(request: AgentRequest) -> (Job, Receiver<AgentReply>) {
    let (tx, rx) = channel();
    (
        Job {
            request,
            delivery: Delivery::Direct(tx),
        },
        rx,
    )
}

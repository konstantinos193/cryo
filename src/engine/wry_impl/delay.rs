//! Deferred one-shot work off the event loop: one worker thread per
//! engine, a min-heap of due times. Exists for the `DownloadStarting` ⇄
//! `NavigationCompleted` race — an aborted navigation may be "this
//! became a download" whose flag lands a beat later, so the verdict
//! re-checks on this worker after a grace period rather than sleeping
//! on a thread spawned per abort.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// Handle to the worker thread — `Clone`, one per engine, handed to
/// each view's navigation wiring.
#[derive(Clone)]
pub(super) struct Delayer {
    tx: Sender<Due>,
}

/// A queued task. Ordering is inverted: `BinaryHeap` pops the greatest,
/// and "greatest" must mean the earliest deadline.
struct Due {
    at: Instant,
    work: Box<dyn FnOnce() + Send>,
}

impl PartialEq for Due {
    fn eq(&self, other: &Self) -> bool {
        self.at == other.at
    }
}

impl Eq for Due {}

impl PartialOrd for Due {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Due {
    fn cmp(&self, other: &Self) -> Ordering {
        other.at.cmp(&self.at)
    }
}

impl Delayer {
    pub(super) fn new() -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || run(rx));
        Self { tx }
    }

    /// Run `work` once, `delay` from now, on the shared worker. A dead
    /// worker drops the task — reported, not silently `let _`'d.
    pub(super) fn after(&self, delay: Duration, work: impl FnOnce() + Send + 'static) {
        let due = Due {
            at: Instant::now() + delay,
            work: Box::new(work),
        };
        if self.tx.send(due).is_err() {
            crate::report!("delay worker gone — deferred task dropped");
        }
    }
}

/// The worker: sleeps until the next task is due (or forever when the
/// queue is empty), runs whatever has come due, repeats. Exits when the
/// last `Delayer` handle drops — engine teardown ends the channel.
fn run(rx: Receiver<Due>) {
    let mut heap: BinaryHeap<Due> = BinaryHeap::new();
    loop {
        let arrived = match heap.peek() {
            Some(d) => match rx.recv_timeout(d.at.saturating_duration_since(Instant::now())) {
                Ok(d) => Some(d),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            },
            None => match rx.recv() {
                Ok(d) => Some(d),
                Err(_) => return,
            },
        };
        if let Some(d) = arrived {
            heap.push(d);
        }
        while let Some(d) = heap.peek() {
            if d.at > Instant::now() {
                break;
            }
            (heap.pop().expect("peeked entry exists").work)();
        }
    }
}

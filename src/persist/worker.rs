//! Ordered, bounded snapshot writes. A worker owns disk I/O; producers
//! replace one pending snapshot, so a locked disk cannot grow a queue.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

struct State<T> {
    pending: Option<Vec<T>>,
    closing: bool,
}

pub(crate) struct WriteQueue<T> {
    state: Arc<(Mutex<State<T>>, Condvar)>,
}

/// Joined by the composition root after the event loop has returned.
pub(crate) struct WriterShutdown {
    close: Box<dyn FnOnce()>,
    thread: Option<JoinHandle<()>>,
}

impl<T: Send + 'static> WriteQueue<T> {
    pub(crate) fn spawn(
        save: impl Fn(&[T]) -> Result<(), String> + Send + 'static,
        failed: impl Fn(String) + Send + 'static,
    ) -> std::io::Result<(Self, WriterShutdown)> {
        let state = Arc::new((
            Mutex::new(State {
                pending: None,
                closing: false,
            }),
            Condvar::new(),
        ));
        let work = state.clone();
        let thread = thread::Builder::new()
            .name("cryo-store".into())
            .spawn(move || {
                let mut reported_failure = false;
                loop {
                    let (lock, wake) = &*work;
                    let mut state = lock.lock().expect("writer state is not poisoned");
                    while state.pending.is_none() && !state.closing {
                        state = wake.wait(state).expect("writer state is not poisoned");
                    }
                    let Some(rows) = state.pending.take() else {
                        break;
                    };
                    let final_attempt = state.closing;
                    drop(state);
                    if let Err(error) = save(&rows) {
                        if !reported_failure {
                            failed(error.clone());
                            reported_failure = true;
                        }
                        let mut state = lock.lock().expect("writer state is not poisoned");
                        if final_attempt {
                            crate::report!("final store write failed: {error}");
                            break;
                        }
                        if state.pending.is_none() {
                            state.pending = Some(rows);
                        }
                        if state.closing {
                            drop(state);
                            continue;
                        }
                        // Retain the failed snapshot until it succeeds or a
                        // newer one arrives. Shutdown interrupts the retry wait.
                        drop(
                            wake.wait_timeout(state, std::time::Duration::from_secs(1))
                                .expect("writer state is not poisoned"),
                        );
                    } else {
                        reported_failure = false;
                    }
                }
            })?;
        let closing = state.clone();
        Ok((
            Self { state },
            WriterShutdown {
                close: Box::new(move || {
                    let (lock, wake) = &*closing;
                    lock.lock().expect("writer state is not poisoned").closing = true;
                    wake.notify_one();
                }),
                thread: Some(thread),
            },
        ))
    }

    pub(crate) fn submit(&self, rows: Vec<T>) {
        let (lock, wake) = &*self.state;
        lock.lock().expect("writer state is not poisoned").pending = Some(rows);
        wake.notify_one();
    }
}

impl Drop for WriterShutdown {
    fn drop(&mut self) {
        // Shutdown is after run_return, so no disk wait blocks window events.
        let close = std::mem::replace(&mut self.close, Box::new(|| {}));
        close();
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                crate::report!("store worker panicked during shutdown");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn blocked_disk_does_not_block_producer_and_only_latest_snapshot_follows() {
        let (started, start) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let saved = Arc::new(Mutex::new(Vec::new()));
        let writes = saved.clone();
        let producer = thread::current().id();
        let (queue, shutdown) = WriteQueue::spawn(
            move |rows: &[u32]| {
                assert_ne!(thread::current().id(), producer);
                if rows == [1] {
                    started.send(()).unwrap();
                    wait.recv().unwrap();
                }
                writes.lock().unwrap().push(rows.to_vec());
                Ok(())
            },
            |e| panic!("unexpected write failure: {e}"),
        )
        .unwrap();
        queue.submit(vec![1]);
        start
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        // These return even though the worker is still inside save().
        queue.submit(vec![2]);
        queue.submit(vec![3]);
        release.send(()).unwrap();
        drop(shutdown);
        assert_eq!(*saved.lock().unwrap(), vec![vec![1], vec![3]]);
    }

    #[test]
    fn failed_snapshot_is_retained_and_shutdown_retries_it() {
        let (failure, failed) = mpsc::channel();
        let attempts = Arc::new(Mutex::new(0));
        let calls = attempts.clone();
        let saved = Arc::new(Mutex::new(Vec::new()));
        let writes = saved.clone();
        let (queue, shutdown) = WriteQueue::spawn(
            move |rows: &[u32]| {
                let mut calls = calls.lock().unwrap();
                *calls += 1;
                if *calls == 1 {
                    return Err("file locked".into());
                }
                writes.lock().unwrap().extend_from_slice(rows);
                Ok(())
            },
            move |e| failure.send(e).unwrap(),
        )
        .unwrap();
        queue.submit(vec![42]);
        assert_eq!(
            failed
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            "file locked"
        );
        drop(shutdown);
        assert_eq!(*saved.lock().unwrap(), vec![42]);
    }
}

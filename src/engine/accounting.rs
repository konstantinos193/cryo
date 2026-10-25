//! Engine-process accounting, never exclusive per-tab ownership.
//! Private bytes are committed private memory; working set includes shared resident pages.
//! Renderer processes remain unattributed because multiple views can share them.
//! A snapshot covers the engine's process set — the WebView2 environment on
//! Windows, the WebKit child-process tree on unix — potentially shared across
//! windows; it excludes the host process and GPU device allocations. Do not
//! sum snapshots from shared environments or sum working sets as if they were
//! unique physical RAM.
//! Call `Engine::measure_memory` on the view thread; the completion runs on a
//! worker, so callers must send results back to the event loop. Enumeration
//! errors are returned immediately where the adapter can; per-process read
//! failures remain in the snapshot.
//! Measurements span started_at..finished_at and are not an atomic OS snapshot.

use std::time::Instant;

pub type MemoryCallback = Box<dyn FnOnce(MemorySnapshot) + Send>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProcessId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessKind {
    Browser,
    Gpu,
    Renderer,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryStats {
    private_bytes: u64,
    working_set_bytes: u64,
}

impl MemoryStats {
    pub fn new(private_bytes: u64, working_set_bytes: u64) -> Self {
        Self {
            private_bytes,
            working_set_bytes,
        }
    }

    pub fn private_bytes(self) -> u64 {
        self.private_bytes
    }

    pub fn working_set_bytes(self) -> u64 {
        self.working_set_bytes
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessMemory {
    id: ProcessId,
    kind: ProcessKind,
    stats: Result<MemoryStats, String>,
}

impl ProcessMemory {
    pub fn new(id: ProcessId, kind: ProcessKind, stats: Result<MemoryStats, String>) -> Self {
        Self { id, kind, stats }
    }

    pub fn id(&self) -> ProcessId {
        self.id
    }

    pub fn kind(&self) -> ProcessKind {
        self.kind
    }

    pub fn stats(&self) -> Result<MemoryStats, &str> {
        self.stats.as_ref().copied().map_err(String::as_str)
    }
}

#[derive(Clone, Debug)]
pub struct MemorySnapshot {
    started_at: Instant,
    finished_at: Instant,
    processes: Vec<ProcessMemory>,
}

impl MemorySnapshot {
    pub fn new(started_at: Instant, finished_at: Instant, processes: Vec<ProcessMemory>) -> Self {
        let mut processes = processes;
        processes.sort_by_key(ProcessMemory::id);
        processes.dedup_by_key(|p| p.id());
        Self {
            started_at,
            finished_at,
            processes,
        }
    }

    pub fn started_at(&self) -> Instant {
        self.started_at
    }

    pub fn finished_at(&self) -> Instant {
        self.finished_at
    }

    pub fn processes(&self) -> &[ProcessMemory] {
        &self.processes
    }

    pub fn total_private_bytes(&self) -> Option<u64> {
        self.processes.iter().try_fold(0u64, |total, process| {
            total.checked_add(process.stats().ok()?.private_bytes())
        })
    }

    pub fn private_bytes(&self, kind: ProcessKind) -> Option<u64> {
        self.processes
            .iter()
            .filter(|p| p.kind() == kind)
            .try_fold(0u64, |total, p| {
                total.checked_add(p.stats().ok()?.private_bytes())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_overhead_and_renderers_are_separate_and_pids_count_once() {
        let now = Instant::now();
        let process = |id, kind, bytes| {
            ProcessMemory::new(ProcessId(id), kind, Ok(MemoryStats::new(bytes, bytes * 2)))
        };
        let snapshot = MemorySnapshot::new(
            now,
            now,
            vec![
                process(1, ProcessKind::Browser, 100),
                process(2, ProcessKind::Gpu, 200),
                process(3, ProcessKind::Renderer, 300),
                process(3, ProcessKind::Renderer, 300),
                process(4, ProcessKind::Renderer, 400),
            ],
        );
        assert_eq!(snapshot.processes().len(), 4);
        assert_eq!(snapshot.total_private_bytes(), Some(1000));
        assert_eq!(snapshot.private_bytes(ProcessKind::Browser), Some(100));
        assert_eq!(snapshot.private_bytes(ProcessKind::Gpu), Some(200));
        assert_eq!(snapshot.private_bytes(ProcessKind::Renderer), Some(700));
        assert_eq!(snapshot.private_bytes(ProcessKind::Other), Some(0));
    }

    #[test]
    fn unavailable_process_does_not_become_zero_memory() {
        let now = Instant::now();
        let snapshot = MemorySnapshot::new(
            now,
            now,
            vec![ProcessMemory::new(
                ProcessId(9),
                ProcessKind::Renderer,
                Err("process exited".into()),
            )],
        );
        assert_eq!(snapshot.total_private_bytes(), None);
        assert_eq!(snapshot.private_bytes(ProcessKind::Renderer), None);
        assert_eq!(snapshot.processes()[0].stats(), Err("process exited"));
    }
}

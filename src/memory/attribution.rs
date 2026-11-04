//! Per-tab attribution of a memory snapshot: sole-owner bytes or an
//! honest reason there is no figure. The rule is settled policy
//! (MEMORY-POLICY-CHECKLIST.md): a renderer's private bytes are billed
//! to a tab only when that process serves no other view; every other
//! renderer is one shared pool. No division, no estimates — a number
//! the row cannot defend is worse than none.

use std::collections::BTreeMap;

use crate::engine::accounting::{MemorySnapshot, ProcessKind};
use crate::tabs::TabId;

/// A live view asking to be billed. `frame` is the engine's handle for
/// the view (`ContentView::frame_id`); `None` where the engine cannot
/// say, which the row reports as [`Attribution::Unattributed`].
#[derive(Clone, Copy, Debug)]
pub struct LiveView {
    pub id: TabId,
    pub frame: Option<u32>,
}

/// What a row can honestly claim about a live tab's footprint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attribution {
    /// Its renderer serves no other view — these private bytes are
    /// the tab's, full stop.
    Sole(u64),
    /// Its renderer also serves other views — inside this window or
    /// outside it — so no single figure is honest.
    Shared,
    /// The engine gave no link between views and processes, or the
    /// view's renderer was not in the snapshot.
    Unattributed,
    /// The renderer was identified but its stats were not.
    Unreadable,
}

/// Process-level totals the surface reports once — never divided into
/// a per-tab figure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pools {
    /// Browser, GPU and utility processes — overhead, not any tab's.
    pub overhead: u64,
    /// Renderer bytes no single tab can own.
    pub shared_renderers: u64,
    /// A stats read failed somewhere; every figure reported is partial.
    pub partial: bool,
}

/// Bill a snapshot to the live views. `live_views` lists views that
/// have a surface — frozen and discarded tabs are not passed at all:
/// they own no process and their rows say their state instead.
pub fn attribute(
    snapshot: &MemorySnapshot,
    live_views: &[LiveView],
) -> (Vec<(TabId, Attribution)>, Pools) {
    let frame_to_tab: BTreeMap<u32, TabId> = live_views
        .iter()
        .filter_map(|v| v.frame.map(|f| (f, v.id)))
        .collect();

    let mut pools = Pools::default();
    // frame id -> what its hosting process said. Two processes claiming
    // the same frame is a contradiction no row may gloss over.
    let mut claims: BTreeMap<u32, Attribution> = BTreeMap::new();

    for process in snapshot.processes() {
        if process.kind() != ProcessKind::Renderer {
            match process.stats() {
                Ok(stats) => pools.overhead += stats.private_bytes(),
                Err(_) => pools.partial = true,
            }
            continue;
        }
        let bytes = process.stats().map(|s| s.private_bytes());
        let owned: Vec<u32> = process
            .frames()
            .iter()
            .copied()
            .filter(|f| frame_to_tab.contains_key(f))
            .collect();
        // Sole owner exactly when the process's only top-level frame is
        // one of ours — a foreign main frame, an unknown frame, or two
        // of our views all make it shared.
        let sole = match (owned.len(), process.frames().len()) {
            (1, 1) => Some(owned[0]),
            _ => None,
        };
        for frame in &owned {
            let claim = match (sole, bytes) {
                (Some(f), Ok(b)) if f == *frame => Attribution::Sole(b),
                (_, Err(_)) => Attribution::Unreadable,
                _ => Attribution::Shared,
            };
            claims
                .entry(*frame)
                .and_modify(|c| {
                    if *c != claim {
                        *c = Attribution::Shared;
                    }
                })
                .or_insert(claim);
        }
        match (sole, bytes) {
            (Some(_), Ok(_)) => {}
            (_, Ok(b)) => pools.shared_renderers += b,
            (_, Err(_)) => pools.partial = true,
        }
    }

    let rows = live_views
        .iter()
        .map(|v| {
            let attribution = match v.frame {
                None => Attribution::Unattributed,
                Some(f) => claims.get(&f).copied().unwrap_or(Attribution::Unattributed),
            };
            (v.id, attribution)
        })
        .collect();
    (rows, pools)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::accounting::{MemoryStats, ProcessId, ProcessMemory};
    use std::time::Instant;

    fn process(id: u32, kind: ProcessKind, bytes: u64, frames: &[u32]) -> ProcessMemory {
        ProcessMemory::new(ProcessId(id), kind, Ok(MemoryStats::new(bytes, bytes * 2)))
            .with_frames(frames.to_vec())
    }

    fn snapshot(processes: Vec<ProcessMemory>) -> MemorySnapshot {
        let now = Instant::now();
        MemorySnapshot::new(now, now, processes)
    }

    fn view(id: u64, frame: u32) -> LiveView {
        LiveView {
            id: TabId::from_raw(id),
            frame: Some(frame),
        }
    }

    #[test]
    fn sole_owner_gets_the_renderers_bytes() {
        let snap = snapshot(vec![
            process(1, ProcessKind::Browser, 100, &[]),
            process(2, ProcessKind::Gpu, 50, &[]),
            process(3, ProcessKind::Renderer, 400, &[10]),
        ]);
        let views = [view(1, 10)];
        let (rows, pools) = attribute(&snap, &views);
        assert_eq!(rows, vec![(TabId::from_raw(1), Attribution::Sole(400))]);
        assert_eq!(pools.overhead, 150);
        assert_eq!(pools.shared_renderers, 0);
        assert!(!pools.partial);
    }

    #[test]
    fn two_views_one_renderer_is_shared_for_both() {
        let snap = snapshot(vec![process(3, ProcessKind::Renderer, 400, &[10, 11])]);
        let views = [view(1, 10), view(2, 11)];
        let (rows, pools) = attribute(&snap, &views);
        assert_eq!(
            rows,
            vec![
                (TabId::from_raw(1), Attribution::Shared),
                (TabId::from_raw(2), Attribution::Shared)
            ]
        );
        assert_eq!(
            pools.shared_renderers, 400,
            "its bytes are a pool, not a share"
        );
    }

    #[test]
    fn a_foreign_top_level_frame_makes_the_renderer_shared() {
        // Frame 99 is hosted but belongs to no view we listed — another
        // window, or a view that closed mid-sample. Either way the bytes
        // are not this tab's alone.
        let snap = snapshot(vec![process(3, ProcessKind::Renderer, 400, &[10, 99])]);
        let views = [view(1, 10)];
        let (rows, pools) = attribute(&snap, &views);
        assert_eq!(rows, vec![(TabId::from_raw(1), Attribution::Shared)]);
        assert_eq!(pools.shared_renderers, 400);
    }

    #[test]
    fn no_frames_anywhere_means_no_attribution() {
        // Old runtime or unix: the engine cannot link views to
        // processes, so every row says so and renderer bytes pool.
        let snap = snapshot(vec![process(3, ProcessKind::Renderer, 400, &[])]);
        let views = [
            view(1, 10),
            LiveView {
                id: TabId::from_raw(2),
                frame: None,
            },
        ];
        let (rows, pools) = attribute(&snap, &views);
        assert_eq!(
            rows,
            vec![
                (TabId::from_raw(1), Attribution::Unattributed),
                (TabId::from_raw(2), Attribution::Unattributed)
            ]
        );
        assert_eq!(pools.shared_renderers, 400);
    }

    #[test]
    fn a_failed_stats_read_marks_row_and_pools() {
        let snap = snapshot(vec![
            process(1, ProcessKind::Browser, 100, &[]),
            ProcessMemory::new(
                ProcessId(3),
                ProcessKind::Renderer,
                Err("process exited".into()),
            )
            .with_frames(vec![10]),
        ]);
        let views = [view(1, 10)];
        let (rows, pools) = attribute(&snap, &views);
        assert_eq!(rows, vec![(TabId::from_raw(1), Attribution::Unreadable)]);
        assert!(pools.partial);
    }

    #[test]
    fn a_view_whose_process_was_not_sampled_is_unattributed() {
        let snap = snapshot(vec![process(3, ProcessKind::Renderer, 400, &[11])]);
        let views = [view(1, 10), view(2, 11)];
        let (rows, _) = attribute(&snap, &views);
        assert_eq!(
            rows,
            vec![
                (TabId::from_raw(1), Attribution::Unattributed),
                (TabId::from_raw(2), Attribution::Sole(400))
            ]
        );
    }
}

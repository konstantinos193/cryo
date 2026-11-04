//! Enumerates the shared WebView2 environment on its COM thread and samples OS memory on a worker.
//!
//! `GetProcessExtendedInfos` (Environment13) carries each process's
//! associated top-level frames — the link between a renderer and the
//! views it hosts. Runtimes older than Environment13 fall back to the
//! `GetProcessInfos` enumeration: pids and kinds only, no frames, and
//! attribution honestly reports nothing per view.

use std::collections::BTreeMap;
use std::mem::size_of;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::time::{Duration, Instant};

use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2Environment, ICoreWebView2Environment13, ICoreWebView2Environment8,
    ICoreWebView2ProcessExtendedInfo, ICoreWebView2ProcessExtendedInfoCollection,
    COREWEBVIEW2_PROCESS_KIND, COREWEBVIEW2_PROCESS_KIND_BROWSER, COREWEBVIEW2_PROCESS_KIND_GPU,
    COREWEBVIEW2_PROCESS_KIND_RENDERER,
};
use windows::core::Interface;
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ};

use crate::engine::accounting::{
    MemoryCallback, MemorySnapshot, MemoryStats, ProcessId, ProcessKind, ProcessMemory,
};
use crate::engine::EngineError;

/// One enumerated process: who it is, what kind, and which top-level
/// frames it hosts. Frames ride with the row so the snapshot is
/// self-contained — no second pass can re-associate it later.
type ProcessRow = (ProcessId, ProcessKind, Vec<u32>);

/// How long the worker waits for the extended-infos callback before it
/// samples nothing. An enumeration that never lands must not strand
/// the measurement — an empty process list is the honest result.
const ENUMERATE_TIMEOUT: Duration = Duration::from_secs(10);

/// Which enumeration answered — the extended call replies on a
/// channel, the legacy one already holds its rows.
enum Source {
    /// `GetProcessExtendedInfos` is in flight; the rows arrive on rx.
    Extended,
    /// Runtimes older than Environment13: pid+kind only, no frames.
    Basic(Vec<ProcessRow>),
    /// No environment at all.
    Empty,
}

pub(in crate::engine::wry_impl) fn measure(
    environment: Option<&ICoreWebView2Environment>,
    complete: MemoryCallback,
) -> Result<(), EngineError> {
    let started_at = Instant::now();
    let (tx, rx) = std::sync::mpsc::channel();
    let source = match environment {
        // The extended-infos walk faults inside the collection on at
        // least one in-the-wild runtime — opt-in while it is
        // investigated; the legacy path enumerates the same processes
        // without frames, and rows honestly say `unattributed`.
        Some(environment) => match std::env::var_os("CRYO_EXTENDED_ENUM")
            .is_some()
            .then(|| environment.cast::<ICoreWebView2Environment13>().ok())
            .flatten()
        {
            Some(env13) => {
                let handler = webview2_com::GetProcessExtendedInfosCompletedHandler::create(
                    Box::new(move |error, collection| {
                        let rows = if error.is_ok() {
                            collection.map(collect_rows).unwrap_or_default()
                        } else {
                            Vec::new()
                        };
                        // A failed send means the worker already
                        // timed out — nothing is left to signal.
                        if tx.send(rows).is_err() {
                            crate::report!("process enumeration arrived with no receiver");
                        }
                        Ok(())
                    }),
                );
                unsafe { env13.GetProcessExtendedInfos(&handler) }
                    .map_err(|e| EngineError::ReadStats(e.to_string()))?;
                Source::Extended
            }
            None => Source::Basic(enumerate_basic(environment)?),
        },
        None => Source::Empty,
    };
    std::thread::Builder::new()
        .name("cryo-memory".into())
        .spawn(move || {
            let rows = match source {
                Source::Extended => rx.recv_timeout(ENUMERATE_TIMEOUT).unwrap_or_default(),
                Source::Basic(rows) => rows,
                Source::Empty => Vec::new(),
            };
            let processes = rows
                .into_iter()
                .map(|(id, kind, frames)| {
                    ProcessMemory::new(id, kind, read_stats(id).map_err(|e| e.to_string()))
                        .with_frames(frames)
                })
                .collect();
            complete(MemorySnapshot::new(started_at, Instant::now(), processes));
        })
        .map_err(|e| EngineError::ReadStats(e.to_string()))?;
    Ok(())
}

/// Walk the extended-infos collection into plain rows on the COM
/// thread — only pid, kind and frame ids cross to the worker.
fn collect_rows(collection: ICoreWebView2ProcessExtendedInfoCollection) -> Vec<ProcessRow> {
    let mut rows = Vec::new();
    unsafe {
        let mut count = 0;
        if collection.Count(&mut count).is_err() {
            return rows;
        }
        for index in 0..count {
            let Ok(info) = collection.GetValueAtIndex(index) else {
                continue;
            };
            let Ok(process) = info.ProcessInfo() else {
                continue;
            };
            let mut pid = 0;
            let mut kind = COREWEBVIEW2_PROCESS_KIND::default();
            if process.ProcessId(&mut pid).is_err() || process.Kind(&mut kind).is_err() || pid == 0
            {
                continue;
            }
            rows.push((
                ProcessId(pid as u32),
                process_kind(kind),
                frame_ids(&info).unwrap_or_default(),
            ));
        }
    }
    rows
}

/// Every frame id a process hosts. Currently a stub: the iterator's
/// `GetCurrent` faults inside the runtime's own frame materialization
/// on WebView2 156.x — an AV, not an HRESULT, on the first valid
/// position (state reads — `HasCurrent`, `MoveNext`, past-end
/// `GetCurrent` — all answer correctly, and the bindings match the
/// shipped `WebView2.h`). The extended-infos interfaces register no
/// proxy stubs, so the collection cannot be marshalled to another
/// apartment either. A runtime defect, not ours — until fixed
/// upstream, rows carry no frames and the pane says `unattributed`.
fn frame_ids(_info: &ICoreWebView2ProcessExtendedInfo) -> windows::core::Result<Vec<u32>> {
    Ok(Vec::new())
}

/// The pre-Environment13 enumeration — the rows it knows are pids and
/// kinds, so frames stay empty and attribution reports nothing.
fn enumerate_basic(environment: &ICoreWebView2Environment) -> Result<Vec<ProcessRow>, EngineError> {
    let enumerate = || -> windows::core::Result<BTreeMap<ProcessId, ProcessKind>> {
        let environment = environment.cast::<ICoreWebView2Environment8>()?;
        let infos = unsafe { environment.GetProcessInfos()? };
        let mut count = 0;
        unsafe { infos.Count(&mut count)? };
        let mut processes = BTreeMap::new();
        for index in 0..count {
            let info = unsafe { infos.GetValueAtIndex(index)? };
            let mut pid = 0;
            let mut kind = COREWEBVIEW2_PROCESS_KIND::default();
            unsafe {
                info.ProcessId(&mut pid)?;
                info.Kind(&mut kind)?;
            }
            if pid != 0 {
                processes.insert(ProcessId(pid as u32), process_kind(kind));
            }
        }
        Ok(processes)
    };
    let processes = enumerate().map_err(|e| EngineError::ReadStats(e.to_string()))?;
    Ok(processes
        .into_iter()
        .map(|(id, kind)| (id, kind, Vec::new()))
        .collect())
}

fn process_kind(kind: COREWEBVIEW2_PROCESS_KIND) -> ProcessKind {
    match kind {
        COREWEBVIEW2_PROCESS_KIND_BROWSER => ProcessKind::Browser,
        COREWEBVIEW2_PROCESS_KIND_GPU => ProcessKind::Gpu,
        COREWEBVIEW2_PROCESS_KIND_RENDERER => ProcessKind::Renderer,
        _ => ProcessKind::Other,
    }
}

fn read_stats(id: ProcessId) -> windows::core::Result<MemoryStats> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, id.0)? };
    let _owned = unsafe { OwnedHandle::from_raw_handle(handle.0) };
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    unsafe {
        GetProcessMemoryInfo(
            handle,
            &mut counters as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
            counters.cb,
        )?;
    }
    Ok(MemoryStats::new(
        counters.PrivateUsage as u64,
        counters.WorkingSetSize as u64,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_environment_completes_on_worker_without_creating_webview2() {
        let caller = std::thread::current().id();
        let (tx, rx) = std::sync::mpsc::channel();
        measure(
            None,
            Box::new(move |snapshot| {
                tx.send((std::thread::current().id(), snapshot)).unwrap();
            }),
        )
        .unwrap();
        let (worker, snapshot) = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_ne!(worker, caller);
        assert!(snapshot.processes().is_empty());
        assert!(snapshot.finished_at() >= snapshot.started_at());
    }

    #[test]
    fn reads_current_process_memory() {
        let stats = read_stats(ProcessId(std::process::id())).unwrap();
        assert!(stats.private_bytes() > 0);
        assert!(stats.working_set_bytes() > 0);
    }

    #[test]
    fn inaccessible_process_is_an_error() {
        assert!(read_stats(ProcessId(0)).is_err());
    }

    #[test]
    fn maps_shared_overhead_separately_from_renderers() {
        assert_eq!(
            process_kind(COREWEBVIEW2_PROCESS_KIND_BROWSER),
            ProcessKind::Browser
        );
        assert_eq!(
            process_kind(COREWEBVIEW2_PROCESS_KIND_GPU),
            ProcessKind::Gpu
        );
        assert_eq!(
            process_kind(COREWEBVIEW2_PROCESS_KIND_RENDERER),
            ProcessKind::Renderer
        );
        assert_eq!(
            process_kind(COREWEBVIEW2_PROCESS_KIND(999)),
            ProcessKind::Other
        );
    }
}

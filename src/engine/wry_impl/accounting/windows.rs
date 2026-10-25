//! Enumerates the shared WebView2 environment on its COM thread and samples OS memory on a worker.

use std::collections::BTreeMap;
use std::mem::size_of;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::time::Instant;

use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2Environment, ICoreWebView2Environment8, COREWEBVIEW2_PROCESS_KIND,
    COREWEBVIEW2_PROCESS_KIND_BROWSER, COREWEBVIEW2_PROCESS_KIND_GPU,
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

pub(in crate::engine::wry_impl) fn measure(
    environment: Option<&ICoreWebView2Environment>,
    complete: MemoryCallback,
) -> Result<(), EngineError> {
    let started_at = Instant::now();
    let processes = environment.map(enumerate).transpose()?.unwrap_or_default();
    std::thread::Builder::new()
        .name("cryo-memory".into())
        .spawn(move || {
            let processes = processes
                .into_iter()
                .map(|(id, kind)| {
                    ProcessMemory::new(id, kind, read_stats(id).map_err(|e| e.to_string()))
                })
                .collect();
            complete(MemorySnapshot::new(started_at, Instant::now(), processes));
        })
        .map_err(|e| EngineError::ReadStats(e.to_string()))?;
    Ok(())
}

fn enumerate(
    environment: &ICoreWebView2Environment,
) -> Result<BTreeMap<ProcessId, ProcessKind>, EngineError> {
    let enumerate = || -> windows::core::Result<_> {
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
    enumerate().map_err(|e| EngineError::ReadStats(e.to_string()))
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

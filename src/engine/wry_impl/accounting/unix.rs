//! WebKit has no environment object — the accounting domain is the engine's
//! process tree. Linux walks ppid descendants in `/proc`; macOS asks libproc
//! for children (which includes WKWebView's XPC services via responsible pid).
//! Enumeration and stat reads all happen on the worker: an unreadable process
//! table degrades to an empty snapshot, which callers treat as "no data".

use std::collections::BTreeMap;
use std::time::Instant;

use crate::engine::accounting::{
    MemoryCallback, MemorySnapshot, MemoryStats, ProcessId, ProcessKind, ProcessMemory,
};
use crate::engine::EngineError;

pub(in crate::engine::wry_impl) fn measure(complete: MemoryCallback) -> Result<(), EngineError> {
    let started_at = Instant::now();
    std::thread::Builder::new()
        .name("cryo-memory".into())
        .spawn(move || {
            let processes = enumerate()
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

/// WebKit process names → our process kinds. Anything else in the engine
/// subtree (sandbox helpers, plugin processes) counts as `Other`.
fn process_kind(exe: &str) -> ProcessKind {
    match exe {
        #[cfg(target_os = "linux")]
        "WebKitWebProcess" => ProcessKind::Renderer,
        #[cfg(target_os = "linux")]
        "WebKitGPUProcess" => ProcessKind::Gpu,
        #[cfg(target_os = "macos")]
        "com.apple.WebKit.WebContent" => ProcessKind::Renderer,
        #[cfg(target_os = "macos")]
        "com.apple.WebKit.GPU" => ProcessKind::Gpu,
        _ => ProcessKind::Other,
    }
}

#[cfg(target_os = "linux")]
fn enumerate() -> BTreeMap<ProcessId, ProcessKind> {
    use std::fs;

    // pid -> (ppid, exe basename)
    let mut procs: BTreeMap<u32, (u32, Option<String>)> = BTreeMap::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return BTreeMap::new();
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        let Some(ppid) = fs::read_to_string(entry.path().join("stat"))
            .ok()
            .and_then(|stat| parse_ppid(&stat))
        else {
            continue;
        };
        let exe = fs::read_link(entry.path().join("exe"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
        procs.insert(pid, (ppid, exe));
    }

    // Flood-fill descendants of this process — WebKitGTK's web and network
    // processes are children of the UI process (directly, or through bwrap).
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (&pid, &(ppid, _)) in &procs {
        children.entry(ppid).or_default().push(pid);
    }
    let mut descendants = BTreeMap::new();
    let mut queue = vec![std::process::id()];
    while let Some(ppid) = queue.pop() {
        for pid in children.get(&ppid).into_iter().flatten().copied() {
            if descendants.insert(pid, ()).is_none() {
                queue.push(pid);
            }
        }
    }
    descendants
        .keys()
        .filter_map(|&pid| procs.get(&pid).map(|(_, exe)| (pid, exe)))
        .map(|(pid, exe)| (ProcessId(pid), process_kind(exe.as_deref().unwrap_or(""))))
        .collect()
}

/// `/proc/<pid>/stat`'s comm field is parenthesized and may itself contain
/// spaces and parens — anchor on the last ')' and take the field after state.
#[cfg(any(target_os = "linux", test))]
fn parse_ppid(stat: &str) -> Option<u32> {
    let rest = stat.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace();
    fields.next()?; // state
    fields.next()?.parse().ok()
}

/// `smaps_rollup` → (private, working set) in bytes: private is the dirty +
/// clean pages only this process maps; Rss is the resident working set.
#[cfg(any(target_os = "linux", test))]
fn parse_smaps_rollup(roll: &str) -> Option<MemoryStats> {
    let mut private = 0;
    let mut saw_private = false;
    let mut rss = None;
    for line in roll.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let kib = || {
            rest.split_whitespace()
                .next()
                .and_then(|v| v.parse::<u64>().ok())
                .map(|k| k * 1024)
        };
        match key {
            "Rss" => rss = kib(),
            "Private_Clean" | "Private_Dirty" => {
                saw_private = true;
                private += kib().unwrap_or(0);
            }
            _ => {}
        }
    }
    if !saw_private {
        return None;
    }
    Some(MemoryStats::new(private, rss?))
}

#[cfg(target_os = "linux")]
fn read_stats(id: ProcessId) -> std::io::Result<MemoryStats> {
    let roll = std::fs::read_to_string(format!("/proc/{}/smaps_rollup", id.0))?;
    parse_smaps_rollup(&roll).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "smaps_rollup missing Rss/Private fields",
        )
    })
}

#[cfg(target_os = "macos")]
fn enumerate() -> BTreeMap<ProcessId, ProcessKind> {
    let ours = std::process::id() as libc::pid_t;
    let mut procs = BTreeMap::new();
    unsafe {
        // proc_listchildpids matches both parent and responsible pid, so
        // WKWebView's launchd-spawned XPC services are included.
        let need = libc::proc_listchildpids(ours, std::ptr::null_mut(), 0);
        if need <= 0 {
            return procs;
        }
        let mut pids: Vec<libc::pid_t> = vec![0; need as usize / size_of::<libc::pid_t>() + 16];
        let got = libc::proc_listchildpids(
            ours,
            pids.as_mut_ptr() as *mut libc::c_void,
            (pids.len() * size_of::<libc::pid_t>()) as libc::c_int,
        );
        if got <= 0 {
            return procs;
        }
        pids.truncate(got as usize / size_of::<libc::pid_t>());
        for pid in pids {
            if pid <= 0 {
                continue;
            }
            let mut name: [libc::c_char; 4096] = [0; 4096];
            if libc::proc_name(
                pid,
                name.as_mut_ptr() as *mut libc::c_void,
                name.len() as u32,
            ) <= 0
            {
                continue;
            }
            let name = std::ffi::CStr::from_ptr(name.as_ptr())
                .to_string_lossy()
                .into_owned();
            procs.insert(ProcessId(pid as u32), process_kind(&name));
        }
    }
    procs
}

/// `phys_footprint` is the committed private footprint Activity Monitor
/// reports; `resident_size` is the working set.
#[cfg(target_os = "macos")]
fn read_stats(id: ProcessId) -> std::io::Result<MemoryStats> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::uninit();
    let rc = unsafe {
        libc::proc_pid_rusage(
            id.0 as libc::pid_t,
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr() as *mut libc::rusage_info_t,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let info = unsafe { info.assume_init() };
    Ok(MemoryStats::new(
        info.ri_phys_footprint,
        info.ri_resident_size,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measure_completes_on_a_worker() {
        let caller = std::thread::current().id();
        let (tx, rx) = std::sync::mpsc::channel();
        measure(Box::new(move |snapshot| {
            tx.send((std::thread::current().id(), snapshot)).unwrap();
        }))
        .unwrap();
        let (worker, snapshot) = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_ne!(worker, caller);
        assert!(snapshot.finished_at() >= snapshot.started_at());
    }

    #[test]
    fn enumeration_never_includes_the_host() {
        let ours = ProcessId(std::process::id());
        assert!(!enumerate().contains_key(&ours));
    }

    #[test]
    fn stat_parser_skips_paren_comm() {
        let stat = "1234 (name with ) parens) S 5678 9 10";
        assert_eq!(parse_ppid(stat), Some(5678));
        assert_eq!(parse_ppid("garbage"), None);
    }

    #[test]
    fn rollup_parser_reads_private_and_rss() {
        let roll = "00400000-7fff0000 ---p 00000000 00:00 0 [rollup]\n\
                    Rss:                1234 kB\nPrivate_Clean:        100 kB\n\
                    Private_Dirty:        200 kB\n";
        let stats = parse_smaps_rollup(roll).unwrap();
        assert_eq!(stats.private_bytes(), 300 * 1024);
        assert_eq!(stats.working_set_bytes(), 1234 * 1024);
        assert!(parse_smaps_rollup("Rss: 10 kB\n").is_none());
    }
}

//! Memory sampling adapters per platform. Each exposes one `measure` that
//! completes a [`MemorySnapshot`] on a worker thread — the enumeration
//! source differs (WebView2's environment on Windows, the engine's
//! process tree on unix) but the snapshot's contract does not.

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use unix::measure;
#[cfg(target_os = "windows")]
pub(super) use windows::measure;

//! The warm keeper: what makes the *next* launch fast.
//!
//! WebView2 runs one browser process per user data folder, and it is
//! that process booting — not anything cryo does — that costs the first
//! controller 600 ms and more. A second process creating a view on the
//! same folder attaches to the running browser process instead and gets
//! its controller in a fraction of that (measured: ~200 ms against
//! ~650 ms on the same machine). So when the last window closes, cryo
//! spawns a small `cryo --warm` process that holds one hidden view open,
//! and the browser process stays up. The next launch attaches to it; the
//! keeper sees the launch and exits. It gives up on its own after
//! `CRYO_WARM_MINUTES` (default 30, `0` turns the keeper off), so an
//! idle machine is not carrying a browser process for a day.
//!
//! The keeper must *own* its browser process, not borrow the closing
//! window's: a keeper that attached to the old process while it was
//! shutting down, or that outlived its creator in one, gave the next
//! window an attach slower than a cold boot (measured 0.9–2.7 s against
//! 0.3 s on a process the keeper booted itself). So the window hands
//! the keeper its browser process id, and the keeper waits for that
//! process to end before creating its view.
//!
//! Two named manual-reset events carry the handshake, named off the exe
//! path like the link pipe. `warm-ready` the keeper holds and sets once
//! its view is up: a window launching while the keeper is still booting
//! waits for it (bounded), and dismisses a keeper that never comes up.
//! `warm` a launching window sets once its own views exist, and the
//! keeper waits on it to go — so the browser process is never without a
//! holder in between. Off Windows there is no keeper: the platforms
//! there have no shared browser process to keep.

use std::time::Duration;

/// Minutes the keeper waits for a launch before giving up. `0` means no
/// keeper is spawned at all.
const DEFAULT_MINUTES: u64 = 30;

/// How long a keeper stays, from `CRYO_WARM_MINUTES`. Anything that is
/// not a whole number of minutes reads as the default rather than as
/// "off": a typo should not quietly make every launch cold.
pub fn keep_for() -> Duration {
    Duration::from_secs(60 * minutes(std::env::var("CRYO_WARM_MINUTES").ok().as_deref()))
}

fn minutes(raw: Option<&str>) -> u64 {
    raw.and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_MINUTES)
}

/// The longest the keeper waits for the old browser process to end
/// before booting its own regardless — a process that is hanging in
/// shutdown should not cost the next launch its keeper too.
const OLD_PROCESS_GRACE: Duration = Duration::from_secs(10);

/// How long a launching window waits for a keeper still booting to
/// come up: the keeper's own boot is about 0.6 s, so this is the rest of
/// one, never a wait on a keeper that is wedged.
const READY_PATIENCE: Duration = Duration::from_millis(1500);

/// How long a launching window gives a dismissed keeper to be gone —
/// view closed, browser process ended — before booting cold anyway.
const DISMISS_PATIENCE: Duration = Duration::from_millis(2500);

/// What a launching window finds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// No keeper: boot cold.
    None,
    /// A keeper with its browser process up: attach to it.
    Ready,
    /// A keeper that did not come up within `READY_PATIENCE`: it is
    /// dismissed and the window boots cold.
    Dismissed,
}

#[cfg(windows)]
pub use win::{
    keep_alive_after_exit, prepare_launch, signal_launched, wait_for_process_end, Keeper,
};

#[cfg(not(windows))]
pub use stub::{
    keep_alive_after_exit, prepare_launch, signal_launched, wait_for_process_end, Keeper,
};

#[cfg(windows)]
mod win {
    use std::os::windows::process::CommandExt;
    use std::time::Duration;

    use windows::core::HSTRING;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        CreateEventW, OpenEventW, OpenProcess, SetEvent, WaitForSingleObject, CREATE_NO_WINDOW,
        DETACHED_PROCESS, PROCESS_SYNCHRONIZE, SYNCHRONIZATION_SYNCHRONIZE,
    };

    use crate::engine::accounting::ProcessId;

    /// The keeper's side of both events, held for its whole life: the
    /// `warm-ready` flag it sets once its view is up, and the `warm`
    /// signal it waits on. Both are claimed the moment the keeper starts
    /// — a window launching during the boot must find the ready flag to
    /// wait on, and a window signalling during the boot must land on a
    /// handle somebody holds, or the set is lost with the event and the
    /// keeper never hears about the launch.
    pub struct Keeper {
        ready: HANDLE,
        launch: HANDLE,
    }

    impl Keeper {
        pub fn claim() -> Option<Self> {
            let ready = event("warm-ready")?;
            let Some(launch) = event("warm") else {
                unsafe {
                    let _ = CloseHandle(ready);
                }
                return None;
            };
            Some(Self { ready, launch })
        }

        /// The view is up: a window may attach now.
        pub fn announce(&self) {
            if let Err(e) = unsafe { SetEvent(self.ready) } {
                crate::report!("warm keeper ready flag not set: {e}");
            }
        }

        /// Whether a window has already signalled — asked before
        /// announcing, to tell a dismissal during the boot from a launch
        /// that attached afterwards.
        pub fn launch_signalled(&self) -> bool {
            unsafe { WaitForSingleObject(self.launch, 0) == WAIT_OBJECT_0 }
        }

        /// Block until a window launches or `timeout` passes — the
        /// keeper's whole life after boot, off the event-loop thread.
        pub fn wait_for_launch(&self, timeout: Duration) {
            let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
            unsafe {
                WaitForSingleObject(self.launch, millis);
            }
        }
    }

    // SAFETY: event handles are plain kernel handles, usable from any
    // thread; the keeper waits on one from a worker.
    unsafe impl Send for Keeper {}
    unsafe impl Sync for Keeper {}

    impl Drop for Keeper {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.ready);
                let _ = CloseHandle(self.launch);
            }
        }
    }

    /// The keeper's ready flag, waited on for up to `timeout`, and not
    /// kept: a handle held here would keep the event alive after the
    /// keeper is gone, and "gone" is what `prepare_launch` watches for.
    /// `None` is no keeper at all.
    fn ready_within(timeout: Duration) -> Option<bool> {
        let name = HSTRING::from(format!(
            r"Local\{}",
            super::super::object_name("warm-ready")
        ));
        let Ok(handle) = (unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, false, &name) }) else {
            return None;
        };
        let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
        let state = unsafe { WaitForSingleObject(handle, millis) };
        unsafe {
            let _ = CloseHandle(handle);
        }
        Some(state == WAIT_OBJECT_0)
    }

    /// A window about to create its first view. A keeper that is up, or
    /// comes up within `READY_PATIENCE`, is left to be attached to. One
    /// that does not is dismissed — signalled as if the launch were
    /// complete — and the window waits for it to be gone
    /// (`DISMISS_PATIENCE` at most) before booting cold; the keeper ends
    /// its browser process before it exits, so its ready flag vanishing
    /// means the user data folder is free.
    ///
    /// That last wait is a poll: the only thing to wait on is the
    /// *absence* of a kernel object, which has no handle to block on. It
    /// runs before the event loop exists, on a thread with nothing else
    /// to do.
    pub fn prepare_launch() -> super::Standing {
        match ready_within(super::READY_PATIENCE) {
            None => return super::Standing::None,
            Some(true) => return super::Standing::Ready,
            Some(false) => {}
        }
        signal_launched();
        let deadline = std::time::Instant::now() + super::DISMISS_PATIENCE;
        while ready_within(Duration::ZERO).is_some() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        super::Standing::Dismissed
    }

    /// Spawn the keeper for the closing window, telling it which browser
    /// process to outwait. Detached and windowless: it must outlive this
    /// process and never show a console, even from a debug build.
    pub fn keep_alive_after_exit(browser: Option<ProcessId>) {
        if super::keep_for().is_zero() {
            return;
        }
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let mut command = std::process::Command::new(exe);
        command.arg("--warm");
        if let Some(ProcessId(pid)) = browser {
            command.arg(pid.to_string());
        }
        // No inherited stdio: a keeper must not hold a console or a
        // redirected log open after the window that spawned it is gone.
        let spawned = command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(DETACHED_PROCESS.0 | CREATE_NO_WINDOW.0)
            .spawn();
        if let Err(e) = spawned {
            crate::report!("warm keeper not started: {e}");
        }
    }

    /// Block until the process ends, or `OLD_PROCESS_GRACE` passes. A pid
    /// that cannot be opened is one already gone.
    pub fn wait_for_process_end(ProcessId(pid): ProcessId) {
        let Ok(handle) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else {
            return;
        };
        let millis = u32::try_from(super::OLD_PROCESS_GRACE.as_millis()).unwrap_or(u32::MAX - 1);
        unsafe {
            WaitForSingleObject(handle, millis);
            let _ = CloseHandle(handle);
        }
    }

    /// A window is up with its views built — any keeper may go. Opening
    /// the event creates it when no keeper holds one; that copy dies
    /// with the handle below, so a keeper spawned later starts unsignalled.
    pub fn signal_launched() {
        let Some(event) = event("warm") else {
            return;
        };
        unsafe {
            if let Err(e) = SetEvent(event) {
                crate::report!("warm keeper not signalled: {e}");
            }
            let _ = CloseHandle(event);
        }
    }

    /// A named manual-reset event, created unset by whoever comes first.
    /// `None` only when the kernel refuses, which the callers treat as
    /// "no keeper today".
    fn event(kind: &str) -> Option<HANDLE> {
        let name = HSTRING::from(format!(r"Local\{}", super::super::object_name(kind)));
        match unsafe { CreateEventW(None, true, false, &name) } {
            Ok(handle) => Some(handle),
            Err(e) => {
                crate::report!("warm keeper event unavailable: {e}");
                None
            }
        }
    }
}

#[cfg(not(windows))]
mod stub {
    use std::time::Duration;

    use crate::engine::accounting::ProcessId;

    pub struct Keeper;

    impl Keeper {
        pub fn claim() -> Option<Self> {
            None
        }

        pub fn announce(&self) {}

        pub fn launch_signalled(&self) -> bool {
            false
        }

        pub fn wait_for_launch(&self, _: Duration) {}
    }

    pub fn prepare_launch() -> super::Standing {
        super::Standing::None
    }

    pub fn keep_alive_after_exit(_: Option<ProcessId>) {}

    pub fn signal_launched() {}

    pub fn wait_for_process_end(_: ProcessId) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minutes_default_and_zero_turn_off() {
        assert_eq!(minutes(None), DEFAULT_MINUTES);
        assert_eq!(minutes(Some("0")), 0);
        assert_eq!(minutes(Some(" 5 ")), 5);
    }

    #[test]
    fn a_typo_keeps_the_default_rather_than_going_cold() {
        assert_eq!(minutes(Some("")), DEFAULT_MINUTES);
        assert_eq!(minutes(Some("ten")), DEFAULT_MINUTES);
        assert_eq!(minutes(Some("-3")), DEFAULT_MINUTES);
    }
}

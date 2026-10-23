//! The single-instance handoff. An external link makes Windows spawn a
//! second `cryo.exe <url>`; without a handoff that process builds its
//! own window. The second process writes the address into the running
//! window's named pipe and exits — the link becomes a tab instead.
//!
//! A `Local\` mutex names the primary (the window that receives links);
//! a `\\.\pipe` carries the addresses. Both names hang off a hash of the
//! exe path, so a dev build and an installed copy — separate browsers —
//! never steal each other's links. Off Windows there is no handoff:
//! `forwarded` never delivers and `claim` never wins.

#[cfg(windows)]
pub use win::{claim, forwarded, Primary};

#[cfg(not(windows))]
pub use stub::{claim, forwarded, Primary};

#[cfg(windows)]
mod win {
    use std::fmt::Write as _;

    use sha2::{Digest, Sha256};
    use windows::core::{Error, HSTRING, PCWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_PIPE_CONNECTED, HANDLE,
        INVALID_HANDLE_VALUE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FlushFileBuffers, ReadFile, WriteFile, FILE_FLAGS_AND_ATTRIBUTES,
        FILE_GENERIC_WRITE, FILE_SHARE_MODE, OPEN_EXISTING, PIPE_ACCESS_INBOUND,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW,
        PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    };
    use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex};

    /// A message is one launch argument — an address. 64 KiB is absurd
    /// for one already; the cap keeps a garbage-spewing client from
    /// growing the read buffer forever.
    const MAX_MESSAGE: usize = 64 * 1024;

    /// The running primary. Hold it for the app's life: dropping it
    /// releases the claim, which only happens at exit.
    pub struct Primary {
        mutex: HANDLE,
    }

    impl Drop for Primary {
        fn drop(&mut self) {
            unsafe {
                let _ = ReleaseMutex(self.mutex);
                let _ = CloseHandle(self.mutex);
            }
        }
    }

    /// Write `message` into the running primary's pipe. `true` means
    /// delivered — the caller exits; any failure means "no primary" and
    /// the caller opens its own window instead.
    pub fn forwarded(message: &str) -> bool {
        let name = HSTRING::from(pipe_name());
        unsafe {
            // Covers the instant where the primary is between pipe
            // instances; a missing pipe fails fast either way.
            let _ = WaitNamedPipeW(PCWSTR(name.as_ptr()), 2000);
            let Ok(pipe) = CreateFileW(
                PCWSTR(name.as_ptr()),
                FILE_GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            ) else {
                return false;
            };
            let mut written = 0u32;
            let ok = WriteFile(pipe, Some(message.as_bytes()), Some(&mut written), None).is_ok()
                && written as usize == message.len();
            // Flush, then close: the handle must not die while its
            // bytes are still in flight.
            let _ = FlushFileBuffers(pipe);
            let _ = CloseHandle(pipe);
            ok
        }
    }

    /// Become the primary — the window external links land in. The
    /// mutex is the claim; the pipe thread is the mailbox, calling
    /// `notify` with each address a second process hands over. `None`
    /// means another window already holds the claim.
    pub fn claim(notify: impl Fn(String) + Send + 'static) -> Option<Primary> {
        unsafe {
            let mutex = CreateMutexW(None, true, &HSTRING::from(mutex_name())).ok()?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(mutex);
                return None;
            }
            std::thread::spawn(move || serve(notify));
            Some(Primary { mutex })
        }
    }

    /// Accept one client per pipe instance, forever. Between closing one
    /// instance and creating the next there is a sliver where no
    /// instance exists; a client hitting it fails to forward and opens
    /// its own window — the pre-handoff behaviour, not a new failure.
    fn serve(notify: impl Fn(String) + Send + 'static) {
        loop {
            let Ok(pipe) = create_pipe() else {
                crate::report!("instance pipe could not be created — links won't arrive");
                return;
            };
            unsafe {
                // ERROR_PIPE_CONNECTED: a client attached between create
                // and connect — that is connected, not a failure.
                let connected =
                    ConnectNamedPipe(pipe, None).is_ok() || GetLastError() == ERROR_PIPE_CONNECTED;
                if connected {
                    if let Some(message) = read_message(pipe) {
                        notify(message);
                    }
                }
                let _ = DisconnectNamedPipe(pipe);
                let _ = CloseHandle(pipe);
            }
        }
    }

    fn create_pipe() -> windows::core::Result<HANDLE> {
        unsafe {
            let handle = CreateNamedPipeW(
                &HSTRING::from(pipe_name()),
                PIPE_ACCESS_INBOUND,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                8192,
                8192,
                0,
                None,
            );
            if handle == INVALID_HANDLE_VALUE {
                Err(Error::from_thread())
            } else {
                Ok(handle)
            }
        }
    }

    /// Read until the client hangs up — its close is the message end.
    fn read_message(pipe: HANDLE) -> Option<String> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let mut read = 0u32;
            match unsafe { ReadFile(pipe, Some(&mut chunk), Some(&mut read), None) } {
                Ok(()) if read == 0 => break,
                Ok(()) => buf.extend_from_slice(&chunk[..read as usize]),
                // ERROR_BROKEN_PIPE and friends: the client is done.
                Err(_) => break,
            }
            if buf.len() > MAX_MESSAGE {
                break;
            }
        }
        let message = String::from_utf8(buf).ok()?;
        (!message.is_empty()).then_some(message)
    }

    /// Pipe and mutex names hang off the exe path's hash: two installs
    /// are separate browsers, and a dev build never crosses an installed
    /// copy's links.
    fn object_name(kind: &str) -> String {
        let exe = std::env::current_exe()
            .map(|p| p.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let digest = Sha256::digest(exe.as_bytes());
        let mut name = format!("cryo-{kind}-");
        for byte in &digest[..8] {
            let _ = write!(name, "{byte:02x}");
        }
        name
    }

    fn pipe_name() -> String {
        format!(r"\\.\pipe\{}", object_name("link"))
    }

    fn mutex_name() -> String {
        format!(r"Local\{}", object_name("link"))
    }
}

#[cfg(not(windows))]
mod stub {
    /// No claim exists off Windows — a marker for the caller to hold.
    pub struct Primary;

    pub fn forwarded(_: &str) -> bool {
        false
    }

    pub fn claim(_: impl Fn(String) + Send + 'static) -> Option<Primary> {
        None
    }
}

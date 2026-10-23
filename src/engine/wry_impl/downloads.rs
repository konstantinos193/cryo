//! Downloads, in our own UI: `DownloadStarting` hands each operation to
//! progress callbacks (`StateChanged` + `BytesReceivedChanged`) and marks
//! the event `Handled` so WebView2's own flyout stays hidden. Files land
//! where the profile's default said — the path is deduplicated against
//! the folder and against downloads still in flight.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2BytesReceivedChangedEventHandler, ICoreWebView2DownloadOperation,
    ICoreWebView2DownloadStartingEventArgs, ICoreWebView2DownloadStartingEventHandler,
    ICoreWebView2StateChangedEventHandler, ICoreWebView2_4,
    COREWEBVIEW2_DOWNLOAD_INTERRUPT_REASON_NONE,
    COREWEBVIEW2_DOWNLOAD_INTERRUPT_REASON_USER_CANCELED,
    COREWEBVIEW2_DOWNLOAD_INTERRUPT_REASON_USER_PAUSED, COREWEBVIEW2_DOWNLOAD_STATE_COMPLETED,
    COREWEBVIEW2_DOWNLOAD_STATE_INTERRUPTED, COREWEBVIEW2_DOWNLOAD_STATE_IN_PROGRESS,
};
use windows::core::{Interface, PWSTR};
use wry::{WebView, WebViewExtWindows};

use crate::engine::{DownloadId, DownloadState, DownloadUpdate, EngineError};

/// `BytesReceivedChanged` can fire per chunk — faster than any UI
/// repaint. Updates thinner than this collapse into the next one;
/// terminal states always emit.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(80);

/// A live download on a view: the operation handle (for cancel) plus
/// the callbacks that must outlive it.
pub(super) struct Active {
    pub op: ICoreWebView2DownloadOperation,
    _state: ICoreWebView2StateChangedEventHandler,
    _bytes: ICoreWebView2BytesReceivedChangedEventHandler,
}

/// The view's in-flight downloads, keyed by the per-view sequence the
/// updates carry. Shared with the `DownloadStarting` closure, which
/// inserts — and with each `StateChanged` closure, which removes its
/// own entry on a terminal state.
pub(super) type ActiveDownloads = Rc<RefCell<HashMap<u64, Active>>>;

/// Attach `DownloadStarting`: it flags the navigation-abort that means
/// "this became a download" (see `navigation_completed` — the two events
/// race and arrive in either order), and when the spec carries
/// `on_download` it also takes the file over — naming it uniquely,
/// suppressing the engine dialog, and reporting progress. `Ok(None)` on
/// runtimes older than _4: a download there keeps the old behavior of
/// reporting a connection failure.
pub(super) fn attach(
    view: &WebView,
    download_started: Arc<AtomicBool>,
    on_navigate: Option<Rc<dyn Fn(String) -> bool>>,
    on_download: Option<Rc<dyn Fn(DownloadUpdate)>>,
    active: ActiveDownloads,
) -> Result<Option<ICoreWebView2DownloadStartingEventHandler>, EngineError> {
    let Ok(webview) = view.webview().cast::<ICoreWebView2_4>() else {
        return Ok(None);
    };
    let seq = Rc::new(Cell::new(0u64));
    let in_flight: Rc<RefCell<HashSet<PathBuf>>> = Rc::new(RefCell::new(HashSet::new()));
    let wv = webview.clone();
    let handler =
        webview2_com::DownloadStartingEventHandler::create(Box::new(move |_sender, args| {
            download_started.store(true, Ordering::SeqCst);
            if let (Some(args), Some(report)) = (args, on_download.as_ref()) {
                unsafe { starting(&args, report, &seq, &in_flight, &active) }?;
            }
            // Either event order leaves the download's URL written into
            // the tab by the nav-start callback — the document never
            // left this view, so re-report the address it still shows.
            if let Some(on_nav) = &on_navigate {
                let mut source = PWSTR::null();
                unsafe { wv.Source(&mut source) }?;
                on_nav(webview2_com::take_pwstr(source));
            }
            Ok(())
        }));
    let mut token = 0i64;
    unsafe {
        webview
            .add_DownloadStarting(&handler, &mut token)
            .map_err(|e| EngineError::Spawn(e.to_string()))?;
    }
    Ok(Some(handler))
}

/// One download beginning: claim a path, wire progress events, park the
/// handles, announce the file. Any step failing still returns `Err` —
/// the download proceeds unreported rather than half-wired.
unsafe fn starting(
    args: &ICoreWebView2DownloadStartingEventArgs,
    report: &Rc<dyn Fn(DownloadUpdate)>,
    seq: &Cell<u64>,
    in_flight: &Rc<RefCell<HashSet<PathBuf>>>,
    active: &ActiveDownloads,
) -> windows::core::Result<()> {
    let op = args.DownloadOperation()?;
    // Our shelf owns the file now — no engine flyout.
    args.SetHandled(true)?;
    let mut raw = PWSTR::null();
    args.ResultFilePath(&mut raw)?;
    let path = unique_path(&PathBuf::from(webview2_com::take_pwstr(raw)), in_flight);
    args.SetResultFilePath(&windows::core::HSTRING::from(path.as_os_str()))?;
    in_flight.borrow_mut().insert(path.clone());
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let path_str = path.to_string_lossy().into_owned();
    let id = seq.get();
    seq.set(id + 1);

    let state_handler = {
        let report = report.clone();
        let active = active.clone();
        let in_flight = in_flight.clone();
        let path = path.clone();
        let name = name.clone();
        let path_str = path_str.clone();
        webview2_com::StateChangedEventHandler::create(Box::new(move |op, _| {
            let Some(op) = op else { return Ok(()) };
            let update = unsafe { read_update(&op, id, &name, &path_str) }?;
            // Paused reports as interrupted-with-a-reason — the operation
            // is still alive and resumable, so it stays in `active`.
            let terminal = !matches!(
                update.state,
                DownloadState::InProgress | DownloadState::Paused
            );
            report(update);
            if terminal {
                active.borrow_mut().remove(&id);
                in_flight.borrow_mut().remove(&path);
            }
            Ok(())
        }))
    };
    let bytes_handler = {
        let report = report.clone();
        let name = name.clone();
        let path_str = path_str.clone();
        let last = Rc::new(Cell::new(Instant::now() - PROGRESS_INTERVAL));
        webview2_com::BytesReceivedChangedEventHandler::create(Box::new(move |op, _| {
            let Some(op) = op else { return Ok(()) };
            if last.get().elapsed() < PROGRESS_INTERVAL {
                return Ok(());
            }
            last.set(Instant::now());
            report(unsafe { read_update(&op, id, &name, &path_str) }?);
            Ok(())
        }))
    };
    let mut token = 0i64;
    op.add_StateChanged(&state_handler, &mut token)?;
    op.add_BytesReceivedChanged(&bytes_handler, &mut token)?;
    active.borrow_mut().insert(
        id,
        Active {
            op: op.clone(),
            _state: state_handler,
            _bytes: bytes_handler,
        },
    );
    // The first report announces the file — the shelf paints it before
    // the first byte chunk lands.
    report(read_update(&op, id, &name, &path_str)?);
    Ok(())
}

/// Pull the operation's counters and state into one update. A positive
/// `TotalBytesToReceive` is the declared length; zero and negative both
/// mean "unknown".
unsafe fn read_update(
    op: &ICoreWebView2DownloadOperation,
    id: u64,
    name: &str,
    path: &str,
) -> windows::core::Result<DownloadUpdate> {
    let mut received = 0i64;
    op.BytesReceived(&mut received)?;
    let mut total = 0i64;
    op.TotalBytesToReceive(&mut total)?;
    let mut uri_raw = PWSTR::null();
    op.Uri(&mut uri_raw)?;
    let uri = webview2_com::take_pwstr(uri_raw);
    let mut raw = COREWEBVIEW2_DOWNLOAD_STATE_IN_PROGRESS;
    op.State(&mut raw)?;
    let state = match raw {
        s if s == COREWEBVIEW2_DOWNLOAD_STATE_COMPLETED => DownloadState::Done,
        s if s == COREWEBVIEW2_DOWNLOAD_STATE_INTERRUPTED => {
            let mut reason = COREWEBVIEW2_DOWNLOAD_INTERRUPT_REASON_NONE;
            op.InterruptReason(&mut reason)?;
            if reason == COREWEBVIEW2_DOWNLOAD_INTERRUPT_REASON_USER_CANCELED {
                DownloadState::Cancelled
            } else if reason == COREWEBVIEW2_DOWNLOAD_INTERRUPT_REASON_USER_PAUSED {
                DownloadState::Paused
            } else {
                DownloadState::Interrupted
            }
        }
        _ => DownloadState::InProgress,
    };
    Ok(DownloadUpdate {
        id: DownloadId(id),
        name: name.to_string(),
        path: path.to_string(),
        uri,
        received: received.max(0) as u64,
        total: (total > 0).then_some(total as u64),
        state,
    })
}

/// `foo.zip` taken → `foo (1).zip`, `foo (2).zip`, ... — against both
/// the folder on disk and names already claimed by live downloads
/// (whose files may not exist yet).
fn unique_path(suggested: &Path, in_flight: &RefCell<HashSet<PathBuf>>) -> PathBuf {
    let taken = |p: &Path| p.exists() || in_flight.borrow().contains(p);
    if !taken(suggested) {
        return suggested.to_path_buf();
    }
    let dir = suggested.parent().unwrap_or(Path::new(""));
    let stem = suggested
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());
    let ext = suggested
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 1..1000u32 {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !taken(&candidate) {
            return candidate;
        }
    }
    suggested.to_path_buf()
}

/// Where `ContentView::cancel_download` lands: `Cancel` fires the
/// operation's own `StateChanged`, which reports the terminal state and
/// drops the entry — nothing to do here beyond the call.
pub(super) fn cancel(active: &ActiveDownloads, id: DownloadId) -> Result<(), EngineError> {
    let op = active.borrow().get(&id.0).map(|d| d.op.clone());
    if let Some(op) = op {
        unsafe { op.Cancel() }.map_err(|e| EngineError::View(e.to_string()))?;
    }
    Ok(())
}

/// `Pause` interrupts with reason USER_PAUSED — the row reads as held,
/// not dead, and `Resume` picks it back up. Both land in the same
/// `StateChanged` reporting path as `Cancel`.
pub(super) fn pause(active: &ActiveDownloads, id: DownloadId) -> Result<(), EngineError> {
    let op = active.borrow().get(&id.0).map(|d| d.op.clone());
    if let Some(op) = op {
        unsafe { op.Pause() }.map_err(|e| EngineError::View(e.to_string()))?;
    }
    Ok(())
}

pub(super) fn resume(active: &ActiveDownloads, id: DownloadId) -> Result<(), EngineError> {
    let op = active.borrow().get(&id.0).map(|d| d.op.clone());
    if let Some(op) = op {
        unsafe { op.Resume() }.map_err(|e| EngineError::View(e.to_string()))?;
    }
    Ok(())
}

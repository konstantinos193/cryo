//! WebView2 event adapter API, grouped by the event family it owns.
mod failure;
mod input;
mod navigation;
mod presentation;
pub(super) use input::attach_accelerator;
pub(super) use navigation::{
    attach_navigation_completed, attach_navigation_starting, attach_source_changed, NavState,
    UNCLASSIFIED_RETRIES,
};
pub(super) use presentation::{attach_favicon, attach_fullscreen_change, attach_process_failed};

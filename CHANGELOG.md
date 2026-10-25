# Changelog

## 0.7.1

### Fixed

- Faster startup on Windows: all views share one WebView2 environment instead of each spawning its own browser process.
- The window stays hidden until its views exist, so the first frame is the finished layout rather than views appearing one by one.

## 0.7.0

### Added

- Saved-password autofill with origin checks and explicit fill/save cards in trusted chrome.
- Origin-bound, session-only permission prompts on Windows for microphone, camera, location, notifications, and clipboard access. Unsupported permission types and backends deny access.

### Fixed

- Dropped or failed webviews cannot send callbacks into replacement tabs. Popup and keyboard callbacks carry the same generation checks as page and download events.
- History, bookmark, and vault writes run on bounded background workers, retry temporary failures, report errors to the window, and drain on shutdown.
- Internal documents and browser commands require explicit view privileges; content views cannot fetch chrome documents or send privileged commands.
- Tab lifecycle transitions invalidate owned prompts and interrupt downloads. Closed-tab download records remain available on the shelf.
- Legacy persisted WebView2 permission grants are cleared before external navigation; new consent remains scoped to the window.

### Refactored

- Budget decisions live in the memory context, separate from surface construction and window layout.
- Chrome markup, styles, scripts, wire types, and navigation policy have separate owners while retaining ordered script initialization.
- Tab registries, persistence services, import parsing, and WebView2 event families are separated into focused modules.

### Verification

- Formatting, Clippy with warnings denied, locked build, and 268 Rust tests pass on Windows.
- JavaScript startup, library panes, and permission actions pass a DOM-stub smoke check.
- Native desktop smoke testing was interrupted; full end-to-end capture-device verification is not claimed.

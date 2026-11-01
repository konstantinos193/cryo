//! Shell chrome: the browser's own UI — tab strip, address bar, layout.
//! Renders as one engine view (`chrome`) sitting above the content views.

pub mod chrome;
pub mod dial;
pub mod error;
pub mod geometry;
pub mod newtab;
pub mod pages;
pub mod search;
pub mod settings;
pub mod theme;

use crate::engine::Rect;

/// Height of the toolbar row inside the chrome strip.
pub const BAR_HEIGHT: f64 = 44.0;

/// Height of the speed-dial row below the toolbar.
pub const DIAL_HEIGHT: f64 = 32.0;

/// Height of the chrome strip in logical pixels: toolbar + dial bar.
pub const CHROME_HEIGHT: f64 = BAR_HEIGHT + DIAL_HEIGHT;

/// Split a logical window size into (chrome, content) rects.
pub fn layout(width: f64, height: f64) -> (Rect, Rect) {
    let chrome = Rect {
        x: 0.0,
        y: 0.0,
        w: width,
        h: CHROME_HEIGHT.min(height),
    };
    let content = Rect {
        x: 0.0,
        y: chrome.h,
        w: width,
        h: (height - chrome.h).max(0.0),
    };
    (chrome, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_stacks_chrome_over_content() {
        let (chrome, content) = layout(800.0, 600.0);
        assert_eq!(chrome.h, CHROME_HEIGHT);
        assert_eq!(content.y, CHROME_HEIGHT);
        assert_eq!(chrome.h + content.h, 600.0);
    }

    #[test]
    fn tiny_window_does_not_go_negative() {
        let (_chrome, content) = layout(800.0, 10.0);
        assert_eq!(content.h, 0.0);
    }
}

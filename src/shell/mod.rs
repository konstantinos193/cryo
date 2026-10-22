//! Shell chrome: the browser's own UI — tab strip, address bar, layout.
//! Renders as one engine view (`chrome`) sitting above the content views.

pub mod chrome;
pub mod pages;

use crate::engine::Rect;

/// Height of the chrome strip in logical pixels.
pub const CHROME_HEIGHT: f64 = 44.0;

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

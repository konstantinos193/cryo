//! Window placement remembered across sessions — how big the window
//! was, where it sat, and whether it was maximized when it closed. The
//! wire DTO lives here; `Placement` is the value object `lib` applies
//! to the builder and the app writes back at close (AGENTS §6).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where the window last sat, in logical units — the units
/// `WindowBuilder::with_inner_size` speaks. A value object: applied to
/// the builder at launch, replaced wholesale on save.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Inner size last seen while not maximized — a maximized close
    /// still reopens at the size the user had before maximizing.
    pub width: f64,
    pub height: f64,
    /// Outer position; `None` lets the OS place the window.
    pub x: Option<f64>,
    pub y: Option<f64>,
    /// Closed while maximized — reopen maximized.
    pub maximized: bool,
}

const CURRENT_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlacementFile {
    version: u32,
    width: f64,
    height: f64,
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
    #[serde(default)]
    maximized: bool,
}

/// `window.json` under the per-user data dir (`crate::data_dir`).
pub fn default_path() -> PathBuf {
    crate::data_dir().join("window.json")
}

/// The stored placement, or `None` — a missing, stale-version or
/// corrupt file all mean "place it yourself", never a launch failure.
pub fn load(path: &Path) -> Option<Placement> {
    let bytes = crate::read_file(path).ok()??;
    let file: PlacementFile = serde_json::from_slice(&bytes).ok()?;
    if file.version != CURRENT_VERSION {
        return None;
    }
    // A hand-edited zero or NaN size must not reach the builder — a
    // window that can't exist is worse than a default-sized one.
    (file.width.is_finite() && file.height.is_finite() && file.width > 0.0 && file.height > 0.0)
        .then_some(Placement {
            width: file.width,
            height: file.height,
            x: file.x.filter(|v| v.is_finite()),
            y: file.y.filter(|v| v.is_finite()),
            maximized: file.maximized,
        })
}

/// Persist where the window ended up. One tiny file written at close —
/// atomic through `crate::write_file_atomic` so a crash mid-write keeps
/// the last good placement.
pub fn save(path: &Path, p: &Placement) -> Result<(), std::io::Error> {
    let file = PlacementFile {
        version: CURRENT_VERSION,
        width: p.width,
        height: p.height,
        x: p.x,
        y: p.y,
        maximized: p.maximized,
    };
    let bytes = serde_json::to_vec(&file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    crate::write_file_atomic(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        crate::test_dir(&format!("geometry-{tag}")).join("window.json")
    }

    #[test]
    fn missing_file_places_itself() {
        assert_eq!(load(&temp("missing")), None);
    }

    #[test]
    fn saved_placement_round_trips() {
        let path = temp("roundtrip");
        let p = Placement {
            width: 1024.0,
            height: 640.0,
            x: Some(40.0),
            y: Some(60.0),
            maximized: false,
        };
        save(&path, &p).unwrap();
        assert_eq!(load(&path), Some(p));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_size_no_window_can_have_is_refused() {
        let path = temp("badsize");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"version":1,"width":0,"height":800}"#).unwrap();
        assert_eq!(load(&path), None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn corrupt_file_places_itself() {
        let path = temp("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(load(&path), None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

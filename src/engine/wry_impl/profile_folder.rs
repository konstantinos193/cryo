//! Where a WebView2 user data folder lives, and moving it there.
//!
//! WebView2's default is a folder beside the executable, which ties every
//! cache read and cookie commit to whatever disk the program happens to be
//! installed on. Installed on an external USB drive, cold reads measured
//! ~41 ms against ~0.8 ms on the machine's NVMe disks, and the same pages
//! took about 45% longer to load. The folder belongs under the data root
//! with every other store.
//!
//! Moving it is a copy of a profile that holds the user's sessions, so it
//! never happens where someone is waiting (thousands of small files on a
//! slow disk take minutes) and never destroys the original. The `--warm`
//! keeper does it after the last window closes and before booting its own
//! browser process; `current` only ever looks. A folder that has not moved
//! keeps working exactly as before, and the next close tries again.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// The folder WebView2 itself creates beneath the one it is given.
const BROWSER_FOLDER: &str = "EBWebView";

/// The file a browser process holds exclusively for as long as it runs.
const BROWSER_LOCK: &str = "lockfile";

/// Everything here the engine rebuilds on demand. Together they are more
/// than half of a used profile and none of what a reader would miss, and
/// they are exactly the part that made the old location slow.
const REBUILT_ON_DEMAND: [&str; 7] = [
    "Cache",
    "Code Cache",
    "DawnGraphiteCache",
    "DawnWebGPUCache",
    "GPUCache",
    "GrShaderCache",
    "ShaderCache",
];

/// How a `relocate` call ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relocation {
    /// The profile now lives at home.
    Moved,
    /// Nothing to do: already at home, or there was never a profile.
    NotNeeded,
    /// A browser process or another keeper has the folder, or a window
    /// launched mid-copy. The old folder stays current.
    Deferred,
}

/// WebView2's default folder for this executable.
pub fn beside_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut name = exe.file_name()?.to_os_string();
    name.push(".WebView2");
    Some(exe.with_file_name(name))
}

/// The folder to open. A profile at home wins; otherwise one still at the
/// old location stays in use until it has been moved, so that opening a
/// window can never look like being signed out of everything.
pub fn current(home: &Path, old: &Path) -> PathBuf {
    if has_profile(home) || !has_profile(old) {
        home.to_path_buf()
    } else {
        old.to_path_buf()
    }
}

/// Copy the profile from `old` to `home`. `home` only appears, by rename,
/// once the whole copy is good, so an interrupted run leaves `old` current
/// and nothing half-built to open. `old` is left in place. `cancelled` is
/// asked between files: a launching window takes the old folder, and a
/// copy of a profile in use is not a copy worth keeping.
pub fn relocate(home: &Path, old: &Path, cancelled: impl Fn() -> bool) -> io::Result<Relocation> {
    if has_profile(home) || !has_profile(old) {
        return Ok(Relocation::NotNeeded);
    }
    let Some(_only_mover) = exclusive(&sibling(home, "moving.lock"))? else {
        return Ok(Relocation::Deferred);
    };
    if browser_holds(old)? {
        return Ok(Relocation::Deferred);
    }
    let staging = sibling(home, "moving");
    remove_if_present(&staging)?;
    let finished = copy_profile(old, &staging, &cancelled)
        .and_then(|copied| Ok(copied && !browser_holds(old)?));
    match finished {
        Ok(true) => {
            // An empty folder is not a profile, and would block the rename.
            if home.is_dir() {
                fs::remove_dir_all(home)?;
            }
            fs::rename(&staging, home)?;
            Ok(Relocation::Moved)
        }
        Ok(false) => {
            remove_if_present(&staging)?;
            Ok(Relocation::Deferred)
        }
        Err(e) => {
            remove_if_present(&staging)?;
            Err(e)
        }
    }
}

fn has_profile(folder: &Path) -> bool {
    folder.join(BROWSER_FOLDER).is_dir()
}

fn sibling(home: &Path, suffix: &str) -> PathBuf {
    let mut name = home.as_os_str().to_os_string();
    name.push(".");
    name.push(suffix);
    PathBuf::from(name)
}

fn remove_if_present(folder: &Path) -> io::Result<()> {
    match fs::remove_dir_all(folder) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Open `path` with no sharing at all. `None` is a sharing violation:
/// someone else holds it, which is an answer rather than a failure.
fn exclusive(path: &Path) -> io::Result<Option<File>> {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let opened = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(path);
    match opened {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => Ok(None),
        Err(e) => Err(e),
    }
}

fn browser_holds(folder: &Path) -> io::Result<bool> {
    Ok(exclusive(&folder.join(BROWSER_FOLDER).join(BROWSER_LOCK))?.is_none())
}

/// `Ok(false)` when `cancelled` stopped the copy before it finished.
fn copy_profile(old: &Path, staging: &Path, cancelled: &impl Fn() -> bool) -> io::Result<bool> {
    let mut pending = vec![PathBuf::from(BROWSER_FOLDER)];
    while let Some(relative) = pending.pop() {
        fs::create_dir_all(staging.join(&relative))?;
        for entry in fs::read_dir(old.join(&relative))? {
            if cancelled() {
                return Ok(false);
            }
            let entry = entry?;
            let name = entry.file_name();
            let path = relative.join(&name);
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if !REBUILT_ON_DEMAND.iter().any(|skip| name == *skip) {
                    pending.push(path);
                }
            } else if kind.is_file() && name != BROWSER_LOCK {
                fs::copy(old.join(&path), staging.join(&path))?;
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Places {
        root: PathBuf,
        home: PathBuf,
        old: PathBuf,
    }

    impl Places {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let root = std::env::temp_dir().join(format!(
                "cryo-profile-folder-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            Self {
                home: root.join("data").join("webview"),
                old: root.join("install").join("cryo.exe.WebView2"),
                root,
            }
        }

        /// A profile with the shape that matters: sessions and rebuildable
        /// caches, at both the top level and inside `Default`.
        fn with_old_profile(self) -> Self {
            for (file, body) in [
                ("EBWebView/Local State", "key"),
                ("EBWebView/lockfile", ""),
                ("EBWebView/GrShaderCache/data_0", "cache"),
                ("EBWebView/Default/Network/Cookies", "session"),
                ("EBWebView/Default/Cache/Cache_Data/f_1", "cache"),
                ("EBWebView/Default/Code Cache/js/index", "cache"),
            ] {
                let path = self.old.join(file);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, body).unwrap();
            }
            self
        }
    }

    impl Drop for Places {
        fn drop(&mut self) {
            if let Err(e) = remove_if_present(&self.root) {
                eprintln!("test folder left behind: {e}");
            }
        }
    }

    #[test]
    fn an_unmoved_profile_stays_current_so_nobody_is_signed_out() {
        let places = Places::new().with_old_profile();
        assert_eq!(current(&places.home, &places.old), places.old);
    }

    #[test]
    fn a_fresh_install_starts_at_home() {
        let places = Places::new();
        assert_eq!(current(&places.home, &places.old), places.home);
    }

    #[test]
    fn moving_keeps_sessions_drops_caches_and_leaves_the_original() {
        let places = Places::new().with_old_profile();
        assert_eq!(
            relocate(&places.home, &places.old, || false).unwrap(),
            Relocation::Moved
        );
        let moved = places.home.join(BROWSER_FOLDER);
        assert_eq!(
            fs::read_to_string(moved.join("Default/Network/Cookies")).unwrap(),
            "session"
        );
        assert!(moved.join("Local State").is_file());
        assert!(!moved.join("Default/Cache").exists());
        assert!(!moved.join("Default/Code Cache").exists());
        assert!(!moved.join("GrShaderCache").exists());
        assert!(!moved.join(BROWSER_LOCK).exists());
        assert!(places
            .old
            .join("EBWebView/Default/Network/Cookies")
            .is_file());
        assert_eq!(current(&places.home, &places.old), places.home);
        assert!(!sibling(&places.home, "moving").exists());
    }

    #[test]
    fn a_running_browser_defers_the_move() {
        let places = Places::new().with_old_profile();
        let _browser = exclusive(&places.old.join("EBWebView").join(BROWSER_LOCK))
            .unwrap()
            .unwrap();
        assert_eq!(
            relocate(&places.home, &places.old, || false).unwrap(),
            Relocation::Deferred
        );
        assert_eq!(current(&places.home, &places.old), places.old);
    }

    #[test]
    fn a_launch_mid_copy_leaves_nothing_half_built() {
        let places = Places::new().with_old_profile();
        assert_eq!(
            relocate(&places.home, &places.old, || true).unwrap(),
            Relocation::Deferred
        );
        assert!(!places.home.exists());
        assert!(!sibling(&places.home, "moving").exists());
        assert_eq!(current(&places.home, &places.old), places.old);
    }

    #[test]
    fn leftovers_of_an_interrupted_move_are_not_mistaken_for_a_profile() {
        let places = Places::new().with_old_profile();
        let staging = sibling(&places.home, "moving").join(BROWSER_FOLDER);
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("stale"), "x").unwrap();
        assert_eq!(current(&places.home, &places.old), places.old);
        relocate(&places.home, &places.old, || false).unwrap();
        assert!(!places.home.join(BROWSER_FOLDER).join("stale").exists());
    }

    #[test]
    fn an_empty_home_does_not_block_the_move() {
        let places = Places::new().with_old_profile();
        fs::create_dir_all(&places.home).unwrap();
        assert_eq!(
            relocate(&places.home, &places.old, || false).unwrap(),
            Relocation::Moved
        );
    }

    #[test]
    fn a_profile_at_home_is_never_overwritten() {
        let places = Places::new().with_old_profile();
        let kept = places.home.join(BROWSER_FOLDER).join("kept");
        fs::create_dir_all(kept.parent().unwrap()).unwrap();
        fs::write(&kept, "mine").unwrap();
        assert_eq!(
            relocate(&places.home, &places.old, || false).unwrap(),
            Relocation::NotNeeded
        );
        assert_eq!(fs::read_to_string(kept).unwrap(), "mine");
    }
}

//! Source reading, format detection and decoding into domain drafts.
use super::*;

/// Read a source into drafts.
///
/// `stamped_at` is the time rows get where the format carries none —
/// password CSVs have no usable timestamp. The caller stamps, so nothing
/// here reads a clock (AGENTS §6).
pub fn read(source: &Source, stamped_at: u64) -> Result<Bundle, TransferError> {
    let path = source.path();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());

    // A profile's file is known by where it came from, so it skips the
    // sniffing and any doubt about a 30 MB one.
    if let Source::Profile(profile) = source {
        let bytes = std::fs::read(&profile.bookmarks).map_err(|source| TransferError::Read {
            path: profile.bookmarks.display().to_string(),
            source,
        })?;
        let bookmarks = if firefox::looks_like(&bytes) {
            firefox::parse_bytes(&bytes)?
        } else {
            chromium::parse(&bytes)?
        };
        return Ok(Bundle {
            payload: Payload::Bookmarks,
            bookmarks,
            source: format!("{} · {}", profile.browser, profile.name),
            ..Bundle::default()
        });
    }

    let bytes = read_bytes(path)?;
    // A compressed Firefox backup is binary; check its magic before
    // any decoding turns it into noise.
    if firefox::looks_like(&bytes) {
        return Ok(Bundle {
            payload: Payload::Bookmarks,
            bookmarks: firefox::parse_bytes(&bytes)?,
            source: name,
            ..Bundle::default()
        });
    }
    let text = decode(&bytes);
    // Order matters only where two sniffs could both answer: a password
    // CSV is asked about before a history one, because losing a
    // password file to the wrong reader is the worse mistake.
    if chromium::looks_like(&text) {
        return Ok(Bundle {
            payload: Payload::Bookmarks,
            bookmarks: chromium::parse(text.as_bytes())?,
            source: name,
            ..Bundle::default()
        });
    }
    if firefox::looks_like_json(&text) {
        return Ok(Bundle {
            payload: Payload::Bookmarks,
            bookmarks: firefox::parse(text.as_bytes())?,
            source: name,
            ..Bundle::default()
        });
    }
    if netscape::looks_like(&text) {
        return Ok(Bundle {
            payload: Payload::Bookmarks,
            bookmarks: netscape::parse(&text),
            source: name,
            ..Bundle::default()
        });
    }
    if passwords::looks_like(&text) {
        return Ok(Bundle {
            payload: Payload::Passwords,
            credentials: passwords::parse(&text, stamped_at)?,
            source: name,
            ..Bundle::default()
        });
    }
    if history::looks_like(&text) {
        let (visits, skipped) = history::parse(&text)?;
        return Ok(Bundle {
            payload: Payload::History,
            visits,
            skipped,
            source: name,
            ..Bundle::default()
        });
    }
    Err(TransferError::Unrecognized(name))
}

/// Everything an export writes goes out through here, so a failed write
/// always names the file (AGENTS §7).
pub fn write(path: &Path, text: &str) -> Result<(), TransferError> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|source| TransferError::Write {
            path: path.display().to_string(),
            source,
        })?;
    }
    std::fs::write(path, text).map_err(|source| TransferError::Write {
        path: path.display().to_string(),
        source,
    })
}

/// Read a file's bytes, bounded by `MAX_FILE` — anything past that is
/// not one of these formats, and reading it would hand a browser's
/// worth of memory to a text parser.
fn read_bytes(path: &Path) -> Result<Vec<u8>, TransferError> {
    let size = std::fs::metadata(path)
        .map_err(|source| TransferError::Read {
            path: path.display().to_string(),
            source,
        })?
        .len();
    if size > MAX_FILE {
        return Err(TransferError::TooBig {
            path: path.display().to_string(),
            bytes: size,
        });
    }
    std::fs::read(path).map_err(|source| TransferError::Read {
        path: path.display().to_string(),
        source,
    })
}

/// Bytes to text: UTF-16 where a byte-order mark says so, UTF-8 (lossy)
/// otherwise.
pub(super) fn decode(bytes: &[u8]) -> String {
    let utf16 = |pairs: &[u8], little: bool| {
        let units: Vec<u16> = pairs
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| {
                if little {
                    u16::from_le_bytes(*p)
                } else {
                    u16::from_be_bytes(*p)
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xff, 0xfe, rest @ ..] => utf16(rest, true),
        [0xfe, 0xff, rest @ ..] => utf16(rest, false),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

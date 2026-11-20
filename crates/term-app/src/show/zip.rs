//! Preview a ZIP's central directory without opening or inflating any member.

use super::{human_size, printable};
use rawzip::{FileReader, RECOMMENDED_BUFFER_SIZE, ZipLocator};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;

pub(crate) struct Listing {
    pub header: String,
    pub lines: Vec<String>,
}

/// Returns at most 32 member lines in archive order, or `None` to keep the
/// normal preview when the file isn't a ZIP or its directory is unreadable.
pub(crate) fn listing(path: &Path, file: &File, rows: usize) -> Option<Listing> {
    // Office and other ZIP-based formats keep their native document previews.
    if !path.extension()?.eq_ignore_ascii_case("zip") {
        return None;
    }
    let mut magic = [0; 4];
    file.read_exact_at(&mut magic, 0).ok()?;
    if !matches!(&magic, b"PK\x03\x04" | b"PK\x05\x06") {
        return None;
    }

    // Positioned reads leave the fallback reader at the start, even on failure.
    let reader = FileReader::from(file.try_clone().ok()?);
    let mut buf = vec![0; RECOMMENDED_BUFFER_SIZE];
    let archive = ZipLocator::new()
        .locate_in_reader(reader, &mut buf, file.metadata().ok()?.len())
        .ok()?;
    let count = archive.entries_hint();
    let mut entries = archive.entries(&mut buf);
    let mut lines = Vec::new();
    let limit = rows.clamp(1, 32);
    while let Some(entry) = entries.next_entry().ok()? {
        if lines.len() as u64 >= count {
            return None;
        }
        if lines.len() == limit {
            return Some(Listing {
                header: format!("ZIP · first {limit} of {count} entries"),
                lines,
            });
        }
        let size = if entry.is_dir() {
            "dir".to_owned()
        } else {
            human_size(entry.uncompressed_size_hint())
        };
        let path = String::from_utf8_lossy(entry.file_path().as_bytes());
        let mut chars = path.chars();
        let mut name: String = chars.by_ref().take(120).collect();
        if chars.next().is_some() {
            name.push('…');
        }
        lines.push(format!("{size:>10}  {}", printable(&name)));
    }
    if lines.len() as u64 != count {
        return None;
    }
    let noun = if count == 1 { "entry" } else { "entries" };
    Some(Listing {
        header: format!("ZIP · {count} {noun}"),
        lines,
    })
}

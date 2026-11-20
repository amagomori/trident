//! Preview a ZIP's central directory without opening or inflating any member.

use super::{human_size, printable};
use rawzip::{
    FileReader, RECOMMENDED_BUFFER_SIZE, ZipFileHeaderRecord, ZipLocator,
    extra_fields::ExtraFieldId,
};
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
        lines.push(format!("{size:>10}  {}", member_name(&entry)));
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

fn member_name(entry: &ZipFileHeaderRecord<'_>) -> String {
    let raw = entry.file_path().as_bytes();
    if entry.flags().is_utf8() {
        return clipped(String::from_utf8_lossy(raw).chars());
    }
    // Info-ZIP can pair a legacy name with its UTF-8 spelling. Its CRC
    // ties that spelling to the name actually stored in the directory.
    if let Some((_, [1, field @ ..])) = entry
        .extra_fields()
        .find(|(id, _)| *id == ExtraFieldId::INFO_ZIP_UNICODE_PATH)
        && let Some((expected, name)) = field.split_first_chunk::<4>()
        && rawzip::crc32(raw) == u32::from_le_bytes(*expected)
        && let Ok(name) = std::str::from_utf8(name)
    {
        return clipped(name.chars());
    }
    clipped(raw.iter().copied().map(oem_cp::Cp437::from).map(char::from))
}

fn clipped(mut chars: impl Iterator<Item = char>) -> String {
    let mut name: String = chars.by_ref().take(120).collect();
    if chars.next().is_some() {
        name.push('…');
    }
    printable(&name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawzip::{Header, ZipArchive, ZipArchiveWriter};

    #[test]
    fn displays_utf8_and_verified_unicode_paths() {
        let mut writer = ZipArchiveWriter::new(Vec::new());
        writer.new_dir("café/").create().unwrap();
        let mut field = vec![1];
        field.extend(rawzip::crc32(b"legacy/").to_le_bytes());
        field.extend("日本/".as_bytes());
        for name in ["legacy/", "changed/"] {
            writer
                .new_dir(name)
                .extra_field(ExtraFieldId::INFO_ZIP_UNICODE_PATH, &field, Header::CENTRAL)
                .unwrap()
                .create()
                .unwrap();
        }
        let data = writer.finish().unwrap();
        let archive = ZipArchive::from_slice(&data).unwrap();
        let names: Vec<_> = archive
            .entries()
            .map(|e| member_name(&e.unwrap()))
            .collect();
        assert_eq!(names, ["café/", "日本/", "changed/"]);
    }
}

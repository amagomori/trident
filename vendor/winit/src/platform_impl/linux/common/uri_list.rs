//! Decode local file URLs from a text/uri-list drag offer.

use std::ffi::OsStr;
use std::io::{self, ErrorKind};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use percent_encoding::percent_decode;

/// Parses a file-manager drop into absolute local paths in source order.
///
/// Accepts LF or CRLF; rejects empty offers, non-local URIs and NUL.
pub fn parse_uri_list(data: &[u8]) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for line in data.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() || line.starts_with(b"#") {
            continue;
        }
        let path = line
            .strip_prefix(b"file://")
            .filter(|p| p.starts_with(b"/"))
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "expected a local file URI"))?;
        let bytes: Vec<u8> = percent_decode(path).collect();
        if bytes.contains(&0) {
            return Err(io::Error::new(ErrorKind::InvalidData, "NUL in file URI"));
        }
        paths.push(PathBuf::from(OsStr::from_bytes(&bytes)));
    }
    if paths.is_empty() {
        return Err(io::Error::new(ErrorKind::InvalidData, "empty URI list"));
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_list_handles_both_line_endings_and_preserves_order() {
        let paths =
            parse_uri_list(b"# comment\r\nfile:///one%20file\n\nfile:///two\r\nfile:///three")
                .unwrap();
        assert_eq!(
            paths,
            [PathBuf::from("/one file"), PathBuf::from("/two"), PathBuf::from("/three")]
        );
    }

    #[test]
    fn uri_list_decodes_filename_bytes_after_splitting() {
        let paths = parse_uri_list(b"file:///a%0D%0Ab%FF%23%25\n").unwrap();
        assert_eq!(paths[0].as_os_str().as_bytes(), b"/a\r\nb\xff#%");
    }

    #[test]
    fn uri_list_rejects_nonlocal_and_empty_offers() {
        for data in ["", "# comment\n", "https://host/path", "file://host/path", "file:///%00"] {
            assert!(parse_uri_list(data.as_bytes()).is_err(), "{data:?}");
        }
    }
}

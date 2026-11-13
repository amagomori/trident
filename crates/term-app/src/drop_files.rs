//! Prototype file drops; none of this runs on the normal text-output path.

use std::path::Path;
use term_core::Terminal;

/// POSIX shell quoting, preserving Unix filename bytes, with a separator for multi-drop.
pub fn quoted_path(path: &Path, bracketed: bool) -> Option<Vec<u8>> {
    #[cfg(unix)]
    let path = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes()
    };
    #[cfg(not(unix))]
    let path = path.to_str()?.as_bytes();
    // Quotes do not protect a live line editor from Enter, ESC, or editing controls.
    if path.iter().any(u8::is_ascii_control) {
        return None;
    }
    let mut bytes = Vec::new();
    if bracketed {
        bytes.extend_from_slice(b"\x1b[200~");
    }
    bytes.push(b'\'');
    for &byte in path {
        if byte == b'\'' {
            bytes.extend_from_slice(b"'\\''");
        } else {
            bytes.push(byte);
        }
    }
    bytes.extend_from_slice(b"' ");
    if bracketed {
        bytes.extend_from_slice(b"\x1b[201~");
    }
    Some(bytes)
}

/// Insert room above the prompt, moving the existing prompt/typed text down intact.
/// The graphic bytes are generated internally by `show`, never read from file contents.
pub fn insert_preview(term: &mut Terminal, image: &[u8], rows: usize) {
    let Some((row, col)) = term.cursor_position() else {
        return;
    };
    if rows == 0 || rows >= term.rows() {
        return;
    }
    let scroll = (row + rows + 1).saturating_sub(term.rows());
    let top = row - scroll;
    let mut output = Vec::new();
    if scroll > 0 {
        output.extend_from_slice(format!("\x1b[{scroll}S").as_bytes());
    }
    output.extend_from_slice(format!("\x1b[{};1H\x1b[{rows}L", top + 1).as_bytes());
    output.extend_from_slice(image);
    output.extend_from_slice(format!("\x1b[{};{}H", top + rows + 1, col + 1).as_bytes());
    term.advance(&output);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_path_survives_shell_metacharacters() {
        let path = "/tmp/Jarett's image $(false); \"hi\" 画.png";
        let quoted = quoted_path(Path::new(path), false).unwrap();
        let output = std::process::Command::new("sh")
            .args([
                "-c",
                &format!("printf '%s' {}", String::from_utf8(quoted).unwrap()),
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, path.as_bytes());
        assert_eq!(
            quoted_path(Path::new("/tmp/a b"), true).unwrap(),
            b"\x1b[200~'/tmp/a b' \x1b[201~"
        );
    }

    #[test]
    fn line_editor_controls_are_not_injected() {
        for path in ["/tmp/a\nb", "/tmp/a\rb", "/tmp/\x1b[201~", "/tmp/a\x7fb"] {
            assert!(quoted_path(Path::new(path), true).is_none());
            assert!(quoted_path(Path::new(path), false).is_none());
        }
    }

    #[test]
    fn preview_keeps_prompt_and_cursor_even_at_bottom() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/sample.png");
        let (image, rows) = crate::show::drop_preview(&path, 80, 24, (8.0, 16.0)).unwrap();
        for row in [0, 10, 23] {
            let mut term = Terminal::new(80, 24);
            term.set_cell_pixel_size(8.0, 16.0);
            term.advance(format!("\x1b[{};1H$ echo ", row + 1).as_bytes());
            insert_preview(&mut term, &image, rows);
            let (cursor_row, col) = term.cursor_position().unwrap();
            assert_eq!(cursor_row, (row + rows).min(23));
            assert_eq!(col, 7);
            term.advance(b"'file.png'");
            let line: String = term
                .viewport_row(cursor_row)
                .cells
                .iter()
                .map(|c| c.c)
                .collect();
            assert!(line.starts_with("$ echo 'file.png'"));
            assert_eq!(term.visible_placements().len(), 1);
        }
    }
}

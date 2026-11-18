//! The target of a hovered OSC 8 link, drawn in a window corner so the printed text
//! cannot hide where a click goes.

use term_core::Link;

/// Whether the strip at `y` overlaps any linked row, including separated OSC 8 pieces.
pub(crate) fn covers_link(
    link: &Link,
    y: f32,
    cell_height: f32,
    first_line: u64,
    grid_top: f32,
) -> bool {
    let row_y = |line: u64| {
        let rows = line.abs_diff(first_line) as f32;
        grid_top + if line < first_line { -rows } else { rows } * cell_height
    };
    std::iter::once((link.start, link.end))
        .chain(link.pieces.iter().copied())
        .any(|(start, end)| {
            row_y(start.line) < y + cell_height && y < row_y(end.line) + cell_height
        })
}

/// Places the strip in physical pixels. Find and the pinned header are reserved bands.
/// If no clear position exists, prefer covering part of the preview over linked text.
pub(crate) fn place(
    [width, height]: [f32; 2],
    [strip_width, ch]: [f32; 2],
    [find, header]: [Option<(f32, f32)>; 2],
    preview: Option<[f32; 4]>,
    grid_top: f32,
    covers_link: impl Fn(f32) -> bool,
) -> Option<[f32; 2]> {
    if height < ch || width < strip_width {
        return None;
    }
    let bottom = height - ch;
    let fits = |y: f32| {
        (0.0..=bottom).contains(&y)
            && [find, header]
                .into_iter()
                .flatten()
                .all(|(t, b)| b <= y || t >= y + ch)
    };
    let row_origin = grid_top.rem_euclid(ch);
    let positions = || {
        [
            Some(bottom),
            Some(0.0),
            header.map(|(_, b)| b),
            find.map(|(t, _)| t - ch),
            preview.map(|[_, t, _, _]| t - ch),
            preview.map(|[_, _, _, b]| b),
        ]
        .into_iter()
        .flatten()
        .chain((0..=(bottom / ch) as usize).map(|row| row_origin + row as f32 * ch))
        .filter(|&y| fits(y))
    };
    positions()
        .filter(|&y| !covers_link(y))
        .find_map(|y| {
            let Some([l, t, r, b]) = preview else {
                return Some([0.0, y]);
            };
            if l >= strip_width || r <= 0.0 || b <= y || t >= y + ch {
                Some([0.0, y])
            } else if r + strip_width <= width {
                Some([width - strip_width, y])
            } else {
                None
            }
        })
        .or_else(|| positions().find(|&y| !covers_link(y)).map(|y| [0.0, y]))
        .or_else(|| positions().next().map(|y| [0.0, y]))
}

/// `uri` as shown: printable ASCII as is, every other character percent-encoded so
/// look-alike letters, bidirectional controls and invisible characters stay visible.
pub fn sanitize(uri: &str) -> String {
    let mut out = String::with_capacity(uri.len());
    for c in uri.chars() {
        if c.is_ascii_graphic() {
            out.push(c);
        } else {
            let mut buf = [0; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

/// `text` in at most `columns` cells: when it is longer, the start (scheme and host) and
/// the end are kept around an ellipsis.
pub fn elide(text: &str, columns: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= columns {
        return text.to_owned();
    }
    if columns == 0 {
        return String::new();
    }
    let keep = columns - 1;
    let tail = keep / 3;
    let head = keep - tail;
    chars[..head]
        .iter()
        .chain(['…'].iter())
        .chain(chars[chars.len() - tail..].iter())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_ascii_is_unchanged() {
        let uri = "https://example.com/a?b=c&d=%20#frag";
        assert_eq!(sanitize(uri), uri);
    }

    #[test]
    fn hidden_and_look_alike_characters_are_encoded() {
        assert_eq!(sanitize("https://ex\u{202e}moc"), "https://ex%E2%80%AEmoc");
        assert_eq!(
            sanitize("https://\u{0430}pple.com"),
            "https://%D0%B0pple.com"
        );
        assert_eq!(sanitize("a b\u{7}\u{200b}"), "a%20b%07%E2%80%8B");
    }

    #[test]
    fn long_targets_keep_both_ends() {
        let uri = "https://example.com/a/very/long/path/to/file.txt";
        assert_eq!(elide(uri, 100), uri);
        let short = elide(uri, 20);
        assert_eq!(short.chars().count(), 20);
        assert_eq!(short, "https://examp…le.txt");
        assert_eq!(elide(uri, 1), "…");
        assert_eq!(elide(uri, 0), "");
    }

    #[test]
    fn shared_id_piece_on_bottom_row_keeps_its_text_visible() {
        let mut term = term_core::Terminal::new(80, 5);
        let link_bytes = b"\x1b]8;id=shared;https://example.com\x07piece\x1b]8;;\x07";
        term.advance(b"\x1b[2;1H");
        term.advance(link_bytes);
        term.advance(b"\x1b[5;1H");
        term.advance(link_bytes);
        let link = term.link_at(term.viewport_to_point(1, 0)).unwrap();
        assert_eq!(link.pieces.len(), 1);
        assert!(covers_link(&link, 80.0, 20.0, 0, 0.0));
        assert_eq!(
            place(
                [800.0, 100.0],
                [240.0, 20.0],
                [None, None],
                None,
                0.0,
                |y| { covers_link(&link, y, 20.0, 0, 0.0) }
            ),
            Some([0.0, 0.0])
        );
    }

    #[test]
    fn bottom_row_target_keeps_find_query_clear_in_short_window() {
        assert_eq!(
            place(
                [1600.0, 270.0],
                [750.0, 24.0],
                [Some((246.0, 270.0)), None],
                Some([0.0, 0.0, 900.0, 245.0]),
                6.0,
                |y| y + 24.0 > 236.0,
            ),
            Some([0.0, 0.0])
        );
    }

    #[test]
    fn top_row_target_fits_beside_preview_in_short_window() {
        assert_eq!(
            place(
                [1600.0, 270.0],
                [600.0, 24.0],
                [None, None],
                Some([0.0, 6.0, 900.0, 270.0]),
                6.0,
                |y| y < 30.0,
            ),
            Some([1000.0, 246.0])
        );
    }

    #[test]
    fn pinned_header_and_find_stay_clear_even_when_a_link_spans_the_window() {
        assert_eq!(
            place(
                [400.0, 60.0],
                [380.0, 20.0],
                [Some((40.0, 60.0)), Some((0.0, 20.0))],
                None,
                0.0,
                |_| true,
            ),
            Some([0.0, 20.0])
        );
    }

    #[test]
    fn uses_the_only_unlinked_row_with_grid_padding() {
        assert_eq!(
            place([400.0, 72.0], [200.0, 20.0], [None, None], None, 6.0, |y| {
                y < 26.0 || y + 20.0 > 46.0
            }),
            Some([0.0, 26.0])
        );
    }
}

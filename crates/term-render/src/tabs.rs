//! The tab bar drawn above the grid: one clickable tab per terminal, each with a close
//! button, and a `+` button that opens a new tab.
//!
//! See [`tab_layout`] for shared drawing and hit-testing geometry.

/// What the tab bar shows this frame.
#[derive(Clone, Debug, PartialEq)]
pub struct TabBar {
    /// Titles in display order, one per tab.
    pub titles: Vec<String>,
    /// Tabs that rang the bell while in the background, marked until shown.
    pub bells: Vec<bool>,
    /// Zero-based active tab index.
    pub active: usize,
    /// Control under the pointer, or `None` outside the bar.
    pub hover: Option<TabHit>,
}

/// The part of the tab bar under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabHit {
    /// A tab body, by zero-based index.
    Tab(usize),
    /// A tab's close button, by zero-based index.
    Close(usize),
    /// The new-tab button.
    New,
}

/// One tab's place in the bar, in physical pixels from the bar's left edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabRect {
    /// Left edge in physical pixels.
    pub x: f32,
    /// Width in physical pixels.
    pub width: f32,
    /// The close button: `[x, y, width, height]`.
    pub close: [f32; 4],
    /// Title columns that fit between the tab's left edge and its close button.
    pub title_columns: usize,
}

/// Tab-bar drawing and hit-test geometry; see [`tab_layout`].
#[derive(Clone, Debug, PartialEq)]
pub struct TabLayout {
    /// Bar height in physical pixels.
    pub height: f32,
    /// Tab rectangles in display order.
    pub tabs: Vec<TabRect>,
    /// The new-tab button: `[x, y, width, height]`.
    pub new: [f32; 4],
}

/// The bar's height for cells `cell_height` tall, in whole physical pixels.
pub fn tab_bar_height(cell_height: f32) -> u32 {
    (cell_height * 1.75).round().max(1.0) as u32
}

const MAX_TAB_COLUMNS: f32 = 28.0;

/// Splits a physical-pixel width among tabs capped at 28 cell columns each.
///
/// Leaves room for the new-tab button. `cw` and `ch` are positive cell width
/// and
/// height in physical pixels.
///
/// # Examples
///
/// ```rust
/// use term_render::{tab_layout, TabHit};
/// let layout = tab_layout(2, 600.0, 10.0, 20.0);
/// assert_eq!(layout.hit(5.0, 10.0), Some(TabHit::Tab(0)));
/// ```
pub fn tab_layout(count: usize, width: f32, cw: f32, ch: f32) -> TabLayout {
    let height = tab_bar_height(ch) as f32;
    let button = height;
    let available = (width - button).max(0.0);
    let tab_width = if count == 0 {
        0.0
    } else {
        (available / count as f32).min(MAX_TAB_COLUMNS * cw).floor()
    };
    let close_size = ch.min(height);
    let tabs = (0..count)
        .map(|i| {
            let x = i as f32 * tab_width;
            let close_x = x + tab_width - close_size - cw * 0.5;
            let title_columns = ((close_x - x - cw) / cw).floor().max(0.0) as usize;
            TabRect {
                x,
                width: tab_width,
                close: [close_x, (height - close_size) / 2.0, close_size, close_size],
                title_columns,
            }
        })
        .collect();
    let new_x = (count as f32 * tab_width).min(available);
    TabLayout {
        height,
        tabs,
        new: [new_x, 0.0, button, height],
    }
}

fn inside([x, y, w, h]: [f32; 4], px: f32, py: f32) -> bool {
    px >= x && px < x + w && py >= y && py < y + h
}

impl TabLayout {
    /// The `[x, width]` runs of tabs other than `active`, which get the bar's tint. The
    /// active tab and the bar past the last tab show the background the frame is
    /// cleared to, so a lone tab costs almost no drawn pixels.
    pub fn inactive_runs(&self, active: usize) -> Vec<[f32; 2]> {
        let span = |tabs: &[TabRect]| match (tabs.first(), tabs.last()) {
            (Some(first), Some(last)) => Some([first.x, last.x + last.width - first.x]),
            _ => None,
        };
        let (before, after) = if active < self.tabs.len() {
            (&self.tabs[..active], &self.tabs[active + 1..])
        } else {
            (&self.tabs[..], &self.tabs[..0])
        };
        [span(before), span(after)].into_iter().flatten().collect()
    }

    /// The part of the bar at `(x, y)`, measured from the bar's top-left corner.
    pub fn hit(&self, x: f32, y: f32) -> Option<TabHit> {
        if y < 0.0 || y >= self.height {
            return None;
        }
        if inside(self.new, x, y) {
            return Some(TabHit::New);
        }
        self.tabs.iter().enumerate().find_map(|(i, tab)| {
            if inside(tab.close, x, y) {
                Some(TabHit::Close(i))
            } else if x >= tab.x && x < tab.x + tab.width {
                Some(TabHit::Tab(i))
            } else {
                None
            }
        })
    }
}

/// `title` cut to `columns` characters, ending in an ellipsis when cut.
pub fn fit_title(title: &str, columns: usize) -> String {
    if title.chars().count() <= columns {
        return title.into();
    }
    if columns == 0 {
        return String::new();
    }
    let mut fitted: String = title.chars().take(columns - 1).collect();
    fitted.push('\u{2026}');
    fitted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_share_the_bar_and_leave_room_for_the_new_button() {
        let layout = tab_layout(3, 600.0, 10.0, 20.0);
        assert_eq!(layout.height, 35.0);
        assert_eq!(layout.tabs.len(), 3);
        assert!(layout.tabs.iter().all(|t| t.width == 188.0));
        assert_eq!(layout.tabs[2].x, 376.0);
        assert_eq!(layout.new, [564.0, 0.0, 35.0, 35.0]);
        let last = layout.tabs.last().unwrap();
        assert!(last.x + last.width <= layout.new[0]);
    }

    #[test]
    fn wide_windows_cap_the_tab_width() {
        let layout = tab_layout(1, 2000.0, 10.0, 20.0);
        assert_eq!(layout.tabs[0].width, 280.0);
        assert_eq!(layout.new[0], 280.0);
    }

    #[test]
    fn hit_test_finds_tabs_close_buttons_and_new() {
        let layout = tab_layout(2, 600.0, 10.0, 20.0);
        let close = layout.tabs[1].close;
        assert_eq!(layout.hit(5.0, 10.0), Some(TabHit::Tab(0)));
        assert_eq!(
            layout.hit(layout.tabs[1].x + 5.0, 10.0),
            Some(TabHit::Tab(1))
        );
        assert_eq!(
            layout.hit(close[0] + 1.0, close[1] + 1.0),
            Some(TabHit::Close(1))
        );
        assert_eq!(layout.hit(layout.new[0] + 1.0, 1.0), Some(TabHit::New));
        assert_eq!(layout.hit(5.0, layout.height), None);
        assert_eq!(layout.hit(5.0, -1.0), None);
        assert_eq!(layout.hit(599.0, 10.0), None);
    }

    #[test]
    fn only_tabs_other_than_the_active_one_are_tinted() {
        let layout = tab_layout(1, 600.0, 10.0, 20.0);
        assert!(layout.inactive_runs(0).is_empty());
        let layout = tab_layout(4, 600.0, 10.0, 20.0);
        let w = layout.tabs[0].width;
        assert_eq!(layout.inactive_runs(0), vec![[w, 3.0 * w]]);
        assert_eq!(layout.inactive_runs(2), vec![[0.0, 2.0 * w], [3.0 * w, w]]);
        assert_eq!(layout.inactive_runs(3), vec![[0.0, 3.0 * w]]);
        assert_eq!(layout.inactive_runs(9), vec![[0.0, 4.0 * w]]);
    }

    #[test]
    fn titles_are_cut_with_an_ellipsis() {
        assert_eq!(fit_title("vim", 5), "vim");
        assert_eq!(fit_title("cargo build", 6), "cargo\u{2026}");
        assert_eq!(fit_title("x", 0), "");
    }
}

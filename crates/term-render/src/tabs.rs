//! The tab bar drawn above the grid: one clickable tab per terminal, each with a close
//! button, and a `+` button that opens a new tab.

/// What the tab bar shows this frame.
#[derive(Clone, Debug, PartialEq)]
pub struct TabBar {
    pub titles: Vec<String>,
    pub active: usize,
    pub hover: Option<TabHit>,
}

/// The part of the tab bar under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabHit {
    Tab(usize),
    Close(usize),
    New,
}

/// One tab's place in the bar, in physical pixels from the bar's left edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabRect {
    pub x: f32,
    pub width: f32,
    /// The close button: `[x, y, width, height]`.
    pub close: [f32; 4],
    /// Title columns that fit between the tab's left edge and its close button.
    pub title_columns: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TabLayout {
    pub height: f32,
    pub tabs: Vec<TabRect>,
    /// The new-tab button: `[x, y, width, height]`.
    pub new: [f32; 4],
}

/// The bar's height for cells `cell_height` tall, in whole physical pixels.
pub fn tab_bar_height(cell_height: f32) -> u32 {
    (cell_height * 1.75).round().max(1.0) as u32
}

const MAX_TAB_COLUMNS: f32 = 28.0;

/// Splits `width` among `count` tabs of at most `MAX_TAB_COLUMNS` cells, leaving room
/// for the `+` button after the last tab.
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
    fn titles_are_cut_with_an_ellipsis() {
        assert_eq!(fit_title("vim", 5), "vim");
        assert_eq!(fit_title("cargo build", 6), "cargo\u{2026}");
        assert_eq!(fit_title("x", 0), "");
    }
}

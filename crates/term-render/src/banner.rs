pub struct Layout {
    pub lines: Vec<String>,
    pub width: f32,
    pub height: f32,
    pub scale: f32,
}

fn wrap(text: &str, columns: usize) -> Option<Vec<String>> {
    let mut lines = Vec::<String>::new();
    for word in text.split_whitespace() {
        if word.chars().count() > columns {
            return None;
        }
        if let Some(line) = lines.last_mut()
            && line.chars().count() + 1 + word.chars().count() <= columns
        {
            line.push(' ');
            line.push_str(word);
        } else {
            lines.push(word.into());
        }
    }
    Some(lines)
}

pub fn layout(text: &str, width: f32, height: f32, cw: f32, ch: f32) -> Layout {
    let columns = ((width / cw).floor() as usize).saturating_sub(2);
    let fits = |lines: &Vec<String>| (lines.len() + 1) as f32 * ch <= height;
    let lines = wrap(text, columns).filter(fits).unwrap_or_else(|| {
        let hint = text
            .strip_prefix("Output paused - press ")
            .or_else(|| text.strip_prefix("Output paused - "))
            .unwrap_or(text);
        let short = format!("Paused: {hint}");
        wrap(&short, columns)
            .filter(fits)
            .unwrap_or_else(|| vec![short])
    });
    let natural_width = (lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0)
        + 2) as f32
        * cw;
    let natural_height = (lines.len() + 1) as f32 * ch;
    let scale = (width / natural_width)
        .min(height / natural_height)
        .min(1.0);
    Layout {
        lines,
        width: natural_width * scale,
        height: natural_height * scale,
        scale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "Output paused - press Ctrl+Q";

    #[test]
    fn wide_banner_is_unchanged() {
        let banner = layout(TEXT, 800.0, 600.0, 10.0, 20.0);
        assert_eq!(banner.lines, [TEXT]);
        assert_eq!(
            (banner.width, banner.height, banner.scale),
            (300.0, 40.0, 1.0)
        );
    }

    #[test]
    fn narrow_banner_wraps_at_words() {
        let banner = layout(TEXT, 180.0, 350.0, 10.0, 20.0);
        assert_eq!(banner.lines, ["Output paused -", "press Ctrl+Q"]);
        assert_eq!(banner.scale, 1.0);
    }

    #[test]
    fn tiny_surfaces_keep_the_whole_key() {
        for (width, height) in [(50.0, 350.0), (180.0, 40.0), (1.0, 1.0)] {
            let banner = layout(TEXT, width, height, 10.0, 20.0);
            assert!(banner.lines.join(" ").contains("Ctrl+Q"));
            assert!(banner.lines.join(" ").starts_with("Paused:"));
            assert!(banner.width <= width && banner.height <= height);
        }
    }

    #[test]
    fn custom_keys_and_empty_text_fit() {
        for text in ["Output paused - press Backspace", ""] {
            let banner = layout(text, 80.0, 30.0, 10.0, 20.0);
            assert!(banner.width <= 80.0 && banner.height <= 30.0);
            if !text.is_empty() {
                assert!(banner.lines.join(" ").contains("Backspace"));
            }
        }
    }
}

//! Kitty animation frames are composited on receipt. The renderer only sees the current canvas.
use super::{Image, Rect, StoredImage, Terminal};
use std::time::{Duration, Instant};

pub(super) struct Frame {
    pub(super) image: Image,
    pub(super) gap: u32,
    pub(super) version: u64,
}

#[derive(Default, PartialEq)]
enum State {
    #[default]
    Stopped,
    Loading,
    Playing,
}

#[derive(Default)]
pub(super) struct Playback {
    pub(super) current: usize,
    state: State,
    due: Option<Instant>,
    limit: u32,
    completed: u32,
}

pub(crate) struct FramePatch {
    pub(crate) target: u32,
    pub(crate) base: u32,
    pub(crate) offset: (u32, u32),
    pub(crate) background: u32,
    pub(crate) replace: bool,
    pub(crate) gap: i32,
}

const MISSING: &str = "ENOENT:no such frame";
const BOUNDS: &str = "EINVAL:rectangle outside frame";
// The pixel budget alone does not bound frame metadata for tiny images.
const MAX_FRAMES: usize = 4096;

fn fits(image: &Image, x: u32, y: u32, w: u32, h: u32) -> bool {
    x <= image.width && y <= image.height && w <= image.width - x && h <= image.height - y
}

fn composite_row(dst: &mut [u8], src: &[u8], replace: bool) {
    if replace {
        dst.copy_from_slice(src);
        return;
    }
    for (d, s) in dst
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(src.as_chunks::<4>().0)
    {
        let a = u32::from(s[3]);
        if a == 0 {
            continue;
        }
        if a == 255 || d[3] == 0 {
            d.copy_from_slice(s);
            continue;
        }
        let b = u32::from(d[3]) * (255 - a);
        let alpha = a * 255 + b;
        for k in 0..3 {
            d[k] = ((u32::from(s[k]) * a * 255 + u32::from(d[k]) * b + alpha / 2) / alpha) as u8;
        }
        d[3] = ((alpha + 127) / 255) as u8;
    }
}

impl StoredImage {
    fn index(&self, number: u32) -> Result<usize, &'static str> {
        number
            .checked_sub(1)
            .map(|n| n as usize)
            .filter(|&i| i < self.frames.len())
            .ok_or(MISSING)
    }

    fn advance(&mut self, now: Instant) -> Option<Instant> {
        let p = &mut self.playback;
        if p.state == State::Stopped || self.frames.len() == 1 {
            return None;
        }
        let gap = self.frames[p.current].gap;
        if gap > 0 {
            let due = *p
                .due
                .get_or_insert_with(|| now + Duration::from_millis(gap.into()));
            if due > now {
                return Some(due);
            }
        }
        // Skip gapless frames without presenting them, including at the end of a finite loop.
        let mut next = p.current;
        for _ in 0..self.frames.len() {
            if next + 1 == self.frames.len() {
                if p.state == State::Loading {
                    return None;
                }
                p.completed = p.completed.saturating_add(1);
                if p.limit != 0 && p.completed >= p.limit {
                    p.state = State::Stopped;
                    p.completed = 0;
                    p.due = None;
                    return None;
                }
            }
            next = (next + 1) % self.frames.len();
            let gap = self.frames[next].gap;
            if gap != 0 {
                p.current = next;
                p.due = Some(now + Duration::from_millis(gap.into()));
                return p.due;
            }
        }
        None
    }
}

impl Terminal {
    pub(crate) fn add_frame(
        &mut self,
        id: u32,
        data: Image,
        patch: FramePatch,
    ) -> Result<(), &'static str> {
        let s = self.images.images.get(&id).ok_or(MISSING)?;
        let root = &s.frames[0].image;
        let (x, y) = patch.offset;
        if !fits(root, x, y, data.width, data.height) {
            return Err(BOUNDS);
        }
        let append = patch.target == 0;
        let index = if append {
            s.frames.len()
        } else {
            s.index(patch.target)?
        };
        let base = (append && patch.base != 0)
            .then(|| s.index(patch.base))
            .transpose()?;
        if append {
            if index >= MAX_FRAMES {
                return Err("ENOSPC:too many frames");
            }
            let bytes = root.pixels.len();
            self.reserve_image(
                id,
                s.bytes()
                    .checked_add(bytes)
                    .ok_or("ENOSPC:image storage full")?,
            )
            .map_err(|_| "ENOSPC:image storage full")?;
            self.images.bytes += bytes;
        }
        let version = self.images.tick();
        let s = self.images.images.get_mut(&id).unwrap();
        s.last_used = version;
        let root = &s.frames[0].image;
        if append && patch.replace && data.width == root.width && data.height == root.height {
            s.frames.push(Frame {
                image: data,
                gap: 40,
                version,
            });
        } else {
            if append {
                let image = match base {
                    Some(base) => s.frames[base].image.clone(),
                    None => Image {
                        width: root.width,
                        height: root.height,
                        pixels: patch.background.to_be_bytes().repeat(root.pixels.len() / 4),
                    },
                };
                s.frames.push(Frame {
                    image,
                    gap: 40,
                    version,
                });
            }
            let canvas = &mut s.frames[index].image;
            let stride = canvas.width as usize * 4;
            for (row, src) in data
                .pixels
                .chunks_exact(data.width as usize * 4)
                .enumerate()
            {
                let start = (row + y as usize) * stride + x as usize * 4;
                composite_row(
                    &mut canvas.pixels[start..start + src.len()],
                    src,
                    patch.replace,
                );
            }
        }
        s.frames[index].version = version;
        if patch.gap != 0 {
            s.frames[index].gap = patch.gap.max(0) as u32;
            if index == s.playback.current {
                s.playback.due = None;
            }
        }
        self.images.generation += 1;
        Ok(())
    }

    pub(crate) fn compose_frame(
        &mut self,
        id: u32,
        source: u32,
        target: u32,
        origin: (u32, u32),
        dest: Rect,
        replace: bool,
    ) -> Result<(), &'static str> {
        let version = self.images.tick();
        let s = self.images.images.get_mut(&id).ok_or(MISSING)?;
        let (src, dst) = (s.index(source)?, s.index(target)?);
        let image = &s.frames[src].image;
        let (sx, sy) = origin;
        let Rect {
            x,
            y,
            width,
            height,
        } = dest;
        let w = if width == 0 { image.width } else { width };
        let h = if height == 0 { image.height } else { height };
        if !fits(image, sx, sy, w, h) || !fits(image, x, y, w, h) {
            return Err(BOUNDS);
        }
        if src == dst && x.abs_diff(sx) < w && y.abs_diff(sy) < h {
            return Err("EINVAL:overlapping rectangles");
        }
        let stride = image.width as usize * 4;
        let len = w as usize * 4;
        let mut row = Vec::with_capacity(len);
        for dy in 0..h {
            let from = (sy + dy) as usize * stride + sx as usize * 4;
            let to = (y + dy) as usize * stride + x as usize * 4;
            row.clear();
            row.extend_from_slice(&s.frames[src].image.pixels[from..from + len]);
            composite_row(&mut s.frames[dst].image.pixels[to..to + len], &row, replace);
        }
        s.frames[dst].version = version;
        self.images.generation += 1;
        Ok(())
    }

    pub(crate) fn control_animation(
        &mut self,
        id: u32,
        state: u32,
        loops: u32,
        target: u32,
        current: u32,
        gap: i32,
    ) -> Result<(), &'static str> {
        let s = self.images.images.get_mut(&id).ok_or(MISSING)?;
        let edit = (target != 0).then(|| s.index(target)).transpose()?;
        let current = (current != 0).then(|| s.index(current)).transpose()?;
        let p = &mut s.playback;
        match state {
            0 => {}
            1 => {
                p.state = State::Stopped;
                p.completed = 0;
                p.due = None;
            }
            2 => p.state = State::Loading,
            3 => p.state = State::Playing,
            _ => return Err("EINVAL:invalid animation state"),
        }
        if loops != 0 {
            p.limit = loops - 1;
        }
        if let Some(i) = current {
            p.current = i;
            p.due = None;
        }
        if let Some(i) = edit
            && gap != 0
        {
            s.frames[i].gap = gap.max(0) as u32;
            if i == p.current {
                p.due = None;
            }
        }
        self.images.generation += 1;
        Ok(())
    }

    /// Advances only animations in the viewport and returns the next frame deadline.
    /// Include one extra row when a smooth scroll exposes part of the preceding row.
    pub fn advance_animations(&mut self, now: Instant, extra_rows: usize) -> Option<Instant> {
        if self.screen().placements.is_empty() {
            return None;
        }
        let mut deadline = None;
        for v in self.visible_placements_above(extra_rows) {
            let p = v.placement;
            if p.source.width == 0 || p.source.height == 0 {
                continue;
            }
            let Some(s) = self.images.images.get_mut(&p.image_id) else {
                continue;
            };
            let old = s.playback.current;
            if let Some(due) = s.advance(now) {
                deadline = Some(deadline.map_or(due, |d: Instant| d.min(due)));
            }
            if old != s.playback.current {
                self.images.generation += 1;
            }
        }
        deadline
    }
}

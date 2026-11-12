//! Color glyphs: embedded bitmaps (sbix, CBDT) and COLR layers, rasterized to RGBA that
//! fits the glyph's cells.

use crate::font::RasterizedGlyph;
use ab_glyph_rasterizer::{Point, Rasterizer, point};
use ttf_parser::colr::{ClipBox, CompositeMode, GradientExtend, Paint, Painter};
use ttf_parser::{Face, GlyphId, OutlineBuilder, RasterImageFormat, RgbaColor, Transform};

/// The box a color glyph is drawn into: `cells` cells wide, one cell high.
#[derive(Clone, Copy)]
pub struct Target {
    pub width: f32,
    pub height: f32,
    pub baseline: f32,
    pub px: f32,
}

pub fn has_color(face: &Face) -> bool {
    let t = face.tables();
    t.colr.is_some() || t.sbix.is_some() || t.cbdt.is_some()
}

pub fn rasterize(face: &Face, id: GlyphId, target: Target) -> Option<RasterizedGlyph> {
    if face.is_color_glyph(id) {
        return colr(face, id, target);
    }
    bitmap(face, id, target)
}

fn bitmap(face: &Face, id: GlyphId, target: Target) -> Option<RasterizedGlyph> {
    let image = face.glyph_raster_image(id, target.px.ceil() as u16)?;
    let (w, h, rgba) = match image.format {
        RasterImageFormat::PNG => decode_png(image.data)?,
        RasterImageFormat::BitmapPremulBgra32 => {
            let (w, h) = (image.width as usize, image.height as usize);
            let px = image.data.get(..w * h * 4)?.as_chunks::<4>().0.iter();
            let straight = |v: u8, a: u8| {
                if a == 0 {
                    0
                } else {
                    (v as u32 * 255 / a as u32).min(255) as u8
                }
            };
            let px = px.flat_map(|p| {
                [
                    straight(p[2], p[3]),
                    straight(p[1], p[3]),
                    straight(p[0], p[3]),
                    p[3],
                ]
            });
            (w, h, px.collect())
        }
        _ => return None,
    };
    if w == 0 || h == 0 {
        return None;
    }
    let scale = (target.px / image.pixels_per_em.max(1) as f32)
        .min(target.width / w as f32)
        .min(target.height / h as f32);
    let (out_w, out_h) = (
        ((w as f32 * scale).round() as usize).max(1),
        ((h as f32 * scale).round() as usize).max(1),
    );
    let coverage = resize(&rgba, w, h, out_w, out_h);
    Some(RasterizedGlyph {
        width: out_w,
        height: out_h,
        left: ((target.width - out_w as f32) / 2.0).round(),
        top: ((target.height - out_h as f32) / 2.0).round(),
        coverage,
        color: true,
    })
}

/// Decodes to straight-alpha RGBA8.
fn decode_png(data: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    let len = w * h * 4;
    (rgba.len() >= len).then(|| (w, h, rgba[..len].to_vec()))
}

/// Area-averaging resample of straight-alpha RGBA, weighting color by alpha.
pub fn resize(src: &[u8], w: usize, h: usize, out_w: usize, out_h: usize) -> Vec<u8> {
    let (sx, sy) = (w as f32 / out_w as f32, h as f32 / out_h as f32);
    let mut out = Vec::with_capacity(out_w * out_h * 4);
    for oy in 0..out_h {
        let (y0, y1) = (oy as f32 * sy, (oy + 1) as f32 * sy);
        for ox in 0..out_w {
            let (x0, x1) = (ox as f32 * sx, (ox + 1) as f32 * sx);
            let mut acc = [0f32; 4];
            let mut area = 0.0;
            for y in y0 as usize..(y1.ceil() as usize).min(h) {
                let wy = (y1.min(y as f32 + 1.0) - y0.max(y as f32)).max(0.0);
                for x in x0 as usize..(x1.ceil() as usize).min(w) {
                    let wx = (x1.min(x as f32 + 1.0) - x0.max(x as f32)).max(0.0);
                    let p = &src[(y * w + x) * 4..][..4];
                    let a = p[3] as f32 * wx * wy;
                    acc[0] += p[0] as f32 * a;
                    acc[1] += p[1] as f32 * a;
                    acc[2] += p[2] as f32 * a;
                    acc[3] += a;
                    area += wx * wy;
                }
            }
            let a = acc[3];
            let c = |v: f32| {
                if a > 0.0 {
                    (v / a).round().min(255.0) as u8
                } else {
                    0
                }
            };
            let alpha = if area > 0.0 {
                (a / area).round().min(255.0) as u8
            } else {
                0
            };
            out.extend([c(acc[0]), c(acc[1]), c(acc[2]), alpha]);
        }
    }
    out
}

fn colr(face: &Face, id: GlyphId, target: Target) -> Option<RasterizedGlyph> {
    let upem = face.units_per_em() as f32;
    let advance = face.glyph_hor_advance(id).unwrap_or(face.units_per_em()) as f32;
    let scale = (target.px / upem).min(target.width / advance.max(1.0));
    let (w, h) = (target.width.ceil() as usize, target.height.ceil() as usize);
    let origin_x = (target.width - advance * scale) / 2.0;
    let base = Transform::new(scale, 0.0, 0.0, -scale, origin_x, target.baseline);
    let mut painter = Canvas {
        face,
        w,
        h,
        transforms: vec![base],
        path: Vec::new(),
        clips: Vec::new(),
        layers: vec![vec![[0.0; 4]; w * h]],
    };
    face.paint_color_glyph(id, 0, RgbaColor::new(255, 255, 255, 255), &mut painter)?;
    let layer = painter.layers.pop()?;
    let coverage = layer
        .iter()
        .flat_map(|&[r, g, b, a]| {
            let c = |v: f32| {
                if a > 0.0 {
                    (v / a * 255.0).round().min(255.0) as u8
                } else {
                    0
                }
            };
            [c(r), c(g), c(b), (a * 255.0).round().min(255.0) as u8]
        })
        .collect();
    Some(RasterizedGlyph {
        width: w,
        height: h,
        left: 0.0,
        top: 0.0,
        coverage,
        color: true,
    })
}

enum Segment {
    Move(f32, f32),
    Line(f32, f32),
    Quad(f32, f32, f32, f32),
    Curve(f32, f32, f32, f32, f32, f32),
    Close,
}

struct Recorder<'a>(&'a mut Vec<Segment>);

impl OutlineBuilder for Recorder<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(Segment::Move(x, y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push(Segment::Line(x, y));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.push(Segment::Quad(x1, y1, x, y));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.push(Segment::Curve(x1, y1, x2, y2, x, y));
    }
    fn close(&mut self) {
        self.0.push(Segment::Close);
    }
}

/// A software COLR painter: premultiplied RGBA layers composited source-over, clips as
/// coverage masks. Gradients are evaluated along their main axis (linear), radius
/// (radial) or angle (sweep).
struct Canvas<'f, 'a> {
    face: &'f Face<'a>,
    w: usize,
    h: usize,
    transforms: Vec<Transform>,
    /// The current outline, in font units.
    path: Vec<Segment>,
    clips: Vec<Vec<f32>>,
    layers: Vec<Vec<[f32; 4]>>,
}

impl Canvas<'_, '_> {
    fn transform(&self) -> Transform {
        *self.transforms.last().expect("base transform")
    }

    /// Coverage of the current path, intersected with the current clip.
    fn mask(&self) -> Vec<f32> {
        let t = self.transform();
        let p = |x: f32, y: f32| point(t.a * x + t.c * y + t.e, t.b * x + t.d * y + t.f);
        let mut r = Clipped(
            Rasterizer::new(self.w, self.h),
            self.w as f32,
            self.h as f32,
        );
        let (mut start, mut last) = (point(0.0, 0.0), point(0.0, 0.0));
        for s in &self.path {
            match *s {
                Segment::Move(x, y) => {
                    if last != start {
                        r.draw_line(last, start);
                    }
                    start = p(x, y);
                    last = start;
                }
                Segment::Line(x, y) => {
                    let to = p(x, y);
                    r.draw_line(last, to);
                    last = to;
                }
                Segment::Quad(x1, y1, x, y) => {
                    let to = p(x, y);
                    r.draw_quad(last, p(x1, y1), to);
                    last = to;
                }
                Segment::Curve(x1, y1, x2, y2, x, y) => {
                    let to = p(x, y);
                    r.draw_cubic(last, p(x1, y1), p(x2, y2), to);
                    last = to;
                }
                Segment::Close => {
                    r.draw_line(last, start);
                    last = start;
                }
            }
        }
        if last != start {
            r.draw_line(last, start);
        }
        let mut mask = vec![0.0; self.w * self.h];
        r.for_each_pixel(|i, v| mask[i] = v.clamp(0.0, 1.0));
        if let Some(clip) = self.clips.last() {
            mask.iter_mut().zip(clip).for_each(|(m, c)| *m *= c);
        }
        mask
    }
}

/// The rasterizer only accepts points inside its bounds, while COLR paths and clip boxes
/// may extend past the glyph's cells, so curves are flattened and lines clipped to the
/// bounds. Parts left of or right of the bounds are moved onto the edge, which keeps the
/// winding inside the same; parts above or below become horizontal and add nothing.
struct Clipped(Rasterizer, f32, f32);

impl Clipped {
    fn draw_line(&mut self, a: Point, b: Point) {
        let mut ts = [0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        let mut n = 2;
        for (pa, pb, edge) in [
            (a.x, b.x, 0.0),
            (a.x, b.x, self.1),
            (a.y, b.y, 0.0),
            (a.y, b.y, self.2),
        ] {
            let t = (edge - pa) / (pb - pa);
            if t > 0.0 && t < 1.0 {
                ts[n] = t;
                n += 1;
            }
        }
        let ts = &mut ts[..n];
        ts.sort_by(f32::total_cmp);
        let at = |t: f32| {
            let x = a.x + (b.x - a.x) * t;
            let y = a.y + (b.y - a.y) * t;
            point(x.clamp(0.0, self.1), y.clamp(0.0, self.2))
        };
        for pair in ts.windows(2) {
            let (p0, p1) = (at(pair[0]), at(pair[1]));
            if p0 != p1 {
                self.0.draw_line(p0, p1);
            }
        }
    }

    fn draw_quad(&mut self, p0: Point, p1: Point, p2: Point) {
        self.draw_cubic(p0, lerp(p0, p1, 2.0 / 3.0), lerp(p2, p1, 2.0 / 3.0), p2);
    }

    fn draw_cubic(&mut self, p0: Point, p1: Point, p2: Point, p3: Point) {
        const STEPS: usize = 16;
        let mut last = p0;
        for i in 1..=STEPS {
            let t = i as f32 / STEPS as f32;
            let (a, b, c) = (lerp(p0, p1, t), lerp(p1, p2, t), lerp(p2, p3, t));
            let next = lerp(lerp(a, b, t), lerp(b, c, t), t);
            self.draw_line(last, next);
            last = next;
        }
    }

    fn for_each_pixel(&self, f: impl FnMut(usize, f32)) {
        self.0.for_each_pixel(f);
    }
}

fn lerp(a: Point, b: Point, t: f32) -> Point {
    point(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

fn rgba(c: RgbaColor) -> [f32; 4] {
    let a = c.alpha as f32 / 255.0;
    [
        c.red as f32 / 255.0 * a,
        c.green as f32 / 255.0 * a,
        c.blue as f32 / 255.0 * a,
        a,
    ]
}

fn extend(t: f32, mode: GradientExtend) -> f32 {
    match mode {
        GradientExtend::Pad => t.clamp(0.0, 1.0),
        GradientExtend::Repeat => t - t.floor(),
        GradientExtend::Reflect => {
            let t = t.rem_euclid(2.0);
            if t > 1.0 { 2.0 - t } else { t }
        }
    }
}

/// Color at `t` along sorted stops, premultiplied.
fn sample(stops: &[(f32, [f32; 4])], t: f32) -> [f32; 4] {
    let Some(first) = stops.first() else {
        return [0.0; 4];
    };
    if t <= first.0 {
        return first.1;
    }
    for pair in stops.windows(2) {
        let ((t0, c0), (t1, c1)) = (pair[0], pair[1]);
        if t <= t1 {
            let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 1.0 };
            return std::array::from_fn(|i| c0[i] + (c1[i] - c0[i]) * f);
        }
    }
    stops.last().map_or([0.0; 4], |s| s.1)
}

fn invert(t: Transform) -> Option<Transform> {
    let det = t.a * t.d - t.b * t.c;
    if det.abs() < 1e-12 {
        return None;
    }
    let (a, b, c, d) = (t.d / det, -t.b / det, -t.c / det, t.a / det);
    Some(Transform::new(
        a,
        b,
        c,
        d,
        -(a * t.e + c * t.f),
        -(b * t.e + d * t.f),
    ))
}

fn sorted(stops: impl Iterator<Item = ttf_parser::colr::ColorStop>) -> Vec<(f32, [f32; 4])> {
    let mut v: Vec<_> = stops.map(|s| (s.stop_offset, rgba(s.color))).collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    v
}

impl<'a> Painter<'a> for Canvas<'_, 'a> {
    fn outline_glyph(&mut self, glyph_id: GlyphId) {
        self.path.clear();
        self.face
            .outline_glyph(glyph_id, &mut Recorder(&mut self.path));
    }

    fn paint(&mut self, paint: Paint<'a>) {
        let mask = if self.path.is_empty() {
            self.clips
                .last()
                .cloned()
                .unwrap_or_else(|| vec![1.0; self.w * self.h])
        } else {
            self.mask()
        };
        let inverse = invert(self.transform());
        let solid = match &paint {
            Paint::Solid(c) => rgba(*c),
            _ => [0.0; 4],
        };
        // Maps a point in font units to its gradient parameter, before extension.
        type Param = Box<dyn Fn(f32, f32) -> f32>;
        let gradient: Option<(Param, GradientExtend, Vec<_>)> = match paint {
            Paint::Solid(_) => None,
            Paint::LinearGradient(g) => {
                let stops = sorted(g.stops(0, &[]));
                let (x0, y0, dx, dy) = (g.x0, g.y0, g.x1 - g.x0, g.y1 - g.y0);
                let len = (dx * dx + dy * dy).max(1e-6);
                let f = move |x: f32, y: f32| ((x - x0) * dx + (y - y0) * dy) / len;
                Some((Box::new(f), g.extend, stops))
            }
            Paint::RadialGradient(g) => {
                let stops = sorted(g.stops(0, &[]));
                let (cx, cy, r0, dr) = (g.x1, g.y1, g.r0, (g.r1 - g.r0).abs().max(1e-6));
                let f =
                    move |x: f32, y: f32| (((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r0) / dr;
                Some((Box::new(f), g.extend, stops))
            }
            Paint::SweepGradient(g) => {
                let stops = sorted(g.stops(0, &[]));
                let (cx, cy, start) = (g.center_x, g.center_y, g.start_angle);
                let span = (g.end_angle - g.start_angle).abs().max(1e-6);
                let f = move |x: f32, y: f32| {
                    let angle = (y - cy).atan2(x - cx).to_degrees() / 180.0;
                    (angle.rem_euclid(2.0) - start) / span
                };
                Some((Box::new(f), g.extend, stops))
            }
        };
        let w = self.w;
        let layer = self.layers.last_mut().expect("base layer");
        for (i, (dst, m)) in layer.iter_mut().zip(&mask).enumerate() {
            if *m <= 0.0 {
                continue;
            }
            let src = match (&gradient, inverse) {
                (Some((t, mode, stops)), Some(inv)) => {
                    let (px, py) = ((i % w) as f32 + 0.5, (i / w) as f32 + 0.5);
                    let (x, y) = (
                        inv.a * px + inv.c * py + inv.e,
                        inv.b * px + inv.d * py + inv.f,
                    );
                    sample(stops, extend(t(x, y), *mode))
                }
                (Some(_), None) => continue,
                (None, _) => solid,
            };
            let a = src[3] * m;
            for c in 0..4 {
                dst[c] = src[c] * m + dst[c] * (1.0 - a);
            }
        }
    }

    /// COLRv1 paints fill the clip rather than the outline, possibly under a different
    /// transform, so the outline is consumed here; COLRv0 layers paint the outline directly.
    fn push_clip(&mut self) {
        let mask = self.mask();
        self.path.clear();
        self.clips.push(mask);
    }

    fn push_clip_box(&mut self, b: ClipBox) {
        self.path = vec![
            Segment::Move(b.x_min, b.y_min),
            Segment::Line(b.x_max, b.y_min),
            Segment::Line(b.x_max, b.y_max),
            Segment::Line(b.x_min, b.y_max),
            Segment::Close,
        ];
        self.push_clip();
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }

    fn push_layer(&mut self, _mode: CompositeMode) {
        self.layers.push(vec![[0.0; 4]; self.w * self.h]);
    }

    fn pop_layer(&mut self) {
        if self.layers.len() < 2 {
            return;
        }
        let top = self.layers.pop().expect("layer");
        let below = self.layers.last_mut().expect("base layer");
        for (dst, src) in below.iter_mut().zip(top) {
            for c in 0..4 {
                dst[c] = src[c] + dst[c] * (1.0 - src[3]);
            }
        }
    }

    fn push_transform(&mut self, t: Transform) {
        let m = self.transform();
        self.transforms.push(Transform::combine(m, t));
    }

    fn pop_transform(&mut self) {
        if self.transforms.len() > 1 {
            self.transforms.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_averages_by_alpha() {
        // A red opaque pixel next to a transparent one halves alpha but stays red.
        let src = [255, 0, 0, 255, 0, 255, 0, 0];
        assert_eq!(resize(&src, 2, 1, 1, 1), vec![255, 0, 0, 128]);
    }

    #[test]
    fn gradient_extend_modes() {
        assert_eq!(extend(1.5, GradientExtend::Pad), 1.0);
        assert_eq!(extend(1.25, GradientExtend::Repeat), 0.25);
        assert_eq!(extend(1.25, GradientExtend::Reflect), 0.75);
    }

    #[test]
    fn sample_interpolates_between_stops() {
        let stops = [(0.0, [0.0, 0.0, 0.0, 1.0]), (1.0, [1.0, 1.0, 1.0, 1.0])];
        assert_eq!(sample(&stops, 0.5), [0.5, 0.5, 0.5, 1.0]);
        assert_eq!(sample(&stops, -1.0), stops[0].1);
    }
}

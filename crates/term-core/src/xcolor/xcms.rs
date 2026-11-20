//! OSC color conversion using the default Xcms screen profile, independent
//! of the window system. See `libX11`'s `src/xcms` and the Tektronix notice
//! in the project license.
mod profile;

use crate::colors::Rgb;
use std::f64::consts::PI;

type Triple = [f64; 3];

const XYZ_TO_RGB: [Triple; 3] = [
    [3.48340481253539, -1.521763749272852, -0.5592313335404978],
    [-1.071527513061936, 1.965937952043724, 0.03673691339553462],
    [0.06351179790497788, -0.2002050100049648, 0.8107094203164822],
];
const RGB_TO_XYZ: [Triple; 3] = [
    [0.3810614910871479, 0.3202571236535211, 0.248345785259331],
    [0.2072974511514085, 0.6805463877637324, 0.1121561610848592],
    [0.02133944350088028, 0.1429719302024648, 1.241728926296655],
];
const WHITE: Triple = [
    RGB_TO_XYZ[0][0] + RGB_TO_XYZ[0][1] + RGB_TO_XYZ[0][2],
    1.0,
    RGB_TO_XYZ[2][0] + RGB_TO_XYZ[2][1] + RGB_TO_XYZ[2][2],
];
const D: f64 = WHITE[0] + 15.0 + 3.0 * WHITE[2];
const WHITE_U: f64 = 4.0 * WHITE[0] / D;
const WHITE_V: f64 = 9.0 / D;
const CHROMA_SCALE: f64 = 7.50725;
// Xcms's angle in degrees from screen white to TekHVC's best red.
const HUE_OFFSET: f64 = 5.105096984039076;

fn transform(matrix: &[Triple; 3], [x, y, z]: Triple) -> Triple {
    matrix.map(|[a, b, c]| a * x + b * y + c * z)
}

fn uv_y_to_xyz(u: f64, v: f64, luminance: f64) -> Option<Triple> {
    let d = 6.0 * u - 16.0 * v + 12.0;
    if d == 0.0 {
        return None;
    }
    xy_y_to_xyz(9.0 * u / d, 4.0 * v / d, luminance)
}

fn xy_y_to_xyz(x: f64, y: f64, luminance: f64) -> Option<Triple> {
    (y != 0.0).then(|| [x * luminance / y, luminance, (1.0 - x - y) * luminance / y])
}

fn lab_to_xyz(l: f64, a: f64, b: f64) -> Triple {
    let t = (l + 16.0) / 116.0;
    let y = t * t * t;
    if y < 0.008856 {
        let y = l / 9.03292;
        [WHITE[0] * (a / 3893.5 + y), y, WHITE[2] * (y - b / 1557.4)]
    } else {
        let x = t + a / 5.0;
        let z = t - b / 2.0;
        [WHITE[0] * x * x * x, y, WHITE[2] * z * z * z]
    }
}

fn luminance(lightness: f64) -> f64 {
    if lightness < 7.99953624 {
        lightness / 903.29
    } else {
        let t = (lightness + 16.0) / 116.0;
        t * t * t
    }
}

fn luv_to_xyz(l: f64, u: f64, v: f64) -> Option<Triple> {
    if l == 0.0 {
        return Some([0.0; 3]);
    }
    let scale = 13.0 * (l / 100.0);
    uv_y_to_xyz(u / scale + WHITE_U, v / scale + WHITE_V, luminance(l))
}

// Xcms uses Gauss's arctangent approximation to determine the hue.
// Its stopping precision matters at the discontinuity above zero intensity.
fn hue_angle(slope: f64) -> f64 {
    if slope.abs() > 1e150 {
        return 90.0_f64.copysign(slope);
    }
    let square = 1.0 + slope * slope;
    let mut a = (1.0 / square).sqrt();
    let mut b = 1.0;
    for _ in 0..64 {
        a = (a + b) / 2.0;
        b = (a * b).sqrt();
        if a == b || (a - b).abs() < slope.min(1.0) * 1e-6 {
            break;
        }
    }
    (slope / (square.sqrt() * a.min(b))) * 180.0 / PI
}

fn hvc_to_xyz(h: f64, v: f64, c: f64) -> Option<Triple> {
    if v == 0.0 || v == 100.0 {
        return Some(WHITE.map(|w| w * v / 100.0));
    }
    let angle = (h + HUE_OFFSET).rem_euclid(360.0) * PI / 180.0;
    let scale = v * CHROMA_SCALE;
    uv_y_to_xyz(
        libm::cos(angle) * c / scale + WHITE_U,
        libm::sin(angle) * c / scale + WHITE_V,
        luminance(v),
    )
}

fn xyz_to_hvc([x, y, z]: Triple) -> Triple {
    let d = x + 15.0 * y + 3.0 * z;
    let (u, v) = if d == 0.0 {
        (0.0, 0.0)
    } else {
        (4.0 * x / d - WHITE_U, 9.0 * y / d - WHITE_V)
    };
    let mut angle = if u == 0.0 { 0.0 } else { hue_angle(v / u) };
    if u < 0.0 {
        angle += 180.0;
    } else if angle < 0.0 {
        angle += 360.0;
    }
    let value = if y < 0.008856 {
        y * 903.29
    } else {
        libm::cbrt(y) * 116.0 - 16.0
    };
    [
        (angle - HUE_OFFSET).rem_euclid(360.0),
        value,
        value * CHROMA_SCALE * (u * u + v * v).sqrt(),
    ]
}

fn clip_chroma(xyz: Triple) -> Option<Triple> {
    let [h, v, _] = xyz_to_hvc(xyz);
    // At the tip of this hue's gamut, one primary is zero and another is
    // full. Find it from an unreachable color of the same hue (Xcms QueryMaxVC).
    let sample = transform(&XYZ_TO_RGB, hvc_to_xyz(h, 40.0, 120.0)?);
    let min = sample.into_iter().reduce(f64::min)?;
    let max = sample.into_iter().reduce(f64::max)?;
    let tip = sample.map(|i| (i - min) / (max - min));
    let tip_xyz = transform(&RGB_TO_XYZ, tip);
    let [_, tip_v, tip_c] = xyz_to_hvc(tip_xyz);
    let (value, chroma) = if v <= tip_v {
        // Below the tip, chroma varies linearly with value.
        (v, v * tip_c / tip_v)
    } else {
        upper_gamut(v, tip_v, tip)
    };
    Some(transform(&XYZ_TO_RGB, hvc_to_xyz(h, value, chroma)?))
}

// Above the tip, Xcms searches toward white in RGB intensity space and
// stops within 0.001 TekHVC value units, before device-code quantization.
fn upper_gamut(value: f64, tip_value: f64, tip: Triple) -> (f64, f64) {
    const EPS: f64 = 0.001;
    let mut estimate = value;
    let mut factor = 1.0;
    let mut current = (value, 100.0);
    let mut previous = (-1.0_f64, 0.0);
    for _ in 0..100 {
        let two_back = previous.0;
        previous = current;
        let t = (estimate - tip_value) / (100.0 - tip_value) * factor;
        let rgb = tip.map(|i| i * (1.0 - t) + t);
        let [_, v, c] = xyz_to_hvc(transform(&RGB_TO_XYZ, rgb));
        current = (v, c);
        if (v - value).abs() <= EPS {
            return current;
        }
        estimate += value - v;
        if estimate < tip_value {
            estimate = tip_value;
            factor *= 0.5;
        } else if estimate > 100.0 {
            break;
        } else if (v - two_back).abs() <= EPS {
            factor *= 0.5;
        }
    }
    if (previous.0 - value).abs() < (current.0 - value).abs() {
        previous
    } else {
        current
    }
}

pub(super) fn convert(prefix: &[u8], values @ [a, b, c]: Triple) -> Option<Rgb> {
    if !values.into_iter().all(f64::is_finite) {
        return None;
    }
    let unit = 0.0..=1.0;
    if prefix.eq_ignore_ascii_case(b"rgbi") {
        return values
            .iter()
            .all(|i| unit.contains(i))
            .then(|| profile::encode_rgb(values));
    }
    let lightness = 0.0..=100.0;
    let xyz = if prefix.eq_ignore_ascii_case(b"CIEXYZ") {
        values
    } else if prefix.eq_ignore_ascii_case(b"CIEuvY") {
        uv_y_to_xyz(a, b, c)?
    } else if prefix.eq_ignore_ascii_case(b"CIExyY") {
        xy_y_to_xyz(a, b, c)?
    } else if prefix.eq_ignore_ascii_case(b"CIELab") && lightness.contains(&a) {
        lab_to_xyz(a, b, c)
    } else if prefix.eq_ignore_ascii_case(b"CIELuv") && lightness.contains(&a) {
        luv_to_xyz(a, b, c)?
    } else if prefix.eq_ignore_ascii_case(b"TekHVC") && lightness.contains(&b) && c >= 0.0 {
        hvc_to_xyz(a, b, c)?
    } else {
        return None;
    };
    if !unit.contains(&xyz[1]) || !xyz.into_iter().all(f64::is_finite) {
        return None;
    }
    let mut rgb = transform(&XYZ_TO_RGB, xyz);
    if rgb.iter().any(|i| !(-0.001..=1.001).contains(i)) {
        rgb = clip_chroma(xyz)?;
    }
    rgb.into_iter()
        .all(f64::is_finite)
        .then(|| profile::encode_rgb(rgb.map(|i| i.clamp(0.0, 1.0))))
}

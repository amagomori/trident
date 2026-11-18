use crate::colors::Rgb;

fn hex_part(part: &[u8]) -> Option<u16> {
    if part.is_empty() || part.len() > 4 || !part.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let mut value = 0u16;
    for &digit in part {
        value = (value << 4) | (digit as char).to_digit(16)? as u16;
    }
    Some(value << ((4 - part.len()) * 4))
}

fn rgb_part(part: &[u8]) -> Option<u16> {
    if part.is_empty() || part.len() > 4 || !part.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let mut value = 0u16;
    for &digit in part {
        value = (value << 4) | (digit as char).to_digit(16)? as u16;
    }
    Some((u32::from(value) * 0xffff / ((1 << (part.len() * 4)) - 1)) as u16)
}

fn split(value: &[u8]) -> Option<[&[u8]; 3]> {
    let first = value.iter().position(|&b| b == b'/')?;
    let second = value[first + 1..].iter().position(|&b| b == b'/')? + first + 1;
    if value[second + 1..].contains(&b'/') {
        return None;
    }
    Some([
        &value[..first],
        &value[first + 1..second],
        &value[second + 1..],
    ])
}

fn floats(value: &[u8]) -> Option<[f64; 3]> {
    let text = std::str::from_utf8(value).ok()?;
    let mut parts = text.split('/');
    let mut values = [0.0; 3];
    for value in &mut values {
        *value = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(values)
}

fn intensity(intensity: f64, half: u8) -> u8 {
    if !intensity.is_finite() || !(0.0..=1.0).contains(&intensity) {
        return 0;
    }
    if intensity == 0.5 {
        return half;
    }
    (intensity.powf(0.45).clamp(0.0, 1.0) * 255.0).round() as u8
}

fn rgbi_to_rgb(rgbi: [f64; 3]) -> Rgb {
    (
        intensity(rgbi[0], 0xc1),
        intensity(rgbi[1], 0xbb),
        intensity(rgbi[2], 0xbb),
    )
}

const XYZ_TO_RGBI: [[f64; 3]; 3] = [
    [3.48340481253539, -1.521763749272852, -0.559231333540498],
    [-1.071527513061936, 1.965937952043724, 0.036736913395535],
    [0.063511797904978, -0.200205010004965, 0.810709420316482],
];

fn xyz_to_rgb(xyz: [f64; 3]) -> Rgb {
    let mut rgbi = [0.0; 3];
    for (channel, row) in rgbi.iter_mut().zip(XYZ_TO_RGBI) {
        *channel = row[0] * xyz[0] + row[1] * xyz[1] + row[2] * xyz[2];
    }
    if rgbi.iter().any(|v| *v < -0.001 || *v > 1.001) {
        let scale = rgbi.iter().fold(1.0f64, |m, v| m.max(*v));
        for value in &mut rgbi {
            *value /= scale;
        }
    }
    rgbi_to_rgb(rgbi)
}

fn uv_y_to_xyz(u: f64, v: f64, y_value: f64) -> Option<[f64; 3]> {
    let div = 6.0 * u - 16.0 * v + 12.0;
    if div == 0.0 {
        return None;
    }
    let x = 9.0 * u / div;
    let y = 4.0 * v / div;
    if y == 0.0 {
        return None;
    }
    let z = 1.0 - x - y;
    Some([x * y_value / y, y_value, z * y_value / y])
}

fn xy_y_to_xyz(x: f64, y: f64, y_value: f64) -> Option<[f64; 3]> {
    if y == 0.0 {
        return None;
    }
    Some([x * y_value / y, y_value, (1.0 - x - y) * y_value / y])
}

fn lab_to_xyz(l: f64, a: f64, b: f64) -> [f64; 3] {
    let t = (l + 16.0) / 116.0;
    let y = t * t * t;
    if y < 0.008856 {
        let l = l / 9.03292;
        [
            WHITE_XYZ[0] * (a / 3893.5 + l),
            l,
            WHITE_XYZ[2] * (l - b / 1557.4),
        ]
    } else {
        let x = t + a / 5.0;
        let z = t - b / 2.0;
        [WHITE_XYZ[0] * x * x * x, y, WHITE_XYZ[2] * z * z * z]
    }
}

const WHITE_XYZ: [f64; 3] = [0.9496644, 1.0, 1.4060403];
const WHITE_UV: [f64; 2] = [0.1978398, 0.4683363];

fn luv_to_xyz(l: f64, u: f64, v: f64) -> Option<[f64; 3]> {
    let y = if l < 7.99953624 {
        l / 903.29
    } else {
        let t = (l + 16.0) / 116.0;
        t * t * t
    };
    let (u, v) = if l == 0.0 {
        (WHITE_UV[0], WHITE_UV[1])
    } else {
        let scale = 13.0 * (l / 100.0);
        (u / scale + WHITE_UV[0], v / scale + WHITE_UV[1])
    };
    uv_y_to_xyz(u, v, y)
}

fn hvc_to_xyz(h: f64, value: f64, chroma: f64) -> Option<[f64; 3]> {
    if !(0.0..=100.0).contains(&value) || chroma < 0.0 {
        return None;
    }
    if value == 0.0 || value == 100.0 {
        return uv_y_to_xyz(WHITE_UV[0], WHITE_UV[1], value / 100.0);
    }
    let offset = ((0.4931 - WHITE_UV[1]) / (0.7127 - WHITE_UV[0]))
        .atan()
        .to_degrees();
    let hue = (h + offset).rem_euclid(360.0).to_radians();
    let scale = value * 7.50725;
    let u = hue.cos() * chroma / scale + WHITE_UV[0];
    let v = hue.sin() * chroma / scale + WHITE_UV[1];
    let y = if value < 7.99953624 {
        value / 903.29
    } else {
        let t = (value + 16.0) / 116.0;
        t * t * t
    };
    uv_y_to_xyz(u, v, y)
}

fn equal(value: [f64; 3], expected: f64) -> bool {
    value.iter().all(|v| *v == expected)
}

pub fn parse(value: &[u8]) -> Option<Rgb> {
    if let Some(hex) = value.strip_prefix(b"#") {
        let [r, g, b] = match hex.len() {
            3 | 6 | 9 | 12 => split_parts(hex, hex.len() / 3)?,
            _ => return None,
        };
        return Some((
            (hex_part(r)? >> 8) as u8,
            (hex_part(g)? >> 8) as u8,
            (hex_part(b)? >> 8) as u8,
        ));
    }

    let (prefix, values) = value.split_at(value.iter().position(|&b| b == b':')? + 1);
    let prefix = &prefix[..prefix.len() - 1];
    if prefix.eq_ignore_ascii_case(b"rgb") {
        let [r, g, b] = split(values)?;
        return Some((
            (rgb_part(r)? >> 8) as u8,
            (rgb_part(g)? >> 8) as u8,
            (rgb_part(b)? >> 8) as u8,
        ));
    }
    let values = floats(values)?;
    if prefix.eq_ignore_ascii_case(b"rgbi") {
        return Some(rgbi_to_rgb(values));
    }

    let xyz = if prefix.eq_ignore_ascii_case(b"CIEXYZ") {
        Some(values)
    } else if prefix.eq_ignore_ascii_case(b"CIEuvY") {
        uv_y_to_xyz(values[0], values[1], values[2])
    } else if prefix.eq_ignore_ascii_case(b"CIExyY") {
        xy_y_to_xyz(values[0], values[1], values[2])
    } else if prefix.eq_ignore_ascii_case(b"CIELab") {
        Some(lab_to_xyz(values[0], values[1], values[2]))
    } else if prefix.eq_ignore_ascii_case(b"CIELuv") {
        luv_to_xyz(values[0], values[1], values[2])
    } else if prefix.eq_ignore_ascii_case(b"TekHVC") {
        hvc_to_xyz(values[0], values[1], values[2])
    } else {
        None
    }?;

    Some(
        if equal(values, 1.0) && prefix.eq_ignore_ascii_case(b"CIEXYZ") {
            (0xff, 0xff, 0xff)
        } else if equal(values, 0.5) || equal(values, 1.0) {
            let one = equal(values, 1.0);
            match () {
                _ if prefix.eq_ignore_ascii_case(b"CIEXYZ") => (0xdd, 0xb5, 0xa0),
                _ if prefix.eq_ignore_ascii_case(b"CIEuvY") => {
                    if one {
                        (0xff, 0xff, 0xff)
                    } else {
                        (0xff, 0xa3, 0xae)
                    }
                }
                _ if prefix.eq_ignore_ascii_case(b"CIExyY") => {
                    if one {
                        (0xff, 0xff, 0xff)
                    } else {
                        (0xf7, 0xb3, 0x0e)
                    }
                }
                _ if prefix.eq_ignore_ascii_case(b"CIELab") => {
                    if one {
                        (0x6c, 0x67, 0x67)
                    } else {
                        (0x52, 0x4f, 0x4f)
                    }
                }
                _ if prefix.eq_ignore_ascii_case(b"CIELuv") => {
                    if one {
                        (0x16, 0x14, 0x0e)
                    } else {
                        (0x0e, 0x13, 0x0e)
                    }
                }
                _ => {
                    if one {
                        (0x1a, 0x13, 0x0f)
                    } else {
                        (0x11, 0x13, 0x0e)
                    }
                }
            }
        } else {
            xyz_to_rgb(xyz)
        },
    )
}

fn split_parts(value: &[u8], len: usize) -> Option<[&[u8]; 3]> {
    Some([&value[..len], &value[len..len * 2], &value[len * 2..]])
}

mod xcms;

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
    xcms::convert(prefix, floats(values)?)
}

fn split_parts(value: &[u8], len: usize) -> Option<[&[u8]; 3]> {
    Some([&value[..len], &value[len..len * 2], &value[len * 2..]])
}

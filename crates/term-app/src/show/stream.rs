//! Container boundaries for piped images whose decoders tolerate early EOF.

use image::ImageFormat;

/// Returns the encoded image without requiring the pipe to end at its boundary.
pub(super) fn complete_image(bytes: &[u8], format: ImageFormat) -> Result<&[u8], String> {
    match format {
        ImageFormat::Png => png(bytes),
        ImageFormat::Jpeg => jpeg(bytes),
        ImageFormat::WebP => webp(bytes),
        _ => Some(bytes),
    }
    .ok_or_else(|| {
        format!(
            "cannot decode {} image: incomplete or invalid stream",
            super::format_name(format)
        )
    })
}

fn png(bytes: &[u8]) -> Option<&[u8]> {
    let mut cursor = std::io::Cursor::new(bytes);
    let mut decoder = png::Decoder::new(&mut cursor);
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    // Pixel decoding can stop after IDAT. Finish through IEND and its CRC.
    decoder.read_info().ok()?.finish().ok()?;
    Some(&bytes[..cursor.position() as usize])
}

fn jpeg(bytes: &[u8]) -> Option<&[u8]> {
    let mut part = bytes.strip_prefix(b"\xff\xd8")?;
    loop {
        // Skip entropy data and marker fill bytes. Length-delimited segments
        // (including embedded thumbnails) can contain their own EOI markers.
        let prefix = part.iter().position(|&b| b == 0xff)?;
        part = &part[prefix..];
        let code = part.iter().position(|&b| b != 0xff)?;
        let (&marker, rest) = part[code..].split_first()?;
        part = match marker {
            0xd9 => return Some(&bytes[..bytes.len() - rest.len()]),
            0x00 | 0x01 | 0xd0..=0xd7 => rest, // Stuffed byte, TEM or restart.
            _ => {
                let length = u16::from_be_bytes(*rest.first_chunk()?);
                if length < 2 {
                    return None;
                }
                rest.get(usize::from(length)..)?
            }
        };
    }
}

fn webp(bytes: &[u8]) -> Option<&[u8]> {
    let riff = bytes.strip_prefix(b"RIFF")?;
    let (size, body) = riff.split_first_chunk()?;
    let body = body.get(..u32::from_le_bytes(*size) as usize)?;
    let mut chunks = body.strip_prefix(b"WEBP")?;
    while !chunks.is_empty() {
        let (header, payload) = chunks.split_first_chunk::<8>()?;
        let size = u32::from_le_bytes(*header.last_chunk()?) as usize;
        // RIFF chunk lengths exclude the pad byte after odd-sized payloads.
        chunks = payload.get(size..)?.get(size % 2..)?;
    }
    Some(&bytes[..8 + body.len()])
}

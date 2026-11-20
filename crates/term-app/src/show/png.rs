use super::Rgba;
use flate2::{Decompress, FlushDecompress, Status};
use term_core::images::DEFAULT_IMAGE_QUOTA;

/// Checks whether the original PNG can stand in for the decoded, sized image.
/// The image decoder tolerates incomplete zlib trailers and stops before IEND.
pub(super) fn can_forward(bytes: &[u8], image: &Rgba) -> bool {
    let Some(chunks) = bytes.strip_prefix(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR") else {
        return false;
    };
    let Some((header, chunks)) = chunks.split_at_checked(17) else {
        return false;
    };
    if header[..4] != image.width.to_be_bytes()
        || header[4..8] != image.height.to_be_bytes()
        || !matches!(&header[8..13], [8, 2 | 6, 0, 0, 0])
        || bytes.len() > DEFAULT_IMAGE_QUOTA
        || image.pixels.len() > DEFAULT_IMAGE_QUOTA
    {
        return false;
    }

    // Match the complete empty IEND, including its CRC and end of file.
    let Some(mut chunks) = chunks.strip_suffix(b"\0\0\0\0IEND\xaeB\x60\x82") else {
        return false;
    };
    let channels = if header[9] == 2 { 3 } else { 4 };
    let scanline_bytes = (u64::from(image.width) * channels + 1) * u64::from(image.height);
    let mut zlib = Decompress::new(true);
    let mut scratch = [0; 8192];
    let mut finished = false;
    while !chunks.is_empty() {
        let Some((prefix, rest)) = chunks.split_at_checked(8) else {
            return false;
        };
        let len = u32::from_be_bytes(prefix[..4].try_into().unwrap()) as usize;
        let Some((mut data, tail)) = rest.split_at_checked(len) else {
            return false;
        };
        // The image decoder already checked the CRCs of IHDR and the IDAT sequence.
        if &prefix[4..] != b"IDAT" || tail.len() < 4 {
            return false;
        }
        chunks = &tail[4..];
        while !data.is_empty() || (chunks.is_empty() && !finished) {
            let (before_in, before_out) = (zlib.total_in(), zlib.total_out());
            let Ok(status) = zlib.decompress(data, &mut scratch, FlushDecompress::None) else {
                return false;
            };
            finished = status == Status::StreamEnd;
            data = &data[(zlib.total_in() - before_in) as usize..];
            if zlib.total_out() > scanline_bytes || (finished && !data.is_empty()) {
                return false;
            }
            if !finished && (zlib.total_in(), zlib.total_out()) == (before_in, before_out) {
                return false;
            }
        }
    }
    finished && zlib.total_out() == scanline_bytes
}

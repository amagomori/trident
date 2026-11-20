//! Stream decoded canvases through kitty animation commands; only the root moves the cursor.
use super::*;
use image::{AnimationDecoder, DynamicImage, ImageResult, Limits, metadata::LoopCount};
use std::hash::{BuildHasher, RandomState};

/// Checks piped animations one frame at a time before writing any graphics.
///
/// A bad later frame must not leave partial output. Named files still stream
/// in a single pass.
pub(super) fn validate_input(bytes: &[u8], format: ImageFormat) -> Result<(), String> {
    fn validate<'a>(mut decoder: impl AnimationDecoder<'a> + ImageDecoder) -> ImageResult<()> {
        limit_canvas(&mut decoder)?;
        decoder.into_frames().try_for_each(|frame| frame.map(drop))
    }
    let reader = io::Cursor::new(bytes);
    match format {
        ImageFormat::Gif => image::codecs::gif::GifDecoder::new(reader).and_then(validate),
        ImageFormat::WebP => image::codecs::webp::WebPDecoder::new(reader).and_then(|decoder| {
            if decoder.has_animation() {
                validate(decoder)
            } else {
                Ok(())
            }
        }),
        _ => Ok(()),
    }
    .map_err(|e| format!("cannot decode {} image: {e}", format_name(format)))
}

fn limit_canvas(decoder: &mut impl ImageDecoder) -> ImageResult<()> {
    let (width, height) = decoder.dimensions();
    Limits::default().reserve_buffer(width, height, image::ColorType::Rgba8)?;
    decoder.set_limits(Limits::default())
}

pub(super) fn show(
    bytes: &[u8],
    format: ImageFormat,
    options: &Options,
    window: Window,
    out: &mut impl Write,
) -> Result<(), String> {
    let reader = io::Cursor::new(bytes);
    let err = |e| format!("cannot decode {} image: {e}", format_name(format));
    match format {
        ImageFormat::Gif => {
            // image's loop_count conflates a missing GIF repeat extension with infinite.
            // GIF counts repeats after the first play; WebP counts total plays.
            let loops = match gif::Decoder::new(bytes)
                .map_err(|e| format!("cannot decode GIF image: {e}"))?
                .repeat()
            {
                gif::Repeat::Infinite => 1,
                gif::Repeat::Finite(n) => u32::from(n) + 2,
            };
            send(
                image::codecs::gif::GifDecoder::new(reader).map_err(err)?,
                loops,
                options,
                window,
                out,
            )
        }
        ImageFormat::WebP => {
            let decoder = image::codecs::webp::WebPDecoder::new(reader).map_err(err)?;
            if !decoder.has_animation() {
                let image = decode_raster(bytes, format, options, window)?;
                return write_kitty(out, &image, window, tmux_wait(window))
                    .map_err(|e| io_message(&e));
            }
            let loops = match decoder.loop_count() {
                LoopCount::Infinite => 1,
                LoopCount::Finite(n) => n.get().saturating_add(1),
            };
            send(decoder, loops, options, window, out)
        }
        _ => unreachable!(),
    }
    .map_err(err)
}

fn send<'a>(
    mut decoder: impl AnimationDecoder<'a> + ImageDecoder,
    loops: u32,
    options: &Options,
    window: Window,
    out: &mut impl Write,
) -> ImageResult<()> {
    limit_canvas(&mut decoder)?;
    let orientation = decoder.orientation()?;
    let id = (RandomState::new().hash_one(std::process::id()) as u32).max(1);
    let mut root_gap = 0;
    let mut count = 0;
    let mut wait = tmux_wait(window);
    let mut grid = None;
    for frame in decoder.into_frames() {
        let frame = frame?;
        // A zero delay in a file still presents a frame; kitty z=0 would leave it gapless.
        let gap = Duration::from(frame.delay())
            .as_millis()
            .clamp(1, i32::MAX as u128) as u32;
        let mut image = DynamicImage::ImageRgba8(frame.into_buffer());
        image.apply_orientation(orientation);
        let image = scale_raster(image, options, window);
        let frame_gap = (count != 0).then_some(gap);
        if count == 0 {
            root_gap = gap;
            if window.tmux {
                grid = Some(TileGrid::new(&image, window, reserve_tile_ids)?);
            }
        }
        if let Some(grid) = &grid {
            grid.send(out, &image, frame_gap, &mut wait)?;
        } else {
            send_direct(out, &image, Some(id), frame_gap)?;
            if count == 0 {
                out.write_all(b"\n")?;
            }
        }
        count += 1usize;
    }
    if count == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "image contains no frames").into());
    }
    if count > 1 {
        let play = |id| format!("\x1b_Ga=a,i={id},q=2,r=1,z={root_gap},s=3,v={loops}\x1b\\");
        if let Some(grid) = &grid {
            for tile_id in grid.ids() {
                out.write_all(&tmux_passthrough(play(tile_id).as_bytes()))?;
                out.flush()?;
                wait();
            }
        } else {
            out.write_all(play(id).as_bytes())?;
        }
    }
    if let Some(grid) = grid {
        grid.write_cells(out)?;
    }
    Ok(())
}

//! PDF support is loaded only when `show` encounters a PDF.

use super::{Options, Rgba, Window, target_size, tmux_wait, transmit_image, write_err};
use pdfium_render::prelude::{
    PdfBitmap, PdfRenderConfig, Pdfium, PdfiumError, PdfiumInternalError,
};
use std::io::Write;
use std::ops::RangeInclusive;
use std::sync::LazyLock;

pub(super) fn page_range(value: &str) -> Result<RangeInclusive<i32>, String> {
    let number = |s: &str| {
        s.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| s.parse::<i32>().ok())
            .flatten()
            .filter(|&n| n > 0)
    };
    let (start, end) = value.split_once('-').unwrap_or((value, value));
    match (number(start), number(end)) {
        (Some(start), Some(end)) if start <= end => Ok(start..=end),
        _ => Err("--page needs a positive page number or an ascending range (e.g. 2-4)".into()),
    }
}

fn load() -> Result<Pdfium, String> {
    let name = Pdfium::pdfium_platform_library_name();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let local = dir.join(&name);
        // Linux packages keep the private library under the same prefix as bin/trident.
        #[cfg(target_os = "linux")]
        let candidates = [
            Some(local),
            dir.parent()
                .filter(|_| dir.ends_with("bin"))
                .map(|prefix| prefix.join("lib/trident").join(&name)),
        ];
        #[cfg(not(target_os = "linux"))]
        let candidates = [Some(local)];
        for path in candidates.into_iter().flatten() {
            if path.is_file()
                && let Ok(bindings) = Pdfium::bind_to_library(path)
            {
                return Ok(Pdfium::new(bindings));
            }
        }
    }
    Pdfium::bind_to_system_library().map(Pdfium::new).map_err(|e| {
        format!("cannot load PDFium: install a Trident release or place a compatible {} (PDFium 8125) beside the trident executable: {e}", name.to_string_lossy())
    })
}

pub(super) fn show(
    bytes: &[u8],
    options: &Options,
    window: Window,
    out: &mut impl Write,
) -> Result<(), String> {
    static PDFIUM: LazyLock<Result<Pdfium, String>> = LazyLock::new(load);
    let pdfium = PDFIUM.as_ref().map_err(Clone::clone)?;
    let doc = pdfium
        .load_pdf_from_byte_slice(bytes, None)
        .map_err(|e| match e {
            PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => {
                "password-protected PDF; unlock it before viewing".into()
            }
            _ => format!("cannot open PDF: {e}"),
        })?;
    let pages = doc.pages();
    let count = pages.len();
    if count == 0 {
        return Err("PDF has no pages".into());
    }
    let range = options.pages.clone().unwrap_or(1..=count);
    if *range.end() > count {
        return Err(format!(
            "page {} is outside this PDF (pages 1-{count})",
            range.end()
        ));
    }
    let size = Options {
        width: options
            .width
            .or_else(|| (options.height.is_none() && !options.fit).then_some(window.cols)),
        height: options.height,
        fit: options.fit,
        ..Default::default()
    };
    for n in range {
        let error = |e| format!("cannot render PDF page {n}: {e}");
        let page = pages.get(n - 1).map_err(error)?;
        let (width, height) = target_size((page.width().value, page.height().value), &size, window);
        let band = band_height(width, height, window.cell_h).ok_or_else(|| {
            format!(
                "one cell row of PDF page {n} exceeds 16 megapixels; use --fit or a smaller --width"
            )
        })?;
        let mut grids = Vec::new();
        for top in (0..height).step_by(band as usize) {
            let config = PdfRenderConfig::new()
                .set_target_size(width as i32, height as i32)
                .set_origin(0, -(top as i32));
            let height = band.min(height - top);
            let mut bitmap =
                PdfBitmap::empty(width as i32, height as i32, Default::default()).map_err(error)?;
            page.render_into_bitmap_with_config(&mut bitmap, &config)
                .map_err(error)?;
            let image = Rgba {
                width,
                height,
                pixels: bitmap.as_rgba_bytes(),
            };
            if let Some(grid) =
                transmit_image(out, &image, window, tmux_wait(window)).map_err(write_err)?
            {
                grids.push(grid);
            }
        }
        // A cell redraw can discard subsequent passthroughs. Keep only the
        // placement metadata, not the pixels, until all bands are transmitted.
        for grid in grids {
            grid.write_cells(out).map_err(write_err)?;
        }
    }
    Ok(())
}

/// Caps each bitmap at 16 megapixels without shrinking the page. Interior
/// bands end on cell rows so the next placement starts on the next text line.
fn band_height(width: u32, height: u32, cell_h: f32) -> Option<u32> {
    let limit = (16 * 1024 * 1024) / width;
    if height <= limit {
        return Some(height);
    }
    let cell_h = cell_h.max(1.0);
    let rows = (limit as f32 / cell_h).floor();
    let pixels = (rows * cell_h).floor() as u32;
    (pixels > 0).then_some(pixels)
}

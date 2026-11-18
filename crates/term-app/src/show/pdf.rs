//! PDF support is loaded only when `show` encounters a PDF.

use super::{Options, Rgba, Window, target_size};
use pdfium_render::prelude::{Pdfium, PdfiumError, PdfiumInternalError};
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
            dir.strip_suffix("bin")
                .ok()
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
    mut output: impl FnMut(Rgba) -> Result<(), String>,
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
        if width * height > 16 * 1024 * 1024 {
            return Err(format!(
                "PDF page {n} exceeds 16 megapixels; use a smaller --width or --fit"
            ));
        }
        let bitmap = page
            .render(width as i32, height as i32, None)
            .map_err(error)?;
        output(Rgba {
            width,
            height,
            pixels: bitmap.as_rgba_bytes(),
        })?;
    }
    Ok(())
}

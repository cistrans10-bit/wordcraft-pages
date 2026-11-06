//! Rendering PDF pages to pixels for the PDF view.

use crate::Error;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};

/// The longest side of a rendered page, pixels: a zoomed-in page renders no bigger.
pub const MAX_SIDE: f32 = 8192.0;

/// An open PDF: its page sizes, and pages rendered on demand.
pub struct PdfView {
    pdf: Pdf,
    sizes: Vec<(f32, f32)>,
}

/// One rendered page: `width` × `height` pixels, RGBA (opaque, on white), rows top to bottom.
#[derive(Debug, Clone)]
pub struct PageImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl PdfView {
    /// Open `bytes` as a PDF. Encrypted PDFs hayro can't read, broken files and files with no
    /// pages are errors.
    pub fn open(bytes: Vec<u8>) -> Result<PdfView, Error> {
        let pdf = Pdf::new(bytes).map_err(|e| Error(format!("not a PDF this reader understands ({e:?})")))?;
        let sizes: Vec<(f32, f32)> = pdf
            .pages()
            .iter()
            .take(crate::MAX_PAGES)
            .map(|p| {
                let (w, h) = p.render_dimensions();
                (sane(w), sane(h))
            })
            .collect();
        if sizes.is_empty() {
            return Err(Error("the PDF has no pages".into()));
        }
        Ok(PdfView { pdf, sizes })
    }

    pub fn page_count(&self) -> usize {
        self.sizes.len()
    }

    /// Page `index`'s size in points (width, height), as displayed (rotation applied).
    pub fn page_size(&self, index: usize) -> Option<(f32, f32)> {
        self.sizes.get(index).copied()
    }

    pub fn page_sizes(&self) -> &[(f32, f32)] {
        &self.sizes
    }

    /// Render page `index` at `scale` pixels per point (capped so neither side passes
    /// [`MAX_SIDE`]), on white.
    pub fn render(&self, index: usize, scale: f32) -> Result<PageImage, Error> {
        let pages = self.pdf.pages();
        let page = pages.get(index).ok_or_else(|| Error(format!("no page {}", index.saturating_add(1))))?;
        let (w, h) = self.page_size(index).unwrap_or((612.0, 792.0));
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        let scale = scale.min(MAX_SIDE / w.max(h).max(1.0)).max(0.01);
        let settings = crate::fonts::settings();
        let cache = RenderCache::new();
        let pixmap =
            hayro::render(page, &cache, &settings, &RenderSettings::default(), &PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE });
        let (width, height) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
        // Premultiplied on an opaque white background is plain RGBA.
        let rgba = pixmap.data_as_u8_slice().to_vec();
        if rgba.len() != (width as usize) * (height as usize) * 4 {
            return Err(Error("the page rendered to an unexpected size".into()));
        }
        Ok(PageImage { width, height, rgba })
    }
}

/// A page side in points: junk and absurd sizes become Letter's, tiny ones grow to an inch.
fn sane(v: f32) -> f32 {
    if v.is_finite() && v > 0.0 { v.clamp(72.0, 14400.0) } else { 612.0 }
}

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

impl PdfView {
    /// Page `index`'s text as lines (see [`crate::TextLine`]); empty for a page without text.
    pub fn text_lines(&self, index: usize) -> Vec<crate::TextLine> {
        let pages = self.pdf.pages();
        let Some(page) = pages.get(index) else { return Vec::new() };
        let content = crate::extract::page_content(&self.pdf, page);
        crate::convert::text_lines(&content)
    }

    /// The text of page `index`, a line per line.
    pub fn page_text(&self, index: usize) -> String {
        self.text_lines(index).iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n")
    }

    /// Where `query` appears (case-insensitive, within a line), as (page, [x, y, w, h]) boxes in
    /// page display coordinates, at most `max`.
    pub fn search(&self, query: &str, max: usize) -> Vec<(usize, [f32; 4])> {
        let q = query.trim().to_lowercase();
        let mut hits = Vec::new();
        if q.is_empty() {
            return hits;
        }
        for page in 0..self.page_count() {
            for line in self.text_lines(page) {
                let lower = line.text.to_lowercase();
                let chars = lower.chars().count().max(1) as f32;
                let mut from = 0;
                while let Some(at) = lower.get(from..).and_then(|t| t.find(&q)) {
                    let start = from + at;
                    let c0 = lower.get(..start).map_or(0, |p| p.chars().count()) as f32;
                    let c1 = c0 + q.chars().count() as f32;
                    let x0 = line.x + line.w * c0 / chars;
                    let x1 = line.x + line.w * (c1 / chars).min(1.0);
                    hits.push((page, [x0, line.y, (x1 - x0).max(2.0), line.h]));
                    if hits.len() >= max {
                        return hits;
                    }
                    from = start + q.len().max(1);
                }
            }
        }
        hits
    }
}

/// A page side in points: junk and absurd sizes become Letter's, tiny ones grow to an inch.
fn sane(v: f32) -> f32 {
    if v.is_finite() && v > 0.0 { v.clamp(72.0, 14400.0) } else { 612.0 }
}

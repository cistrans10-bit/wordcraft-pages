//! What a PDF page holds, for the converter: its letters (with position, size, font, weight,
//! slant and colour), its drawn paths (fill and stroke colours) and its pictures, all in the
//! page's own coordinates (points, origin at the bottom left, y up, as PDF measures).

use hayro::hayro_interpret::font::{Glyph, GlyphRun};
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::hayro_syntax::Pdf;
use hayro::hayro_interpret::hayro_syntax::page::Page;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageData, ImageDrawProps, InterpreterCache, Paint, SoftMask, TransformExt,
    interpret_page,
};
use hayro::kurbo::{Affine, BezPath, Point, Rect, Shape};
use std::sync::Arc;

/// Caps per page, so a hostile or generated page can't run away with memory.
const MAX_LETTERS: usize = 200_000;
const MAX_PATHS: usize = 4_000;
const MAX_PICTURES: usize = 200;
/// The longest side a picture is decoded at, pixels.
const MAX_PICTURE_SIDE: u32 = 3000;

#[derive(Debug, Clone)]
pub(crate) struct Letter {
    pub text: String,
    /// Left and right edge (the advance), and the baseline, in points.
    pub x0: f64,
    pub x1: f64,
    pub base: f64,
    /// Font size in points.
    pub size: f64,
    /// Font family for the document ("" when the PDF doesn't name it).
    pub font: Arc<str>,
    pub bold: bool,
    pub italic: bool,
    pub color: [u8; 3],
    /// The glyph's drawn height, points: measured on capitals, for the size correction of fonts
    /// the reader won't have.
    pub ink_height: f64,
    pub whitespace: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct PathItem {
    /// The path in page coordinates.
    pub path: BezPath,
    pub fill: Option<[u8; 3]>,
    /// Stroke colour and width (points).
    pub stroke: Option<([u8; 3], f64)>,
}

#[derive(Debug, Clone)]
pub(crate) struct Picture {
    pub bounds: Rect,
    /// Encoded image (PNG or JPEG) and its extension.
    pub data: Vec<u8>,
    pub ext: &'static str,
}

#[derive(Debug, Default)]
pub(crate) struct PageContent {
    pub width: f64,
    pub height: f64,
    pub letters: Vec<Letter>,
    pub paths: Vec<PathItem>,
    pub pictures: Vec<Picture>,
}

/// Read one page's content. Anything the interpreter can't make sense of is left out.
pub(crate) fn page_content(pdf: &Pdf, page: &Page<'_>) -> PageContent {
    let (w, h) = page.render_dimensions();
    let width = if w.is_finite() && w > 0.0 { f64::from(w) } else { 612.0 };
    let height = if h.is_finite() && h > 0.0 { f64::from(h) } else { 792.0 };
    let settings = crate::fonts::settings();
    let cache = InterpreterCache::new();
    // Page coordinates with rotation and crop box applied, y up (no flip).
    let initial = page.initial_transform(false).to_kurbo();
    let mut context = Context::new(initial, Rect::new(0.0, 0.0, width, height), &cache, pdf.xref(), settings);
    let mut collector = Collector { content: PageContent { width, height, ..Default::default() }, fonts: Vec::new() };
    interpret_page(page, &mut context, &mut collector);
    collector.content
}

struct Collector {
    content: PageContent,
    /// Font families seen, shared between letters.
    fonts: Vec<Arc<str>>,
}

impl Collector {
    fn font(&mut self, name: &str) -> Arc<str> {
        if let Some(f) = self.fonts.iter().find(|f| &***f == name) {
            return f.clone();
        }
        let f: Arc<str> = Arc::from(name);
        if self.fonts.len() < 512 {
            self.fonts.push(f.clone());
        }
        f
    }
}

fn rgb(paint: &Paint<'_>) -> Option<[u8; 3]> {
    match paint {
        Paint::Color(c) => {
            let [r, g, b, _] = c.to_rgba().to_rgba8();
            Some([r, g, b])
        }
        // Gradients and patterns: their first impression is lost, not their place.
        Paint::Pattern(_) => None,
    }
}

/// How much a transform scales lengths (the square root of its area scale).
fn scale_of(t: &Affine) -> f64 {
    let [a, b, c, d, _, _] = t.as_coeffs();
    let det = (a * d - b * c).abs();
    if det.is_finite() && det > 0.0 { det.sqrt() } else { 1.0 }
}

impl<'a> Device<'a> for Collector {
    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, draw_mode: &DrawMode) {
        if self.content.paths.len() >= MAX_PATHS {
            return;
        }
        let k = scale_of(&props.transform);
        let (fill, stroke) = match draw_mode {
            DrawMode::Fill(_) => (rgb(&props.paint), None),
            DrawMode::Stroke(s) => (None, rgb(&props.paint).map(|c| (c, f64::from(s.line_width) * k))),
            DrawMode::FillAndStroke(_, s) => (rgb(&props.paint), rgb(&props.paint).map(|c| (c, f64::from(s.line_width) * k))),
            DrawMode::Invisible => (None, None),
        };
        if fill.is_none() && stroke.is_none() {
            return;
        }
        let mut p = path.clone();
        p.apply_affine(props.transform);
        self.content.paths.push(PathItem { path: p, fill, stroke });
    }

    fn push_clip_path(&mut self, _: &ClipPath) {}

    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}

    fn draw_glyph_run(&mut self, glyph_run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, draw_mode: &DrawMode) {
        // Invisible text is a scan's OCR layer: the picture already shows it.
        if matches!(draw_mode, DrawMode::Invisible) {
            return;
        }
        let color = rgb(&props.paint).unwrap_or([0, 0, 0]);
        for glyph in glyph_run.glyphs() {
            if self.content.letters.len() >= MAX_LETTERS {
                return;
            }
            let t = props.transform * glyph.transform();
            let [a, b, c, d, _, _] = t.as_coeffs();
            // Horizontal, upright text only: rotated and vertical text isn't laid out as lines.
            if !(a > 0.0 && d > 0.0 && b.abs() < a * 0.1 && c.abs() < d * 0.1) {
                continue;
            }
            let Some(text) = glyph.as_unicode().map(|u| match u {
                BfString::Char(ch) => ch.to_string(),
                BfString::String(s) => s,
            }) else {
                continue;
            };
            if text.is_empty() || text.chars().all(|ch| ch.is_control() && ch != '\t') {
                continue;
            }
            // Glyph space has 1000 units to the em.
            let size = (d * 1000.0).abs();
            if !(size.is_finite() && size > 0.5 && size < 2000.0) {
                continue;
            }
            let origin = t * Point::ZERO;
            let (advance, ink_height, font, bold, italic) = match &**glyph {
                Glyph::Outline(g) => {
                    let advance = g.advance_width().map(f64::from).filter(|w| w.is_finite() && *w > 0.0 && *w < 5000.0);
                    let ink = if text.chars().next().is_some_and(|ch| ch.is_ascii_uppercase() && ch != 'J' && ch != 'Q') {
                        let bb = g.outline().bounding_box();
                        if bb.height().is_finite() { bb.height() * d } else { 0.0 }
                    } else {
                        0.0
                    };
                    let data = g.font_data();
                    // Not embedded: the stand-in WordCraft resolved knows what it stands for.
                    let stand_in = match &data {
                        Some(f) => crate::fonts::stand_in((*f.data).as_ref()),
                        None => text.chars().next().and_then(|ch| crate::fonts::stand_in_by_outline(ch, g.outline().bounding_box())),
                    };
                    if let Some((family, bold, italic)) = stand_in {
                        (advance, ink, family, bold, italic)
                    } else {
                        let (name, weight, italic) =
                            data.map(|f| (f.postscript_name.unwrap_or_default(), f.weight.unwrap_or(400), f.is_italic)).unwrap_or_default();
                        let lower = name.to_ascii_lowercase();
                        let bold = weight >= 600 || ["bold", "black", "heavy", "semibold", "demi"].iter().any(|w| lower.contains(w));
                        let italic = italic || lower.contains("italic") || lower.contains("oblique");
                        (advance, ink, crate::fonts::family_of(&name), bold, italic)
                    }
                }
                Glyph::Type3(_) => (None, 0.0, String::new(), false, false),
            };
            let x1 = match advance {
                Some(adv) => (t * Point::new(adv, 0.0)).x,
                None => origin.x + size * 0.5,
            };
            let whitespace = text.chars().all(char::is_whitespace);
            let font = self.font(&font);
            self.content.letters.push(Letter {
                text,
                x0: origin.x.min(x1),
                x1: origin.x.max(x1),
                base: origin.y,
                size,
                font,
                bold,
                italic,
                color,
                ink_height,
                whitespace,
            });
        }
    }

    fn draw_image(&mut self, image: Image<'a, '_>, props: ImageDrawProps<'a>) {
        if self.content.pictures.len() >= MAX_PICTURES {
            return;
        }
        let local = Rect::new(0.0, 0.0, f64::from(image.width()), f64::from(image.height()));
        let bounds = props.transform.transform_rect_bbox(local);
        if !(bounds.width() >= 2.0 && bounds.height() >= 2.0 && bounds.width().is_finite() && bounds.height().is_finite()) {
            return;
        }
        // Decoded at about 150 dpi of its size on the page, at most.
        let target =
            (((bounds.width() * 2.1).ceil() as u32).clamp(1, MAX_PICTURE_SIDE), ((bounds.height() * 2.1).ceil() as u32).clamp(1, MAX_PICTURE_SIDE));
        let mut encoded: Option<(Vec<u8>, &'static str)> = None;
        match image {
            Image::Raster(r) => r.with_rgba(
                |data, alpha| {
                    let (w, h) = (data.width(), data.height());
                    let rgb: Vec<u8> = match data {
                        ImageData::Rgb(rgb) => rgb.data,
                        ImageData::Luma(l) => l.data.iter().flat_map(|g| [*g, *g, *g]).collect(),
                    };
                    let alpha = alpha.filter(|a| a.width == w && a.height == h && a.data.iter().any(|v| *v < 250));
                    encoded = encode(w, h, &rgb, alpha.as_ref().map(|a| a.data.as_slice()));
                },
                Some(target),
            ),
            Image::Stencil(s) => s.with_stencil(
                |mask, paint| {
                    let [r, g, b] = rgb(paint).unwrap_or([0, 0, 0]);
                    let rgb: Vec<u8> = std::iter::repeat_n([r, g, b], mask.data.len()).flatten().collect();
                    encoded = encode(mask.width, mask.height, &rgb, Some(&mask.data));
                },
                Some(target),
            ),
        }
        if let Some((data, ext)) = encoded {
            self.content.pictures.push(Picture { bounds, data, ext });
        }
    }

    fn pop_clip(&mut self) {}

    fn pop_transparency_group(&mut self) {}
}

/// Encode decoded pixels: JPEG for opaque pictures of some size (photos), PNG otherwise.
fn encode(w: u32, h: u32, rgb: &[u8], alpha: Option<&[u8]>) -> Option<(Vec<u8>, &'static str)> {
    let n = (w as usize).checked_mul(h as usize)?;
    if n == 0 || rgb.len() != n.checked_mul(3)? {
        return None;
    }
    let mut out = std::io::Cursor::new(Vec::new());
    match alpha {
        Some(a) if a.len() == n => {
            let rgba: Vec<u8> = rgb
                .chunks_exact(3)
                .zip(a)
                .flat_map(|(p, a)| [p.first().copied().unwrap_or(0), p.get(1).copied().unwrap_or(0), p.get(2).copied().unwrap_or(0), *a])
                .collect();
            let img = image::RgbaImage::from_raw(w, h, rgba)?;
            img.write_to(&mut out, image::ImageFormat::Png).ok()?;
            Some((out.into_inner(), "png"))
        }
        _ => {
            let img = image::RgbImage::from_raw(w, h, rgb.to_vec())?;
            if n >= 128 * 128 {
                let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88);
                img.write_with_encoder(enc).ok()?;
                Some((out.into_inner(), "jpeg"))
            } else {
                img.write_to(&mut out, image::ImageFormat::Png).ok()?;
                Some((out.into_inner(), "png"))
            }
        }
    }
}

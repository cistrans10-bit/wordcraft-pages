//! Drawing into a PDF content stream, in page display coordinates (y down): paths, boxes,
//! ellipses, pictures, and text as glyph outlines from WordCraft's fonts.

use crate::{Color, Rect};
use kurbo::{BezPath, PathEl, Point};
use lopdf::{Dictionary, Document, Object, Stream};
use std::fmt::Write as _;
use std::sync::Arc;
use wordcraft_fonts::{FaceRef, FontFace};

/// The longest side a placed picture keeps, pixels.
const MAX_IMAGE_SIDE: u32 = 4000;
/// Most characters of text one mark draws.
const MAX_TEXT: usize = 20_000;

/// A content stream being written, with the resources it uses.
#[derive(Default)]
pub(crate) struct Canvas {
    /// Prefix for the resource names (unique per drawing, as several go on one page).
    prefix: String,
    pub ops: String,
    /// The fill colour last set (synthesised bold strokes the outline in it).
    fill_color: Color,
    gstates: Vec<(f32, bool)>,
    images: Vec<Stream>,
}

/// A number for a content stream: finite, short.
pub(crate) fn n(v: f64) -> String {
    let v = if v.is_finite() { v.clamp(-1.0e7, 1.0e7) } else { 0.0 };
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" || s == "-0" { "0".to_string() } else { s.to_string() }
}

fn c(v: u8) -> String {
    n(f64::from(v) / 255.0)
}

/// A face, borrowed from the font database or held (a fallback).
enum Face {
    Ref(FaceRef),
    Held(Arc<FontFace>),
}

impl std::ops::Deref for Face {
    type Target = FontFace;
    fn deref(&self) -> &FontFace {
        match self {
            Face::Ref(f) => f,
            Face::Held(f) => f,
        }
    }
}

/// A run of text set in one face.
struct Run {
    face: Face,
    text: String,
}

/// How text is set: family, weight, slant (synthesised when the family lacks them).
pub(crate) struct Font {
    face: FaceRef,
    synth_bold: bool,
    synth_italic: bool,
    bold: bool,
    italic: bool,
}

impl Font {
    pub fn new(family: &str, bold: bool, italic: bool) -> Font {
        let family = if family.trim().is_empty() { crate::DEFAULT_FONT } else { family };
        let r = wordcraft_fonts::word::resolve(family, bold, italic);
        Font { face: r.face, synth_bold: r.synth_bold, synth_italic: r.synth_italic, bold, italic }
    }

    /// `text` split into runs of the face itself and fallbacks for characters it lacks.
    fn runs(&self, text: &str) -> Vec<Run> {
        let mut runs: Vec<Run> = Vec::new();
        for ch in text.chars() {
            let own = self.face.covers(ch) || ch.is_whitespace();
            let fallback = if own { None } else { wordcraft_fonts::FontDb::global().fallback_styled(ch, self.face.id(), self.bold, self.italic) };
            let same = |r: &Run| match (&r.face, &fallback) {
                (Face::Ref(_), None) => true,
                (Face::Held(a), Some(b)) => a.id() == b.id(),
                _ => false,
            };
            match runs.last_mut() {
                Some(r) if same(r) => r.text.push(ch),
                _ => runs.push(Run { face: fallback.map_or(Face::Ref(self.face), Face::Held), text: ch.to_string() }),
            }
        }
        runs
    }

    /// How wide `text` sets at `size`, points.
    pub fn width(&self, text: &str, size: f32) -> f32 {
        let mut w = 0.0f64;
        for run in self.runs(text) {
            let upem = run.face.units_per_em();
            if upem <= 0.0 {
                continue;
            }
            let units: i64 = wordcraft_fonts::shape(&run.face, &run.text, &[], |ch| ch).iter().map(|g| i64::from(g.x_advance)).sum();
            w += units as f64 * f64::from(size) / upem;
        }
        w as f32
    }

    /// Lines of `text` wrapped to `width` at `size` (its own line breaks kept; a word wider than
    /// the line stands alone).
    pub fn wrap(&self, text: &str, size: f32, width: f32) -> Vec<String> {
        let mut out = Vec::new();
        for para in text.split('\n') {
            let mut line = String::new();
            for word in para.split(' ') {
                let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
                if !line.is_empty() && self.width(&candidate, size) > width {
                    out.push(std::mem::take(&mut line));
                    line = word.to_string();
                } else {
                    line = candidate;
                }
            }
            out.push(line);
        }
        out
    }

    /// The largest size (≤ `max`) at which `text` wrapped to `w` fits `h` (line height 1.2 em).
    pub fn fit(&self, text: &str, w: f32, h: f32, max: f32) -> f32 {
        let fits = |s: f32| {
            let lines = self.wrap(text, s, w);
            let widest = lines.iter().map(|l| self.width(l, s)).fold(0.0f32, f32::max);
            lines.len() as f32 * s * 1.2 <= h && widest <= w * 1.001
        };
        let (mut lo, mut hi) = (2.0f32, max.clamp(2.0, 400.0));
        if fits(hi) {
            return hi;
        }
        for _ in 0..20 {
            let mid = (lo + hi) / 2.0;
            if fits(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }
}

impl Canvas {
    pub fn new(prefix: impl Into<String>) -> Canvas {
        Canvas { prefix: prefix.into(), ..Default::default() }
    }

    pub fn fill(&mut self, color: Color) {
        self.fill_color = color;
        let _ = writeln!(self.ops, "{} {} {} rg", c(color[0]), c(color[1]), c(color[2]));
    }

    pub fn stroke(&mut self, color: Color, width: f32) {
        let _ = writeln!(self.ops, "{} {} {} RG {} w", c(color[0]), c(color[1]), c(color[2]), n(f64::from(width.clamp(0.0, 500.0))));
    }

    pub fn raw(&mut self, s: &str) {
        self.ops.push_str(s);
        self.ops.push('\n');
    }

    /// Opacity for what follows (`multiply`: the Multiply blend, so text under stays readable).
    pub fn opacity(&mut self, alpha: f32, multiply: bool) {
        let i = self.gstates.len();
        self.gstates.push((alpha.clamp(0.0, 1.0), multiply));
        let _ = writeln!(self.ops, "/{}gs{i} gs", self.prefix);
    }

    pub fn rect(&mut self, r: Rect) {
        let _ = writeln!(self.ops, "{} {} {} {} re", n(r.x.into()), n(r.y.into()), n(r.w.into()), n(r.h.into()));
    }

    pub fn ellipse(&mut self, r: Rect) {
        let mut p = BezPath::new();
        let k = 0.552_284_8;
        let (cx, cy, rx, ry) = (f64::from(r.x + r.w / 2.0), f64::from(r.y + r.h / 2.0), f64::from(r.w / 2.0), f64::from(r.h / 2.0));
        p.move_to((cx + rx, cy));
        p.curve_to((cx + rx, cy + ry * k), (cx + rx * k, cy + ry), (cx, cy + ry));
        p.curve_to((cx - rx * k, cy + ry), (cx - rx, cy + ry * k), (cx - rx, cy));
        p.curve_to((cx - rx, cy - ry * k), (cx - rx * k, cy - ry), (cx, cy - ry));
        p.curve_to((cx + rx * k, cy - ry), (cx + rx, cy - ry * k), (cx + rx, cy));
        p.close_path();
        self.path(&p);
    }

    pub fn polyline(&mut self, pts: &[[f32; 2]]) {
        for (i, p) in pts.iter().enumerate() {
            let _ = writeln!(self.ops, "{} {} {}", n(p[0].into()), n(p[1].into()), if i == 0 { "m" } else { "l" });
        }
    }

    pub fn path(&mut self, p: &BezPath) {
        let mut last = Point::ZERO;
        for el in p.elements() {
            match *el {
                PathEl::MoveTo(a) => {
                    let _ = writeln!(self.ops, "{} {} m", n(a.x), n(a.y));
                    last = a;
                }
                PathEl::LineTo(a) => {
                    let _ = writeln!(self.ops, "{} {} l", n(a.x), n(a.y));
                    last = a;
                }
                PathEl::QuadTo(q, a) => {
                    let c1 = last + (q - last) * (2.0 / 3.0);
                    let c2 = a + (q - a) * (2.0 / 3.0);
                    let _ = writeln!(self.ops, "{} {} {} {} {} {} c", n(c1.x), n(c1.y), n(c2.x), n(c2.y), n(a.x), n(a.y));
                    last = a;
                }
                PathEl::CurveTo(c1, c2, a) => {
                    let _ = writeln!(self.ops, "{} {} {} {} {} {} c", n(c1.x), n(c1.y), n(c2.x), n(c2.y), n(a.x), n(a.y));
                    last = a;
                }
                PathEl::ClosePath => self.ops.push_str("h\n"),
            }
        }
    }

    /// `text` as filled glyph outlines from `x` on the baseline `base`, in the current fill
    /// colour; returns the width it took.
    pub fn text(&mut self, font: &Font, text: &str, size: f32, x: f32, base: f32) -> f32 {
        let text: String = text.chars().filter(|ch| !ch.is_control()).take(MAX_TEXT).collect();
        let mut pen = f64::from(x);
        let mut any = false;
        let mut outline = BezPath::new();
        for run in font.runs(&text) {
            let upem = run.face.units_per_em();
            if upem <= 0.0 {
                continue;
            }
            let k = f64::from(size) / upem;
            for g in wordcraft_fonts::shape(&run.face, &run.text, &[], |ch| ch) {
                let gx = pen + f64::from(g.x_offset) * k;
                let gy = f64::from(base) - f64::from(g.y_offset) * k;
                let skew = if font.synth_italic { 0.2 } else { 0.0 };
                // The database's outlines run y down, in font units.
                let glyph = run.face.glyph_outline(g.gid);
                let map = |p: Point| Point::new(gx + p.x * k - skew * p.y * k, gy + p.y * k);
                for el in glyph.elements() {
                    match *el {
                        PathEl::MoveTo(a) => outline.move_to(map(a)),
                        PathEl::LineTo(a) => outline.line_to(map(a)),
                        PathEl::QuadTo(a, b) => outline.quad_to(map(a), map(b)),
                        PathEl::CurveTo(a, b, d) => outline.curve_to(map(a), map(b), map(d)),
                        PathEl::ClosePath => outline.close_path(),
                    }
                    any = true;
                }
                pen += f64::from(g.x_advance) * k;
            }
        }
        if any {
            if font.synth_bold {
                // Emboldened: filled and stroked in the fill colour.
                let [r, g, b] = self.fill_color;
                let _ = writeln!(self.ops, "q {} {} {} RG {} w 1 j", c(r), c(g), c(b), n(f64::from(size) * 0.035));
                self.path(&outline);
                self.ops.push_str("B\nQ\n");
            } else {
                self.path(&outline);
                self.ops.push_str("f\n");
            }
        }
        (pen - f64::from(x)) as f32
    }

    /// A picture stretched to `r`; false when it can't be read.
    pub fn image(&mut self, data: &[u8], r: Rect) -> bool {
        let Ok(img) = image::load_from_memory(data) else { return false };
        let img = if img.width().max(img.height()) > MAX_IMAGE_SIDE {
            img.resize(MAX_IMAGE_SIDE, MAX_IMAGE_SIDE, image::imageops::FilterType::Triangle)
        } else {
            img
        };
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        if w == 0 || h == 0 {
            return false;
        }
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        let mut alpha = Vec::with_capacity((w * h) as usize);
        for p in rgba.pixels() {
            rgb.extend_from_slice(&p.0[..3]);
            alpha.push(p.0[3]);
        }
        let mut dict = Dictionary::new();
        dict.set("Type", Object::Name(b"XObject".to_vec()));
        dict.set("Subtype", Object::Name(b"Image".to_vec()));
        dict.set("Width", i64::from(w));
        dict.set("Height", i64::from(h));
        dict.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
        dict.set("BitsPerComponent", 8);
        let mut stream = Stream::new(dict, rgb);
        let _ = stream.compress();
        if alpha.iter().any(|a| *a < 255) {
            let mut md = Dictionary::new();
            md.set("Type", Object::Name(b"XObject".to_vec()));
            md.set("Subtype", Object::Name(b"Image".to_vec()));
            md.set("Width", i64::from(w));
            md.set("Height", i64::from(h));
            md.set("ColorSpace", Object::Name(b"DeviceGray".to_vec()));
            md.set("BitsPerComponent", 8);
            let mut mask = Stream::new(md, alpha);
            let _ = mask.compress();
            // The mask is stored with the picture and added to the document with it.
            stream.dict.set("WCSMask", Object::Stream(mask));
        }
        let i = self.images.len();
        self.images.push(stream);
        // Unit square, flipped: the picture's first row at the top of the box.
        let _ = writeln!(
            self.ops,
            "q {} 0 0 {} {} {} cm /{}im{i} Do Q",
            n(r.w.into()),
            n((-r.h).into()),
            n(r.x.into()),
            n((r.y + r.h).into()),
            self.prefix
        );
        true
    }

    /// The finished content: the operators, and the resources they name (added to `doc`).
    pub fn finish(mut self, doc: &mut Document) -> (Vec<u8>, Dictionary) {
        let mut res = Dictionary::new();
        if !self.gstates.is_empty() {
            let mut gs = Dictionary::new();
            for (i, (alpha, multiply)) in self.gstates.iter().enumerate() {
                let mut d = Dictionary::new();
                d.set("Type", Object::Name(b"ExtGState".to_vec()));
                d.set("CA", Object::Real(*alpha));
                d.set("ca", Object::Real(*alpha));
                if *multiply {
                    d.set("BM", Object::Name(b"Multiply".to_vec()));
                }
                gs.set(format!("{}gs{i}", self.prefix), Object::Dictionary(d));
            }
            res.set("ExtGState", Object::Dictionary(gs));
        }
        if !self.images.is_empty() {
            let mut xo = Dictionary::new();
            let images = std::mem::take(&mut self.images);
            for (i, mut img) in images.into_iter().enumerate() {
                if let Some(Object::Stream(mask)) = img.dict.remove(b"WCSMask") {
                    let id = doc.add_object(Object::Stream(mask));
                    img.dict.set("SMask", Object::Reference(id));
                }
                let id = doc.add_object(Object::Stream(img));
                xo.set(format!("{}im{i}", self.prefix), Object::Reference(id));
            }
            res.set("XObject", Object::Dictionary(xo));
        }
        (self.ops.into_bytes(), res)
    }
}

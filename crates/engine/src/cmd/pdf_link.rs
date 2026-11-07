//! PDF edits shown in the document too. A shape, line, arrow, ink, signature, picture, text box,
//! stamp, whiteout or redaction drawn on the PDF gets a twin in the document: a floating object
//! in front of the text, at the same place on the same page. Highlighting, underlining and
//! striking text formats the same words in the document, and Edit Text changes them there.
//!
//! Twins follow their marks: moving, resizing, recolouring, deleting, undoing and redoing a PDF
//! edit changes the document to match ([`sync`], after every `pdf.*` command). Each sync that
//! changes the document is one undo step in it. Marks made after pages were turned, moved,
//! removed, added or cropped aren't twinned (the PDF's pages no longer match the document's).
//!
//! The PDF view draws a mark itself over the PDF the document was opened from; over the
//! document's own export the twin is already there, so the mark isn't drawn again
//! ([`shown_ops`]).

use std::collections::{HashMap, HashSet};

use serde_json::json;
use wordcraft_doc::para::{AltText, Anchor, Float, InlineObject, OBJ, ShapeKind, Wrap};
use wordcraft_doc::props::{Align, CharProps, Highlight, LineSpacing, ParaProps, Rgb, TextColor, Underline, VAlign};
use wordcraft_doc::{Paragraph, PartKind, Pos, StoryRef, para_block};
use wordcraft_pdfedit::{Color, Mark, Op, Rect};

use crate::Selection;
use crate::Session;

/// Prefix of the alternative-text title that names a twin (`wordcraft-pdf <session> <mark>`).
const TAG: &str = "wordcraft-pdf";

/// A mark's twin in the document.
#[derive(Debug, Clone)]
pub struct Twin {
    /// The mark as it was twinned: a changed mark gets a new twin.
    mark: Mark,
    page: usize,
    kind: TwinKind,
}

#[derive(Debug, Clone)]
enum TwinKind {
    /// A floating object, known by its alternative-text title.
    Object(String),
    /// Highlight, underline or strikethrough on the words under `rects`.
    Format(Vec<Rect>),
    /// The words at (`x`, `y`) changed from `old` to `new`.
    Text { x: f32, y: f32, old: String, new: String },
}

fn tag(s: &Session, id: u64) -> String {
    format!("{TAG} {:x} {id}", s.pdf.nonce)
}

fn rgb(c: Color) -> Rgb {
    Rgb(c[0], c[1], c[2])
}

/// The ops the PDF view draws over its base: all of them over the PDF the document was opened
/// from; over the document's export, all but the marks whose twin is in the document (it is
/// in the export already).
pub fn shown_ops(s: &Session, export: bool) -> Vec<Op> {
    if !export || s.pdf.twins.is_empty() {
        return s.pdf.ops.clone();
    }
    let present = tags(s);
    s.pdf
        .ops
        .iter()
        .filter(|o| match o {
            Op::Mark { id, mark, .. } => !s.pdf.twins.get(id).is_some_and(|t| {
                t.mark == *mark
                    && match &t.kind {
                        TwinKind::Object(tag) => present.contains(tag),
                        TwinKind::Format(_) | TwinKind::Text { .. } => true,
                    }
            }),
            _ => true,
        })
        .cloned()
        .collect()
}

/// The twin titles in the document's body.
fn tags(s: &Session) -> HashSet<String> {
    let mut out = HashSet::new();
    for path in s.doc.para_paths(StoryRef::Body) {
        let Some(p) = s.doc.para(StoryRef::Body, &path) else { continue };
        for o in &p.objects {
            if let Some(f) = float_of(o)
                && f.alt.title.starts_with(TAG)
            {
                out.insert(f.alt.title.clone());
            }
        }
    }
    out
}

fn float_of(o: &InlineObject) -> Option<&Float> {
    match o {
        InlineObject::Shape { float, .. } | InlineObject::Image { float, .. } => Some(float),
        _ => None,
    }
}

/// Whether a mark has a twin (notes, and marks of other kinds, stay in the PDF).
fn twinned(m: &Mark) -> bool {
    !matches!(m, Mark::Note { .. })
}

/// Bring the document's twins in line with the PDF's marks (after a `pdf.*` command).
pub(crate) fn sync(s: &mut Session) {
    if s.pdf.synced_rev == s.pdf.rev {
        return;
    }
    s.pdf.synced_rev = s.pdf.rev;
    if s.pdf.nonce == 0 {
        s.pdf.nonce = wordcraft_pdfedit::nonce().max(1);
    }
    // The marks to twin: those made while the pages still match the document's.
    let mut want: Vec<(u64, usize, Mark)> = Vec::new();
    let mut pages_match = true;
    for op in &s.pdf.ops {
        match op {
            Op::Mark { id, page, mark } if pages_match && twinned(mark) => want.push((*id, *page, mark.clone())),
            Op::Mark { .. } | Op::Watermark { .. } | Op::PageText { .. } | Op::Metadata { .. } | Op::Fill { .. } => {}
            _ => pages_match = false,
        }
    }
    let mut stale: Vec<u64> =
        s.pdf.twins.iter().filter(|(id, t)| !want.iter().any(|(w, p, m)| w == *id && *p == t.page && *m == t.mark)).map(|(id, _)| *id).collect();
    let add: Vec<(u64, usize, Mark)> = want.into_iter().filter(|(id, ..)| !s.pdf.twins.contains_key(id) || stale.contains(id)).collect();
    if stale.is_empty() && add.is_empty() {
        return;
    }
    let before = s.doc.clone();
    let rev_before = s.rev();
    let original = s.pdf_source.is_some() && s.pdf_source_rev == rev_before;
    let sel = s.sel.clone();
    // Undone newest first, so changed words go back the way they came.
    stale.sort_unstable_by(|a, b| b.cmp(a));
    for id in stale {
        if let Some(t) = s.pdf.twins.remove(&id) {
            remove(s, &t);
            s.touch();
        }
    }
    for (id, page, mark) in add {
        if let Some(kind) = make(s, id, page, &mark) {
            s.pdf.twins.insert(id, Twin { mark, page, kind });
        }
        s.touch();
    }
    s.doc.prune_text_boxes();
    s.sel = sel;
    s.clamp_selection();
    if s.doc == before {
        return;
    }
    // One undo step in the document.
    let after = std::mem::replace(&mut s.doc, before);
    s.checkpoint("Mark Up");
    s.doc = after;
    s.touch();
    // Still the PDF it was opened from, plus the marks the PDF view draws itself.
    if original {
        s.pdf_source_rev = s.rev();
    }
}

/// Make the twin of mark `id` on page `page` (0-based); `None` when it can't be placed.
fn make(s: &mut Session, id: u64, page: usize, mark: &Mark) -> Option<TwinKind> {
    match mark {
        Mark::Highlight { rects, color } => format(s, page, rects, &mark_format(mark, *color)).then(|| TwinKind::Format(rects.clone())),
        Mark::Underline { rects, color } | Mark::Strikeout { rects, color } => {
            format(s, page, rects, &mark_format(mark, *color)).then(|| TwinKind::Format(rects.clone()))
        }
        Mark::EditText { rect, text, .. } => edit_text(s, page, *rect, text),
        _ => {
            let tag = tag(s, id);
            let obj = object(s, mark, &tag)?;
            let at = anchor(s, page)?;
            let props = s.doc.para_at(&at).map(|p| p.props_at(at.off).clone()).unwrap_or_default();
            s.doc.insert_object(&at, obj, &props).ok()?;
            Some(TwinKind::Object(tag))
        }
    }
}

fn remove(s: &mut Session, t: &Twin) {
    match &t.kind {
        TwinKind::Object(tag) => {
            if let Some((a, b)) = find(s, tag) {
                let _ = s.doc.delete_range(&a, &b);
            }
        }
        TwinKind::Format(rects) => {
            let undo = match t.mark {
                Mark::Highlight { .. } => Fmt::Highlight(Highlight::None),
                Mark::Underline { .. } => Fmt::Underline(None),
                _ => Fmt::Strike(false),
            };
            format(s, t.page, rects, &undo);
        }
        TwinKind::Text { x, y, old, new } => {
            let Some(a) = s.layout().hit(t.page, *x + 0.5, *y, StoryRef::Body) else { return };
            let Some(para) = s.doc.para_at(&a) else { return };
            // The changed words nearest where they were put.
            let Some(start) = para.text.match_indices(new.as_str()).map(|(i, _)| i).min_by_key(|i| i.abs_diff(a.off)) else { return };
            let props = para.props_at(start).clone();
            let (from, to) = (Pos { off: start, ..a.clone() }, Pos { off: start + new.len(), ..a });
            if s.doc.delete_range(&from, &to).is_ok() {
                let _ = s.doc.insert_text(&from, old, &props);
            }
        }
    }
}

/// The twin with this title: its anchor character.
fn find(s: &Session, tag: &str) -> Option<(Pos, Pos)> {
    for path in s.doc.para_paths(StoryRef::Body) {
        let Some(p) = s.doc.para(StoryRef::Body, &path) else { continue };
        for off in p.object_offsets() {
            if p.object_at(off).and_then(float_of).is_some_and(|f| f.alt.title == tag) {
                let a = Pos { story: StoryRef::Body, path: path.clone(), off };
                return Some((a.clone(), Pos { off: off + OBJ.len_utf8(), ..a }));
            }
        }
    }
    None
}

/// Where a twin on `page` is anchored: the start of the page's first line.
fn anchor(s: &mut Session, page: usize) -> Option<Pos> {
    let lay = s.layout();
    if page >= lay.pages.len() {
        return None;
    }
    lay.hit(page, 0.0, 0.0, StoryRef::Body).filter(|p| p.story == StoryRef::Body)
}

fn float(r: Rect, tag: &str) -> Float {
    Float {
        wrap: Wrap::InFrontOfText,
        h_rel: Anchor::Page,
        v_rel: Anchor::Page,
        x: r.x,
        y: r.y,
        alt: AltText { title: tag.to_string(), ..Default::default() },
        ..Default::default()
    }
}

fn shape(kind: ShapeKind, w: f32, h: f32, fill: Option<Color>, stroke: Option<Color>, width: f32, float: Float) -> InlineObject {
    InlineObject::Shape {
        kind,
        w: w.max(0.5),
        h: h.max(0.5),
        fill: fill.map(rgb),
        stroke: stroke.map(rgb),
        stroke_width: width,
        float,
        story: None,
        freeform: None,
        effects: Default::default(),
        extra: Default::default(),
    }
}

/// The floating object twinning a drawn mark.
fn object(s: &mut Session, mark: &Mark, tag: &str) -> Option<InlineObject> {
    let r = mark.bounds();
    if !r.is_sane() {
        return None;
    }
    Some(match mark {
        Mark::Rectangle { rect, stroke, fill, width } | Mark::Ellipse { rect, stroke, fill, width } => {
            let r = rect.normalized();
            let kind = if matches!(mark, Mark::Rectangle { .. }) { ShapeKind::Rectangle } else { ShapeKind::Ellipse };
            shape(kind, r.w, r.h, *fill, *stroke, *width, float(r, tag))
        }
        Mark::Whiteout { rect } => {
            let r = rect.normalized();
            shape(ShapeKind::Rectangle, r.w, r.h, Some([255, 255, 255]), None, 0.0, float(r, tag))
        }
        Mark::Redact { rects } => {
            let r = rects.first()?.normalized();
            shape(ShapeKind::Rectangle, r.w, r.h, Some([0, 0, 0]), None, 0.0, float(r, tag))
        }
        Mark::Line { from, to, color, width, arrow } => {
            // DrawingML's line runs from its frame's top left to bottom right; flips turn it.
            let b = Rect::new(from[0].min(to[0]), from[1].min(to[1]), (from[0] - to[0]).abs(), (from[1] - to[1]).abs());
            let mut f = float(b, tag);
            f.flip_h = to[0] < from[0];
            f.flip_v = to[1] < from[1];
            let mut obj = shape(ShapeKind::Line, b.w, b.h, None, Some(*color), *width, f);
            if let InlineObject::Shape { extra, .. } = &mut obj {
                extra.arrow_end = *arrow;
            }
            obj
        }
        Mark::Ink { strokes, color, width } => {
            let pad = width / 2.0;
            let b = Rect::new(r.x - pad, r.y - pad, r.w + 2.0 * pad, r.h + 2.0 * pad);
            let paths = strokes
                .iter()
                .filter(|st| !st.is_empty())
                .map(|st| wordcraft_doc::freeform::FreePath { pts: st.iter().map(|p| [p[0] - b.x, p[1] - b.y]).collect(), closed: false })
                .collect();
            let ff = wordcraft_doc::freeform::Freeform { w: b.w.max(1.0), h: b.h.max(1.0), paths, alpha: 1.0, ink: None, order: None }.sanitized();
            let mut obj = shape(ShapeKind::Freeform, b.w, b.h, None, Some(*color), *width, float(b, tag));
            if let InlineObject::Shape { freeform, .. } = &mut obj {
                *freeform = Some(std::sync::Arc::new(ff));
            }
            obj
        }
        Mark::Image { rect, data } => {
            let r = rect.normalized();
            let ext = match image::guess_format(data) {
                Ok(image::ImageFormat::Jpeg) => "jpeg",
                Ok(image::ImageFormat::Png) => "png",
                Ok(image::ImageFormat::Gif) => "gif",
                Ok(image::ImageFormat::Bmp) => "bmp",
                _ => return None,
            };
            let media = s.doc.add_media(data.as_ref().clone(), ext);
            InlineObject::Image { media, w: r.w.max(1.0), h: r.h.max(1.0), alt: String::new(), float: float(r, tag), crop: [0.0; 4], ole: None }
        }
        Mark::TextBox { rect, text, color, font, bold, italic, fill, border, .. } => {
            let r = rect.normalized();
            let size = wordcraft_pdfedit::text_size(mark)?;
            let chr = CharProps {
                font: Some(font.clone()),
                size: Some(size),
                bold: Some(*bold),
                italic: Some(*italic),
                color: Some(TextColor::Rgb(rgb(*color))),
                ..Default::default()
            };
            let id = text_story(s, text, &chr, size, None)?;
            text_box(s, id, r, *fill, *border, 1.0, VAlign::Top, tag)?
        }
        Mark::Stamp { rect, text, color } => {
            let r = rect.normalized();
            let size = wordcraft_pdfedit::text_size(mark)?;
            let chr = CharProps {
                font: Some(wordcraft_pdfedit::DEFAULT_FONT.to_string()),
                size: Some(size),
                bold: Some(true),
                color: Some(TextColor::Rgb(rgb(*color))),
                ..Default::default()
            };
            let id = text_story(s, text, &chr, size, Some(Align::Center))?;
            let border = (r.h * 0.06).clamp(1.0, 6.0);
            let inner = Rect::new(r.x + border, r.y + border, (r.w - 2.0 * border).max(1.0), (r.h - 2.0 * border).max(1.0));
            text_box(s, id, inner, None, Some(*color), border, VAlign::Center, tag)?
        }
        _ => return None,
    })
}

/// A text box's story: one paragraph per line of `text`, spaced as the PDF draws them.
fn text_story(s: &mut Session, text: &str, chr: &CharProps, size: f32, align: Option<Align>) -> Option<u32> {
    let blocks = text
        .split('\n')
        .map(|line| {
            let mut p = Paragraph::with_text(line, chr.clone());
            p.mark = chr.clone();
            p.props = ParaProps {
                align,
                space_before: Some(0.0),
                space_after: Some(0.0),
                line_spacing: Some(LineSpacing::Exactly(size * 1.2)),
                ..Default::default()
            };
            para_block(p)
        })
        .collect();
    let id = s.doc.try_add_part(PartKind::TextBox, blocks).ok()?;
    if let Some(part) = s.doc.parts.get_mut(&id) {
        // The PDF's 3 pt inside the box.
        part.body.inset = Some([38_100; 4]);
        part.body.anchor = VAlign::Top;
    }
    Some(id)
}

#[allow(clippy::too_many_arguments)]
fn text_box(
    s: &mut Session,
    story: u32,
    r: Rect,
    fill: Option<Color>,
    border: Option<Color>,
    width: f32,
    valign: VAlign,
    tag: &str,
) -> Option<InlineObject> {
    if let Some(part) = s.doc.parts.get_mut(&story) {
        part.body.anchor = valign;
    }
    let mut obj = shape(ShapeKind::TextBox, r.w, r.h, fill, border, if border.is_some() { width } else { 0.0 }, float(r, tag));
    if let InlineObject::Shape { story: st, .. } = &mut obj {
        *st = Some(story);
    }
    Some(obj)
}

/// A change to the words' formatting.
enum Fmt {
    Highlight(Highlight),
    Underline(Option<Rgb>),
    Strike(bool),
}

fn mark_format(m: &Mark, c: Color) -> Fmt {
    match m {
        Mark::Highlight { .. } => Fmt::Highlight(nearest_highlight(c)),
        Mark::Underline { .. } => Fmt::Underline(Some(rgb(c))),
        _ => Fmt::Strike(true),
    }
}

/// Word's highlight colour nearest `c`.
fn nearest_highlight(c: Color) -> Highlight {
    let d = |h: &Highlight| {
        h.rgb().map_or(u32::MAX, |Rgb(r, g, b)| {
            let dd = |x: u8, y: u8| u32::from(x.abs_diff(y)).pow(2);
            dd(r, c[0]) + dd(g, c[1]) + dd(b, c[2])
        })
    };
    Highlight::ALL.iter().filter(|h| **h != Highlight::None).min_by_key(|h| d(h)).copied().unwrap_or(Highlight::Yellow)
}

/// Format the words under `rects` on `page`; whether any were found.
fn format(s: &mut Session, page: usize, rects: &[Rect], f: &Fmt) -> bool {
    let mut any = false;
    for r in rects.iter().take(500) {
        let r = r.normalized();
        let y = r.y + r.h / 2.0;
        let lay = s.layout();
        let (Some(a), Some(b)) = (lay.hit(page, r.x + 0.5, y, StoryRef::Body), lay.hit(page, r.x + r.w - 0.5, y, StoryRef::Body)) else { continue };
        if a.story != b.story || a.path != b.path || b.off <= a.off {
            continue;
        }
        s.sel = Selection { anchor: a, focus: b };
        let done = super::format::apply(s, &|c: &mut CharProps| match f {
            Fmt::Highlight(h) => c.highlight = Some(*h),
            Fmt::Underline(Some(color)) => {
                c.underline = Some(Underline::Single);
                c.underline_color = Some(*color);
            }
            Fmt::Underline(None) => {
                c.underline = Some(Underline::None);
                c.underline_color = None;
            }
            Fmt::Strike(on) => c.strike = Some(*on),
        });
        any |= done.is_ok();
        s.touch();
    }
    any
}

/// Change the words under `rect` on `page` to `new` (Edit Text).
fn edit_text(s: &mut Session, page: usize, rect: Rect, new: &str) -> Option<TwinKind> {
    let r = rect.normalized();
    let y = r.y + r.h / 2.0;
    let lay = s.layout();
    let a = lay.hit(page, r.x + 1.5, y, StoryRef::Body)?;
    let b = lay.hit(page, r.x + r.w - 1.5, y, StoryRef::Body)?;
    if a.story != b.story || a.path != b.path || b.off <= a.off {
        return None;
    }
    let para = s.doc.para_at(&a)?;
    let old = para.text.get(a.off..b.off)?.to_string();
    if old.contains(OBJ) || old == new {
        return None;
    }
    let props = para.props_at(a.off).clone();
    s.doc.delete_range(&a, &b).ok()?;
    s.doc.insert_text(&a, new, &props).ok()?;
    Some(TwinKind::Text { x: r.x + 1.5, y, old, new: new.to_string() })
}

/// The twins, for `pdf.edits` (mark id → what it is in the document).
pub(crate) fn describe(s: &Session) -> HashMap<u64, serde_json::Value> {
    s.pdf
        .twins
        .iter()
        .map(|(id, t)| {
            let v = match &t.kind {
                TwinKind::Object(_) => json!("object"),
                TwinKind::Format(_) => json!("format"),
                TwinKind::Text { .. } => json!("text"),
            };
            (*id, v)
        })
        .collect()
}

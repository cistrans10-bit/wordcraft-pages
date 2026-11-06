//! Replaying edits over a PDF, and the whole-file operations (extract, split, decrypt).

use crate::draw::{Canvas, Font, n};
use crate::forms::encode_text;
use crate::pages::{self, Geometry, Pages, real_array};
use crate::{Color, Error, Mark, NOTE_SIZE, Op, Position, Rect, SaveOptions};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use std::collections::HashSet;
use std::fmt::Write as _;

/// Pixels per point pages are rendered at when they become pictures (redaction, flattening).
const RASTER_SCALE: f32 = 2.5;

/// Open `bytes` for editing; an encrypted PDF opens when it has no open password.
pub(crate) fn load(bytes: &[u8]) -> Result<Document, Error> {
    let mut doc = Document::load_mem(bytes).map_err(|e| Error(format!("not a PDF that can be edited ({e})")))?;
    if doc.is_encrypted() {
        doc.decrypt("").map_err(|_| Error("the PDF is protected by a password".into()))?;
    }
    Ok(doc)
}

/// Whether the PDF is encrypted at all (with or without a password to open it).
pub fn has_encryption(bytes: &[u8]) -> bool {
    bytes.windows(8).any(|w| w == b"/Encrypt") && Document::load_mem(bytes).is_ok_and(|d| d.trailer.has(b"Encrypt") || d.is_encrypted())
}

/// Whether the PDF needs a password to open.
pub fn is_encrypted(bytes: &[u8]) -> bool {
    match Document::load_mem(bytes) {
        Ok(mut doc) => doc.is_encrypted() && doc.decrypt("").is_err(),
        Err(_) => false,
    }
}

/// The PDF opened with `password`, written back without encryption (for viewing and editing).
pub fn decrypt(bytes: &[u8], password: &str) -> Result<Vec<u8>, Error> {
    let mut doc = Document::load_mem_with_options(bytes, lopdf::LoadOptions::with_password(password))
        .or_else(|_| Document::load_mem(bytes))
        .map_err(|e| Error(format!("not a PDF ({e})")))?;
    if doc.is_encrypted() {
        doc.decrypt(password).map_err(|_| Error("wrong password".into()))?;
    }
    doc.trailer.remove(b"Encrypt");
    save_doc(&mut doc)
}

fn save_doc(doc: &mut Document) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    doc.save_to(&mut out).map_err(|e| Error(format!("couldn't write the PDF ({e})")))?;
    Ok(out)
}

/// Shared state while replaying.
struct State {
    pages: Pages,
    wrapped: HashSet<ObjectId>,
    counter: u64,
}

impl State {
    fn page(&self, i: usize) -> Result<ObjectId, Error> {
        self.pages.list.get(i).copied().ok_or_else(|| Error(format!("no page {}", i.saturating_add(1))))
    }

    fn prefix(&mut self) -> String {
        self.counter += 1;
        format!("WC{}_", self.counter)
    }
}

/// Replay `ops` over the PDF `base` and write the result. An edit that can't be applied (a page
/// that isn't there any more, say) is left out; the rest still apply.
pub fn apply(base: &[u8], ops: &[Op], save: &SaveOptions) -> Result<Vec<u8>, Error> {
    let mut doc = load(base)?;
    let list = pages::flatten(&mut doc)?;
    let mut st = State { pages: list, wrapped: HashSet::new(), counter: 0 };
    for op in ops {
        if let Err(e) = apply_op(&mut doc, &mut st, op) {
            log::debug!("PDF edit skipped ({}): {e}", op.describe());
        }
    }
    if save.flatten {
        for i in 0..st.pages.list.len() {
            rasterize(&mut doc, &mut st, i, &[])?;
        }
    }
    pages::commit(&mut doc, &st.pages)?;
    doc.prune_objects();
    if save.compress {
        doc.compress();
    }
    if let Some(pw) = save.password.as_deref().filter(|p| !p.is_empty()) {
        encrypt(&mut doc, pw)?;
    }
    save_doc(&mut doc)
}

fn encrypt(doc: &mut Document, password: &str) -> Result<(), Error> {
    use lopdf::encryption::crypt_filters::{Aes128CryptFilter, CryptFilter};
    use std::collections::BTreeMap;
    use std::sync::Arc;
    // A file ID, which the key derivation uses.
    if !doc.trailer.has(b"ID") {
        let mut id = [0u8; 16];
        getrandom::fill(&mut id).map_err(|e| Error(format!("no randomness for the encryption key ({e})")))?;
        let s = Object::String(id.to_vec(), lopdf::StringFormat::Hexadecimal);
        doc.trailer.set("ID", Object::Array(vec![s.clone(), s]));
    }
    let filter: Arc<dyn CryptFilter> = Arc::new(Aes128CryptFilter);
    let version = lopdf::EncryptionVersion::V4 {
        document: doc,
        encrypt_metadata: true,
        crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), filter)]),
        stream_filter: b"StdCF".to_vec(),
        string_filter: b"StdCF".to_vec(),
        owner_password: password,
        user_password: password,
        permissions: lopdf::Permissions::all(),
    };
    let state = lopdf::EncryptionState::try_from(version).map_err(|e| Error(format!("couldn't encrypt ({e})")))?;
    doc.encrypt(&state).map_err(|e| Error(format!("couldn't encrypt ({e})")))
}

/// Only the pages `keep` (in that order), as a new PDF.
pub fn extract(bytes: &[u8], keep: &[usize]) -> Result<Vec<u8>, Error> {
    let mut doc = load(bytes)?;
    let mut p = pages::flatten(&mut doc)?;
    let list: Vec<ObjectId> = keep.iter().filter_map(|i| p.list.get(*i).copied()).collect();
    if list.is_empty() {
        return Err(Error("no pages to extract".into()));
    }
    p.list = list;
    pages::commit(&mut doc, &p)?;
    // Form fields and outlines point at pages that may be gone: drop them.
    if let Ok(cat) = doc.catalog_mut() {
        cat.remove(b"Outlines");
        cat.remove(b"AcroForm");
    }
    doc.prune_objects();
    save_doc(&mut doc)
}

/// The PDF cut into parts of `every` pages.
pub fn split(bytes: &[u8], every: usize) -> Result<Vec<Vec<u8>>, Error> {
    let count = {
        let mut doc = load(bytes)?;
        pages::flatten(&mut doc)?.list.len()
    };
    let every = every.clamp(1, crate::MAX_PAGES);
    let mut out = Vec::new();
    let mut start = 0;
    while start < count {
        let part: Vec<usize> = (start..(start + every).min(count)).collect();
        out.push(extract(bytes, &part)?);
        start += every;
    }
    Ok(out)
}

fn pages_of(list: &Option<Vec<usize>>, count: usize) -> Vec<usize> {
    match list {
        Some(l) => l.iter().copied().filter(|p| *p < count).collect(),
        None => (0..count).collect(),
    }
}

fn apply_op(doc: &mut Document, st: &mut State, op: &Op) -> Result<(), Error> {
    let count = st.pages.list.len();
    match op {
        Op::Mark { id, page, mark } => apply_mark(doc, st, *id, *page, mark),
        Op::Rotate { pages, by } => {
            for p in pages {
                let id = st.page(*p)?;
                let geo = Geometry::of(doc, id);
                let r = ((geo.rotate + by.rem_euclid(360) / 90 * 90) % 360) as i64;
                doc.get_dictionary_mut(id)?.set("Rotate", r);
            }
            Ok(())
        }
        Op::Delete { pages } => {
            let set: HashSet<usize> = pages.iter().copied().filter(|p| *p < count).collect();
            if set.len() >= count {
                return Err(Error("a PDF keeps at least one page".into()));
            }
            let mut i = 0;
            st.pages.list.retain(|_| {
                let keep = !set.contains(&i);
                i += 1;
                keep
            });
            Ok(())
        }
        Op::Move { from, to } => {
            if *from >= count {
                return Err(Error(format!("no page {}", from + 1)));
            }
            let id = st.pages.list.remove(*from);
            let to = (*to).min(st.pages.list.len());
            st.pages.list.insert(to, id);
            Ok(())
        }
        Op::Duplicate { page } => {
            if count >= crate::MAX_PAGES {
                return Err(Error("too many pages".into()));
            }
            let id = st.page(*page)?;
            let mut d = doc.get_dictionary(id)?.clone();
            let annots = d.get(b"Annots").ok().cloned();
            d.remove(b"Annots");
            let copy = doc.add_object(Object::Dictionary(d));
            // The page's marks come along (form fields don't: a field is one field).
            let list = match annots.as_ref().map(|o| doc.dereference(o)) {
                Some(Ok((_, Object::Array(a)))) => a.clone(),
                _ => Vec::new(),
            };
            for a in list.iter().take(5000) {
                let Ok((_, Object::Dictionary(ad))) = doc.dereference(a) else { continue };
                if ad.get(b"Subtype").and_then(Object::as_name).is_ok_and(|n| n == b"Widget" || n == b"Popup") {
                    continue;
                }
                let mut ad = ad.clone();
                ad.set("P", Object::Reference(copy));
                ad.remove(b"Popup");
                ad.remove(b"Parent");
                let nid = doc.add_object(Object::Dictionary(ad));
                pages::add_annot(doc, copy, nid)?;
            }
            st.pages.list.insert(page + 1, copy);
            Ok(())
        }
        Op::InsertBlank { at, width, height } => {
            if count >= crate::MAX_PAGES {
                return Err(Error("too many pages".into()));
            }
            let id = pages::blank(doc, f64::from(*width), f64::from(*height));
            st.pages.list.insert((*at).min(count), id);
            Ok(())
        }
        Op::InsertPdf { at, data } => {
            let mut other = load(data)?;
            pages::flatten(&mut other)?;
            other.renumber_objects_with(doc.max_id.saturating_add(1));
            let ids: Vec<ObjectId> = other.get_pages().into_values().collect();
            if count + ids.len() > crate::MAX_PAGES {
                return Err(Error("too many pages".into()));
            }
            doc.max_id = doc.max_id.max(other.max_id);
            for (id, obj) in std::mem::take(&mut other.objects) {
                doc.objects.insert(id, obj);
            }
            let at = (*at).min(count);
            for (i, id) in ids.into_iter().enumerate() {
                st.pages.list.insert(at + i, id);
            }
            Ok(())
        }
        Op::Crop { page, rect } => {
            let id = st.page(*page)?;
            let geo = Geometry::of(doc, id);
            let r = rect.normalized();
            if !r.is_sane() || r.w < 4.0 || r.h < 4.0 {
                return Err(Error("the crop box is too small".into()));
            }
            let u = geo.rect(r);
            let m = geo.media;
            let c = [u[0].max(m[0]), u[1].max(m[1]), u[2].min(m[2]), u[3].min(m[3])];
            if c[2] - c[0] < 4.0 || c[3] - c[1] < 4.0 {
                return Err(Error("the crop box is outside the page".into()));
            }
            doc.get_dictionary_mut(id)?.set("CropBox", real_array(&c));
            Ok(())
        }
        Op::Transform { pages, flip_h, flip_v, scale } => {
            let s = if scale.is_finite() { f64::from(scale.clamp(0.05, 20.0)) } else { 1.0 };
            for p in pages {
                let id = st.page(*p)?;
                let geo = Geometry::of(doc, id);
                // A flip across the page as shown is the other axis on a quarter-turned page.
                let (fh, fv) = if geo.rotate % 180 == 0 { (*flip_h, *flip_v) } else { (*flip_v, *flip_h) };
                let sx = if fh { -s } else { s };
                let sy = if fv { -s } else { s };
                let cx = (geo.crop[0] + geo.crop[2]) / 2.0;
                let cy = (geo.crop[1] + geo.crop[3]) / 2.0;
                pages::wrap_content(doc, id, [sx, 0.0, 0.0, sy, cx - sx * cx, cy - sy * cy])?;
            }
            Ok(())
        }
        Op::Watermark { text, size, color, opacity, angle, pages: which } => {
            let text: String = text.chars().take(500).collect();
            for p in pages_of(which, count) {
                let id = st.page(p)?;
                let geo = Geometry::of(doc, id);
                let (w, h) = geo.display_size();
                let font = Font::new(crate::DEFAULT_FONT, true, false);
                let size = size.clamp(4.0, 400.0);
                let mut cv = Canvas::new(st.prefix());
                let a = f64::from(angle.clamp(-360.0, 360.0)).to_radians();
                cv.opacity(opacity.clamp(0.02, 1.0), false);
                cv.fill(*color);
                let _ = writeln!(cv.ops, "q {} {} {} {} {} {} cm", n(a.cos()), n(-a.sin()), n(a.sin()), n(a.cos()), n(w / 2.0), n(h / 2.0));
                let tw = font.width(&text, size);
                cv.text(&font, &text, size, -tw / 2.0, size * 0.35);
                cv.raw("Q");
                page_draw(doc, st, id, &geo, cv)?;
            }
            Ok(())
        }
        Op::PageText { text, position, size, color, start, pages: which } => {
            let list = pages_of(which, count);
            let total = count.to_string();
            let font = Font::new(crate::DEFAULT_FONT, false, false);
            let size = size.clamp(4.0, 72.0);
            for p in list {
                let id = st.page(p)?;
                let geo = Geometry::of(doc, id);
                let (w, h) = geo.display_size();
                let num = (p as i64).saturating_add(*start).to_string();
                let s: String = text.replace("{n}", &num).replace("{N}", &total).chars().take(500).collect();
                let tw = f64::from(font.width(&s, size));
                let margin = 28.0f64;
                let x = match position {
                    Position::TopLeft | Position::BottomLeft => margin,
                    Position::TopCenter | Position::BottomCenter => (w - tw) / 2.0,
                    Position::TopRight | Position::BottomRight => w - margin - tw,
                };
                let base = match position {
                    Position::TopLeft | Position::TopCenter | Position::TopRight => margin * 0.8 + f64::from(size),
                    _ => h - margin * 0.8,
                };
                let mut cv = Canvas::new(st.prefix());
                cv.fill(*color);
                cv.text(&font, &s, size, x as f32, base as f32);
                page_draw(doc, st, id, &geo, cv)?;
            }
            Ok(())
        }
        Op::Metadata { title, author, subject, keywords } => {
            let info_id = match doc.trailer.get(b"Info").and_then(Object::as_reference) {
                Ok(id) => id,
                Err(_) => {
                    let id = doc.add_object(Object::Dictionary(Dictionary::new()));
                    doc.trailer.set("Info", Object::Reference(id));
                    id
                }
            };
            let info = doc.get_dictionary_mut(info_id)?;
            for (key, v) in [("Title", title), ("Author", author), ("Subject", subject), ("Keywords", keywords)] {
                if let Some(v) = v {
                    info.set(key, encode_text(&v.chars().take(2000).collect::<String>()));
                }
            }
            Ok(())
        }
        Op::Fill { field, value } => crate::forms::fill(doc, &st.pages, field, value, &mut st.counter),
    }
}

/// Draw `cv` (display coordinates) over page `id`.
fn page_draw(doc: &mut Document, st: &mut State, id: ObjectId, geo: &Geometry, cv: Canvas) -> Result<(), Error> {
    let (ops, res) = cv.finish(doc);
    let m = geo.display_to_page();
    let mut content = format!("q {} {} {} {} {} {} cm\n", n(m[0]), n(m[1]), n(m[2]), n(m[3]), n(m[4]), n(m[5])).into_bytes();
    content.extend_from_slice(&ops);
    content.extend_from_slice(b"\nQ\n");
    pages::append_content(doc, id, &mut st.wrapped, content, res)
}

/// A form XObject drawing `cv` (display coordinates) over `bbox`, for an appearance stream.
pub(crate) fn form_xobject(doc: &mut Document, cv: Canvas, bbox: Rect, geo: &Geometry) -> ObjectId {
    let (ops, res) = cv.finish(doc);
    let mut d = Dictionary::new();
    d.set("Type", Object::Name(b"XObject".to_vec()));
    d.set("Subtype", Object::Name(b"Form".to_vec()));
    d.set("BBox", real_array(&[f64::from(bbox.x), f64::from(bbox.y), f64::from(bbox.x + bbox.w), f64::from(bbox.y + bbox.h)]));
    d.set("Matrix", real_array(&geo.display_to_page()));
    d.set("Resources", Object::Dictionary(res));
    let mut s = Stream::new(d, ops);
    let _ = s.compress();
    doc.add_object(Object::Stream(s))
}

fn color_array(c: Color) -> Object {
    real_array(&[f64::from(c[0]) / 255.0, f64::from(c[1]) / 255.0, f64::from(c[2]) / 255.0])
}

/// Add an annotation of `subtype` covering `bbox` (display) with appearance `cv` and `extra` keys.
fn annotation(
    doc: &mut Document,
    st: &mut State,
    page: ObjectId,
    id: u64,
    subtype: &str,
    bbox: Rect,
    cv: Canvas,
    extra: Dictionary,
) -> Result<(), Error> {
    let geo = Geometry::of(doc, page);
    let bbox = Rect { x: bbox.x - 1.0, y: bbox.y - 1.0, w: bbox.w + 2.0, h: bbox.h + 2.0 };
    let ap = form_xobject(doc, cv, bbox, &geo);
    let mut d = Dictionary::new();
    d.set("Type", Object::Name(b"Annot".to_vec()));
    d.set("Subtype", Object::Name(subtype.as_bytes().to_vec()));
    d.set("Rect", real_array(&geo.rect(bbox)));
    d.set("F", 4);
    d.set("NM", Object::string_literal(format!("wordcraft-{id}")));
    d.set("P", Object::Reference(page));
    let mut apd = Dictionary::new();
    apd.set("N", Object::Reference(ap));
    d.set("AP", Object::Dictionary(apd));
    for (k, v) in extra.iter() {
        d.set(k.clone(), v.clone());
    }
    let annot = doc.add_object(Object::Dictionary(d));
    let _ = st;
    pages::add_annot(doc, page, annot)
}

fn quad_points(geo: &Geometry, rects: &[Rect]) -> Object {
    let mut v = Vec::new();
    for r in rects {
        for p in [[r.x, r.y], [r.x + r.w, r.y], [r.x, r.y + r.h], [r.x + r.w, r.y + r.h]] {
            let u = geo.point(p);
            v.extend_from_slice(&u);
        }
    }
    real_array(&v)
}

fn sane_rects(rects: &[Rect]) -> Vec<Rect> {
    rects.iter().map(|r| r.normalized()).filter(Rect::is_sane).take(2000).collect()
}

fn apply_mark(doc: &mut Document, st: &mut State, id: u64, page: usize, mark: &Mark) -> Result<(), Error> {
    let pid = st.page(page)?;
    let geo = Geometry::of(doc, pid);
    let bounds = mark.bounds();
    if !bounds.is_sane() {
        return Err(Error("the mark is outside any page".into()));
    }
    let mut cv = Canvas::new(st.prefix());
    let mut extra = Dictionary::new();
    let contents = |extra: &mut Dictionary, text: &str| extra.set("Contents", encode_text(&text.chars().take(10_000).collect::<String>()));
    match mark {
        Mark::Highlight { rects, color } | Mark::Underline { rects, color } | Mark::Strikeout { rects, color } => {
            let rects = sane_rects(rects);
            if rects.is_empty() {
                return Err(Error("nothing to mark".into()));
            }
            let subtype = match mark {
                Mark::Highlight { .. } => {
                    // Multiply keeps the text readable; the opacity is for readers without blending.
                    cv.opacity(0.45, true);
                    cv.fill(*color);
                    for r in &rects {
                        cv.rect(*r);
                    }
                    cv.raw("f");
                    "Highlight"
                }
                Mark::Underline { .. } => {
                    cv.fill(*color);
                    for r in &rects {
                        let t = (r.h * 0.07).max(0.6);
                        cv.rect(Rect { x: r.x, y: r.y + r.h - t * 1.5, w: r.w, h: t });
                    }
                    cv.raw("f");
                    "Underline"
                }
                _ => {
                    cv.fill(*color);
                    for r in &rects {
                        let t = (r.h * 0.07).max(0.6);
                        cv.rect(Rect { x: r.x, y: r.y + r.h * 0.55 - t / 2.0, w: r.w, h: t });
                    }
                    cv.raw("f");
                    "StrikeOut"
                }
            };
            extra.set("QuadPoints", quad_points(&geo, &rects));
            extra.set("C", color_array(*color));
            annotation(doc, st, pid, id, subtype, bounds, cv, extra)
        }
        Mark::Ink { strokes, color, width } => {
            let width = width.clamp(0.1, 100.0);
            cv.stroke(*color, width);
            cv.raw("1 J 1 j");
            let mut list = Vec::new();
            for s in strokes.iter().take(2000) {
                let pts: Vec<[f32; 2]> = s.iter().copied().filter(|p| p[0].is_finite() && p[1].is_finite()).take(20_000).collect();
                if pts.is_empty() {
                    continue;
                }
                if pts.len() == 1 {
                    let p = pts[0];
                    cv.polyline(&[p, [p[0] + 0.01, p[1]]]);
                } else {
                    cv.polyline(&pts);
                }
                list.push(real_array(&pts.iter().flat_map(|p| geo.point(*p)).collect::<Vec<_>>()));
            }
            if list.is_empty() {
                return Err(Error("nothing drawn".into()));
            }
            cv.raw("S");
            extra.set("InkList", Object::Array(list));
            extra.set("C", color_array(*color));
            extra.set("BS", Object::Dictionary(Dictionary::from_iter(vec![("W", Object::Real(width))])));
            annotation(doc, st, pid, id, "Ink", bounds, cv, extra)
        }
        Mark::Line { from, to, color, width, arrow } => {
            let width = width.clamp(0.1, 100.0);
            cv.stroke(*color, width);
            cv.raw("1 J 1 j");
            let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
            let len = (dx * dx + dy * dy).sqrt();
            let mut end = *to;
            if *arrow && len > 0.1 {
                // The line stops at the arrowhead's base; the head is filled.
                let head = (width * 4.0 + 6.0).min(len);
                let (ux, uy) = (dx / len, dy / len);
                end = [to[0] - ux * head * 0.8, to[1] - uy * head * 0.8];
                cv.polyline(&[*from, end]);
                cv.raw("S");
                cv.fill(*color);
                let base = [to[0] - ux * head, to[1] - uy * head];
                let (px, py) = (-uy * head * 0.45, ux * head * 0.45);
                cv.polyline(&[*to, [base[0] + px, base[1] + py], [base[0] - px, base[1] - py]]);
                cv.raw("h f");
            } else {
                cv.polyline(&[*from, end]);
                cv.raw("S");
            }
            let a = geo.point(*from);
            let b = geo.point(*to);
            extra.set("L", real_array(&[a[0], a[1], b[0], b[1]]));
            extra.set("C", color_array(*color));
            extra.set("BS", Object::Dictionary(Dictionary::from_iter(vec![("W", Object::Real(width))])));
            if *arrow {
                extra.set("LE", Object::Array(vec![Object::Name(b"None".to_vec()), Object::Name(b"ClosedArrow".to_vec())]));
                extra.set("IC", color_array(*color));
            }
            annotation(doc, st, pid, id, "Line", bounds, cv, extra)
        }
        Mark::Rectangle { rect, stroke, fill, width } | Mark::Ellipse { rect, stroke, fill, width } => {
            let width = width.clamp(0.0, 100.0);
            let r = rect.normalized();
            let ellipse = matches!(mark, Mark::Ellipse { .. });
            let inner = Rect { x: r.x + width / 2.0, y: r.y + width / 2.0, w: (r.w - width).max(0.1), h: (r.h - width).max(0.1) };
            if let Some(f) = fill {
                cv.fill(*f);
            }
            if let Some(s) = stroke {
                cv.stroke(*s, width);
                extra.set("C", color_array(*s));
            }
            if let Some(f) = fill {
                extra.set("IC", color_array(*f));
            }
            if ellipse {
                cv.ellipse(inner);
            } else {
                cv.rect(inner);
            }
            cv.raw(match (fill.is_some(), stroke.is_some() && width > 0.0) {
                (true, true) => "B",
                (true, false) => "f",
                (false, true) => "S",
                (false, false) => "n",
            });
            extra.set("BS", Object::Dictionary(Dictionary::from_iter(vec![("W", Object::Real(width))])));
            annotation(doc, st, pid, id, if ellipse { "Circle" } else { "Square" }, r, cv, extra)
        }
        Mark::TextBox { rect, text, size, color, font, bold, italic, fit, fill, border } => {
            let r = rect.normalized();
            let f = Font::new(font, *bold, *italic);
            let pad = 3.0f32;
            let inner_w = (r.w - 2.0 * pad).max(1.0);
            let size = if *fit { f.fit(text, inner_w, (r.h - 2.0 * pad).max(1.0), 200.0) } else { size.clamp(2.0, 400.0) };
            if let Some(bg) = fill {
                cv.fill(*bg);
                cv.rect(r);
                cv.raw("f");
            }
            if let Some(b) = border {
                cv.stroke(*b, 1.0);
                cv.rect(r);
                cv.raw("S");
            }
            cv.fill(*color);
            for (i, line) in f.wrap(text, size, inner_w).iter().enumerate() {
                cv.text(&f, line, size, r.x + pad, r.y + pad + size * (0.85 + 1.2 * i as f32));
            }
            contents(&mut extra, text);
            let [cr, cg, cb] = *color;
            extra.set(
                "DA",
                Object::string_literal(format!(
                    "{} {} {} rg /Helv {} Tf",
                    n(f64::from(cr) / 255.0),
                    n(f64::from(cg) / 255.0),
                    n(f64::from(cb) / 255.0),
                    n(f64::from(size))
                )),
            );
            annotation(doc, st, pid, id, "FreeText", r, cv, extra)
        }
        Mark::Note { at, text, color } => {
            let r = Rect { x: at[0], y: at[1], w: NOTE_SIZE, h: NOTE_SIZE };
            cv.fill(*color);
            cv.stroke([90, 90, 90], 0.8);
            cv.rect(r);
            cv.raw("B");
            cv.stroke([60, 60, 60], 1.2);
            for i in 0..3 {
                let y = r.y + 6.0 + 5.0 * i as f32;
                cv.polyline(&[[r.x + 5.0, y], [r.x + r.w - 5.0, y]]);
            }
            cv.raw("S");
            contents(&mut extra, text);
            extra.set("Name", Object::Name(b"Comment".to_vec()));
            extra.set("Open", false);
            extra.set("C", color_array(*color));
            annotation(doc, st, pid, id, "Text", r, cv, extra)
        }
        Mark::Stamp { rect, text, color } => {
            let r = rect.normalized();
            let f = Font::new(crate::DEFAULT_FONT, true, false);
            let border = (r.h * 0.06).clamp(1.0, 6.0);
            cv.stroke(*color, border);
            cv.rect(Rect { x: r.x + border, y: r.y + border, w: (r.w - 2.0 * border).max(1.0), h: (r.h - 2.0 * border).max(1.0) });
            cv.raw("S");
            let inner_w = (r.w - 6.0 * border).max(1.0);
            let size = f.fit(text, inner_w, (r.h - 4.0 * border).max(1.0), 200.0);
            let lines = f.wrap(text, size, inner_w);
            let total = lines.len() as f32 * size * 1.2;
            cv.fill(*color);
            for (i, line) in lines.iter().enumerate() {
                let w = f.width(line, size);
                let base = r.y + (r.h - total) / 2.0 + size * (0.95 + 1.2 * i as f32);
                cv.text(&f, line, size, r.x + (r.w - w) / 2.0, base);
            }
            contents(&mut extra, text);
            extra.set("Name", Object::Name(b"Draft".to_vec()));
            annotation(doc, st, pid, id, "Stamp", r, cv, extra)
        }
        Mark::Image { rect, data } => {
            let r = rect.normalized();
            if !cv.image(data, r) {
                return Err(Error("the picture can't be read".into()));
            }
            annotation(doc, st, pid, id, "Stamp", r, cv, extra)
        }
        Mark::Whiteout { rect } => {
            cv.fill([255, 255, 255]);
            cv.rect(rect.normalized());
            cv.raw("f");
            page_draw(doc, st, pid, &geo, cv)
        }
        Mark::EditText { rect, base, text, size, color, font, bold, italic, bg } => {
            let r = rect.normalized();
            cv.fill(bg.unwrap_or([255, 255, 255]));
            cv.rect(Rect { x: r.x - 1.0, y: r.y - 1.0, w: r.w + 2.0, h: r.h + 2.0 });
            cv.raw("f");
            let f = Font::new(font, *bold, *italic);
            cv.fill(*color);
            cv.text(&f, text, size.clamp(1.0, 400.0), r.x, *base);
            page_draw(doc, st, pid, &geo, cv)
        }
        Mark::Redact { rects } => {
            let rects = sane_rects(rects);
            rasterize(doc, st, page, &rects)
        }
    }
}

/// Replace page `index` with a picture of itself (with `black` boxes painted out): nothing under
/// the boxes, nor any text at all, is left on the page.
fn rasterize(doc: &mut Document, st: &mut State, index: usize, black: &[Rect]) -> Result<(), Error> {
    let pid = st.page(index)?;
    // The page as it stands now, rendered.
    pages::commit(doc, &st.pages)?;
    let bytes = save_doc(doc)?;
    let view = wordcraft_pdfread::PdfView::open(bytes).map_err(|e| Error(e.to_string()))?;
    let (w, h) = view.page_size(index).ok_or_else(|| Error("no such page".into()))?;
    let mut img = view.render(index, RASTER_SCALE).map_err(|e| Error(e.to_string()))?;
    let (pw, ph) = (img.width as f32, img.height as f32);
    let (kx, ky) = (pw / w.max(1.0), ph / h.max(1.0));
    for r in black {
        let x0 = (r.x * kx).floor().clamp(0.0, pw) as usize;
        let x1 = ((r.x + r.w) * kx).ceil().clamp(0.0, pw) as usize;
        let y0 = (r.y * ky).floor().clamp(0.0, ph) as usize;
        let y1 = ((r.y + r.h) * ky).ceil().clamp(0.0, ph) as usize;
        for y in y0..y1 {
            for x in x0..x1 {
                let i = (y * img.width as usize + x) * 4;
                if let Some(px) = img.rgba.get_mut(i..i + 3) {
                    px.fill(0);
                }
            }
        }
    }
    let rgb: Vec<u8> = img.rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    let mut d = Dictionary::new();
    d.set("Type", Object::Name(b"XObject".to_vec()));
    d.set("Subtype", Object::Name(b"Image".to_vec()));
    d.set("Width", i64::from(img.width));
    d.set("Height", i64::from(img.height));
    d.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
    d.set("BitsPerComponent", 8);
    let mut s = Stream::new(d, rgb);
    let _ = s.compress();
    let im = doc.add_object(Object::Stream(s));
    let content = format!("q {} 0 0 {} 0 0 cm /WCraster Do Q\n", n(f64::from(w)), n(f64::from(h))).into_bytes();
    let mut cs = Stream::new(Dictionary::new(), content);
    let _ = cs.compress();
    let cid = doc.add_object(Object::Stream(cs));
    let mut xo = Dictionary::new();
    xo.set("WCraster", Object::Reference(im));
    let mut res = Dictionary::new();
    res.set("XObject", Object::Dictionary(xo));
    let page = doc.get_dictionary_mut(pid)?;
    page.set("MediaBox", real_array(&[0.0, 0.0, f64::from(w), f64::from(h)]));
    page.remove(b"CropBox");
    page.remove(b"Rotate");
    page.remove(b"Annots");
    page.set("Resources", Object::Dictionary(res));
    page.set("Contents", Object::Reference(cid));
    st.wrapped.remove(&pid);
    Ok(())
}

/// Where each mark ends up: its page in the edited PDF (0-based), by mark id. A mark is left out
/// when a later edit turns, crops, flips or removes its page (it is no longer where it was put,
/// in display terms) or when its page can't be followed.
pub fn mark_pages(base: &[u8], ops: &[Op]) -> std::collections::HashMap<u64, usize> {
    let mut count = load(base).map(|d| d.get_pages().len()).unwrap_or(0);
    // Each mark's page so far.
    let mut marks: Vec<(u64, Option<usize>)> = Vec::new();
    let follow = |marks: &mut Vec<(u64, Option<usize>)>, f: &dyn Fn(usize) -> Option<usize>| {
        for m in marks.iter_mut() {
            m.1 = m.1.and_then(f);
        }
    };
    for op in ops {
        match op {
            Op::Mark { id, page, .. } => marks.push((*id, Some(*page).filter(|p| *p < count))),
            Op::Rotate { pages, by } if by.rem_euclid(360) / 90 != 0 => {
                follow(&mut marks, &|p| (!pages.contains(&p)).then_some(p));
            }
            Op::Crop { page, .. } => follow(&mut marks, &|p| (p != *page).then_some(p)),
            Op::Transform { pages, .. } => follow(&mut marks, &|p| (!pages.contains(&p)).then_some(p)),
            Op::Delete { pages } => {
                let set: HashSet<usize> = pages.iter().copied().filter(|p| *p < count).collect();
                if set.len() < count {
                    follow(&mut marks, &|p| (!set.contains(&p)).then(|| p - set.iter().filter(|d| **d < p).count()));
                    count -= set.len();
                }
            }
            Op::Move { from, to } if *from < count => {
                let (from, to) = (*from, (*to).min(count - 1));
                follow(&mut marks, &|p| {
                    Some(if p == from {
                        to
                    } else {
                        let q = if p > from { p - 1 } else { p };
                        if q >= to { q + 1 } else { q }
                    })
                });
            }
            Op::Duplicate { page } if *page < count && count < crate::MAX_PAGES => {
                follow(&mut marks, &|p| Some(if p > *page { p + 1 } else { p }));
                count += 1;
            }
            Op::InsertBlank { at, .. } if count < crate::MAX_PAGES => {
                let at = (*at).min(count);
                follow(&mut marks, &|p| Some(if p >= at { p + 1 } else { p }));
                count += 1;
            }
            Op::InsertPdf { at, data } => {
                let n = load(data).map(|d| d.get_pages().len()).unwrap_or(0);
                if n > 0 && count + n <= crate::MAX_PAGES {
                    let at = (*at).min(count);
                    follow(&mut marks, &|p| Some(if p >= at { p + n } else { p }));
                    count += n;
                }
            }
            _ => {}
        }
    }
    marks.into_iter().filter_map(|(id, p)| p.map(|p| (id, p))).collect()
}

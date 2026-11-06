//! The page list and each page's geometry, on a lopdf document.

use crate::Error;
use crate::Rect;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

/// The document's pages as one flat list under the root page tree node.
pub(crate) struct Pages {
    pub root: ObjectId,
    pub list: Vec<ObjectId>,
}

const INHERITED: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

/// The pages, each holding its own inherited attributes, so they can be moved, removed and
/// added freely. The tree itself is rewritten flat by [`commit`].
pub(crate) fn flatten(doc: &mut Document) -> Result<Pages, Error> {
    let root = doc.catalog()?.get(b"Pages")?.as_reference()?;
    let list: Vec<ObjectId> = doc.get_pages().into_values().collect();
    if list.is_empty() {
        return Err(Error("the PDF has no pages".into()));
    }
    for &id in &list {
        let mut found: Vec<(&[u8], Object)> = Vec::new();
        for key in INHERITED {
            let mut node = Some(id);
            let mut depth = 0;
            while let Some(n) = node {
                depth += 1;
                if depth > 64 {
                    break;
                }
                let Ok(d) = doc.get_dictionary(n) else { break };
                if let Ok(v) = d.get(key) {
                    if n != id {
                        found.push((key, v.clone()));
                    }
                    break;
                }
                node = d.get(b"Parent").and_then(Object::as_reference).ok();
            }
        }
        if let Ok(d) = doc.get_dictionary_mut(id) {
            for (k, v) in found {
                d.set(k, v);
            }
            if !d.has(b"MediaBox") {
                d.set("MediaBox", vec![0.into(), 0.into(), 612.into(), 792.into()]);
            }
        }
    }
    Ok(Pages { root, list })
}

/// Write the list back as the root node's children.
pub(crate) fn commit(doc: &mut Document, pages: &Pages) -> Result<(), Error> {
    let kids: Vec<Object> = pages.list.iter().map(|id| Object::Reference(*id)).collect();
    let root = doc.get_dictionary_mut(pages.root)?;
    root.set("Kids", kids);
    root.set("Count", pages.list.len() as i64);
    for key in INHERITED {
        root.remove(key);
    }
    for id in &pages.list {
        if let Ok(d) = doc.get_dictionary_mut(*id) {
            d.set("Parent", Object::Reference(pages.root));
        }
    }
    Ok(())
}

fn floats(doc: &Document, o: &Object) -> Option<[f64; 4]> {
    let (_, o) = doc.dereference(o).ok()?;
    let a = o.as_array().ok()?;
    let mut v = [0.0f64; 4];
    for (i, slot) in v.iter_mut().enumerate() {
        let (_, x) = doc.dereference(a.get(i)?).ok()?;
        *slot = f64::from(x.as_float().ok()?);
    }
    if v.iter().any(|x| !x.is_finite()) {
        return None;
    }
    Some([v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])])
}

/// A page's shown area and turn, and the map from its display coordinates (points from the top
/// left of the page as shown, y down) to its own.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Geometry {
    /// The crop box (within the media box), in the page's own space.
    pub crop: [f64; 4],
    pub media: [f64; 4],
    pub rotate: i32,
}

impl Geometry {
    pub fn of(doc: &Document, page: ObjectId) -> Geometry {
        let d = doc.get_dictionary(page).ok();
        let media = d.and_then(|d| d.get(b"MediaBox").ok()).and_then(|o| floats(doc, o)).unwrap_or([0.0, 0.0, 612.0, 792.0]);
        let crop = d
            .and_then(|d| d.get(b"CropBox").ok())
            .and_then(|o| floats(doc, o))
            .map(|c| [c[0].max(media[0]), c[1].max(media[1]), c[2].min(media[2]), c[3].min(media[3])])
            .filter(|c| c[2] - c[0] > 1.0 && c[3] - c[1] > 1.0)
            .unwrap_or(media);
        let r = d.and_then(|d| d.get(b"Rotate").ok()).and_then(|o| doc.dereference(o).ok()).and_then(|(_, o)| o.as_i64().ok()).unwrap_or(0);
        let rotate = (((r % 360) + 360) % 360 / 90 * 90) as i32;
        Geometry { crop, media, rotate }
    }

    /// The page's size as shown (width, height).
    pub fn display_size(&self) -> (f64, f64) {
        let (w, h) = (self.crop[2] - self.crop[0], self.crop[3] - self.crop[1]);
        if self.rotate % 180 == 0 { (w, h) } else { (h, w) }
    }

    /// Display → page space, as a PDF matrix [a b c d e f].
    pub fn display_to_page(&self) -> [f64; 6] {
        let [x0, y0, x1, y1] = self.crop;
        match self.rotate {
            90 => [0.0, 1.0, 1.0, 0.0, x0, y0],
            180 => [-1.0, 0.0, 0.0, 1.0, x1, y0],
            270 => [0.0, -1.0, -1.0, 0.0, x1, y1],
            _ => [1.0, 0.0, 0.0, -1.0, x0, y1],
        }
    }

    pub fn point(&self, p: [f32; 2]) -> [f64; 2] {
        let [a, b, c, d, e, f] = self.display_to_page();
        let (x, y) = (f64::from(p[0]), f64::from(p[1]));
        [a * x + c * y + e, b * x + d * y + f]
    }

    /// A display box's bounds in page space [x0 y0 x1 y1].
    pub fn rect(&self, r: Rect) -> [f64; 4] {
        let pts = [self.point([r.x, r.y]), self.point([r.x + r.w, r.y]), self.point([r.x, r.y + r.h]), self.point([r.x + r.w, r.y + r.h])];
        let xs = pts.iter().map(|p| p[0]);
        let ys = pts.iter().map(|p| p[1]);
        [
            xs.clone().fold(f64::INFINITY, f64::min),
            ys.clone().fold(f64::INFINITY, f64::min),
            xs.fold(f64::NEG_INFINITY, f64::max),
            ys.fold(f64::NEG_INFINITY, f64::max),
        ]
    }

    /// Page space → display, for a box [x0 y0 x1 y1].
    pub fn display_rect(&self, u: [f64; 4]) -> Rect {
        let [a, b, c, d, e, f] = self.display_to_page();
        // The map is a rotation/flip by quarter turns: its inverse is its transpose.
        let det = a * d - b * c;
        if det.abs() < 1e-9 {
            return Rect::default();
        }
        let inv = |x: f64, y: f64| {
            let (x, y) = (x - e, y - f);
            [(d * x - c * y) / det, (-b * x + a * y) / det]
        };
        let pts = [inv(u[0], u[1]), inv(u[2], u[1]), inv(u[0], u[3]), inv(u[2], u[3])];
        let x0 = pts.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
        let y0 = pts.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
        let x1 = pts.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max);
        let y1 = pts.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max);
        Rect { x: x0 as f32, y: y0 as f32, w: (x1 - x0) as f32, h: (y1 - y0) as f32 }
    }
}

pub(crate) fn real_array(v: &[f64]) -> Object {
    Object::Array(v.iter().map(|x| Object::Real(if x.is_finite() { *x as f32 } else { 0.0 })).collect())
}

/// A copy of `o` resolved to a dictionary (a reference followed), or an empty one.
fn direct_dict(doc: &Document, o: Option<&Object>) -> Dictionary {
    o.and_then(|o| doc.dereference(o).ok()).and_then(|(_, o)| o.as_dict().ok().cloned()).unwrap_or_default()
}

/// Add `res` (ExtGState and XObject entries) to the page's resources, which become the page's
/// own (a shared resource dictionary isn't changed for the other pages).
pub(crate) fn add_resources(doc: &mut Document, page: ObjectId, res: Dictionary) -> Result<(), Error> {
    let current = doc.get_dictionary(page)?.get(b"Resources").ok().cloned();
    let mut own = direct_dict(doc, current.as_ref());
    for (category, entries) in res.iter() {
        let Ok(entries) = entries.as_dict() else { continue };
        let mut sub = direct_dict(doc, own.get(category).ok());
        for (k, v) in entries.iter() {
            sub.set(k.clone(), v.clone());
        }
        own.set(category.clone(), Object::Dictionary(sub));
    }
    doc.get_dictionary_mut(page)?.set("Resources", Object::Dictionary(own));
    Ok(())
}

/// The page's content streams, as references.
fn contents(doc: &Document, page: ObjectId) -> Vec<Object> {
    let Ok(d) = doc.get_dictionary(page) else { return Vec::new() };
    match d.get(b"Contents") {
        Ok(Object::Reference(id)) => match doc.get_object(*id) {
            Ok(Object::Array(a)) => a.clone(),
            _ => vec![Object::Reference(*id)],
        },
        Ok(Object::Array(a)) => a.clone(),
        _ => Vec::new(),
    }
}

fn stream(doc: &mut Document, content: Vec<u8>) -> Object {
    let mut s = Stream::new(Dictionary::new(), content);
    let _ = s.compress();
    Object::Reference(doc.add_object(Object::Stream(s)))
}

/// Put `content` (with `res`) over the page. The page's own content is wrapped in q/Q the first
/// time, so its graphics state doesn't leak into what is added.
pub(crate) fn append_content(
    doc: &mut Document,
    page: ObjectId,
    wrapped: &mut std::collections::HashSet<ObjectId>,
    content: Vec<u8>,
    res: Dictionary,
) -> Result<(), Error> {
    add_resources(doc, page, res)?;
    let mut list = contents(doc, page);
    if wrapped.insert(page) {
        let q = stream(doc, b"q\n".to_vec());
        let qq = stream(doc, b"\nQ\n".to_vec());
        list.insert(0, q);
        list.push(qq);
    }
    let s = stream(doc, content);
    list.push(s);
    doc.get_dictionary_mut(page)?.set("Contents", Object::Array(list));
    Ok(())
}

/// Wrap the page's content in `q <m> cm … Q` (flip and scale).
pub(crate) fn wrap_content(doc: &mut Document, page: ObjectId, m: [f64; 6]) -> Result<(), Error> {
    let mut list = contents(doc, page);
    let pre = format!("q {} {} {} {} {} {} cm\n", m[0], m[1], m[2], m[3], m[4], m[5]).into_bytes();
    let pre = stream(doc, pre);
    let post = stream(doc, b"\nQ\n".to_vec());
    list.insert(0, pre);
    list.push(post);
    doc.get_dictionary_mut(page)?.set("Contents", Object::Array(list));
    Ok(())
}

/// Add an annotation (already in the document) to the page.
pub(crate) fn add_annot(doc: &mut Document, page: ObjectId, annot: ObjectId) -> Result<(), Error> {
    let current = doc.get_dictionary(page)?.get(b"Annots").ok().cloned();
    let mut list = match current.as_ref().map(|o| doc.dereference(o)) {
        Some(Ok((_, Object::Array(a)))) => a.clone(),
        _ => Vec::new(),
    };
    list.push(Object::Reference(annot));
    doc.get_dictionary_mut(page)?.set("Annots", Object::Array(list));
    Ok(())
}

/// A new empty page of `w` × `h` points.
pub(crate) fn blank(doc: &mut Document, w: f64, h: f64) -> ObjectId {
    let content = stream(doc, Vec::new());
    let mut d = Dictionary::new();
    d.set("Type", Object::Name(b"Page".to_vec()));
    d.set("MediaBox", real_array(&[0.0, 0.0, w.clamp(36.0, 14400.0), h.clamp(36.0, 14400.0)]));
    d.set("Resources", Object::Dictionary(Dictionary::new()));
    d.set("Contents", content);
    doc.add_object(Object::Dictionary(d))
}

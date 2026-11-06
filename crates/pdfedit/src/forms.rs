//! Form fields (AcroForm): listing them for the PDF view's live controls, and filling them in.

use crate::draw::{Canvas, Font};
use crate::pages::{Geometry, Pages};
use crate::{Error, Rect};
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::HashMap;

/// What kind of field it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    CheckBox,
    Radio,
    Choice,
    PushButton,
    Signature,
}

/// A form field: its full name, kind and value, its options (choices; the states check boxes
/// and radio buttons turn on to), and where its widgets are (page, box in display coordinates).
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub kind: FieldKind,
    pub value: String,
    pub options: Vec<String>,
    pub widgets: Vec<(usize, Rect, Vec<String>)>,
    pub multiline: bool,
    pub read_only: bool,
}

const MAX_FIELDS: usize = 5000;

fn text_of(o: &Object) -> String {
    match o {
        Object::String(b, _) => decode_text(b),
        Object::Name(n) => String::from_utf8_lossy(n).into_owned(),
        _ => String::new(),
    }
}

/// A PDF text string: UTF-16BE with its byte order mark, or PDFDocEncoding (read as Latin-1).
pub(crate) fn decode_text(b: &[u8]) -> String {
    if b.len() >= 2 && b[..2] == [0xFE, 0xFF] {
        let units: Vec<u16> = b[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c.get(1).copied().unwrap_or(0)])).collect();
        String::from_utf16_lossy(&units)
    } else {
        b.iter().map(|c| char::from(*c)).collect()
    }
}

/// A PDF text string for `s`: plain when it is ASCII, UTF-16BE otherwise.
pub(crate) fn encode_text(s: &str) -> Object {
    if s.is_ascii() {
        Object::string_literal(s.as_bytes().to_vec())
    } else {
        let mut b = vec![0xFE, 0xFF];
        for u in s.encode_utf16() {
            b.extend_from_slice(&u.to_be_bytes());
        }
        Object::String(b, lopdf::StringFormat::Hexadecimal)
    }
}

struct Walk<'a> {
    doc: &'a Document,
    page_of: HashMap<ObjectId, usize>,
    geo: Vec<Geometry>,
    annot_page: HashMap<ObjectId, usize>,
    out: Vec<(ObjectId, Field)>,
}

impl Walk<'_> {
    fn field(&mut self, id: ObjectId, parent_name: &str, inherited: &Dictionary, depth: usize) {
        if depth > 32 || self.out.len() >= MAX_FIELDS {
            return;
        }
        let Ok(d) = self.doc.get_dictionary(id) else { return };
        let part = d.get(b"T").map(text_of).unwrap_or_default();
        let name = match (parent_name.is_empty(), part.is_empty()) {
            (true, _) => part.clone(),
            (false, true) => parent_name.to_string(),
            (false, false) => format!("{parent_name}.{part}"),
        };
        let mut inh = inherited.clone();
        for k in [b"FT".as_slice(), b"Ff", b"V", b"Opt", b"DA"] {
            if let Ok(v) = d.get(k) {
                inh.set(k.to_vec(), v.clone());
            }
        }
        // Kids that are fields (they have a name) are walked; the rest are this field's widgets.
        let kids: Vec<ObjectId> =
            d.get(b"Kids").and_then(Object::as_array).map(|a| a.iter().filter_map(|k| k.as_reference().ok()).collect()).unwrap_or_default();
        let field_kids: Vec<ObjectId> = kids.iter().copied().filter(|k| self.doc.get_dictionary(*k).is_ok_and(|kd| kd.has(b"T"))).collect();
        if !field_kids.is_empty() {
            for k in field_kids {
                self.field(k, &name, &inh, depth + 1);
            }
            return;
        }
        let widgets: Vec<ObjectId> = if kids.is_empty() { vec![id] } else { kids };
        let ff = inh.get(b"Ff").and_then(Object::as_i64).unwrap_or(0);
        let kind = match inh.get(b"FT").and_then(Object::as_name).unwrap_or(b"") {
            b"Tx" => FieldKind::Text,
            b"Ch" => FieldKind::Choice,
            b"Sig" => FieldKind::Signature,
            b"Btn" if ff & (1 << 16) != 0 => FieldKind::PushButton,
            b"Btn" if ff & (1 << 15) != 0 => FieldKind::Radio,
            b"Btn" => FieldKind::CheckBox,
            _ => return,
        };
        let value = inh.get(b"V").map(|v| self.doc.dereference(v).map(|(_, v)| text_of(v)).unwrap_or_default()).unwrap_or_default();
        let options = inh
            .get(b"Opt")
            .and_then(Object::as_array)
            .map(|a| {
                a.iter()
                    .map(|o| match o {
                        Object::Array(pair) => pair.get(1).map(text_of).unwrap_or_default(),
                        o => text_of(o),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut placed = Vec::new();
        for w in widgets {
            let Ok(wd) = self.doc.get_dictionary(w) else { continue };
            let page = wd
                .get(b"P")
                .and_then(Object::as_reference)
                .ok()
                .and_then(|p| self.page_of.get(&p).copied())
                .or_else(|| self.annot_page.get(&w).copied());
            let Some(page) = page else { continue };
            let Some(geo) = self.geo.get(page) else { continue };
            let Some(r) = wd.get(b"Rect").ok().and_then(|o| rect4(self.doc, o)) else { continue };
            let on: Vec<String> = wd
                .get(b"AP")
                .and_then(Object::as_dict)
                .and_then(|ap| ap.get(b"N"))
                .ok()
                .and_then(|n| self.doc.dereference(n).ok())
                .and_then(|(_, n)| n.as_dict().ok())
                .map(|n| n.iter().map(|(k, _)| String::from_utf8_lossy(k).into_owned()).filter(|k| k != "Off").collect())
                .unwrap_or_default();
            placed.push((page, geo.display_rect(r), on));
        }
        let field = Field { name, kind, value, options, widgets: placed, multiline: ff & (1 << 12) != 0, read_only: ff & 1 != 0 };
        self.out.push((id, field));
    }
}

fn rect4(doc: &Document, o: &Object) -> Option<[f64; 4]> {
    let (_, o) = doc.dereference(o).ok()?;
    let a = o.as_array().ok()?;
    let mut v = [0.0f64; 4];
    for (i, slot) in v.iter_mut().enumerate() {
        *slot = f64::from(a.get(i)?.as_float().ok()?);
    }
    Some([v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])])
}

fn walk(doc: &Document, pages: &[ObjectId]) -> Vec<(ObjectId, Field)> {
    let mut w = Walk {
        doc,
        page_of: pages.iter().enumerate().map(|(i, id)| (*id, i)).collect(),
        geo: pages.iter().map(|id| Geometry::of(doc, *id)).collect(),
        annot_page: HashMap::new(),
        out: Vec::new(),
    };
    for (i, p) in pages.iter().enumerate() {
        let annots = doc
            .get_dictionary(*p)
            .ok()
            .and_then(|d| d.get(b"Annots").ok())
            .and_then(|o| doc.dereference(o).ok())
            .and_then(|(_, o)| o.as_array().ok());
        for a in annots.into_iter().flatten() {
            if let Ok(id) = a.as_reference() {
                w.annot_page.insert(id, i);
            }
        }
    }
    let fields = doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"AcroForm").ok())
        .and_then(|o| doc.dereference(o).ok())
        .and_then(|(_, o)| o.as_dict().ok())
        .and_then(|af| af.get(b"Fields").ok())
        .and_then(|o| doc.dereference(o).ok())
        .and_then(|(_, o)| o.as_array().ok())
        .map(|a| a.iter().filter_map(|f| f.as_reference().ok()).collect::<Vec<_>>())
        .unwrap_or_default();
    for f in fields {
        w.field(f, "", &Dictionary::new(), 0);
    }
    w.out
}

/// The PDF's form fields.
pub fn fields(bytes: &[u8]) -> Result<Vec<Field>, Error> {
    let mut doc = crate::apply::load(bytes)?;
    let pages = crate::pages::flatten(&mut doc)?;
    Ok(walk(&doc, &pages.list).into_iter().map(|(_, f)| f).collect())
}

/// The font size a widget's default appearance asks for (0 = fit), from `/DA`.
fn da_size(da: &str) -> f32 {
    let parts: Vec<&str> = da.split_whitespace().collect();
    parts
        .windows(2)
        .find(|w| w.get(1) == Some(&"Tf"))
        .and_then(|w| w.first())
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(0.0)
}

/// Fill in field `name` with `value`, with appearances for its widgets drawn so every reader
/// shows it at once.
pub(crate) fn fill(doc: &mut Document, pages: &Pages, name: &str, value: &str, counter: &mut u64) -> Result<(), Error> {
    let all = walk(doc, &pages.list);
    let Some((id, field)) = all.into_iter().find(|(_, f)| f.name == name) else {
        return Err(Error(format!("no form field named \"{name}\"")));
    };
    if field.read_only {
        return Err(Error(format!("the field \"{name}\" is read-only")));
    }
    let value: String = value.chars().take(10_000).collect();
    match field.kind {
        FieldKind::CheckBox | FieldKind::Radio => {
            doc.get_dictionary_mut(id)?.set("V", Object::Name(value.as_bytes().to_vec()));
            let widgets = widget_ids(doc, id);
            for w in widgets {
                let on = doc
                    .get_dictionary(w)
                    .ok()
                    .and_then(|d| d.get(b"AP").ok())
                    .and_then(|o| o.as_dict().ok())
                    .and_then(|ap| ap.get(b"N").ok())
                    .and_then(|n| doc.dereference(n).ok())
                    .and_then(|(_, n)| n.as_dict().ok())
                    .is_some_and(|n| n.has(value.as_bytes()));
                let state = if on && value != "Off" { value.as_bytes().to_vec() } else { b"Off".to_vec() };
                doc.get_dictionary_mut(w)?.set("AS", Object::Name(state));
            }
        }
        FieldKind::Text | FieldKind::Choice => {
            doc.get_dictionary_mut(id)?.set("V", encode_text(&value));
            let da = doc.get_dictionary(id).ok().and_then(|d| d.get(b"DA").ok()).map(text_of).unwrap_or_default();
            let size = da_size(&da);
            for w in widget_ids(doc, id) {
                let Some(page_id) = page_of_widget(doc, pages, w) else { continue };
                let geo = Geometry::of(doc, page_id);
                let Some(r) = doc.get_dictionary(w).ok().and_then(|d| d.get(b"Rect").ok()).and_then(|o| rect4(doc, o)) else { continue };
                let rect = geo.display_rect(r);
                *counter += 1;
                let mut cv = Canvas::new(format!("WCf{counter}"));
                let font = Font::new("Arial", false, false);
                let pad = 2.0f32;
                let inner = (rect.w - 2.0 * pad).max(1.0);
                cv.fill([0, 0, 0]);
                if field.multiline {
                    let s = if size > 0.0 { size } else { font.fit(&value, inner, (rect.h - 2.0 * pad).max(1.0), 12.0) };
                    for (i, line) in font.wrap(&value, s, inner).iter().enumerate() {
                        cv.text(&font, line, s, rect.x + pad, rect.y + pad + s * (0.9 + 1.2 * i as f32));
                    }
                } else {
                    let s = if size > 0.0 { size } else { (rect.h * 0.7).clamp(4.0, 14.0).min(font.fit(&value, inner, rect.h, 14.0)) };
                    cv.text(&font, &value, s, rect.x + pad, rect.y + rect.h / 2.0 + s * 0.35);
                }
                let ap = crate::apply::form_xobject(doc, cv, rect, &geo);
                let mut apd = Dictionary::new();
                apd.set("N", Object::Reference(ap));
                doc.get_dictionary_mut(w)?.set("AP", Object::Dictionary(apd));
            }
        }
        FieldKind::PushButton | FieldKind::Signature => return Err(Error(format!("the field \"{name}\" can't be filled in"))),
    }
    Ok(())
}

fn widget_ids(doc: &Document, field: ObjectId) -> Vec<ObjectId> {
    match doc.get_dictionary(field).ok().and_then(|d| d.get(b"Kids").ok()).and_then(|k| k.as_array().ok()) {
        Some(kids) if !kids.is_empty() => kids.iter().filter_map(|k| k.as_reference().ok()).collect(),
        _ => vec![field],
    }
}

/// The page a widget is on: its /P, or the page whose annotations list it.
fn page_of_widget(doc: &Document, pages: &Pages, w: ObjectId) -> Option<ObjectId> {
    if let Some(p) = doc.get_dictionary(w).ok().and_then(|d| d.get(b"P").ok()).and_then(|p| p.as_reference().ok()).filter(|p| pages.list.contains(p))
    {
        return Some(p);
    }
    pages.list.iter().copied().find(|p| {
        doc.get_dictionary(*p)
            .ok()
            .and_then(|d| d.get(b"Annots").ok())
            .and_then(|o| doc.dereference(o).ok())
            .and_then(|(_, o)| o.as_array().ok())
            .is_some_and(|a| a.iter().any(|x| x.as_reference().ok() == Some(w)))
    })
}

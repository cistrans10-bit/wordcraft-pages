//! PDF editing (`pdf.*`): markup, page organizing, stamps, forms, redaction and saving, on the
//! PDF the document was opened from (or the document's own PDF), as edits kept in the session
//! and replayed over it (`wordcraft_pdfedit`). The PDF view shows the result; `pdf.save` writes it.
//!
//! Pages are counted from 1 in parameters, as everywhere else; positions and boxes are in points
//! from the top left of the page as shown (`[x, y]`, `[x, y, w, h]`).

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use wordcraft_pdfedit::{Color, Mark, Op, Position, Rect, SaveOptions};

use crate::{CmdError, CmdResult, CommandSpec, Session, p};

/// Most undo steps kept for PDF edits.
const MAX_UNDO: usize = 200;
/// Largest file a PDF edit reads (a picture, a PDF to insert).
const MAX_FILE: u64 = 200 << 20;

/// The session's PDF edits.
#[derive(Debug, Clone, Default)]
pub struct PdfEdits {
    pub ops: Vec<Op>,
    undo: Vec<Vec<Op>>,
    redo: Vec<Vec<Op>>,
    next_id: u64,
    /// Where Save PDF writes: the PDF the document was opened from.
    pub path: Option<PathBuf>,
    /// The password the PDF was opened with (it is saved with it again).
    pub password: Option<String>,
    /// Bumped on every change, so the PDF view knows to show the PDF again.
    pub rev: u64,
    pub saved_rev: u64,
}

impl PdfEdits {
    pub fn dirty(&self) -> bool {
        self.rev != self.saved_rev
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The mark with this id, and its page (0-based).
    pub fn mark(&self, id: u64) -> Option<(usize, &Mark)> {
        self.ops.iter().find_map(|o| match o {
            Op::Mark { id: i, page, mark } if *i == id => Some((*page, mark)),
            _ => None,
        })
    }

    fn change(&mut self, f: impl FnOnce(&mut Vec<Op>, &mut u64) -> Result<Value, CmdError>) -> Result<Value, CmdError> {
        let before = self.ops.clone();
        let r = f(&mut self.ops, &mut self.next_id)?;
        if self.ops != before {
            self.undo.push(before);
            if self.undo.len() > MAX_UNDO {
                self.undo.remove(0);
            }
            self.redo.clear();
            self.rev += 1;
        }
        Ok(r)
    }
}

/// The PDF the edits apply to: the one the document was opened from while the document is as
/// it was converted, otherwise the document's export.
pub fn base(s: &Session) -> Result<Arc<Vec<u8>>, String> {
    if let Some(src) = s.pdf_source.as_ref().filter(|_| s.rev() == s.pdf_source_rev) {
        return Ok(src.clone());
    }
    crate::io::save_bytes("document.pdf", &s.doc).map(Arc::new)
}

/// The edited PDF, as Save PDF writes it.
pub fn edited(s: &Session, opts: &SaveOptions) -> Result<Vec<u8>, String> {
    let base = base(s)?;
    wordcraft_pdfedit::apply(&base, &s.pdf.ops, opts).map_err(|e| e.to_string())
}

fn color(v: &Value, k: &str, default: Color) -> Color {
    match v.get(k) {
        Some(Value::String(s)) => {
            let h = s.trim_start_matches('#');
            if h.len() == 6
                && let Ok(n) = u32::from_str_radix(h, 16)
            {
                return [(n >> 16) as u8, (n >> 8) as u8, n as u8];
            }
            default
        }
        Some(Value::Array(a)) if a.len() == 3 => {
            let c = |i: usize| a.get(i).and_then(Value::as_f64).map_or(0, |x| x.clamp(0.0, 255.0) as u8);
            [c(0), c(1), c(2)]
        }
        _ => default,
    }
}

fn opt_color(v: &Value, k: &str) -> Option<Color> {
    match v.get(k) {
        None | Some(Value::Null) | Some(Value::Bool(false)) => None,
        Some(_) => Some(color(v, k, [0, 0, 0])),
    }
}

fn num(x: &Value) -> Option<f32> {
    x.as_f64().filter(|f| f.is_finite() && f.abs() < 1.0e6).map(|f| f as f32)
}

fn point(v: &Value) -> Option<[f32; 2]> {
    let a = v.as_array()?;
    Some([num(a.first()?)?, num(a.get(1)?)?])
}

fn rect_of(v: &Value) -> Option<Rect> {
    let a = v.as_array()?;
    Some(Rect::new(num(a.first()?)?, num(a.get(1)?)?, num(a.get(2)?)?, num(a.get(3)?)?))
}

fn rect(v: &Value, k: &str) -> Result<Rect, CmdError> {
    v.get(k).and_then(rect_of).ok_or_else(|| CmdError::Params(format!("`{k}` ([x, y, w, h] in points) is required")))
}

fn rects(v: &Value) -> Result<Vec<Rect>, CmdError> {
    if let Some(a) = v.get("rects").and_then(Value::as_array) {
        let r: Vec<Rect> = a.iter().filter_map(rect_of).take(2000).collect();
        if !r.is_empty() {
            return Ok(r);
        }
    }
    rect(v, "rect").map(|r| vec![r])
}

/// A page number (1-based) from `k`, as an index.
fn page(v: &Value, k: &str) -> Result<usize, CmdError> {
    p::u64(v, k)
        .filter(|n| *n >= 1 && *n <= wordcraft_pdfedit::MAX_PAGES as u64)
        .map(|n| n as usize - 1)
        .ok_or_else(|| CmdError::Params(format!("`{k}` (a page number, from 1) is required")))
}

/// `pages` (a list of page numbers) or `page`, as indexes.
fn pages(v: &Value) -> Result<Vec<usize>, CmdError> {
    if let Some(a) = v.get("pages").and_then(Value::as_array) {
        let list: Vec<usize> =
            a.iter().filter_map(Value::as_u64).filter(|n| *n >= 1 && *n <= wordcraft_pdfedit::MAX_PAGES as u64).map(|n| n as usize - 1).collect();
        if !list.is_empty() {
            return Ok(list);
        }
    }
    page(v, "page").map(|p| vec![p])
}

fn opt_pages(v: &Value) -> Option<Vec<usize>> {
    pages(v).ok()
}

fn text(v: &Value, k: &str) -> String {
    p::str(v, k).unwrap_or("").chars().take(20_000).collect()
}

fn file_bytes(v: &Value) -> Result<Arc<Vec<u8>>, CmdError> {
    if let Some(data) = p::str(v, "data") {
        return super::insert::base64_decode(data).map(Arc::new).ok_or_else(|| CmdError::Params("`data` isn't base64".into()));
    }
    let path = p::req_str(v, "path")?;
    #[cfg(not(target_arch = "wasm32"))]
    let bytes = crate::io::read_file_capped(path, MAX_FILE, "file");
    #[cfg(target_arch = "wasm32")]
    let bytes = crate::io::read_file(path).and_then(|b| if b.len() as u64 > MAX_FILE { Err(format!("{path}: too large")) } else { Ok(b) });
    bytes.map(Arc::new).map_err(CmdError::Failed)
}

/// A mark from the parameters of `pdf.mark`.
fn mark_of(v: &Value) -> Result<Mark, CmdError> {
    let kind = p::req_str(v, "kind")?;
    let c = |d: Color| color(v, "color", d);
    let width = p::f32(v, "width").unwrap_or(2.0).clamp(0.0, 100.0);
    let size = p::f32(v, "size").unwrap_or(12.0).clamp(2.0, 400.0);
    let font = p::str(v, "font").unwrap_or(wordcraft_pdfedit::DEFAULT_FONT).chars().take(128).collect::<String>();
    Ok(match kind {
        "highlight" => Mark::Highlight { rects: rects(v)?, color: c([255, 230, 0]) },
        "underline" => Mark::Underline { rects: rects(v)?, color: c([0, 90, 200]) },
        "strikeout" => Mark::Strikeout { rects: rects(v)?, color: c([200, 0, 0]) },
        "ink" | "signature" if v.get("strokes").is_some() => {
            let strokes: Vec<Vec<[f32; 2]>> = v
                .get("strokes")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .take(2000)
                        .map(|s| s.as_array().map(|pts| pts.iter().filter_map(point).take(20_000).collect()).unwrap_or_default())
                        .collect()
                })
                .unwrap_or_default();
            if strokes.iter().all(Vec::is_empty) {
                return Err(CmdError::Params("`strokes` ([[[x, y], …], …]) is required".into()));
            }
            Mark::Ink { strokes, color: c([20, 20, 120]), width: width.max(0.1) }
        }
        "line" | "arrow" => Mark::Line {
            from: v.get("from").and_then(point).ok_or_else(|| CmdError::Params("`from` ([x, y]) is required".into()))?,
            to: v.get("to").and_then(point).ok_or_else(|| CmdError::Params("`to` ([x, y]) is required".into()))?,
            color: c([200, 0, 0]),
            width: width.max(0.1),
            arrow: kind == "arrow",
        },
        "rectangle" | "ellipse" => {
            let r = rect(v, "rect")?;
            let stroke = if v.get("stroke").is_some() { opt_color(v, "stroke") } else { Some(c([200, 0, 0])) };
            let fill = opt_color(v, "fill");
            if kind == "rectangle" { Mark::Rectangle { rect: r, stroke, fill, width } } else { Mark::Ellipse { rect: r, stroke, fill, width } }
        }
        "textBox" => Mark::TextBox {
            rect: rect(v, "rect")?,
            text: text(v, "text"),
            size,
            color: c([0, 0, 0]),
            font,
            bold: p::bool(v, "bold").unwrap_or(false),
            italic: p::bool(v, "italic").unwrap_or(false),
            fit: p::bool(v, "fit").unwrap_or(false),
            fill: opt_color(v, "fill"),
            border: opt_color(v, "border"),
        },
        "note" => Mark::Note {
            at: v
                .get("at")
                .and_then(point)
                .or_else(|| v.get("rect").and_then(rect_of).map(|r| [r.x, r.y]))
                .ok_or_else(|| CmdError::Params("`at` ([x, y]) is required".into()))?,
            text: text(v, "text"),
            color: c([255, 214, 0]),
        },
        "stamp" => Mark::Stamp {
            rect: rect(v, "rect")?,
            text: Some(text(v, "text")).filter(|t| !t.is_empty()).unwrap_or_else(|| "APPROVED".into()),
            color: c([0, 130, 60]),
        },
        "image" | "signature" => Mark::Image { rect: rect(v, "rect")?, data: file_bytes(v)? },
        "whiteout" => Mark::Whiteout { rect: rect(v, "rect")? },
        "editText" => {
            let r = rect(v, "rect")?;
            Mark::EditText {
                rect: r,
                base: p::f32(v, "base").unwrap_or(r.y + r.h * 0.8),
                text: text(v, "text"),
                size,
                color: c([0, 0, 0]),
                font,
                bold: p::bool(v, "bold").unwrap_or(false),
                italic: p::bool(v, "italic").unwrap_or(false),
                bg: opt_color(v, "bg"),
            }
        }
        "redact" => Mark::Redact { rects: rects(v)? },
        other => return Err(CmdError::Params(format!("unknown `kind` \"{other}\""))),
    })
}

/// Change a mark's properties from `pdf.updateMark` parameters.
fn update_mark(mark: &mut Mark, v: &Value) {
    if let Some(r) = v.get("rect").and_then(rect_of) {
        *mark = mark.resized(r);
    }
    if let Some(d) = v.get("move").and_then(point) {
        *mark = mark.moved(d[0], d[1]);
    }
    let new_text = p::str(v, "text").map(|t| t.chars().take(20_000).collect::<String>());
    let new_color = v.get("color").map(|_| color(v, "color", [0, 0, 0]));
    let new_size = p::f32(v, "size").map(|s| s.clamp(2.0, 400.0));
    let new_width = p::f32(v, "width").map(|w| w.clamp(0.1, 100.0));
    match mark {
        Mark::Highlight { color, .. } | Mark::Underline { color, .. } | Mark::Strikeout { color, .. } => {
            if let Some(c) = new_color {
                *color = c;
            }
        }
        Mark::Ink { color, width, .. } | Mark::Line { color, width, .. } => {
            if let Some(c) = new_color {
                *color = c;
            }
            if let Some(w) = new_width {
                *width = w;
            }
        }
        Mark::Rectangle { stroke, fill, width, .. } | Mark::Ellipse { stroke, fill, width, .. } => {
            if new_color.is_some() {
                *stroke = new_color;
            }
            if v.get("fill").is_some() {
                *fill = opt_color(v, "fill");
            }
            if let Some(w) = new_width {
                *width = w;
            }
        }
        Mark::TextBox { text, color, size, fit, bold, italic, font, .. } => {
            if let Some(t) = new_text {
                *text = t;
            }
            if let Some(c) = new_color {
                *color = c;
            }
            if let Some(s) = new_size {
                *size = s;
            }
            if let Some(f) = p::bool(v, "fit") {
                *fit = f;
            }
            if let Some(b) = p::bool(v, "bold") {
                *bold = b;
            }
            if let Some(i) = p::bool(v, "italic") {
                *italic = i;
            }
            if let Some(f) = p::str(v, "font") {
                *font = f.chars().take(128).collect();
            }
        }
        Mark::Note { text, color, .. } | Mark::Stamp { text, color, .. } | Mark::EditText { text, color, .. } => {
            if let Some(t) = new_text {
                *text = t;
            }
            if let Some(c) = new_color {
                *color = c;
            }
        }
        Mark::Image { .. } | Mark::Whiteout { .. } | Mark::Redact { .. } => {}
    }
}

fn mark(s: &mut Session, v: &Value) -> CmdResult {
    let pg = page(v, "page")?;
    let mut m = mark_of(v)?;
    if !m.bounds().is_sane() {
        return Err(CmdError::Params("the mark's position isn't a usable number".into()));
    }
    // Edited text is covered in the colour behind it (a dark page stays dark).
    if let Mark::EditText { rect, bg: bg @ None, .. } = &mut m {
        *bg = background(s, pg, *rect);
    }
    s.pdf.change(|ops, next| {
        *next += 1;
        let id = *next;
        ops.push(Op::Mark { id, page: pg, mark: m });
        Ok(json!({"id": id}))
    })
}

/// The colour most of `r` on page `page` is (as the PDF shows it now): the background behind
/// the text there.
fn background(s: &Session, page: usize, r: Rect) -> Option<Color> {
    let pv = view(s).ok()?;
    let img = pv.render(page, 1.0).ok()?;
    let (w, h) = (img.width as i64, img.height as i64);
    let (x0, y0) = ((r.x.floor() as i64).clamp(0, w), (r.y.floor() as i64).clamp(0, h));
    let (x1, y1) = (((r.x + r.w).ceil() as i64).clamp(0, w), ((r.y + r.h).ceil() as i64).clamp(0, h));
    let mut counts: std::collections::HashMap<[u8; 3], usize> = std::collections::HashMap::new();
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * w + x) * 4) as usize;
            if let Some(px) = img.rgba.get(i..i + 3) {
                // Quantized, so a gradient or noise still has a most common colour.
                *counts.entry([px[0] & 0xF8, px[1] & 0xF8, px[2] & 0xF8]).or_default() += 1;
            }
        }
    }
    let ([r, g, b], _) = counts.into_iter().max_by_key(|(c, n)| (*n, *c))?;
    let lift = |c: u8| c.saturating_add(4);
    Some([lift(r), lift(g), lift(b)])
}

fn find_mark<'a>(ops: &'a mut [Op], v: &Value) -> Result<&'a mut Mark, CmdError> {
    let id = p::u64(v, "id").ok_or_else(|| CmdError::Params("`id` (a mark's id, from pdf.mark) is required".into()))?;
    ops.iter_mut()
        .find_map(|o| match o {
            Op::Mark { id: i, mark, .. } if *i == id => Some(mark),
            _ => None,
        })
        .ok_or_else(|| CmdError::Failed(format!("no PDF mark {id}")))
}

fn save(s: &mut Session, v: &Value) -> CmdResult {
    let path = match p::str(v, "path").map(PathBuf::from).or_else(|| s.pdf.path.clone()) {
        Some(p) => p,
        None => {
            s.ui_requests.push(json!({"open": "savePdf"}));
            return Ok(json!({"saved": false}));
        }
    };
    let password = match v.get("password") {
        Some(Value::String(pw)) if !pw.is_empty() => Some(pw.clone()),
        Some(Value::String(_)) | Some(Value::Null) => None,
        _ => s.pdf.password.clone(),
    };
    let opts =
        SaveOptions { password: password.clone(), flatten: p::bool(v, "flatten").unwrap_or(false), compress: p::bool(v, "compress").unwrap_or(true) };
    let bytes = edited(s, &opts).map_err(CmdError::Failed)?;
    let path_str = path.to_string_lossy().into_owned();
    crate::io::write_file(&path_str, &bytes).map_err(CmdError::Failed)?;
    // Save As: the PDF saved is the one Save writes from now on.
    if p::bool(v, "saveAs").unwrap_or(false) {
        s.pdf.path = Some(path.clone());
    }
    // Saving to where Save writes: the edits are saved.
    if s.pdf.path.as_ref() == Some(&path) {
        s.pdf.saved_rev = s.pdf.rev;
        s.pdf.password = password;
        if s.path.is_none() {
            s.dirty = false;
        }
    }
    Ok(json!({"saved": true, "path": path_str, "bytes": bytes.len()}))
}

fn view(s: &Session) -> Result<wordcraft_pdfread::PdfView, CmdError> {
    let bytes = edited(s, &SaveOptions::default()).map_err(CmdError::Failed)?;
    wordcraft_pdfread::PdfView::open(bytes).map_err(|e| CmdError::Failed(e.to_string()))
}

fn position(v: &Value) -> Position {
    p::str(v, "position").and_then(Position::parse).unwrap_or(Position::BottomCenter)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("pdf.mark", "Mark Up", "PDF › Comment", mark)
            .params(r##"{"page": n, "kind": "highlight|underline|strikeout|ink|line|arrow|rectangle|ellipse|textBox|note|stamp|image|signature|whiteout|editText|redact", "rect"?: [x, y, w, h] (points from the page's top left), "rects"?: [[x, y, w, h], …], "strokes"?: [[[x, y], …], …], "from"?: [x, y], "to"?: [x, y], "at"?: [x, y], "text"?: string, "color"?: "#RRGGBB", "fill"?: "#RRGGBB", "stroke"?: "#RRGGBB", "border"?: "#RRGGBB", "width"?: pt, "size"?: pt, "font"?: family, "bold"?: bool, "italic"?: bool, "fit"?: bool (text box: the size fits the box), "base"?: y (editText baseline), "bg"?: "#RRGGBB" (editText: the colour behind; default the page's there), "path"?: picture file, "data"?: base64 picture}"##)
            .pure(),
        CommandSpec::new("pdf.updateMark", "Change Mark", "PDF › Comment", |s, v| {
            s.pdf.change(|ops, _| {
                let m = find_mark(ops, v)?;
                update_mark(m, v);
                Ok(json!({"kind": m.kind()}))
            })
        })
        .params(r##"{"id": n, "rect"?: [x, y, w, h] (move and resize), "move"?: [dx, dy], "text"?: string, "color"?: "#RRGGBB", "fill"?: "#RRGGBB"|null, "size"?: pt, "width"?: pt, "fit"?: bool, "bold"?: bool, "italic"?: bool, "font"?: family}"##)
        .pure(),
        CommandSpec::new("pdf.deleteMark", "Delete Mark", "PDF › Comment", |s, v| {
            let id = p::u64(v, "id").ok_or_else(|| CmdError::Params("`id` is required".into()))?;
            s.pdf.change(|ops, _| {
                let before = ops.len();
                ops.retain(|o| !matches!(o, Op::Mark { id: i, .. } if *i == id));
                if ops.len() == before { Err(CmdError::Failed(format!("no PDF mark {id}"))) } else { Ok(json!({"deleted": id})) }
            })
        })
        .params(r#"{"id": n}"#)
        .pure(),
        CommandSpec::new("pdf.rotate", "Rotate", "PDF › Pages", |s, v| {
            let list = pages(v)?;
            let by = v.get("by").and_then(Value::as_i64).unwrap_or(90).clamp(-36_000, 36_000) as i32;
            s.pdf.change(|ops, _| {
                ops.push(Op::Rotate { pages: list, by });
                Ok(json!({}))
            })
        })
        .params(r#"{"pages"?: [n, …], "page"?: n, "by"?: degrees (90, -90, 180)}"#)
        .pure(),
        CommandSpec::new("pdf.deletePages", "Delete Page", "PDF › Pages", |s, v| {
            let list = pages(v)?;
            s.pdf.change(|ops, _| {
                ops.push(Op::Delete { pages: list });
                Ok(json!({}))
            })
        })
        .params(r#"{"pages"?: [n, …], "page"?: n}"#)
        .pure(),
        CommandSpec::new("pdf.movePage", "Move Page", "PDF › Pages", |s, v| {
            let from = page(v, "page")?;
            let to = page(v, "to")?;
            s.pdf.change(|ops, _| {
                ops.push(Op::Move { from, to });
                Ok(json!({}))
            })
        })
        .params(r#"{"page": n, "to": n (where it ends up, from 1)}"#)
        .pure(),
        CommandSpec::new("pdf.duplicatePage", "Duplicate Page", "PDF › Pages", |s, v| {
            let pg = page(v, "page")?;
            s.pdf.change(|ops, _| {
                ops.push(Op::Duplicate { page: pg });
                Ok(json!({}))
            })
        })
        .params(r#"{"page": n (from 1)}"#)
        .pure(),
        CommandSpec::new("pdf.insertBlank", "Insert Blank Page", "PDF › Pages", |s, v| {
            let at = p::u64(v, "at").map_or(0, |n| n.saturating_sub(1) as usize).min(wordcraft_pdfedit::MAX_PAGES);
            let width = p::f32(v, "width").unwrap_or(612.0).clamp(36.0, 14_400.0);
            let height = p::f32(v, "height").unwrap_or(792.0).clamp(36.0, 14_400.0);
            s.pdf.change(|ops, _| {
                ops.push(Op::InsertBlank { at, width, height });
                Ok(json!({}))
            })
        })
        .params(r#"{"at"?: n (the new page's number; default 1), "width"?: pt, "height"?: pt}"#)
        .pure(),
        CommandSpec::new("pdf.insertPdf", "Insert Pages from PDF", "PDF › Pages", |s, v| {
            let at = p::u64(v, "at").map_or(usize::MAX, |n| n.saturating_sub(1) as usize);
            let data = file_bytes(v)?;
            if !wordcraft_pdfread::is_pdf(&data) {
                return Err(CmdError::Failed("that file isn't a PDF".into()));
            }
            s.pdf.change(|ops, _| {
                ops.push(Op::InsertPdf { at, data });
                Ok(json!({}))
            })
        })
        .params(r#"{"path"?: string, "data"?: base64, "at"?: n (where its first page goes; default the end)}"#)
        .pure(),
        CommandSpec::new("pdf.crop", "Crop", "PDF › Pages", |s, v| {
            let pg = page(v, "page")?;
            let r = rect(v, "rect")?;
            s.pdf.change(|ops, _| {
                ops.push(Op::Crop { page: pg, rect: r });
                Ok(json!({}))
            })
        })
        .params(r#"{"page": n, "rect": [x, y, w, h] (what stays)}"#)
        .pure(),
        CommandSpec::new("pdf.transform", "Scale", "PDF › Pages", |s, v| {
            let list = pages(v)?;
            let (flip_h, flip_v) = (p::bool(v, "flipH").unwrap_or(false), p::bool(v, "flipV").unwrap_or(false));
            let scale = p::f32(v, "scale").unwrap_or(1.0).clamp(0.05, 20.0);
            s.pdf.change(|ops, _| {
                ops.push(Op::Transform { pages: list, flip_h, flip_v, scale });
                Ok(json!({}))
            })
        })
        .params(r#"{"pages"?: [n, …], "page"?: n, "flipH"?: bool, "flipV"?: bool, "scale"?: factor}"#)
        .pure(),
        CommandSpec::new("pdf.watermark", "Watermark", "PDF › Document", |s, v| {
            let t = text(v, "text");
            if t.trim().is_empty() {
                return Err(CmdError::Params("`text` is required".into()));
            }
            let op = Op::Watermark {
                text: t,
                size: p::f32(v, "size").unwrap_or(72.0).clamp(4.0, 400.0),
                color: color(v, "color", [160, 160, 160]),
                opacity: p::f32(v, "opacity").unwrap_or(0.3).clamp(0.02, 1.0),
                angle: p::f32(v, "angle").unwrap_or(45.0).clamp(-360.0, 360.0),
                pages: opt_pages(v),
            };
            s.pdf.change(|ops, _| {
                ops.push(op);
                Ok(json!({}))
            })
        })
        .params(r##"{"text": string, "size"?: pt, "color"?: "#RRGGBB", "opacity"?: 0–1, "angle"?: degrees, "pages"?: [n, …] (default all)}"##)
        .pure(),
        CommandSpec::new("pdf.pageText", "Header & Footer", "PDF › Document", |s, v| {
            let t = p::str(v, "text").unwrap_or("{n}").chars().take(500).collect::<String>();
            let op = Op::PageText {
                text: t,
                position: position(v),
                size: p::f32(v, "size").unwrap_or(10.0).clamp(4.0, 72.0),
                color: color(v, "color", [0, 0, 0]),
                start: v.get("start").and_then(Value::as_i64).unwrap_or(1).clamp(-1_000_000, 1_000_000),
                pages: opt_pages(v),
            };
            s.pdf.change(|ops, _| {
                ops.push(op);
                Ok(json!({}))
            })
        })
        .params(r##"{"text"?: string ({n} the page number, {N} the page count; default "{n}"), "position"?: "topLeft|topCenter|topRight|bottomLeft|bottomCenter|bottomRight", "size"?: pt, "color"?: "#RRGGBB", "start"?: n (the first page's number), "pages"?: [n, …]}"##)
        .pure(),
        CommandSpec::new("pdf.metadata", "Properties", "PDF › Document", |s, v| {
            let f = |k: &str| p::str(v, k).map(|t| t.chars().take(2000).collect::<String>());
            let op = Op::Metadata { title: f("title"), author: f("author"), subject: f("subject"), keywords: f("keywords") };
            s.pdf.change(|ops, _| {
                ops.push(op);
                Ok(json!({}))
            })
        })
        .params(r#"{"title"?: string, "author"?: string, "subject"?: string, "keywords"?: string}"#)
        .pure(),
        CommandSpec::new("pdf.fill", "Fill", "PDF › Document", |s, v| {
            let field = p::req_str(v, "field")?.to_string();
            let value = match v.get("value") {
                Some(Value::String(t)) => t.chars().take(10_000).collect(),
                Some(Value::Bool(true)) => "Yes".to_string(),
                Some(Value::Bool(false)) => "Off".to_string(),
                _ => return Err(CmdError::Params("`value` (string; true/false for a check box) is required".into())),
            };
            s.pdf.change(|ops, _| {
                // A later value for the same field replaces the earlier one.
                ops.retain(|o| !matches!(o, Op::Fill { field: f, .. } if *f == field));
                ops.push(Op::Fill { field, value });
                Ok(json!({}))
            })
        })
        .params(r#"{"field": string (its full name, from pdf.fields), "value": string | bool}"#)
        .pure(),
        CommandSpec::new("pdf.undo", "Undo", "PDF", |s, _| {
            let Some(prev) = s.pdf.undo.pop() else { return Err(CmdError::Disabled("nothing to undo".into())) };
            let now = std::mem::replace(&mut s.pdf.ops, prev);
            s.pdf.redo.push(now);
            s.pdf.rev += 1;
            Ok(json!({"edits": s.pdf.ops.len()}))
        })
        .pure(),
        CommandSpec::new("pdf.redo", "Redo", "PDF", |s, _| {
            let Some(next) = s.pdf.redo.pop() else { return Err(CmdError::Disabled("nothing to redo".into())) };
            let now = std::mem::replace(&mut s.pdf.ops, next);
            s.pdf.undo.push(now);
            s.pdf.rev += 1;
            Ok(json!({"edits": s.pdf.ops.len()}))
        })
        .pure(),
        CommandSpec::new("pdf.save", "Save PDF", "PDF", save)
            .params(r#"{"path"?: string (default: the PDF the document was opened from), "password"?: string (encrypt with it; "" saves without), "flatten"?: bool (every page a picture), "compress"?: bool (default true), "saveAs"?: bool (Save writes there from now on)}"#)
            .pure(),
        CommandSpec::new("pdf.extract", "Extract Pages", "PDF › Pages", |s, v| {
            let path = p::req_str(v, "path")?.to_string();
            let list = pages(v)?;
            let bytes = edited(s, &SaveOptions::default()).map_err(CmdError::Failed)?;
            let out = wordcraft_pdfedit::extract(&bytes, &list).map_err(|e| CmdError::Failed(e.to_string()))?;
            crate::io::write_file(&path, &out).map_err(CmdError::Failed)?;
            Ok(json!({"path": path, "pages": list.len()}))
        })
        .params(r#"{"path": string, "pages"?: [n, …], "page"?: n}"#)
        .pure(),
        CommandSpec::new("pdf.split", "Split", "PDF › Pages", |s, v| {
            let dir = PathBuf::from(p::req_str(v, "dir")?);
            let every = p::u64(v, "every").unwrap_or(1).clamp(1, wordcraft_pdfedit::MAX_PAGES as u64) as usize;
            let stem = p::str(v, "name").map(str::to_string).or_else(|| s.pdf.path.as_ref().and_then(|p| p.file_stem()).map(|n| n.to_string_lossy().into_owned())).unwrap_or_else(|| "part".into());
            let bytes = edited(s, &SaveOptions::default()).map_err(CmdError::Failed)?;
            let parts = wordcraft_pdfedit::split(&bytes, every).map_err(|e| CmdError::Failed(e.to_string()))?;
            let mut written = Vec::new();
            for (i, part) in parts.iter().enumerate() {
                let path = dir.join(format!("{stem}-{}.pdf", i + 1)).to_string_lossy().into_owned();
                crate::io::write_file(&path, part).map_err(CmdError::Failed)?;
                written.push(path);
            }
            Ok(json!({"files": written}))
        })
        .params(r#"{"dir": string (folder for the parts), "every"?: n (pages per part; default 1), "name"?: string (file name stem)}"#)
        .pure(),
        CommandSpec::new("pdf.exportImages", "Export Pages as Images", "PDF › Export", |s, v| {
            let dir = PathBuf::from(p::req_str(v, "dir")?);
            let jpeg = p::str(v, "format") == Some("jpeg");
            let dpi = p::f32(v, "dpi").unwrap_or(150.0).clamp(18.0, 600.0);
            let pv = view(s)?;
            let list: Vec<usize> = opt_pages(v).unwrap_or_else(|| (0..pv.page_count()).collect());
            let stem = s.pdf.path.as_ref().and_then(|p| p.file_stem()).map_or_else(|| "page".to_string(), |n| n.to_string_lossy().into_owned());
            let mut written = Vec::new();
            for pg in list.into_iter().filter(|pg| *pg < pv.page_count()).take(2000) {
                let img = pv.render(pg, dpi / 72.0).map_err(|e| CmdError::Failed(e.to_string()))?;
                let Some(rgba) = image::RgbaImage::from_raw(img.width, img.height, img.rgba) else { continue };
                let mut out = std::io::Cursor::new(Vec::new());
                let r = if jpeg {
                    image::DynamicImage::ImageRgba8(rgba).to_rgb8().write_to(&mut out, image::ImageFormat::Jpeg)
                } else {
                    rgba.write_to(&mut out, image::ImageFormat::Png)
                };
                r.map_err(|e| CmdError::Failed(e.to_string()))?;
                let path = dir.join(format!("{stem}-{}.{}", pg + 1, if jpeg { "jpg" } else { "png" })).to_string_lossy().into_owned();
                crate::io::write_file(&path, out.get_ref()).map_err(CmdError::Failed)?;
                written.push(path);
            }
            Ok(json!({"files": written}))
        })
        .params(r#"{"dir": string, "pages"?: [n, …] (default all), "format"?: "png|jpeg", "dpi"?: n}"#)
        .pure(),
        CommandSpec::new("pdf.fields", "Form Fields", "PDF › Document", |s, _| {
            let bytes = edited(s, &SaveOptions::default()).map_err(CmdError::Failed)?;
            let f = wordcraft_pdfedit::fields(&bytes).map_err(|e| CmdError::Failed(e.to_string()))?;
            let list: Vec<Value> = f
                .iter()
                .map(|f| {
                    json!({"name": f.name, "kind": format!("{:?}", f.kind), "value": f.value, "options": f.options, "readOnly": f.read_only,
                        "widgets": f.widgets.iter().map(|(pg, r, on)| json!({"page": pg + 1, "rect": [r.x, r.y, r.w, r.h], "on": on})).collect::<Vec<_>>()})
                })
                .collect();
            Ok(json!({"fields": list}))
        })
        .pure(),
        CommandSpec::new("pdf.text", "Text", "PDF", |s, v| {
            let pv = view(s)?;
            let list: Vec<usize> = opt_pages(v).unwrap_or_else(|| (0..pv.page_count()).collect());
            let text: Vec<String> = list.into_iter().filter(|pg| *pg < pv.page_count()).map(|pg| pv.page_text(pg)).collect();
            Ok(json!({"text": text.join("\n\u{c}\n")}))
        })
        .params(r#"{"pages"?: [n, …]}"#)
        .pure(),
        CommandSpec::new("pdf.search", "Find in PDF", "PDF", |s, v| {
            let q = p::req_str(v, "text")?;
            let pv = view(s)?;
            let hits: Vec<Value> = pv.search(q, 5000).into_iter().map(|(pg, r)| json!({"page": pg + 1, "rect": r})).collect();
            Ok(json!({"hits": hits}))
        })
        .params(r#"{"text": string}"#)
        .pure(),
        CommandSpec::new("pdf.edits", "PDF Edits", "PDF", |s, _| {
            let list: Vec<Value> = s
                .pdf
                .ops
                .iter()
                .map(|o| match o {
                    Op::Mark { id, page, mark } => {
                        let b = mark.bounds();
                        json!({"id": id, "page": page + 1, "kind": mark.kind(), "rect": [b.x, b.y, b.w, b.h], "text": mark.text(), "describe": o.describe()})
                    }
                    o => json!({"describe": o.describe()}),
                })
                .collect();
            Ok(json!({"edits": list, "canUndo": s.pdf.can_undo(), "canRedo": s.pdf.can_redo(), "dirty": s.pdf.dirty(), "path": s.pdf.path}))
        })
        .pure(),
    ]
}

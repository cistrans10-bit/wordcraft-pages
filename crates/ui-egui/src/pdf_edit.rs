//! Editing the PDF in the PDF view: the tools (select, edit text, text box, highlight, underline,
//! strikethrough, pen, line, arrow, rectangle, oval, note, stamp, picture, signature, whiteout,
//! redact, crop), the Pages panel (turn, delete, duplicate, move, insert, extract), forms, find,
//! and the document's stamps, properties and saving.
//!
//! Everything here is a `pdf.*` command (`wordcraft_engine::cmd::pdf`): the view only turns
//! pointer drags into their parameters and shows the result, which the worker applies over the
//! PDF the moment it is made.

use crate::WordApp;
use crate::file_dialogs::{AfterPick, FileDialogRequest};
use crate::pdf_view::{Job, PdfMode};
use crate::theme::Tokens;
use egui::{Align2, Color32, CursorIcon, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use wordcraft_engine::pdfedit::{FieldKind, Mark, Op, Rect as PRect};
use wordcraft_engine::pdfread::TextLine;

/// The editing tools.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Tool {
    #[default]
    Select,
    EditText,
    TextBox,
    Highlight,
    Underline,
    Strikeout,
    Pen,
    Line,
    Arrow,
    Rectangle,
    Ellipse,
    Note,
    Stamp,
    Image,
    Signature,
    Whiteout,
    Redact,
    Crop,
}

impl Tool {
    pub const ALL: [Tool; 18] = [
        Tool::Select,
        Tool::EditText,
        Tool::TextBox,
        Tool::Highlight,
        Tool::Underline,
        Tool::Strikeout,
        Tool::Pen,
        Tool::Line,
        Tool::Arrow,
        Tool::Rectangle,
        Tool::Ellipse,
        Tool::Note,
        Tool::Stamp,
        Tool::Image,
        Tool::Signature,
        Tool::Whiteout,
        Tool::Redact,
        Tool::Crop,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Tool::Select => "select",
            Tool::EditText => "editText",
            Tool::TextBox => "textBox",
            Tool::Highlight => "highlight",
            Tool::Underline => "underline",
            Tool::Strikeout => "strikeout",
            Tool::Pen => "ink",
            Tool::Line => "line",
            Tool::Arrow => "arrow",
            Tool::Rectangle => "rectangle",
            Tool::Ellipse => "ellipse",
            Tool::Note => "note",
            Tool::Stamp => "stamp",
            Tool::Image => "image",
            Tool::Signature => "signature",
            Tool::Whiteout => "whiteout",
            Tool::Redact => "redact",
            Tool::Crop => "crop",
        }
    }

    pub fn parse(s: &str) -> Option<Tool> {
        Tool::ALL.into_iter().find(|t| t.id() == s || (s == "pen" && *t == Tool::Pen))
    }

    fn label(self) -> &'static str {
        match self {
            Tool::Select => tl!("Select"),
            Tool::EditText => tl!("Edit Text"),
            Tool::TextBox => tl!("Text Box"),
            Tool::Highlight => tl!("Highlight"),
            Tool::Underline => tl!("Underline"),
            Tool::Strikeout => tl!("Strikethrough"),
            Tool::Pen => tl!("Pen"),
            Tool::Line => tl!("Line"),
            Tool::Arrow => tl!("Arrow"),
            Tool::Rectangle => tl!("Rectangle"),
            Tool::Ellipse => tl!("Oval"),
            Tool::Note => tl!("Note"),
            Tool::Stamp => tl!("Stamp"),
            Tool::Image => tl!("Picture"),
            Tool::Signature => tl!("Signature"),
            Tool::Whiteout => tl!("Whiteout"),
            Tool::Redact => tl!("Redact"),
            Tool::Crop => tl!("Crop"),
        }
    }

    fn tip(self) -> &'static str {
        match self {
            Tool::Select => tl!("Select, move and resize marks; drag over text to select it; fill in form fields"),
            Tool::EditText => tl!("Click a line of the PDF's text to change it, in a matching font"),
            Tool::TextBox => tl!("Drag a box and type; the text fits the box as you resize it"),
            Tool::Highlight | Tool::Underline | Tool::Strikeout => tl!("Drag over text"),
            Tool::Pen => tl!("Draw freehand"),
            Tool::Line | Tool::Arrow | Tool::Rectangle | Tool::Ellipse => tl!("Drag on the page"),
            Tool::Note => tl!("Click to add a note"),
            Tool::Stamp => tl!("Click or drag to stamp"),
            Tool::Image => tl!("Click or drag, then pick a picture"),
            Tool::Signature => tl!("Draw your signature once, then click or drag to sign"),
            Tool::Whiteout => tl!("Cover part of the page with white"),
            Tool::Redact => tl!("Black out part of the page for good: the text, pictures and drawing under it are removed when the page is saved"),
            Tool::Crop => tl!("Drag the part of the page to keep"),
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Tool::Select => "select",
            Tool::EditText => "addText",
            Tool::TextBox => "textBox",
            Tool::Highlight => "highlight",
            Tool::Underline => "underline",
            Tool::Strikeout => "strike",
            Tool::Pen => "pen",
            Tool::Line => "shape.line",
            Tool::Arrow => "shape.arrow",
            Tool::Rectangle => "shape.rectangle",
            Tool::Ellipse => "shape.ellipse",
            Tool::Note => "comment",
            Tool::Stamp => "check",
            Tool::Image => "picture",
            Tool::Signature => "signature",
            Tool::Whiteout => "eraser",
            Tool::Redact => "redact",
            Tool::Crop => "crop",
        }
    }

    fn marks_text(self) -> bool {
        matches!(self, Tool::Highlight | Tool::Underline | Tool::Strikeout)
    }

    fn default_color(self) -> [u8; 3] {
        match self {
            Tool::Highlight => [255, 230, 0],
            Tool::Underline => [0, 90, 200],
            Tool::Strikeout | Tool::Line | Tool::Arrow | Tool::Rectangle | Tool::Ellipse => [200, 0, 0],
            Tool::Pen | Tool::Signature => [20, 20, 120],
            Tool::Note => [255, 214, 0],
            Tool::Stamp => [0, 130, 60],
            _ => [0, 0, 0],
        }
    }

    fn has_color(self) -> bool {
        !matches!(self, Tool::Select | Tool::EditText | Tool::Image | Tool::Whiteout | Tool::Redact | Tool::Crop)
    }

    fn has_width(self) -> bool {
        matches!(self, Tool::Pen | Tool::Line | Tool::Arrow | Tool::Rectangle | Tool::Ellipse | Tool::Signature)
    }
}

/// Swatches offered for colours.
const SWATCHES: [[u8; 3]; 10] = [
    [255, 230, 0],
    [120, 220, 80],
    [80, 200, 255],
    [255, 120, 200],
    [200, 0, 0],
    [240, 130, 0],
    [0, 90, 200],
    [120, 40, 170],
    [0, 130, 60],
    [0, 0, 0],
];

/// Stamps offered (any text can be typed).
const STAMPS: [&str; 6] = ["APPROVED", "DRAFT", "CONFIDENTIAL", "FINAL", "REVIEWED", "NOT APPROVED"];

/// Font families offered for text boxes.
const FONTS: [&str; 7] = ["Arial", "Times New Roman", "Courier New", "Georgia", "Verdana", "Calibri", "Cambria"];

/// Where page text goes, as `pdf.pageText` names it.
const POSITIONS: [&str; 6] = ["topLeft", "topCenter", "topRight", "bottomLeft", "bottomCenter", "bottomRight"];

fn position_label(i: usize) -> &'static str {
    match i {
        0 => tl!("Top Left"),
        1 => tl!("Top Center"),
        2 => tl!("Top Right"),
        3 => tl!("Bottom Left"),
        5 => tl!("Bottom Right"),
        _ => tl!("Bottom Center"),
    }
}

#[derive(Default)]
enum Drag {
    #[default]
    None,
    Create {
        page: usize,
        from: [f32; 2],
        to: [f32; 2],
        points: Vec<[f32; 2]>,
    },
    Move {
        id: u64,
        from: [f32; 2],
        to: [f32; 2],
    },
    Resize {
        id: u64,
        handle: usize,
        orig: PRect,
        to: [f32; 2],
    },
    Text {
        page: usize,
        from: [f32; 2],
        to: [f32; 2],
    },
}

/// What an inline editor's text becomes.
#[derive(Clone)]
enum Target {
    NewTextBox,
    NewNote { at: [f32; 2] },
    Mark(u64),
    Line(Box<TextLine>),
    Field(String),
}

struct Inline {
    page: usize,
    rect: PRect,
    text: String,
    size: f32,
    target: Target,
    multiline: bool,
    focused: bool,
}

/// Text selected on a page: its boxes and words.
#[derive(Clone)]
pub struct TextSel {
    pub page: usize,
    pub rects: Vec<PRect>,
    pub text: String,
}

/// Document dialogs.
pub enum Dialog {
    Watermark { text: String, size: f32, opacity: f32, angle: f32, color: [u8; 3] },
    PageText { text: String, position: usize, size: f32, start: i64 },
    Properties { title: String, author: String, subject: String, keywords: String },
    Save { password: String, confirm: String, flatten: bool, compress: bool },
    Split { every: u32 },
    Images { dpi: f32, jpeg: bool },
    Signature { strokes: Vec<Vec<[f32; 2]>>, place: Option<(usize, PRect)> },
}

/// What to do with a file picked for the PDF.
pub(crate) enum PdfPick {
    Save { params: Value },
    InsertPdf { at: usize },
    Image { page: usize, rect: PRect },
    Extract { pages: Vec<usize> },
    Split { every: u32 },
    Images { dpi: f32, jpeg: bool },
}

/// The editing state of the PDF view.
pub struct Editor {
    pub tool: Tool,
    colors: HashMap<Tool, [u8; 3]>,
    pub width: f32,
    pub size: f32,
    pub font: String,
    pub bold: bool,
    pub italic: bool,
    pub fit: bool,
    pub fill: Option<[u8; 3]>,
    pub stamp: String,
    /// The selected mark.
    pub selected: Option<u64>,
    drag: Drag,
    edit: Option<Inline>,
    /// Each page's text lines (version they are of).
    lines: HashMap<usize, (u64, Vec<TextLine>)>,
    lines_pending: HashSet<(u64, usize)>,
    pub text_sel: Option<TextSel>,
    pub search: String,
    hits: Vec<(usize, [f32; 4])>,
    hits_query: String,
    hit: usize,
    focus_search: bool,
    pub pages_panel: bool,
    pub page_sel: Option<usize>,
    pub dialog: Option<Dialog>,
    /// The signature, drawn once: strokes in a 3 × 1 box.
    signature: Vec<Vec<[f32; 2]>>,
    /// A choice field's options open: field, options, page, the field's box.
    choice: Option<(String, Vec<String>, usize, PRect)>,
    pub status: Option<String>,
    /// Keys go to the PDF (the pointer last pressed in it).
    pub active: bool,
    /// When a press elsewhere last finished the text being typed (its click does nothing more).
    committed_at: f64,
}

impl Default for Editor {
    fn default() -> Self {
        Editor {
            tool: Tool::Select,
            colors: HashMap::new(),
            width: 2.0,
            size: 14.0,
            font: "Arial".into(),
            bold: false,
            italic: false,
            fit: true,
            fill: None,
            stamp: STAMPS[0].into(),
            selected: None,
            drag: Drag::None,
            edit: None,
            lines: HashMap::new(),
            lines_pending: HashSet::new(),
            text_sel: None,
            search: String::new(),
            hits: Vec::new(),
            hits_query: String::new(),
            hit: 0,
            focus_search: false,
            pages_panel: true,
            page_sel: None,
            dialog: None,
            signature: Vec::new(),
            choice: None,
            status: None,
            active: false,
            committed_at: f64::NEG_INFINITY,
        }
    }
}

impl Editor {
    pub fn color(&self) -> [u8; 3] {
        self.colors.get(&self.tool).copied().unwrap_or_else(|| self.tool.default_color())
    }

    fn set_color(&mut self, c: [u8; 3]) {
        self.colors.insert(self.tool, c);
    }

    /// A new PDF is on screen: text lines and hits of the old one are stale.
    pub(crate) fn new_version(&mut self, version: u64) {
        self.lines_pending.retain(|p| p.0 >= version);
        if !self.hits_query.is_empty() {
            self.hits_query.clear();
            self.hits.clear();
        }
    }

    pub(crate) fn got_lines(&mut self, version: u64, page: usize, lines: Vec<TextLine>) {
        self.lines_pending.remove(&(version, page));
        if self.lines.get(&page).is_none_or(|l| l.0 <= version) {
            self.lines.insert(page, (version, lines));
        }
    }

    pub(crate) fn got_hits(&mut self, _version: u64, query: String, hits: Vec<(usize, [f32; 4])>) {
        if query == self.search.trim() {
            self.hits = hits;
            self.hits_query = query;
            self.hit = 0;
        }
    }

    fn page_lines(&self, page: usize) -> &[TextLine] {
        self.lines.get(&page).map_or(&[], |l| l.1.as_slice())
    }
}

fn c32(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

fn to_pt(p: Pos2, r: Rect, scale: f32) -> [f32; 2] {
    [(p.x - r.min.x) / scale, (p.y - r.min.y) / scale]
}

fn to_px(q: [f32; 2], r: Rect, scale: f32) -> Pos2 {
    pos2(r.min.x + q[0] * scale, r.min.y + q[1] * scale)
}

fn rect_px(b: PRect, r: Rect, scale: f32) -> Rect {
    Rect::from_min_size(to_px([b.x, b.y], r, scale), vec2(b.w * scale, b.h * scale))
}

fn span(a: [f32; 2], b: [f32; 2]) -> PRect {
    PRect::new(a[0], a[1], b[0] - a[0], b[1] - a[1])
}

fn arr(r: PRect) -> Value {
    json!([r.x, r.y, r.w, r.h])
}

/// Run a PDF command; its error goes to the status line. The result when it worked.
fn run(app: &mut WordApp, id: &str, params: Value) -> Option<Value> {
    match app.run(id, params) {
        Ok(v) => Some(v),
        Err(e) => {
            app.status(e);
            None
        }
    }
}

fn add_mark(app: &mut WordApp, page: usize, kind: &str, mut params: Value) -> Option<u64> {
    params["page"] = json!(page + 1);
    params["kind"] = json!(kind);
    run(app, "pdf.mark", params).and_then(|v| v.get("id").and_then(Value::as_u64))
}

/// The mark on top at `at` on `page`.
fn mark_at(app: &WordApp, page: usize, at: [f32; 2]) -> Option<u64> {
    let shown = app.pdf.shown.as_ref()?;
    app.session.pdf.ops.iter().rev().find_map(|o| match o {
        Op::Mark { id, mark, .. } if shown.marks.get(id) == Some(&page) => {
            let b = mark.bounds();
            let pad = 3.0;
            PRect::new(b.x - pad, b.y - pad, b.w + 2.0 * pad, b.h + 2.0 * pad).contains(at).then_some(*id)
        }
        _ => None,
    })
}

fn mark_of(app: &WordApp, id: u64) -> Option<(usize, Mark)> {
    let page = *app.pdf.shown.as_ref()?.marks.get(&id)?;
    app.session.pdf.mark(id).map(|(_, m)| (page, m.clone()))
}

fn resizable(m: &Mark) -> bool {
    !matches!(m, Mark::Note { .. } | Mark::Highlight { .. } | Mark::Underline { .. } | Mark::Strikeout { .. } | Mark::Redact { .. })
}

/// The handles of a box: corners and sides, clockwise from the top left.
fn handles(b: PRect) -> [[f32; 2]; 8] {
    let (x0, y0, x1, y1, xm, ym) = (b.x, b.y, b.x + b.w, b.y + b.h, b.x + b.w / 2.0, b.y + b.h / 2.0);
    [[x0, y0], [xm, y0], [x1, y0], [x1, ym], [x1, y1], [xm, y1], [x0, y1], [x0, ym]]
}

fn resized(orig: PRect, handle: usize, to: [f32; 2]) -> PRect {
    let (mut x0, mut y0, mut x1, mut y1) = (orig.x, orig.y, orig.x + orig.w, orig.y + orig.h);
    match handle {
        0 => (x0, y0) = (to[0], to[1]),
        1 => y0 = to[1],
        2 => (x1, y0) = (to[0], to[1]),
        3 => x1 = to[0],
        4 => (x1, y1) = (to[0], to[1]),
        5 => y1 = to[1],
        6 => (x0, y1) = (to[0], to[1]),
        _ => x0 = to[0],
    }
    PRect::new(x0, y0, x1 - x0, y1 - y0)
}

/// The words between `a` and `b` (in reading order): their boxes, line by line, and their text.
fn text_between(lines: &[TextLine], a: [f32; 2], b: [f32; 2]) -> (Vec<PRect>, String) {
    let (top, bottom) = if (a[1], a[0]) <= (b[1], b[0]) { (a, b) } else { (b, a) };
    let (x_min, x_max) = (a[0].min(b[0]), a[0].max(b[0]));
    let mut picked: Vec<&TextLine> = lines
        .iter()
        .filter(|l| l.y + l.h >= top[1] && l.y <= bottom[1])
        .filter(|l| l.x <= x_max.max(l.x + l.w * 0.0) && l.x + l.w >= x_min.min(l.x + l.w) || (bottom[1] - top[1]) > l.h)
        .collect();
    picked.sort_by(|p, q| (p.y, p.x).partial_cmp(&(q.y, q.x)).unwrap_or(std::cmp::Ordering::Equal));
    let one_row = picked.iter().all(|l| l.y <= top[1] + 0.5 && l.y + l.h >= bottom[1] - 0.5);
    let mut rects = Vec::new();
    let mut text = String::new();
    let n = picked.len();
    for (i, l) in picked.into_iter().enumerate() {
        let words: Vec<&str> = l.text.split_whitespace().collect();
        let spans = &l.word_spans;
        let usable = spans.len() == words.len() && !spans.is_empty();
        let keep = |s: &(f32, f32)| -> bool {
            if one_row {
                s.1 >= x_min && s.0 <= x_max
            } else if i == 0 {
                s.1 >= top[0]
            } else if i + 1 == n {
                s.0 <= bottom[0]
            } else {
                true
            }
        };
        let (x0, x1, words_kept) = if usable {
            let kept: Vec<usize> = (0..spans.len()).filter(|&k| spans.get(k).is_some_and(keep)).collect();
            let (Some(first), Some(last)) = (kept.first(), kept.last()) else { continue };
            let (Some(s0), Some(s1)) = (spans.get(*first), spans.get(*last)) else { continue };
            (s0.0, s1.1, kept.iter().filter_map(|k| words.get(*k).copied()).collect::<Vec<_>>().join(" "))
        } else {
            (l.x, l.x + l.w, l.text.clone())
        };
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&words_kept);
        rects.push(PRect::new(x0, l.y, x1 - x0, l.h));
    }
    (rects, text)
}

/// The text line at `at`.
fn line_at(lines: &[TextLine], at: [f32; 2]) -> Option<TextLine> {
    lines.iter().find(|l| PRect::new(l.x - 2.0, l.y - 1.0, l.w + 4.0, l.h + 2.0).contains(at)).cloned()
}

/// Ask the worker for the text lines of `pages` it hasn't given yet.
fn want_lines(app: &mut WordApp, pages: &[usize]) {
    let Some(version) = app.pdf.shown_version() else { return };
    for &p in pages {
        let have = app.pdf.ed.lines.get(&p).is_some_and(|l| l.0 == version);
        if !have
            && app.pdf.ed.lines_pending.insert((version, p))
            && let Some(w) = app.pdf.worker.as_mut()
        {
            w.send(Job::Lines { version, page: p });
        }
    }
}

/// Marking up, selecting and filling in over the pages on screen (`pages`: index, screen rect).
pub fn overlay(app: &mut WordApp, ui: &mut Ui, pages: &[(usize, Rect)], scale: f32) {
    if app.pdf.shown.is_none() {
        return;
    }
    let tool = app.pdf.ed.tool;
    if matches!(tool, Tool::Select | Tool::EditText) || tool.marks_text() {
        let list: Vec<usize> = pages.iter().map(|p| p.0).collect();
        want_lines(app, &list);
    }
    let painter = ui.painter().clone();
    for &(page, r) in pages {
        let resp = ui.interact(r, ui.id().with(("pdf-page", page)), Sense::click_and_drag());
        pointer(app, ui, &resp, page, r, scale);
        draw(app, &painter, page, r, scale);
    }
    inline_editor(app, ui, pages, scale);
    selection_bar(app, ui, pages, scale);
    choice_menu(app, ui, pages, scale);
}

/// What the pointer does on page `page` (on screen at `r`).
fn pointer(app: &mut WordApp, ui: &Ui, resp: &egui::Response, page: usize, r: Rect, scale: f32) {
    let tool = app.pdf.ed.tool;
    let at_of = |p: Pos2| to_pt(p, r, scale);
    // The cursor says what a press does.
    if let Some(h) = resp.hover_pos() {
        let at = at_of(h);
        let icon = match tool {
            Tool::Select => {
                if let Some((id, b)) = app.pdf.ed.selected.and_then(|id| mark_of(app, id).filter(|m| m.0 == page).map(|m| (id, m.1))) {
                    let _ = id;
                    if resizable(&b) && handles(b.bounds()).iter().any(|q| (q[0] - at[0]).abs() * scale < 6.0 && (q[1] - at[1]).abs() * scale < 6.0) {
                        CursorIcon::Crosshair
                    } else if b.bounds().contains(at) {
                        CursorIcon::Move
                    } else {
                        CursorIcon::Default
                    }
                } else if mark_at(app, page, at).is_some() || field_at(app, page, at).is_some() {
                    CursorIcon::PointingHand
                } else if line_at(app.pdf.ed.page_lines(page), at).is_some() {
                    CursorIcon::Text
                } else {
                    CursorIcon::Default
                }
            }
            Tool::EditText => CursorIcon::Text,
            t if t.marks_text() => CursorIcon::Text,
            _ => CursorIcon::Crosshair,
        };
        ui.ctx().set_cursor_icon(icon);
    }
    // A press on the page finishes the text being typed (and does nothing else).
    let now = ui.input(|i| i.time);
    if (resp.clicked() || resp.drag_started()) && app.pdf.ed.edit.as_ref().is_some_and(|e| e.focused) {
        if let Some(ed) = app.pdf.ed.edit.take() {
            commit(app, ed);
        }
        return;
    }
    // The press that took the focus from the text already finished it.
    if (resp.clicked() || resp.drag_started()) && now - app.pdf.ed.committed_at < 0.8 {
        app.pdf.ed.committed_at = f64::NEG_INFINITY;
        return;
    }
    if resp.clicked() || resp.drag_started() || resp.secondary_clicked() {
        app.pdf.ed.active = true;
        app.pdf.ed.choice = None;
        if let Some((id, _)) = app.canvas.widget_id {
            ui.ctx().memory_mut(|m| m.surrender_focus(id));
        }
    }
    if resp.drag_started() {
        let origin = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()).unwrap_or(r.min);
        let at = at_of(origin);
        let drag = match tool {
            Tool::Select => {
                let sel = app.pdf.ed.selected.and_then(|id| mark_of(app, id).filter(|m| m.0 == page).map(|m| (id, m.1)));
                let handle = sel.as_ref().filter(|(_, m)| resizable(m)).and_then(|(id, m)| {
                    let b = m.bounds();
                    handles(b).iter().position(|q| (q[0] - at[0]).abs() * scale < 6.0 && (q[1] - at[1]).abs() * scale < 6.0).map(|h| (*id, h, b))
                });
                if let Some((id, handle, orig)) = handle {
                    Drag::Resize { id, handle, orig, to: at }
                } else if let Some((id, _)) = sel.filter(|(_, m)| m.bounds().contains(at)) {
                    Drag::Move { id, from: at, to: at }
                } else if let Some(id) = mark_at(app, page, at) {
                    app.pdf.ed.selected = Some(id);
                    Drag::Move { id, from: at, to: at }
                } else {
                    app.pdf.ed.selected = None;
                    app.pdf.ed.text_sel = None;
                    Drag::Text { page, from: at, to: at }
                }
            }
            Tool::EditText | Tool::Note => Drag::None,
            _ => Drag::Create { page, from: at, to: at, points: vec![at] },
        };
        app.pdf.ed.drag = drag;
    }
    if resp.dragged()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let at = at_of(p);
        let page_lines: Vec<TextLine> = app.pdf.ed.page_lines(page).to_vec();
        match &mut app.pdf.ed.drag {
            Drag::Create { to, points, .. } => {
                *to = at;
                if points.last().is_none_or(|l| (l[0] - at[0]).abs() + (l[1] - at[1]).abs() > 0.8) && points.len() < 20_000 {
                    points.push(at);
                }
            }
            Drag::Move { to, .. } | Drag::Resize { to, .. } => *to = at,
            Drag::Text { page: tp, from, to } => {
                *to = at;
                let (rects, text) = text_between(&page_lines, *from, at);
                let tp = *tp;
                app.pdf.ed.text_sel = (!rects.is_empty()).then_some(TextSel { page: tp, rects, text });
            }
            Drag::None => {}
        }
    }
    if resp.drag_stopped() {
        let drag = std::mem::take(&mut app.pdf.ed.drag);
        finish_drag(app, drag);
    }
    if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        click(app, page, at_of(p));
    }
    if resp.double_clicked()
        && let Some(p) = resp.interact_pointer_pos()
        && tool == Tool::Select
        && let Some(id) = mark_at(app, page, at_of(p))
    {
        edit_mark_text(app, id);
    }
    resp.context_menu(|ui| {
        if let Some(id) = app.pdf.ed.selected {
            if ui.button(tl!("Delete")).clicked() {
                run(app, "pdf.deleteMark", json!({"id": id}));
                app.pdf.ed.selected = None;
                ui.close();
            }
            if mark_of(app, id).is_some_and(|m| m.1.text().is_some()) && ui.button(tl!("Edit Text")).clicked() {
                edit_mark_text(app, id);
                ui.close();
            }
        }
        if let Some(sel) = app.pdf.ed.text_sel.clone() {
            if ui.button(tl!("Copy")).clicked() {
                ui.ctx().copy_text(sel.text.clone());
                ui.close();
            }
            mark_selection_buttons(app, ui, &sel);
        }
        page_menu_items(app, ui, page);
    });
}

/// Open the inline editor on a mark's text.
fn edit_mark_text(app: &mut WordApp, id: u64) {
    let Some((page, m)) = mark_of(app, id) else { return };
    let Some(text) = m.text().map(str::to_string) else { return };
    let size = match &m {
        Mark::TextBox { size, .. } | Mark::EditText { size, .. } => *size,
        _ => 12.0,
    };
    let b = m.bounds();
    let rect = if matches!(m, Mark::Note { .. }) { PRect::new(b.x, b.y + b.h + 2.0, 200.0, 90.0) } else { b };
    app.pdf.ed.selected = Some(id);
    app.pdf.ed.edit = Some(Inline { page, rect, text, size, target: Target::Mark(id), multiline: true, focused: false });
}

fn click(app: &mut WordApp, page: usize, at: [f32; 2]) {
    let tool = app.pdf.ed.tool;
    match tool {
        Tool::Select => {
            if let Some(id) = mark_at(app, page, at) {
                app.pdf.ed.selected = Some(id);
                app.pdf.ed.text_sel = None;
            } else if let Some(field) = field_at(app, page, at) {
                app.pdf.ed.selected = None;
                field_click(app, page, field);
            } else {
                app.pdf.ed.selected = None;
                app.pdf.ed.text_sel = None;
            }
        }
        Tool::EditText => {
            // A line already changed (or a text box) is changed again; otherwise the PDF's own line.
            let existing = mark_at(app, page, at).filter(|id| {
                mark_of(app, *id).is_some_and(|m| matches!(m.1, Mark::EditText { .. } | Mark::TextBox { .. }) && m.1.bounds().contains(at))
            });
            if let Some(id) = existing {
                edit_mark_text(app, id);
            } else if let Some(line) = line_at(app.pdf.ed.page_lines(page), at) {
                let rect = PRect::new(line.x - 1.0, line.y - 1.0, line.w + 2.0, line.h + 2.0);
                app.pdf.ed.edit = Some(Inline {
                    page,
                    rect,
                    text: line.text.clone(),
                    size: line.size,
                    target: Target::Line(Box::new(line)),
                    multiline: false,
                    focused: false,
                });
            } else {
                app.status(tl!("No text there to edit: use Text Box to add some"));
            }
        }
        Tool::TextBox => {
            let rect = PRect::new(at[0], at[1], 200.0, app.pdf.ed.size * 2.4);
            app.pdf.ed.edit =
                Some(Inline { page, rect, text: String::new(), size: app.pdf.ed.size, target: Target::NewTextBox, multiline: true, focused: false });
        }
        Tool::Note => {
            let rect = PRect::new(at[0], at[1] + 24.0, 200.0, 90.0);
            app.pdf.ed.edit =
                Some(Inline { page, rect, text: String::new(), size: 12.0, target: Target::NewNote { at }, multiline: true, focused: false });
        }
        Tool::Stamp => place(app, page, PRect::new(at[0] - 85.0, at[1] - 25.0, 170.0, 50.0)),
        Tool::Image => place(app, page, PRect::new(at[0], at[1], 200.0, 150.0)),
        Tool::Signature => place(app, page, PRect::new(at[0], at[1] - 30.0, 180.0, 60.0)),
        _ => {}
    }
}

/// Put a stamp, picture or signature in `rect`.
fn place(app: &mut WordApp, page: usize, rect: PRect) {
    match app.pdf.ed.tool {
        Tool::Stamp => {
            let (text, color) = (app.pdf.ed.stamp.clone(), app.pdf.ed.color());
            app.pdf.ed.selected = add_mark(app, page, "stamp", json!({"rect": arr(rect), "text": text, "color": hex(color)}));
        }
        Tool::Image => {
            let _ = app.ask_file(FileDialogRequest::Open { purpose: "picture".into() }, AfterPick::Pdf(PdfPick::Image { page, rect }));
        }
        Tool::Signature => {
            if app.pdf.ed.signature.is_empty() {
                app.pdf.ed.dialog = Some(Dialog::Signature { strokes: Vec::new(), place: Some((page, rect)) });
            } else {
                sign(app, page, rect);
            }
        }
        _ => {}
    }
}

/// The signature, as ink in `rect` (its shape kept).
fn sign(app: &mut WordApp, page: usize, rect: PRect) {
    let k = (rect.w / 3.0).min(rect.h);
    let (ox, oy) = (rect.x + (rect.w - 3.0 * k) / 2.0, rect.y + (rect.h - k) / 2.0);
    let strokes: Vec<Vec<[f32; 2]>> = app.pdf.ed.signature.iter().map(|s| s.iter().map(|p| [ox + p[0] * k, oy + p[1] * k]).collect()).collect();
    let color = app.pdf.ed.colors.get(&Tool::Signature).copied().unwrap_or(Tool::Signature.default_color());
    let width = (k / 40.0).clamp(0.8, 4.0);
    app.pdf.ed.selected = add_mark(app, page, "ink", json!({"strokes": strokes, "color": hex(color), "width": width}));
}

fn finish_drag(app: &mut WordApp, drag: Drag) {
    match drag {
        Drag::None | Drag::Text { .. } => {}
        Drag::Move { id, from, to, .. } => {
            let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
            if dx.abs() + dy.abs() > 0.3 {
                run(app, "pdf.updateMark", json!({"id": id, "move": [dx, dy]}));
            }
        }
        Drag::Resize { id, handle, orig, to, .. } => {
            let r = resized(orig, handle, to);
            if r.w >= 2.0 && r.h >= 2.0 {
                run(app, "pdf.updateMark", json!({"id": id, "rect": arr(r)}));
            }
        }
        Drag::Create { page, from, to, points } => create(app, page, from, to, points),
    }
}

fn create(app: &mut WordApp, page: usize, from: [f32; 2], to: [f32; 2], points: Vec<[f32; 2]>) {
    let ed = &app.pdf.ed;
    let (tool, color, width) = (ed.tool, ed.color(), ed.width);
    let r = span(from, to);
    let tiny = r.w < 4.0 && r.h < 4.0;
    let c = hex(color);
    let id = match tool {
        t if t.marks_text() => {
            let (rects, _) = text_between(ed.page_lines(page), from, to);
            let rects: Vec<Value> = if rects.is_empty() && !tiny { vec![arr(r)] } else { rects.into_iter().map(arr).collect() };
            if rects.is_empty() {
                return;
            }
            add_mark(app, page, t.id(), json!({"rects": rects, "color": c}))
        }
        Tool::Pen if points.len() > 1 => add_mark(app, page, "ink", json!({"strokes": [points], "color": c, "width": width})),
        Tool::Line | Tool::Arrow if !tiny => add_mark(app, page, tool.id(), json!({"from": from, "to": to, "color": c, "width": width})),
        Tool::Rectangle | Tool::Ellipse if !tiny => {
            let fill = ed.fill.map(hex);
            add_mark(app, page, tool.id(), json!({"rect": arr(r), "stroke": c, "fill": fill, "width": width}))
        }
        Tool::TextBox => {
            let rect = if tiny { PRect::new(from[0], from[1], 200.0, ed.size * 2.4) } else { r };
            app.pdf.ed.edit =
                Some(Inline { page, rect, text: String::new(), size: ed.size, target: Target::NewTextBox, multiline: true, focused: false });
            None
        }
        Tool::Stamp | Tool::Image | Tool::Signature => {
            if tiny {
                click(app, page, from);
            } else {
                place(app, page, r);
            }
            None
        }
        Tool::Whiteout if !tiny => add_mark(app, page, "whiteout", json!({"rect": arr(r)})),
        Tool::Redact if !tiny => add_mark(app, page, "redact", json!({"rects": [arr(r)]})),
        Tool::Crop if r.w > 8.0 && r.h > 8.0 => {
            run(app, "pdf.crop", json!({"page": page + 1, "rect": arr(r)}));
            None
        }
        _ => None,
    };
    if id.is_some() {
        app.pdf.ed.selected = id;
    }
}

/// The form field (name, widget box, its on-state) at `at`.
fn field_at(app: &WordApp, page: usize, at: [f32; 2]) -> Option<(String, PRect, Option<String>)> {
    let shown = app.pdf.shown.as_ref()?;
    shown
        .fields
        .iter()
        .filter(|f| !f.read_only && !matches!(f.kind, FieldKind::PushButton | FieldKind::Signature))
        .find_map(|f| f.widgets.iter().find(|w| w.0 == page && w.1.contains(at)).map(|w| (f.name.clone(), w.1, w.2.first().cloned())))
}

fn field_click(app: &mut WordApp, page: usize, (name, rect, on): (String, PRect, Option<String>)) {
    let Some(f) = app.pdf.shown.as_ref().and_then(|s| s.fields.iter().find(|f| f.name == name)).cloned() else { return };
    match f.kind {
        FieldKind::CheckBox => {
            let checked = !f.value.is_empty() && f.value != "Off";
            let v = if checked { "Off".to_string() } else { on.unwrap_or_else(|| "Yes".into()) };
            run(app, "pdf.fill", json!({"field": name, "value": v}));
        }
        FieldKind::Radio => {
            if let Some(v) = on {
                run(app, "pdf.fill", json!({"field": name, "value": v}));
            }
        }
        FieldKind::Choice => {
            app.pdf.ed.choice = Some((name, f.options.clone(), page, rect));
        }
        FieldKind::Text => {
            let size = (rect.h * 0.62).clamp(6.0, 14.0);
            app.pdf.ed.edit =
                Some(Inline { page, rect, text: f.value.clone(), size, target: Target::Field(name), multiline: f.multiline, focused: false });
        }
        FieldKind::PushButton | FieldKind::Signature => {}
    }
}

/// Paint what goes over page `page`: form fields, found text, the selection, the selected mark
/// and what is being drawn.
fn draw(app: &WordApp, p: &egui::Painter, page: usize, r: Rect, scale: f32) {
    let ed = &app.pdf.ed;
    let t = Tokens::get(p.ctx());
    let Some(shown) = app.pdf.shown.as_ref() else { return };
    let field_fill = Color32::from_rgba_unmultiplied(80, 140, 255, 34);
    for f in &shown.fields {
        if f.read_only || matches!(f.kind, FieldKind::PushButton) {
            continue;
        }
        for w in f.widgets.iter().filter(|w| w.0 == page) {
            let wr = rect_px(w.1, r, scale);
            p.rect(wr, 1.0, field_fill, Stroke::new(1.0, Color32::from_rgba_unmultiplied(80, 140, 255, 140)), egui::StrokeKind::Inside);
        }
    }
    for (i, (hp, b)) in ed.hits.iter().enumerate() {
        if *hp == page {
            let a = if i == ed.hit { 150 } else { 70 };
            p.rect_filled(rect_px(PRect::new(b[0], b[1], b[2], b[3]), r, scale), 1.0, Color32::from_rgba_unmultiplied(255, 140, 0, a));
        }
    }
    if let Some(sel) = ed.text_sel.as_ref().filter(|s| s.page == page) {
        for b in &sel.rects {
            p.rect_filled(rect_px(*b, r, scale), 0.0, Color32::from_rgba_unmultiplied(40, 120, 255, 70));
        }
    }
    let accent = Stroke::new(1.5, t.accent);
    if let Some(id) = ed.selected
        && let Some((mp, m)) = mark_of(app, id)
        && mp == page
    {
        let mut b = m.bounds();
        match &ed.drag {
            Drag::Move { id: d, from, to, .. } if *d == id => b = b.translate(to[0] - from[0], to[1] - from[1]),
            Drag::Resize { id: d, handle, orig, to, .. } if *d == id => b = resized(*orig, *handle, *to),
            _ => {}
        }
        let br = rect_px(b, r, scale).expand(2.0);
        p.rect_stroke(br, 0.0, accent, egui::StrokeKind::Outside);
        if resizable(&m) {
            for q in handles(PRect::new(br.min.x, br.min.y, br.width(), br.height())) {
                let hr = Rect::from_center_size(pos2(q[0], q[1]), vec2(7.0, 7.0));
                p.rect(hr, 1.0, Color32::WHITE, accent, egui::StrokeKind::Inside);
            }
        }
    }
    if let Drag::Create { page: dp, from, to, points } = &ed.drag
        && *dp == page
    {
        let color = c32(ed.color());
        let (a, b) = (to_px(*from, r, scale), to_px(*to, r, scale));
        let stroke = Stroke::new((ed.width * scale).max(1.0), color);
        let dash = Stroke::new(1.0, t.accent);
        match ed.tool {
            t if t.marks_text() => {
                let (rects, _) = text_between(ed.page_lines(page), *from, *to);
                for q in rects {
                    p.rect_filled(rect_px(q, r, scale), 0.0, color.gamma_multiply(0.4));
                }
            }
            Tool::Pen => {
                let pts: Vec<Pos2> = points.iter().map(|q| to_px(*q, r, scale)).collect();
                p.add(egui::Shape::line(pts, stroke));
            }
            Tool::Line | Tool::Arrow => {
                p.line_segment([a, b], stroke);
            }
            Tool::Ellipse => {
                let e = Rect::from_two_pos(a, b);
                p.add(egui::Shape::ellipse_stroke(e.center(), e.size() / 2.0, stroke));
            }
            Tool::Whiteout => {
                p.rect(Rect::from_two_pos(a, b), 0.0, Color32::WHITE, dash, egui::StrokeKind::Inside);
            }
            Tool::Redact => {
                p.rect_filled(Rect::from_two_pos(a, b), 0.0, Color32::from_black_alpha(160));
            }
            Tool::Rectangle => {
                let fill = ed.fill.map_or(Color32::TRANSPARENT, c32);
                p.rect(Rect::from_two_pos(a, b), 0.0, fill, stroke, egui::StrokeKind::Middle);
            }
            _ => {
                p.rect_stroke(Rect::from_two_pos(a, b), 0.0, dash, egui::StrokeKind::Middle);
            }
        }
    }
}

/// The text being typed into a text box, note, line or field, over the page.
fn inline_editor(app: &mut WordApp, ui: &mut Ui, pages: &[(usize, Rect)], scale: f32) {
    let Some(ed) = app.pdf.ed.edit.as_mut() else { return };
    let Some(&(_, r)) = pages.iter().find(|p| p.0 == ed.page) else { return };
    let b = ed.rect;
    let font_px = (ed.size * scale).clamp(8.0, 72.0);
    let w = (b.w * scale).max(80.0);
    let h = (b.h * scale).max(font_px * 1.6);
    let screen = Rect::from_min_size(to_px([b.x, b.y], r, scale), vec2(w, h));
    let font = egui::FontId::proportional(font_px);
    let id = ui.id().with("pdf-inline");
    let te = if ed.multiline { egui::TextEdit::multiline(&mut ed.text) } else { egui::TextEdit::singleline(&mut ed.text) };
    let te =
        te.id(id).font(font).desired_width(w - 8.0).hint_text(tl!("Type here")).background_color(Color32::from_rgba_unmultiplied(255, 255, 255, 235));
    let resp = ui.put(screen, te);
    if !ed.focused {
        resp.request_focus();
        ed.focused = true;
    }
    let cancel = resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape));
    let done_key = resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
    if cancel {
        app.pdf.ed.edit = None;
        return;
    }
    if resp.lost_focus() || done_key {
        if resp.lost_focus() && ui.input(|i| i.pointer.any_down()) {
            app.pdf.ed.committed_at = ui.input(|i| i.time);
        }
        if let Some(ed) = app.pdf.ed.edit.take() {
            commit(app, ed);
        }
    }
}

fn commit(app: &mut WordApp, ed: Inline) {
    let text = ed.text.trim_end().to_string();
    match ed.target {
        Target::NewTextBox => {
            if !text.trim().is_empty() {
                let e = &app.pdf.ed;
                let p = json!({"rect": arr(ed.rect), "text": text, "size": e.size, "color": hex(e.color()), "font": e.font, "bold": e.bold, "italic": e.italic, "fit": e.fit, "fill": e.fill.map(hex)});
                app.pdf.ed.selected = add_mark(app, ed.page, "textBox", p);
            }
        }
        Target::NewNote { at } => {
            if !text.trim().is_empty() {
                let color = hex(app.pdf.ed.color());
                app.pdf.ed.selected = add_mark(app, ed.page, "note", json!({"at": at, "text": text, "color": color}));
            }
        }
        Target::Mark(id) => {
            if app.session.pdf.mark(id).is_some_and(|(_, m)| m.text() != Some(text.as_str())) {
                run(app, "pdf.updateMark", json!({"id": id, "text": text}));
            }
        }
        Target::Line(line) => {
            if text != line.text {
                let p = json!({
                    "rect": arr(ed.rect), "base": line.base, "text": text, "size": line.size, "color": hex(line.color),
                    "font": line.font, "bold": line.bold, "italic": line.italic
                });
                add_mark(app, ed.page, "editText", p);
            }
        }
        Target::Field(name) => {
            run(app, "pdf.fill", json!({"field": name, "value": ed.text}));
        }
    }
}

fn mark_selection_buttons(app: &mut WordApp, ui: &mut Ui, sel: &TextSel) {
    let rects: Vec<Value> = sel.rects.iter().map(|r| arr(*r)).collect();
    for (tool, label) in [(Tool::Highlight, tl!("Highlight")), (Tool::Underline, tl!("Underline")), (Tool::Strikeout, tl!("Strikethrough"))] {
        if ui.button(label).clicked() {
            let c = hex(app.pdf.ed.colors.get(&tool).copied().unwrap_or(tool.default_color()));
            add_mark(app, sel.page, tool.id(), json!({"rects": rects, "color": c}));
            app.pdf.ed.text_sel = None;
            ui.close();
        }
    }
    if ui.button(tl!("Redact")).on_hover_text(Tool::Redact.tip()).clicked() {
        add_mark(app, sel.page, "redact", json!({"rects": rects}));
        app.pdf.ed.text_sel = None;
        ui.close();
    }
}

/// The small bar by selected text: copy it or mark it up.
fn selection_bar(app: &mut WordApp, ui: &mut Ui, pages: &[(usize, Rect)], scale: f32) {
    if !matches!(app.pdf.ed.drag, Drag::None) || app.pdf.ed.edit.is_some() {
        return;
    }
    let Some(sel) = app.pdf.ed.text_sel.clone() else { return };
    let Some(&(_, r)) = pages.iter().find(|p| p.0 == sel.page) else { return };
    let Some(last) = sel.rects.last() else { return };
    let at = to_px([last.x, last.y + last.h + 4.0], r, scale);
    egui::Area::new(ui.id().with("pdf-sel-bar")).fixed_pos(at).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button(tl!("Copy")).clicked() {
                    ui.ctx().copy_text(sel.text.clone());
                    app.pdf.ed.status = Some(tl!("Copied").to_string());
                }
                mark_selection_buttons(app, ui, &sel);
            });
        });
    });
}

/// The options of a choice field, by it.
fn choice_menu(app: &mut WordApp, ui: &mut Ui, pages: &[(usize, Rect)], scale: f32) {
    let Some((name, options, page, b)) = app.pdf.ed.choice.clone() else { return };
    let Some(&(_, pr)) = pages.iter().find(|p| p.0 == page) else { return };
    let r = to_px([b.x, b.y + b.h], pr, scale);
    let mut picked = None;
    let mut close = false;
    egui::Area::new(ui.id().with("pdf-choice")).fixed_pos(r).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_min_width(140.0);
            for o in &options {
                if ui.button(o).clicked() {
                    picked = Some(o.clone());
                }
            }
            if options.is_empty() {
                ui.label(tl!("No choices"));
            }
            if ui.button(tl!("Cancel")).clicked() {
                close = true;
            }
        });
    });
    if let Some(v) = picked {
        run(app, "pdf.fill", json!({"field": name, "value": v}));
        close = true;
    }
    if close {
        app.pdf.ed.choice = None;
    }
}

/// A square icon button; `on` when its tool is chosen.
fn icon_button(ui: &mut Ui, t: &Tokens, icon: &str, on: bool, tip: &str, label: &str) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
    let fill = if on {
        t.accent.gamma_multiply(0.25)
    } else if resp.hovered() {
        t.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect(r, 4.0, fill, Stroke::new(1.0, if on { t.accent } else { Color32::TRANSPARENT }), egui::StrokeKind::Inside);
    let ir = r.shrink(4.0);
    match icon {
        "redact" => {
            ui.painter().rect_filled(Rect::from_center_size(ir.center(), vec2(ir.width(), ir.height() * 0.42)), 1.0, t.text);
        }
        _ => crate::icons::paint(ui.painter(), ir, icon, t.text, t.accent),
    }
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, label));
    resp.on_hover_text(format!("{label}\n{tip}"))
}

fn swatches(app: &mut WordApp, ui: &mut Ui, current: [u8; 3], selected: Option<u64>) {
    for c in SWATCHES {
        let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
        let stroke = if c == current { Stroke::new(2.0, Tokens::get(ui.ctx()).accent) } else { Stroke::new(1.0, Color32::from_gray(140)) };
        ui.painter().rect(r, 3.0, c32(c), stroke, egui::StrokeKind::Inside);
        if resp.clicked() {
            app.pdf.ed.set_color(c);
            if let Some(id) = selected {
                run(app, "pdf.updateMark", json!({"id": id, "color": hex(c)}));
            }
        }
    }
    let mut c = current;
    if egui::color_picker::color_edit_button_srgb(ui, &mut c).on_hover_text(tl!("More Colors")).changed() {
        app.pdf.ed.set_color(c);
    }
}

/// The editing toolbar: tools and their options, find, undo, saving and the document's menu.
pub fn toolbar(app: &mut WordApp, ui: &mut Ui, t: &Tokens) {
    egui::Frame::NONE.fill(t.ribbon).inner_margin(egui::Margin::symmetric(8, 3)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
            for tool in Tool::ALL {
                let on = app.pdf.ed.tool == tool;
                if icon_button(ui, t, tool.icon(), on, tool.tip(), tool.label()).clicked() {
                    choose(app, tool);
                }
                if matches!(tool, Tool::Select | Tool::TextBox | Tool::Strikeout | Tool::Ellipse | Tool::Signature) {
                    ui.add_space(6.0);
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 2.0);
            options(app, ui);
            ui.separator();
            find_bar(app, ui);
            ui.separator();
            let (can_undo, can_redo) = (app.session.pdf.can_undo(), app.session.pdf.can_redo());
            if ui.add_enabled(can_undo, egui::Button::new(tl!("Undo"))).on_hover_text("Ctrl+Z").clicked() {
                run(app, "pdf.undo", json!({}));
            }
            if ui.add_enabled(can_redo, egui::Button::new(tl!("Redo"))).on_hover_text("Ctrl+Y").clicked() {
                run(app, "pdf.redo", json!({}));
            }
            let label = if app.session.pdf.dirty() { format!("{} •", tl!("Save PDF")) } else { tl!("Save PDF").to_string() };
            if ui.button(label).on_hover_text("Ctrl+S").clicked() {
                save(app);
            }
            ui.menu_button(tl!("Save As"), |ui| {
                if ui.button(tl!("Save PDF As…")).clicked() {
                    save_as(app, json!({}));
                    ui.close();
                }
                if ui.button(tl!("Save with Password…")).clicked() {
                    app.pdf.ed.dialog = Some(Dialog::Save { password: String::new(), confirm: String::new(), flatten: false, compress: true });
                    ui.close();
                }
                if ui
                    .button(tl!("Save Flattened…"))
                    .on_hover_text(tl!("Every page becomes a picture: nothing on it can be changed or copied"))
                    .clicked()
                {
                    save_as(app, json!({"flatten": true}));
                    ui.close();
                }
            });
            ui.menu_button(tl!("Document"), |ui| document_menu(app, ui));
        });
    });
    let r = ui.max_rect();
    ui.painter().hline(r.x_range(), ui.min_rect().max.y, Stroke::new(1.0, t.border));
}

fn choose(app: &mut WordApp, tool: Tool) {
    let ed = &mut app.pdf.ed;
    ed.tool = tool;
    ed.drag = Drag::None;
    if tool != Tool::Select {
        ed.selected = None;
        ed.text_sel = None;
    }
    if tool == Tool::Signature && ed.signature.is_empty() {
        ed.dialog = Some(Dialog::Signature { strokes: Vec::new(), place: None });
    }
}

/// The chosen tool's options, or the selected mark's.
fn options(app: &mut WordApp, ui: &mut Ui) {
    let sel = app.pdf.ed.selected.and_then(|id| mark_of(app, id).map(|m| (id, m.1)));
    let tool = app.pdf.ed.tool;
    let (has_color, has_width, has_text) = match &sel {
        Some((_, m)) => (
            !matches!(m, Mark::Image { .. } | Mark::Whiteout { .. } | Mark::Redact { .. }),
            matches!(m, Mark::Ink { .. } | Mark::Line { .. } | Mark::Rectangle { .. } | Mark::Ellipse { .. }),
            matches!(m, Mark::TextBox { .. }),
        ),
        None => (tool.has_color(), tool.has_width(), tool == Tool::TextBox),
    };
    let sel_id = sel.as_ref().map(|s| s.0);
    if has_color {
        let current = match &sel {
            Some((_, m)) => mark_color(m).unwrap_or(app.pdf.ed.color()),
            None => app.pdf.ed.color(),
        };
        swatches(app, ui, current, sel_id);
    }
    if has_width {
        ui.label(tl!("Thickness"));
        let mut w = match &sel {
            Some((_, Mark::Ink { width, .. } | Mark::Line { width, .. } | Mark::Rectangle { width, .. } | Mark::Ellipse { width, .. })) => *width,
            _ => app.pdf.ed.width,
        };
        let r = ui.add(egui::DragValue::new(&mut w).range(0.5..=24.0).speed(0.1).suffix(" pt"));
        if r.changed() {
            app.pdf.ed.width = w;
        }
        if let Some(id) = sel_id
            && (r.drag_stopped() || (r.changed() && !r.dragged()))
        {
            run(app, "pdf.updateMark", json!({"id": id, "width": w}));
        }
    }
    if matches!(tool, Tool::Rectangle | Tool::Ellipse | Tool::TextBox) && sel.is_none() {
        let mut filled = app.pdf.ed.fill.is_some();
        if ui.checkbox(&mut filled, tl!("Fill")).changed() {
            app.pdf.ed.fill = filled.then_some([255, 255, 255]);
        }
        if let Some(mut f) = app.pdf.ed.fill
            && egui::color_picker::color_edit_button_srgb(ui, &mut f).changed()
        {
            app.pdf.ed.fill = Some(f);
        }
    }
    if has_text {
        let (mut size, mut font, mut bold, mut italic, mut fit) = match &sel {
            Some((_, Mark::TextBox { size, font, bold, italic, fit, .. })) => (*size, font.clone(), *bold, *italic, *fit),
            _ => (app.pdf.ed.size, app.pdf.ed.font.clone(), app.pdf.ed.bold, app.pdf.ed.italic, app.pdf.ed.fit),
        };
        let mut changed = json!({});
        egui::ComboBox::from_id_salt("pdf-font").width(130.0).selected_text(font.clone()).show_ui(ui, |ui| {
            for f in FONTS {
                if ui.selectable_label(font == f, f).clicked() {
                    font = f.to_string();
                    changed["font"] = json!(f);
                }
            }
        });
        let r = ui.add(egui::DragValue::new(&mut size).range(4.0..=144.0).speed(0.2).suffix(" pt")).on_hover_text(tl!("Font Size"));
        if r.drag_stopped() || (r.changed() && !r.dragged()) {
            changed["size"] = json!(size);
            changed["fit"] = json!(false);
            fit = false;
        }
        if ui.toggle_value(&mut bold, egui::RichText::new("B").strong()).on_hover_text(tl!("Bold")).changed() {
            changed["bold"] = json!(bold);
        }
        if ui.toggle_value(&mut italic, egui::RichText::new("I").italics()).on_hover_text(tl!("Italic")).changed() {
            changed["italic"] = json!(italic);
        }
        if ui
            .checkbox(&mut fit, tl!("Fit text to box"))
            .on_hover_text(tl!("The font size follows the box: resize it and the text grows or shrinks to fill it"))
            .changed()
        {
            changed["fit"] = json!(fit);
        }
        let ed = &mut app.pdf.ed;
        if sel.is_none() {
            (ed.size, ed.font, ed.bold, ed.italic, ed.fit) = (size, font, bold, italic, fit);
        } else if let Some(id) = sel_id
            && changed.as_object().is_some_and(|o| !o.is_empty())
        {
            changed["id"] = json!(id);
            run(app, "pdf.updateMark", changed);
        }
    }
    if tool == Tool::Stamp && sel.is_none() {
        let mut stamp = app.pdf.ed.stamp.clone();
        egui::ComboBox::from_id_salt("pdf-stamp").width(130.0).selected_text(stamp.clone()).show_ui(ui, |ui| {
            for s in STAMPS {
                ui.selectable_value(&mut stamp, s.to_string(), s);
            }
        });
        ui.add(egui::TextEdit::singleline(&mut stamp).desired_width(110.0).hint_text(tl!("Custom")));
        app.pdf.ed.stamp = stamp.chars().take(60).collect();
    }
    if tool == Tool::Signature && sel.is_none() && ui.button(tl!("Draw Signature…")).clicked() {
        app.pdf.ed.dialog = Some(Dialog::Signature { strokes: app.pdf.ed.signature.clone(), place: None });
    }
    if let Some(id) = sel_id {
        if sel.as_ref().is_some_and(|m| m.1.text().is_some()) && ui.button(tl!("Edit Text")).clicked() {
            edit_mark_text(app, id);
        }
        if ui.button(tl!("Delete")).on_hover_text("Delete").clicked() {
            run(app, "pdf.deleteMark", json!({"id": id}));
            app.pdf.ed.selected = None;
        }
    }
}

fn mark_color(m: &Mark) -> Option<[u8; 3]> {
    match m {
        Mark::Highlight { color, .. }
        | Mark::Underline { color, .. }
        | Mark::Strikeout { color, .. }
        | Mark::Ink { color, .. }
        | Mark::Line { color, .. }
        | Mark::TextBox { color, .. }
        | Mark::Note { color, .. }
        | Mark::Stamp { color, .. }
        | Mark::EditText { color, .. } => Some(*color),
        Mark::Rectangle { stroke, .. } | Mark::Ellipse { stroke, .. } => *stroke,
        _ => None,
    }
}

fn find_bar(app: &mut WordApp, ui: &mut Ui) {
    let id = egui::Id::new("pdf-find");
    let resp = ui.add(egui::TextEdit::singleline(&mut app.pdf.ed.search).id(id).desired_width(130.0).hint_text(tl!("Find in PDF")));
    if app.pdf.ed.focus_search {
        app.pdf.ed.focus_search = false;
        resp.request_focus();
    }
    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    let q = app.pdf.ed.search.trim().to_string();
    if enter && !q.is_empty() {
        if q == app.pdf.ed.hits_query && !app.pdf.ed.hits.is_empty() {
            step_hit(app, 1);
        } else if let Some(version) = app.pdf.shown_version()
            && let Some(w) = app.pdf.worker.as_mut()
        {
            w.send(Job::Search { version, query: q.clone() });
        }
        resp.request_focus();
    }
    let n = app.pdf.ed.hits.len();
    if !app.pdf.ed.hits_query.is_empty() && app.pdf.ed.hits_query == q {
        let label = if n == 0 {
            tl!("No matches").to_string()
        } else {
            crate::i18n::fmt(tl!("{i} of {n}"), &[("i", &(app.pdf.ed.hit + 1).to_string()), ("n", &n.to_string())])
        };
        ui.label(label);
    }
    if ui.add_enabled(n > 0, egui::Button::new("▲")).on_hover_text(tl!("Find Previous")).clicked() {
        step_hit(app, -1);
    }
    if ui.add_enabled(n > 0, egui::Button::new("▼")).on_hover_text(tl!("Find Next")).clicked() {
        step_hit(app, 1);
    }
}

fn step_hit(app: &mut WordApp, by: i64) {
    let n = app.pdf.ed.hits.len() as i64;
    if n == 0 {
        return;
    }
    let i = (app.pdf.ed.hit as i64 + by).rem_euclid(n) as usize;
    app.pdf.ed.hit = i;
    if let Some((page, _)) = app.pdf.ed.hits.get(i) {
        app.pdf.scroll_to = Some(*page);
    }
}

fn document_menu(app: &mut WordApp, ui: &mut Ui) {
    if ui.button(tl!("Watermark…")).clicked() {
        app.pdf.ed.dialog = Some(Dialog::Watermark { text: "CONFIDENTIAL".into(), size: 64.0, opacity: 0.25, angle: 45.0, color: [160, 160, 160] });
        ui.close();
    }
    if ui.button(tl!("Header, Footer and Page Numbers…")).clicked() {
        app.pdf.ed.dialog = Some(Dialog::PageText { text: tl!("Page {n} of {N}").to_string(), position: 4, size: 10.0, start: 1 });
        ui.close();
    }
    if ui.button(tl!("Properties…")).clicked() {
        let core = &app.session.doc.core;
        app.pdf.ed.dialog = Some(Dialog::Properties {
            title: core.title.clone(),
            author: core.creator.clone(),
            subject: core.subject.clone(),
            keywords: core.keywords.clone(),
        });
        ui.close();
    }
    ui.separator();
    if ui.button(tl!("Insert Pages from PDF…")).clicked() {
        let at = app.pdf.ed.page_sel.unwrap_or(app.pdf.page) + 1;
        let _ = app.ask_file(FileDialogRequest::Open { purpose: "pdf".into() }, AfterPick::Pdf(PdfPick::InsertPdf { at }));
        ui.close();
    }
    if ui.button(tl!("Split…")).clicked() {
        app.pdf.ed.dialog = Some(Dialog::Split { every: 1 });
        ui.close();
    }
    if ui.button(tl!("Export Pages as Images…")).clicked() {
        app.pdf.ed.dialog = Some(Dialog::Images { dpi: 150.0, jpeg: false });
        ui.close();
    }
    if ui.button(tl!("Copy All Text")).clicked() {
        if let Some(version) = app.pdf.shown_version()
            && let Some(w) = app.pdf.worker.as_mut()
        {
            w.send(Job::Text { version });
        }
        ui.close();
    }
    ui.separator();
    let mut panel = app.pdf.ed.pages_panel;
    if ui.checkbox(&mut panel, tl!("Pages Panel")).changed() {
        app.pdf.ed.pages_panel = panel;
    }
}

/// Save PDF: over the PDF the document was opened from, or where the user says.
pub fn save(app: &mut WordApp) {
    if let Some(v) = run(app, "pdf.save", json!({}))
        && v.get("saved").and_then(Value::as_bool) == Some(true)
    {
        app.status(crate::i18n::fmt(tl!("Saved {path}"), &[("path", v.get("path").and_then(Value::as_str).unwrap_or(""))]));
    }
}

/// Ask where to save the PDF, then save it there with `params` (password, flatten, compress).
pub fn save_as(app: &mut WordApp, params: Value) {
    let name = match &app.session.pdf.path {
        Some(p) => p.file_name().map_or_else(|| format!("{}.pdf", app.title_stem()), |n| n.to_string_lossy().into_owned()),
        None => format!("{}.pdf", app.title_stem()),
    };
    let _ = app.ask_file(FileDialogRequest::Save { name }, AfterPick::Pdf(PdfPick::Save { params }));
}

/// Do what a file picked for the PDF was for (`None`: cancelled).
pub(crate) fn finish_pick(app: &mut WordApp, pick: PdfPick, picked: Option<String>) -> Result<Value, String> {
    let Some(path) = picked.filter(|p| !p.is_empty()) else { return Ok(json!({"cancelled": true})) };
    let folder = || std::path::Path::new(&path).parent().map_or_else(|| ".".to_string(), |d| d.to_string_lossy().into_owned());
    let r = match pick {
        PdfPick::Save { mut params } => {
            let path = if path.to_ascii_lowercase().ends_with(".pdf") { path.clone() } else { format!("{path}.pdf") };
            params["path"] = json!(path);
            params["saveAs"] = json!(params.get("flatten").and_then(Value::as_bool) != Some(true));
            app.run("pdf.save", params)
        }
        PdfPick::InsertPdf { at } => app.run("pdf.insertPdf", json!({"path": path, "at": at + 1})),
        PdfPick::Image { page, rect } => {
            // The picture keeps its shape, inside the box drawn.
            let rect = match image::image_dimensions(&path) {
                Ok((w, h)) if w > 0 && h > 0 => {
                    let k = (rect.w / w as f32).min(rect.h / h as f32);
                    let (iw, ih) = (w as f32 * k, h as f32 * k);
                    PRect::new(rect.x + (rect.w - iw) / 2.0, rect.y + (rect.h - ih) / 2.0, iw, ih)
                }
                _ => rect,
            };
            app.run("pdf.mark", json!({"page": page + 1, "kind": "image", "rect": arr(rect), "path": path}))
        }
        PdfPick::Extract { pages } => app.run("pdf.extract", json!({"path": path, "pages": pages.iter().map(|p| p + 1).collect::<Vec<_>>()})),
        PdfPick::Split { every } => {
            let stem = std::path::Path::new(&path).file_stem().map_or_else(|| "part".to_string(), |s| s.to_string_lossy().into_owned());
            let stem = stem.trim_end_matches("-1").to_string();
            app.run("pdf.split", json!({"dir": folder(), "every": every, "name": stem}))
        }
        PdfPick::Images { dpi, jpeg } => {
            app.run("pdf.exportImages", json!({"dir": folder(), "dpi": dpi, "format": if jpeg { "jpeg" } else { "png" }}))
        }
    };
    match &r {
        Ok(v) => {
            let n = v.get("files").and_then(Value::as_array).map(Vec::len);
            let msg = match (n, v.get("path").and_then(Value::as_str)) {
                (Some(n), _) => crate::i18n::fmt(tl!("Wrote {n} files"), &[("n", &n.to_string())]),
                (None, Some(p)) => crate::i18n::fmt(tl!("Saved {path}"), &[("path", p)]),
                _ => String::new(),
            };
            if !msg.is_empty() {
                app.status(msg);
            }
        }
        Err(e) => app.status(e.clone()),
    }
    r
}

/// Keys for the PDF while it has them: undo, redo, save, copy, find, delete and nudge marks.
pub fn keys(app: &mut WordApp, ctx: &egui::Context) {
    // A press elsewhere hands the keys back to the document.
    if ctx.input(|i| i.pointer.any_pressed())
        && let (Some(p), Some(r)) = (ctx.pointer_interact_pos(), app.pdf.rect)
        && !r.contains(p)
    {
        app.pdf.ed.active = false;
    }
    if ctx.egui_wants_keyboard_input() || !(app.pdf.mode == PdfMode::Pdf || app.pdf.ed.active) || app.dialog.is_some() {
        return;
    }
    let cmd = egui::Modifiers::COMMAND;
    let shift_cmd = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;
    if ctx.input_mut(|i| i.consume_key(shift_cmd, egui::Key::Z) || i.consume_key(cmd, egui::Key::Y)) {
        let _ = app.run("pdf.redo", json!({}));
    }
    if ctx.input_mut(|i| i.consume_key(cmd, egui::Key::Z)) {
        let _ = app.run("pdf.undo", json!({}));
    }
    if ctx.input_mut(|i| i.consume_key(cmd, egui::Key::S)) {
        save(app);
    }
    if ctx.input_mut(|i| i.consume_key(cmd, egui::Key::F)) {
        app.pdf.ed.focus_search = true;
    }
    let copy = ctx.input_mut(|i| {
        let had = i.events.iter().any(|e| matches!(e, egui::Event::Copy));
        i.events.retain(|e| !matches!(e, egui::Event::Copy));
        had || i.consume_key(cmd, egui::Key::C)
    });
    if copy && let Some(sel) = &app.pdf.ed.text_sel {
        ctx.copy_text(sel.text.clone());
    }
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        let ed = &mut app.pdf.ed;
        if ed.selected.is_some() || ed.text_sel.is_some() {
            ed.selected = None;
            ed.text_sel = None;
        } else {
            ed.tool = Tool::Select;
        }
        ed.drag = Drag::None;
    }
    if let Some(id) = app.pdf.ed.selected {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete) || i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)) {
            let _ = app.run("pdf.deleteMark", json!({"id": id}));
            app.pdf.ed.selected = None;
        }
        let mut d = [0.0f32, 0.0];
        for (key, dx, dy) in
            [(egui::Key::ArrowLeft, -1.0, 0.0), (egui::Key::ArrowRight, 1.0, 0.0), (egui::Key::ArrowUp, 0.0, -1.0), (egui::Key::ArrowDown, 0.0, 1.0)]
        {
            for (m, k) in [(egui::Modifiers::NONE, 1.0), (egui::Modifiers::SHIFT, 10.0)] {
                if ctx.input_mut(|i| i.consume_key(m, key)) {
                    d = [d[0] + dx * k, d[1] + dy * k];
                }
            }
        }
        if d != [0.0, 0.0] {
            let _ = app.run("pdf.updateMark", json!({"id": id, "move": d}));
        }
    }
}

/// The document dialogs.
pub fn dialogs(app: &mut WordApp, ctx: &egui::Context) {
    let Some(mut d) = app.pdf.ed.dialog.take() else { return };
    let mut open = true;
    let mut cancel = false;
    let mut done = false;
    let title = match &d {
        Dialog::Watermark { .. } => tl!("Watermark"),
        Dialog::PageText { .. } => tl!("Header & Footer"),
        Dialog::Properties { .. } => tl!("Properties"),
        Dialog::Save { .. } => tl!("Save with Password"),
        Dialog::Split { .. } => tl!("Split"),
        Dialog::Images { .. } => tl!("Export Pages as Images"),
        Dialog::Signature { .. } => tl!("Signature"),
    };
    egui::Window::new(title)
        .id(egui::Id::new("pdf-dialog"))
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .open(&mut open)
        .show(ctx, |ui| {
            let grid = |ui: &mut Ui, body: &mut dyn FnMut(&mut Ui)| {
                egui::Grid::new("pdf-dialog-grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| body(ui))
            };
            match &mut d {
                Dialog::Watermark { text, size, opacity, angle, color } => {
                    grid(ui, &mut |ui| {
                        ui.label(tl!("Text:"));
                        ui.text_edit_singleline(text);
                        ui.end_row();
                        ui.label(tl!("Size"));
                        ui.add(egui::DragValue::new(size).range(8.0..=300.0).suffix(" pt"));
                        ui.end_row();
                        ui.label(tl!("Transparency"));
                        let mut t = 1.0 - *opacity;
                        if ui.add(egui::Slider::new(&mut t, 0.0..=0.95)).changed() {
                            *opacity = 1.0 - t;
                        }
                        ui.end_row();
                        ui.label(tl!("Angle"));
                        ui.add(egui::DragValue::new(angle).range(-90.0..=90.0).suffix("°"));
                        ui.end_row();
                        ui.label(tl!("Color"));
                        egui::color_picker::color_edit_button_srgb(ui, color);
                        ui.end_row();
                    });
                }
                Dialog::PageText { text, position, size, start } => {
                    grid(ui, &mut |ui| {
                        ui.label(tl!("Text:"));
                        ui.text_edit_singleline(text);
                        ui.end_row();
                        ui.label("");
                        ui.label(egui::RichText::new(tl!("{n} is the page number, {N} the number of pages")).small());
                        ui.end_row();
                        ui.label(tl!("Position"));
                        egui::ComboBox::from_id_salt("pdf-pos").selected_text(position_label(*position)).show_ui(ui, |ui| {
                            for i in 0..POSITIONS.len() {
                                ui.selectable_value(position, i, position_label(i));
                            }
                        });
                        ui.end_row();
                        ui.label(tl!("Size"));
                        ui.add(egui::DragValue::new(size).range(5.0..=48.0).suffix(" pt"));
                        ui.end_row();
                        ui.label(tl!("Start at:"));
                        ui.add(egui::DragValue::new(start).range(-9999..=99999));
                        ui.end_row();
                    });
                }
                Dialog::Properties { title, author, subject, keywords } => {
                    grid(ui, &mut |ui| {
                        for (label, v) in [
                            (tl!("Title"), &mut *title),
                            (tl!("Author"), &mut *author),
                            (tl!("Subject"), &mut *subject),
                            (tl!("Keywords"), &mut *keywords),
                        ] {
                            ui.label(label);
                            ui.text_edit_singleline(v);
                            ui.end_row();
                        }
                    });
                }
                Dialog::Save { password, confirm, flatten, compress } => {
                    grid(ui, &mut |ui| {
                        ui.label(tl!("Password"));
                        ui.add(egui::TextEdit::singleline(password).password(true));
                        ui.end_row();
                        ui.label(tl!("Reenter password"));
                        ui.add(egui::TextEdit::singleline(confirm).password(true));
                        ui.end_row();
                    });
                    ui.checkbox(flatten, tl!("Flatten (every page becomes a picture)"));
                    ui.checkbox(compress, tl!("Compress"));
                    if !password.is_empty() && password != confirm {
                        ui.colored_label(Color32::from_rgb(0xC4, 0x2B, 0x1C), tl!("The passwords don't match"));
                    }
                }
                Dialog::Split { every } => {
                    ui.horizontal(|ui| {
                        ui.label(tl!("Pages per file"));
                        ui.add(egui::DragValue::new(every).range(1..=10_000));
                    });
                    ui.label(egui::RichText::new(tl!("The parts are saved next to the file you name, numbered")).small());
                }
                Dialog::Images { dpi, jpeg } => {
                    ui.horizontal(|ui| {
                        ui.label(tl!("Resolution"));
                        ui.add(egui::DragValue::new(dpi).range(36.0..=600.0).suffix(" dpi"));
                    });
                    ui.horizontal(|ui| {
                        ui.radio_value(jpeg, false, "PNG");
                        ui.radio_value(jpeg, true, "JPEG");
                    });
                }
                Dialog::Signature { strokes, .. } => {
                    ui.label(tl!("Draw your signature"));
                    let (r, resp) = ui.allocate_exact_size(vec2(420.0, 140.0), Sense::drag());
                    ui.painter().rect(r, 4.0, Color32::WHITE, Stroke::new(1.0, Color32::from_gray(160)), egui::StrokeKind::Inside);
                    ui.painter().hline(r.x_range().shrink(20.0), r.max.y - 30.0, Stroke::new(1.0, Color32::from_gray(200)));
                    let k = r.height();
                    if resp.drag_started() {
                        strokes.push(Vec::new());
                    }
                    if resp.dragged()
                        && let Some(p) = resp.interact_pointer_pos()
                        && let Some(s) = strokes.last_mut()
                    {
                        let q = [((p.x - r.min.x) / k).clamp(0.0, 3.0), ((p.y - r.min.y) / k).clamp(0.0, 1.0)];
                        if s.last() != Some(&q) && s.len() < 5000 {
                            s.push(q);
                        }
                    }
                    for s in strokes.iter() {
                        let pts: Vec<Pos2> = s.iter().map(|q| pos2(r.min.x + q[0] * k, r.min.y + q[1] * k)).collect();
                        ui.painter().add(egui::Shape::line(pts, Stroke::new(2.0, Color32::from_rgb(20, 20, 120))));
                    }
                    if ui.button(tl!("Clear")).clicked() {
                        strokes.clear();
                    }
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ok_enabled = match &d {
                    Dialog::Save { password, confirm, .. } => !password.is_empty() && password == confirm,
                    Dialog::Watermark { text, .. } => !text.trim().is_empty(),
                    Dialog::Signature { strokes, .. } => strokes.iter().any(|s| s.len() > 1),
                    _ => true,
                };
                if ui.add_enabled(ok_enabled, egui::Button::new(tl!("OK"))).clicked() {
                    done = true;
                }
                if ui.button(tl!("Cancel")).clicked() {
                    cancel = true;
                }
            });
        });
    if cancel {
        open = false;
    }
    if done {
        apply_dialog(app, d);
    } else if open {
        app.pdf.ed.dialog = Some(d);
    }
}

fn apply_dialog(app: &mut WordApp, d: Dialog) {
    match d {
        Dialog::Watermark { text, size, opacity, angle, color } => {
            run(app, "pdf.watermark", json!({"text": text, "size": size, "opacity": opacity, "angle": angle, "color": hex(color)}));
        }
        Dialog::PageText { text, position, size, start } => {
            let pos = POSITIONS.get(position).copied().unwrap_or("bottomCenter");
            run(app, "pdf.pageText", json!({"text": text, "position": pos, "size": size, "start": start}));
        }
        Dialog::Properties { title, author, subject, keywords } => {
            run(app, "pdf.metadata", json!({"title": title, "author": author, "subject": subject, "keywords": keywords}));
        }
        Dialog::Save { password, flatten, compress, .. } => save_as(app, json!({"password": password, "flatten": flatten, "compress": compress})),
        Dialog::Split { every } => {
            let name = format!("{}-1.pdf", app.title_stem());
            let _ = app.ask_file(FileDialogRequest::Save { name }, AfterPick::Pdf(PdfPick::Split { every }));
        }
        Dialog::Images { dpi, jpeg } => {
            let name = format!("{}-1.{}", app.title_stem(), if jpeg { "jpg" } else { "png" });
            let _ = app.ask_file(FileDialogRequest::Save { name }, AfterPick::Pdf(PdfPick::Images { dpi, jpeg }));
        }
        Dialog::Signature { strokes, place } => {
            app.pdf.ed.signature = strokes.into_iter().filter(|s| s.len() > 1).collect();
            app.pdf.ed.tool = Tool::Signature;
            if let Some((page, rect)) = place {
                sign(app, page, rect);
            }
        }
    }
}

/// Page actions, for the page's menu and the Pages panel.
fn page_menu_items(app: &mut WordApp, ui: &mut Ui, page: usize) {
    let count = app.pdf.shown.as_ref().map_or(0, |s| s.sizes.len());
    let n = page + 1;
    ui.separator();
    if ui.button(tl!("Rotate Right 90°")).clicked() {
        run(app, "pdf.rotate", json!({"page": n, "by": 90}));
        ui.close();
    }
    if ui.button(tl!("Rotate Left 90°")).clicked() {
        run(app, "pdf.rotate", json!({"page": n, "by": -90}));
        ui.close();
    }
    if ui.button(tl!("Duplicate Page")).clicked() {
        run(app, "pdf.duplicatePage", json!({"page": n}));
        ui.close();
    }
    if ui.button(tl!("Insert Blank Page")).clicked() {
        let (w, h) = app.pdf.shown.as_ref().and_then(|s| s.sizes.get(page).copied()).unwrap_or((612.0, 792.0));
        run(app, "pdf.insertBlank", json!({"at": n + 1, "width": w, "height": h}));
        ui.close();
    }
    if ui.button(tl!("Insert Pages from PDF…")).clicked() {
        let _ = app.ask_file(FileDialogRequest::Open { purpose: "pdf".into() }, AfterPick::Pdf(PdfPick::InsertPdf { at: n }));
        ui.close();
    }
    if ui.add_enabled(page > 0, egui::Button::new(tl!("Move Up"))).clicked() {
        run(app, "pdf.movePage", json!({"page": n, "to": n - 1}));
        app.pdf.ed.page_sel = Some(page - 1);
        ui.close();
    }
    if ui.add_enabled(n < count, egui::Button::new(tl!("Move Down"))).clicked() {
        run(app, "pdf.movePage", json!({"page": n, "to": n + 1}));
        app.pdf.ed.page_sel = Some(page + 1);
        ui.close();
    }
    if ui.button(tl!("Extract Page…")).clicked() {
        let name = format!("{}-{n}.pdf", app.title_stem());
        let _ = app.ask_file(FileDialogRequest::Save { name }, AfterPick::Pdf(PdfPick::Extract { pages: vec![page] }));
        ui.close();
    }
    if ui.add_enabled(count > 1, egui::Button::new(tl!("Delete Page"))).clicked() {
        run(app, "pdf.deletePages", json!({"page": n}));
        ui.close();
    }
}

/// The Pages panel: thumbnails to go to a page, and its actions.
pub fn pages_panel(app: &mut WordApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let Some((version, sizes)) = app.pdf.shown.as_ref().map(|s| (s.version, s.sizes.clone())) else { return };
    let current = app.pdf.ed.page_sel.unwrap_or(app.pdf.page).min(sizes.len().saturating_sub(1));
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        ui.label(egui::RichText::new(tl!("Pages")).strong().color(t.text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let more = icon_button(ui, &t, "more", false, "", tl!("Page actions"));
            egui::Popup::menu(&more).show(|ui| page_menu_items(app, ui, current));
        });
    });
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
        let n = current + 1;
        if icon_button(ui, &t, "rotate", false, "", tl!("Rotate Right 90°")).clicked() {
            run(app, "pdf.rotate", json!({"page": n, "by": 90}));
        }
        if icon_button(ui, &t, "duplicatePage", false, "", tl!("Duplicate Page")).clicked() {
            run(app, "pdf.duplicatePage", json!({"page": n}));
        }
        if icon_button(ui, &t, "blankPage", false, "", tl!("Insert Blank Page")).clicked() {
            let (w, h) = sizes.get(current).copied().unwrap_or((612.0, 792.0));
            run(app, "pdf.insertBlank", json!({"at": n + 1, "width": w, "height": h}));
        }
        if icon_button(ui, &t, "chevronUp", false, "", tl!("Move Up")).clicked() && current > 0 {
            run(app, "pdf.movePage", json!({"page": n, "to": n - 1}));
            app.pdf.ed.page_sel = Some(current - 1);
        }
        if icon_button(ui, &t, "chevronDown", false, "", tl!("Move Down")).clicked() && n < sizes.len() {
            run(app, "pdf.movePage", json!({"page": n, "to": n + 1}));
            app.pdf.ed.page_sel = Some(current + 1);
        }
        if icon_button(ui, &t, "deletePage", false, "", tl!("Delete Page")).clicked() && sizes.len() > 1 {
            run(app, "pdf.deletePages", json!({"page": n}));
        }
    });
    ui.separator();
    let ppp = ui.ctx().pixels_per_point();
    egui::ScrollArea::vertical().id_salt("pdf-thumbs").auto_shrink([false, false]).show(ui, |ui| {
        let w = (ui.available_width() - 28.0).max(40.0);
        for (i, (pw, ph)) in sizes.iter().enumerate() {
            let h = w * ph / pw.max(1.0);
            let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h + 26.0), Sense::click());
            let r = Rect::from_min_size(pos2(row.center().x - w / 2.0, row.min.y + 6.0), vec2(w, h));
            let on = i == current;
            if on {
                ui.painter().rect_filled(row.shrink(2.0), 4.0, t.accent.gamma_multiply(0.18));
            }
            ui.painter().rect_filled(r, 0.0, Color32::WHITE);
            if let Some((_, tex)) = app.pdf.thumbs.get(&i) {
                ui.painter().image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            ui.painter().rect_stroke(
                r,
                0.0,
                Stroke::new(if on { 2.0 } else { 1.0 }, if on { t.accent } else { t.border }),
                egui::StrokeKind::Outside,
            );
            ui.painter().text(
                pos2(row.center().x, r.max.y + 11.0),
                Align2::CENTER_CENTER,
                (i + 1).to_string(),
                egui::FontId::proportional(11.0),
                t.text_dim,
            );
            if ui.is_rect_visible(r) && app.pdf.thumbs.get(&i).is_none_or(|th| th.0 != version) && app.pdf.thumbs_pending.insert((version, i)) {
                let scale = (w * ppp / pw.max(1.0)).clamp(0.05, 2.0);
                if let Some(wk) = app.pdf.worker.as_mut() {
                    wk.send(Job::Render { version, page: i, scale, key: 0, thumb: true });
                }
            }
            if resp.clicked() {
                app.pdf.ed.page_sel = Some(i);
                app.pdf.scroll_to = Some(i);
            }
            resp.context_menu(|ui| {
                app.pdf.ed.page_sel = Some(i);
                page_menu_items(app, ui, i);
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, x: f32, y: f32, spans: Vec<(f32, f32)>) -> TextLine {
        let w = spans.last().map_or(0.0, |s| s.1) - x;
        TextLine {
            text: text.into(),
            x,
            y,
            w,
            h: 12.0,
            base: y + 10.0,
            size: 11.0,
            font: "Arial".into(),
            bold: false,
            italic: false,
            color: [0, 0, 0],
            word_spans: spans,
        }
    }

    #[test]
    fn text_is_selected_in_reading_order() {
        let lines = vec![
            line("one two three", 10.0, 10.0, vec![(10.0, 30.0), (35.0, 55.0), (60.0, 90.0)]),
            line("four five", 10.0, 30.0, vec![(10.0, 35.0), (40.0, 60.0)]),
            line("six seven", 10.0, 50.0, vec![(10.0, 25.0), (30.0, 60.0)]),
        ];
        let (rects, text) = text_between(&lines, [36.0, 15.0], [56.0, 16.0]);
        assert_eq!(text, "two");
        assert_eq!(rects.len(), 1);
        let (rects, text) = text_between(&lines, [36.0, 15.0], [20.0, 55.0]);
        assert_eq!(text, "two three\nfour five\nsix");
        assert_eq!(rects.len(), 3);
        // Dragged backwards, the same.
        assert_eq!(text_between(&lines, [20.0, 55.0], [36.0, 15.0]).1, "two three\nfour five\nsix");
        assert!(text_between(&lines, [200.0, 200.0], [220.0, 220.0]).0.is_empty());
    }

    use crate::{Services, WordApp};
    use wordcraft_engine::Session;

    type H = egui_kittest::Harness<'static, WordApp>;

    /// The app with a PDF opened (WordCraft's own export of two lines of text).
    fn opened() -> H {
        let mut s = Session::new(wordcraft_doc::Document::new());
        s.run("text.insert", &json!({"text": "Highlight these words please"})).unwrap();
        s.run("text.newParagraph", &json!({})).unwrap();
        s.run("text.insert", &json!({"text": "A second line of text"})).unwrap();
        let pdf = wordcraft_engine::io::save_bytes("x.pdf", &s.doc).unwrap();
        let mut app = WordApp::new(Session::new(wordcraft_doc::Document::new()), Services::default());
        let data = wordcraft_engine::cmd::insert::base64_encode(&pdf);
        app.run("file.open", json!({"path": "/tmp/marked.pdf", "data": data})).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1440.0, 900.0)).build_ui_state(
            |ui, app: &mut WordApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        until(&mut h, |a| !a.pdf.on_screen.0.is_empty() && a.pdf.ed.lines.get(&0).is_some_and(|l| !l.1.is_empty()));
        h
    }

    fn until(h: &mut H, done: impl Fn(&WordApp) -> bool) {
        for _ in 0..600 {
            h.step();
            if done(h.state()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("timed out: {}", h.state().pdf.info());
    }

    fn screen(h: &H, at: [f32; 2]) -> Pos2 {
        let (pages, scale) = &h.state().pdf.on_screen;
        let r = pages.iter().find(|p| p.0 == 0).map(|p| p.1).unwrap();
        to_px(at, r, *scale)
    }

    fn drag(h: &mut H, from: [f32; 2], to: [f32; 2]) {
        let (from, to) = (screen(h, from), screen(h, to));
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        h.input_mut().events.push(egui::Event::PointerMoved(from));
        h.step();
        h.input_mut().events.push(button(from, true));
        h.step();
        for k in 1..=6 {
            let p = from + (to - from) * (k as f32 / 6.0);
            h.input_mut().events.push(egui::Event::PointerMoved(p));
            h.step();
        }
        h.input_mut().events.push(button(to, false));
        h.step();
        h.step();
    }

    #[test]
    fn a_pdf_opens_ready_to_mark_up_and_marks_are_drawn_selected_moved_and_deleted() {
        let mut h = opened();
        assert_eq!(h.state().pdf.mode, PdfMode::Pdf, "a PDF opens as the PDF");
        // Highlight: drag across the first line's words.
        let line = h.state().pdf.ed.lines[&0].1[0].clone();
        assert!(line.text.starts_with("Highlight"), "{}", line.text);
        h.state_mut().run("ui.pdfView", json!({"tool": "highlight"})).unwrap();
        drag(&mut h, [line.x + 1.0, line.y + line.h / 2.0], [line.x + line.w - 1.0, line.y + line.h / 2.0]);
        let edits = h.state_mut().run("pdf.edits", json!({})).unwrap();
        assert_eq!(edits["edits"][0]["kind"], "highlight", "{edits}");
        let r = &edits["edits"][0]["rect"];
        assert!((r[0].as_f64().unwrap() - f64::from(line.x)).abs() < 2.0, "snapped to the words: {r}");
        // Rectangle, then select it, move it with the arrow keys and delete it.
        h.state_mut().run("ui.pdfView", json!({"tool": "rectangle"})).unwrap();
        drag(&mut h, [100.0, 300.0], [200.0, 360.0]);
        let id = h.state().session.pdf.ops.len() as u64;
        assert_eq!(h.state().pdf.ed.selected, Some(id), "the new mark is selected");
        until(&mut h, |a| a.pdf.shown.as_ref().is_some_and(|s| s.marks.contains_key(&id)));
        h.state_mut().run("ui.pdfView", json!({"tool": "select"})).unwrap();
        drag(&mut h, [150.0, 330.0], [170.0, 340.0]);
        let b = h.state().session.pdf.mark(id).unwrap().1.bounds();
        assert!((b.x - 120.0).abs() < 1.5 && (b.y - 310.0).abs() < 1.5, "moved by the drag: {b:?}");
        h.key_press(egui::Key::Delete);
        h.step();
        assert!(h.state().session.pdf.mark(id).is_none(), "deleted");
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.step();
        assert!(h.state().session.pdf.mark(id).is_some(), "undone");
    }

    #[test]
    fn a_text_box_is_dragged_out_and_typed_into() {
        let mut h = opened();
        h.state_mut().run("ui.pdfView", json!({"tool": "textBox"})).unwrap();
        drag(&mut h, [300.0, 400.0], [500.0, 460.0]);
        assert!(h.state().pdf.ed.edit.is_some(), "an editor opens on the box");
        h.step();
        h.input_mut().events.push(egui::Event::Text("Signed off".into()));
        h.step();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
        h.step();
        let edits = h.state_mut().run("pdf.edits", json!({})).unwrap();
        assert_eq!(edits["edits"][0]["kind"], "textBox", "{edits}");
        assert_eq!(edits["edits"][0]["text"], "Signed off");
        // A second box, finished by a click on the page (which starts nothing else).
        drag(&mut h, [300.0, 500.0], [500.0, 540.0]);
        h.step();
        h.input_mut().events.push(egui::Event::Text("Second".into()));
        h.step();
        let at = screen(&h, [100.0, 650.0]);
        h.input_mut().events.push(egui::Event::PointerMoved(at));
        h.step();
        for pressed in [true, false] {
            h.input_mut().events.push(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        h.step();
        h.step();
        let edits = h.state_mut().run("pdf.edits", json!({})).unwrap();
        assert_eq!(edits["edits"][1]["text"], "Second", "{edits}");
        assert!(h.state().pdf.ed.edit.is_none(), "the click only finished the text");
        // The same with the press and release in frames of their own.
        drag(&mut h, [300.0, 560.0], [500.0, 600.0]);
        h.step();
        h.input_mut().events.push(egui::Event::Text("Third".into()));
        h.step();
        for pressed in [true, false] {
            h.input_mut().events.push(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
            h.step();
        }
        h.step();
        let edits = h.state_mut().run("pdf.edits", json!({})).unwrap();
        assert_eq!(edits["edits"][2]["text"], "Third", "{edits}");
        assert!(h.state().pdf.ed.edit.is_none(), "the click only finished the text");
        // Edit Text: a click on the PDF's own line opens it to change.
        h.state_mut().run("ui.pdfView", json!({"tool": "editText"})).unwrap();
        let line = h.state().pdf.ed.lines[&0].1[1].clone();
        let at = screen(&h, [line.x + 4.0, line.y + line.h / 2.0]);
        h.input_mut().events.push(egui::Event::PointerMoved(at));
        h.step();
        for pressed in [true, false] {
            h.input_mut().events.push(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
            h.step();
        }
        let ed = h.state().pdf.ed.edit.as_ref().map(|e| e.text.clone());
        assert_eq!(ed.as_deref(), Some(line.text.as_str()));
    }

    #[test]
    fn handles_resize_the_box() {
        let b = PRect::new(10.0, 10.0, 100.0, 50.0);
        let r = resized(b, 4, [150.0, 100.0]);
        assert_eq!((r.x, r.y, r.w, r.h), (10.0, 10.0, 140.0, 90.0));
        let r = resized(b, 0, [200.0, 200.0]);
        assert!(r.w >= 0.0 && r.h >= 0.0, "dragged past the other corner, still a box");
    }
}

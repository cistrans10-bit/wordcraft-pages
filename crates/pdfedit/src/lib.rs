//! WordCraft's PDF editing: everything the PDF view can do to a PDF, as a list of edits replayed
//! over it.
//!
//! An edit ([`Op`]) is markup (highlights, ink, shapes, text boxes, notes, stamps, pictures,
//! signatures), a change to the page text (whiteout, edited text, true redaction), a change to
//! the pages (rotate, delete, move, duplicate, insert blank pages or another PDF, crop, flip and
//! scale), a stamp on every page (watermark, page numbers, headers and footers), the document's
//! properties, or a form field's value. [`apply`] replays a list of them over a PDF and writes the
//! result, optionally encrypted, compressed or flattened to pictures.
//!
//! Replaying keeps every edit editable until the PDF is saved (move a text box, change a
//! highlight's colour, undo a deleted page), and lets the same edits ride over a PDF made again
//! from the document after it changes (WordCraft's DOCX + PDF view).
//!
//! **Coordinates.** Edits are placed in page display coordinates: points from the top left of the
//! page as it is shown, rotation and crop applied, y down, which is what the PDF view draws and
//! the pointer gives. They are mapped into each page's own space when applied, so markup lands
//! where it was drawn on a rotated or cropped page, and text reads upright.
//!
//! **Text** in markup is drawn as glyph outlines from WordCraft's fonts, so any script shows the
//! same in every PDF reader, embedded nothing; the annotation's contents carry the text itself.
//!
//! Annotations are real PDF annotations with appearance streams (other readers show, move and
//! delete them); whiteout, edited text, stamps on every page, pictures placed into the page and
//! redaction go into the page itself. Redaction renders the page to pixels with the boxes black
//! and puts the picture in place of the page, so the text under them is gone, not hidden.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod apply;
mod draw;
mod forms;
mod pages;

use std::sync::Arc;

pub use apply::{apply, decrypt, extract, has_encryption, is_encrypted, mark_pages, split};
pub use forms::{Field, FieldKind, fields};

/// An RGB colour.
pub type Color = [u8; 3];

/// A box in page display coordinates (points from the top left of the page as shown, y down).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }.normalized()
    }

    /// The same box with a positive width and height.
    pub fn normalized(self) -> Rect {
        let (x0, x1) = if self.w < 0.0 { (self.x + self.w, self.x) } else { (self.x, self.x + self.w) };
        let (y0, y1) = if self.h < 0.0 { (self.y + self.h, self.y) } else { (self.y, self.y + self.h) };
        Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
    }

    pub fn is_sane(&self) -> bool {
        [self.x, self.y, self.w, self.h].iter().all(|v| v.is_finite() && v.abs() < 1.0e6) && self.w >= 0.0 && self.h >= 0.0
    }

    pub fn contains(&self, p: [f32; 2]) -> bool {
        p[0] >= self.x && p[0] <= self.x + self.w && p[1] >= self.y && p[1] <= self.y + self.h
    }

    pub fn translate(self, dx: f32, dy: f32) -> Rect {
        Rect { x: self.x + dx, y: self.y + dy, ..self }
    }
}

/// The fonts text boxes and stamps can use (any family WordCraft has; these are the defaults).
pub const DEFAULT_FONT: &str = "Arial";

/// A piece of markup or a change to a page's text.
#[derive(Debug, Clone, PartialEq)]
pub enum Mark {
    /// Text highlighted (a translucent box per line).
    Highlight {
        rects: Vec<Rect>,
        color: Color,
    },
    Underline {
        rects: Vec<Rect>,
        color: Color,
    },
    Strikeout {
        rects: Vec<Rect>,
        color: Color,
    },
    /// Freehand drawing: strokes of points.
    Ink {
        strokes: Vec<Vec<[f32; 2]>>,
        color: Color,
        width: f32,
    },
    /// A line, with an arrowhead at its end.
    Line {
        from: [f32; 2],
        to: [f32; 2],
        color: Color,
        width: f32,
        arrow: bool,
    },
    Rectangle {
        rect: Rect,
        stroke: Option<Color>,
        fill: Option<Color>,
        width: f32,
    },
    Ellipse {
        rect: Rect,
        stroke: Option<Color>,
        fill: Option<Color>,
        width: f32,
    },
    /// A word-wrapping text box. With `fit`, the font size is whatever makes the text fill the
    /// box (resizing the box resizes the text); `size` is then where fitting starts.
    TextBox {
        rect: Rect,
        text: String,
        size: f32,
        color: Color,
        font: String,
        bold: bool,
        italic: bool,
        fit: bool,
        fill: Option<Color>,
        border: Option<Color>,
    },
    /// A sticky note at a point.
    Note {
        at: [f32; 2],
        text: String,
        color: Color,
    },
    /// A rubber stamp: a word in a frame ("APPROVED", "DRAFT"…).
    Stamp {
        rect: Rect,
        text: String,
        color: Color,
    },
    /// A picture (PNG, JPEG…), stretched to the box.
    Image {
        rect: Rect,
        data: Arc<Vec<u8>>,
    },
    /// White over the page (in the page itself).
    Whiteout {
        rect: Rect,
    },
    /// The page's text in `rect` replaced: the background colour (`bg`, white by default) over
    /// it, and `text` drawn on the baseline in the given font (the PDF view's Edit Text, matched
    /// to the original).
    EditText {
        rect: Rect,
        base: f32,
        text: String,
        size: f32,
        color: Color,
        font: String,
        bold: bool,
        italic: bool,
        bg: Option<Color>,
    },
    /// True redaction: the page becomes a picture of itself with these boxes black.
    Redact {
        rects: Vec<Rect>,
    },
}

impl Mark {
    /// The kind, as the commands name it.
    pub fn kind(&self) -> &'static str {
        match self {
            Mark::Highlight { .. } => "highlight",
            Mark::Underline { .. } => "underline",
            Mark::Strikeout { .. } => "strikeout",
            Mark::Ink { .. } => "ink",
            Mark::Line { arrow: true, .. } => "arrow",
            Mark::Line { .. } => "line",
            Mark::Rectangle { .. } => "rectangle",
            Mark::Ellipse { .. } => "ellipse",
            Mark::TextBox { .. } => "textBox",
            Mark::Note { .. } => "note",
            Mark::Stamp { .. } => "stamp",
            Mark::Image { .. } => "image",
            Mark::Whiteout { .. } => "whiteout",
            Mark::EditText { .. } => "editText",
            Mark::Redact { .. } => "redact",
        }
    }

    /// The box the mark covers (for selecting and moving it).
    pub fn bounds(&self) -> Rect {
        fn union(rs: impl Iterator<Item = Rect>) -> Rect {
            let mut out: Option<(f32, f32, f32, f32)> = None;
            for r in rs {
                let (x0, y0, x1, y1) = (r.x, r.y, r.x + r.w, r.y + r.h);
                out = Some(match out {
                    None => (x0, y0, x1, y1),
                    Some((a, b, c, d)) => (a.min(x0), b.min(y0), c.max(x1), d.max(y1)),
                });
            }
            out.map_or(Rect::default(), |(a, b, c, d)| Rect { x: a, y: b, w: c - a, h: d - b })
        }
        let pts =
            |ps: &mut dyn Iterator<Item = [f32; 2]>, pad: f32| union(ps.map(|p| Rect { x: p[0] - pad, y: p[1] - pad, w: 2.0 * pad, h: 2.0 * pad }));
        match self {
            Mark::Highlight { rects, .. } | Mark::Underline { rects, .. } | Mark::Strikeout { rects, .. } | Mark::Redact { rects } => {
                union(rects.iter().copied())
            }
            Mark::Ink { strokes, width, .. } => pts(&mut strokes.iter().flatten().copied(), width / 2.0 + 1.0),
            Mark::Line { from, to, width, .. } => pts(&mut [*from, *to].into_iter(), width * 2.0 + 2.0),
            Mark::Rectangle { rect, .. }
            | Mark::Ellipse { rect, .. }
            | Mark::TextBox { rect, .. }
            | Mark::Stamp { rect, .. }
            | Mark::Image { rect, .. }
            | Mark::Whiteout { rect }
            | Mark::EditText { rect, .. } => *rect,
            Mark::Note { at, .. } => Rect { x: at[0], y: at[1], w: NOTE_SIZE, h: NOTE_SIZE },
        }
    }

    /// The mark moved by (dx, dy).
    pub fn moved(&self, dx: f32, dy: f32) -> Mark {
        let mv = |r: &Rect| r.translate(dx, dy);
        let mp = |p: &[f32; 2]| [p[0] + dx, p[1] + dy];
        let mut m = self.clone();
        match &mut m {
            Mark::Highlight { rects, .. } | Mark::Underline { rects, .. } | Mark::Strikeout { rects, .. } | Mark::Redact { rects } => {
                rects.iter_mut().for_each(|r| *r = mv(r))
            }
            Mark::Ink { strokes, .. } => strokes.iter_mut().flatten().for_each(|p| *p = mp(p)),
            Mark::Line { from, to, .. } => {
                *from = mp(from);
                *to = mp(to);
            }
            Mark::Rectangle { rect, .. }
            | Mark::Ellipse { rect, .. }
            | Mark::TextBox { rect, .. }
            | Mark::Stamp { rect, .. }
            | Mark::Image { rect, .. }
            | Mark::Whiteout { rect } => *rect = mv(rect),
            Mark::EditText { rect, base, .. } => {
                *rect = mv(rect);
                *base += dy;
            }
            Mark::Note { at, .. } => *at = mp(at),
        }
        m
    }

    /// The mark stretched from its bounds to `to` (corner dragging). Text boxes keep their text,
    /// which refits when they fit it; ink and lines scale their points.
    pub fn resized(&self, to: Rect) -> Mark {
        let from = self.bounds();
        let to = to.normalized();
        let sx = if from.w > 0.01 { to.w / from.w } else { 1.0 };
        let sy = if from.h > 0.01 { to.h / from.h } else { 1.0 };
        let map = |p: [f32; 2]| [to.x + (p[0] - from.x) * sx, to.y + (p[1] - from.y) * sy];
        let map_r = |r: &Rect| {
            let a = map([r.x, r.y]);
            Rect { x: a[0], y: a[1], w: r.w * sx, h: r.h * sy }
        };
        let mut m = self.clone();
        match &mut m {
            Mark::Highlight { rects, .. } | Mark::Underline { rects, .. } | Mark::Strikeout { rects, .. } | Mark::Redact { rects } => {
                rects.iter_mut().for_each(|r| *r = map_r(r))
            }
            Mark::Ink { strokes, .. } => strokes.iter_mut().flatten().for_each(|p| *p = map(*p)),
            Mark::Line { from: a, to: b, .. } => {
                *a = map(*a);
                *b = map(*b);
            }
            Mark::Rectangle { rect, .. }
            | Mark::Ellipse { rect, .. }
            | Mark::TextBox { rect, .. }
            | Mark::Stamp { rect, .. }
            | Mark::Image { rect, .. }
            | Mark::Whiteout { rect }
            | Mark::EditText { rect, .. } => *rect = to,
            Mark::Note { at, .. } => *at = [to.x, to.y],
        }
        m
    }

    /// The text a mark carries (text boxes, notes, stamps, edited text).
    pub fn text(&self) -> Option<&str> {
        match self {
            Mark::TextBox { text, .. } | Mark::Note { text, .. } | Mark::Stamp { text, .. } | Mark::EditText { text, .. } => Some(text),
            _ => None,
        }
    }
}

/// A sticky note's icon side, points.
pub const NOTE_SIZE: f32 = 22.0;

/// Where on the page a header, footer or page number goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl Position {
    pub fn parse(s: &str) -> Option<Position> {
        Some(match s {
            "topLeft" => Position::TopLeft,
            "topCenter" | "top" => Position::TopCenter,
            "topRight" => Position::TopRight,
            "bottomLeft" => Position::BottomLeft,
            "bottomCenter" | "bottom" => Position::BottomCenter,
            "bottomRight" => Position::BottomRight,
            _ => return None,
        })
    }
}

/// One edit to a PDF. Pages are counted from 0, as they stand when the edit is applied (after the
/// edits before it).
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Markup or a change to the page text; `id` lets the editor find it again.
    Mark {
        id: u64,
        page: usize,
        mark: Mark,
    },
    /// Turn pages by a multiple of 90° (clockwise).
    Rotate {
        pages: Vec<usize>,
        by: i32,
    },
    Delete {
        pages: Vec<usize>,
    },
    /// Move a page to stand at `to`.
    Move {
        from: usize,
        to: usize,
    },
    Duplicate {
        page: usize,
    },
    /// A blank page of this size (points) inserted at `at`.
    InsertBlank {
        at: usize,
        width: f32,
        height: f32,
    },
    /// Another PDF's pages inserted at `at`.
    InsertPdf {
        at: usize,
        data: Arc<Vec<u8>>,
    },
    /// Show only this part of the page (display coordinates).
    Crop {
        page: usize,
        rect: Rect,
    },
    /// Mirror and scale the page's content about its centre.
    Transform {
        pages: Vec<usize>,
        flip_h: bool,
        flip_v: bool,
        scale: f32,
    },
    /// Text across every page (or these), turned by `angle` degrees.
    Watermark {
        text: String,
        size: f32,
        color: Color,
        opacity: f32,
        angle: f32,
        pages: Option<Vec<usize>>,
    },
    /// Text at the top or bottom of every page (or these): page numbers, headers, footers.
    /// `{n}` is the page number (from `start`), `{N}` the page count.
    PageText {
        text: String,
        position: Position,
        size: f32,
        color: Color,
        start: i64,
        pages: Option<Vec<usize>>,
    },
    /// Document properties (title, author, subject, keywords); `None` leaves one as it is.
    Metadata {
        title: Option<String>,
        author: Option<String>,
        subject: Option<String>,
        keywords: Option<String>,
    },
    /// A form field's value: text, the chosen option, or for check boxes and radio buttons the
    /// state to turn on ("Off" turns them off).
    Fill {
        field: String,
        value: String,
    },
}

impl Op {
    /// A short description, for the list of edits.
    pub fn describe(&self) -> String {
        match self {
            Op::Mark { id, page, mark } => format!("{} #{id} on page {}", mark.kind(), page + 1),
            Op::Rotate { pages, by } => format!("rotate {} page(s) by {by}°", pages.len()),
            Op::Delete { pages } => format!("delete {} page(s)", pages.len()),
            Op::Move { from, to } => format!("move page {} to {}", from + 1, to + 1),
            Op::Duplicate { page } => format!("duplicate page {}", page + 1),
            Op::InsertBlank { at, .. } => format!("insert a blank page at {}", at + 1),
            Op::InsertPdf { at, .. } => format!("insert a PDF at page {}", at + 1),
            Op::Crop { page, .. } => format!("crop page {}", page + 1),
            Op::Transform { pages, .. } => format!("flip or scale {} page(s)", pages.len()),
            Op::Watermark { text, .. } => format!("watermark \"{text}\""),
            Op::PageText { text, .. } => format!("page text \"{text}\""),
            Op::Metadata { .. } => "document properties".into(),
            Op::Fill { field, .. } => format!("fill field \"{field}\""),
        }
    }
}

/// How [`apply`] writes the PDF.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SaveOptions {
    /// Encrypt with AES-128, needing this password to open.
    pub password: Option<String>,
    /// Every page turned into a picture of itself (nothing selectable or editable left).
    pub flatten: bool,
    /// Streams compressed and unused objects dropped.
    pub compress: bool,
}

/// Why a PDF couldn't be edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<lopdf::Error> for Error {
    fn from(e: lopdf::Error) -> Error {
        Error(format!("PDF: {e}"))
    }
}

/// Most pages a PDF may grow to through edits (inserting, duplicating).
pub const MAX_PAGES: usize = 10_000;

#[cfg(test)]
mod tests;

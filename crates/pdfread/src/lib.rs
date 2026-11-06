//! WordCraft's PDF reading, for working on a document as a Word document and as a PDF at once:
//!
//! - [`PdfView`] opens a PDF and renders its pages to pixels, for the PDF view beside the page
//!   (View › PDF, or DOCX + PDF side by side), whether the PDF is one the user opened or the
//!   document's own export, re-made as it is edited.
//! - [`to_docx`] converts a PDF into an editable Word document, page for page: every PDF page is
//!   one section of the same size, its text becomes paragraphs at their places with their fonts,
//!   sizes, colours and weights, and its pictures and drawing are anchored to the page behind the
//!   text. The engine opens a PDF through it (File › Open).
//!
//! Parsing and rendering are hayro's (pure Rust). Input is hostile: nothing here panics, sizes and
//! counts are capped, and a page that can't be read gives an empty page rather than an error.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod convert;
mod extract;
mod fonts;
mod view;

pub use convert::to_docx;
pub use view::{PageImage, PdfView};

/// A line of a page's text (one segment of it: a table cell or column is its own line), in page
/// display coordinates: points from the top left of the page as shown (rotation applied).
#[derive(Debug, Clone, PartialEq)]
pub struct TextLine {
    pub text: String,
    /// The line's box (left, top, width, height) and its baseline.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub base: f32,
    /// The size, font family, weight, slant and colour of its first word, as the document shows
    /// them (a font the reader lacks is sized like the original).
    pub size: f32,
    pub font: String,
    pub bold: bool,
    pub italic: bool,
    pub color: [u8; 3],
    /// Each word's left and right edge, for finding where a match sits in the line.
    pub word_spans: Vec<(f32, f32)>,
}

/// Pages read from one PDF, at most (a larger file opens its first pages).
pub const MAX_PAGES: usize = 5000;

/// Why a PDF couldn't be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// Does `bytes` look like a PDF (`%PDF-` within its first kilobyte, as readers accept)?
pub fn is_pdf(bytes: &[u8]) -> bool {
    let head = bytes.get(..bytes.len().min(1024)).unwrap_or(&[]);
    head.windows(5).any(|w| w == b"%PDF-")
}

#[cfg(test)]
mod tests;

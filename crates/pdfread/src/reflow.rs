//! PDF → Word document as flowing text (the PdfToDocx style): each block of text becomes one
//! plain paragraph, in reading order from the top of the page down, with pictures inline where
//! they come and, optionally, a page break after each PDF page, on A4 with 2 cm margins.
//!
//! Unlike [`crate::to_docx`] it doesn't keep the PDF's layout (positions, frames, drawing): the
//! result is ordinary text that reflows and edits like any document.
//!
//! Blocks: a page's lines (see `convert::build_lines`) are taken one column segment at a time; a
//! segment joins the block above it when it overlaps it across, sits within 1.6 lines of it and
//! is set at about the same size. A block's text is its lines joined by spaces, with words broken
//! by a hyphen at the end of a line joined again. Its size, weight and slant are its first
//! word's. Background pictures on a page with text (page-sized, or one picture tiled across it)
//! are left out.

use crate::Error;
use crate::convert::{Line, Package, Seg, build_lines, emu, xml};
use crate::extract::{PageContent, Picture};
use hayro::hayro_interpret::hayro_syntax::Pdf;
use std::fmt::Write as _;

/// The widest a picture is set, points: A4's text width between 2 cm margins.
const MAX_PICTURE_WIDTH: f64 = 481.0;

/// What the flowing conversion includes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlowOptions {
    /// The PDF's pictures, inline where they come on the page.
    pub images: bool,
    /// A page break after each PDF page.
    pub page_breaks: bool,
}

impl Default for FlowOptions {
    fn default() -> Self {
        FlowOptions { images: true, page_breaks: true }
    }
}

/// Convert a PDF to a Word document of flowing text (.docx package bytes).
pub fn to_docx_flowing(bytes: &[u8], opts: FlowOptions) -> Result<Vec<u8>, Error> {
    let pdf = Pdf::new(bytes.to_vec()).map_err(|e| Error(format!("not a PDF this reader understands ({e:?})")))?;
    let pages = pdf.pages();
    let count = pages.len().min(crate::MAX_PAGES);
    if count == 0 {
        return Err(Error("the PDF has no pages".into()));
    }
    let mut pkg = Package::default();
    let mut body = String::new();
    for (i, page) in pages.iter().take(count).enumerate() {
        let content = crate::extract::page_content(&pdf, page);
        body.push_str(&page_xml(&content, &mut pkg, opts));
        if opts.page_breaks && i + 1 < count {
            body.push_str("<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>");
        }
    }
    // A4, 2 cm margins.
    body.push_str(
        "<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
         <w:pgMar w:top=\"1134\" w:right=\"1134\" w:bottom=\"1134\" w:left=\"1134\" w:header=\"709\" w:footer=\"709\" w:gutter=\"0\"/></w:sectPr>",
    );
    pkg.write(&body)
}

/// A block of text: its lines' segments, top to bottom.
struct Block<'a> {
    segs: Vec<(&'a Line, &'a Seg)>,
    left: f64,
    right: f64,
}

impl Block<'_> {
    fn top(&self) -> f64 {
        self.segs.first().map_or(0.0, |(l, _)| l.base + l.size)
    }
    fn last(&self) -> Option<&(&Line, &Seg)> {
        self.segs.last()
    }
}

fn blocks(lines: &[Line]) -> Vec<Block<'_>> {
    let mut out: Vec<Block<'_>> = Vec::new();
    for line in lines {
        for seg in &line.segs {
            if seg.words.is_empty() {
                continue;
            }
            let (l, r) = (seg.left(), seg.right());
            let joins = out.iter().rposition(|b| {
                let Some((pl, _)) = b.last() else { return false };
                let gap = pl.base - line.base;
                let same_size = (pl.size - line.size).abs() <= pl.size.max(line.size) * 0.15;
                let across = l < b.right && r > b.left;
                // A line below (not beside: segments of one line are separate blocks).
                gap > 0.0 && gap <= 1.6 * pl.size.max(line.size) && same_size && across
            });
            match joins.and_then(|i| out.get_mut(i)) {
                Some(b) => {
                    b.segs.push((line, seg));
                    b.left = b.left.min(l);
                    b.right = b.right.max(r);
                }
                None => out.push(Block { segs: vec![(line, seg)], left: l, right: r }),
            }
        }
    }
    out
}

/// A block's text: its lines joined, a line-end hyphen between lowercase letters taken out.
fn block_text(b: &Block<'_>) -> String {
    let lines: Vec<String> = b.segs.iter().map(|(_, s)| s.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")).collect();
    let mut text = String::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let joined =
            text.ends_with('-') && text.chars().rev().nth(1).is_some_and(char::is_lowercase) && line.chars().next().is_some_and(char::is_lowercase);
        if joined {
            text.pop();
        } else if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(line);
    }
    text.chars().filter(|c| *c == '\t' || !c.is_control()).collect()
}

fn paragraph_xml(b: &Block<'_>) -> Option<String> {
    let text = block_text(b);
    if text.is_empty() {
        return None;
    }
    let st = &b.segs.first()?.1.words.first()?.style;
    let font = xml(&st.font);
    let mut r =
        format!("<w:p><w:pPr><w:spacing w:after=\"120\"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii=\"{font}\" w:hAnsi=\"{font}\" w:cs=\"{font}\"/>");
    if st.bold {
        r.push_str("<w:b/><w:bCs/>");
    }
    if st.italic {
        r.push_str("<w:i/><w:iCs/>");
    }
    let _ = write!(
        r,
        "<w:sz w:val=\"{0}\"/><w:szCs w:val=\"{0}\"/></w:rPr><w:t xml:space=\"preserve\">{1}</w:t></w:r></w:p>",
        st.half_points.max(2),
        xml(&text)
    );
    Some(r)
}

fn picture_xml(pic: &Picture, pkg: &mut Package) -> String {
    let (mut w, mut h) = (pic.bounds.width().max(1.0), pic.bounds.height().max(1.0));
    if w > MAX_PICTURE_WIDTH {
        h *= MAX_PICTURE_WIDTH / w;
        w = MAX_PICTURE_WIDTH;
    }
    let (cx, cy) = (emu(w).max(1), emu(h).max(1));
    let rel = pkg.add_media(pic.data.clone(), pic.ext);
    let id = pkg.id();
    format!(
        "<w:p><w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\"><wp:extent cx=\"{cx}\" cy=\"{cy}\"/>\
         <wp:effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/><wp:docPr id=\"{id}\" name=\"Image {id}\"/>\
         <wp:cNvGraphicFramePr><a:graphicFrameLocks noChangeAspect=\"1\"/></wp:cNvGraphicFramePr>\
         <a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic>\
         <pic:nvPicPr><pic:cNvPr id=\"{id}\" name=\"image{id}\"/><pic:cNvPicPr/></pic:nvPicPr>\
         <pic:blipFill><a:blip r:embed=\"{rel}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
         <pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr>\
         </pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"
    )
}

fn page_xml(c: &PageContent, pkg: &mut Package, opts: FlowOptions) -> String {
    let lines = build_lines(&c.letters);
    // (top of the item, its XML), written top to bottom.
    let mut items: Vec<(f64, String)> = blocks(&lines).iter().filter_map(|b| paragraph_xml(b).map(|x| (b.top(), x))).collect();
    if opts.images {
        // A picture under most of a page with text on it is its background, not content (a
        // scanned page, with no text, keeps its picture).
        // So is one picture repeated across the page (a tiled texture).
        let has_text = !items.is_empty();
        let mut repeats: std::collections::HashMap<&[u8], usize> = std::collections::HashMap::new();
        for p in &c.pictures {
            *repeats.entry(p.data.as_slice()).or_default() += 1;
        }
        let background =
            |p: &Picture| has_text && (p.bounds.area() > c.width * c.height * 0.5 || repeats.get(p.data.as_slice()).is_some_and(|n| *n >= 3));
        for pic in c.pictures.iter().filter(|p| !background(p)) {
            items.push((pic.bounds.y1, picture_xml(pic, pkg)));
        }
    }
    items.sort_by(|a, b| b.0.round().total_cmp(&a.0.round()));
    items.into_iter().map(|(_, x)| x).collect()
}

//! PDF → Word document (.docx bytes), page for page.
//!
//! Every PDF page becomes one section of the same size and orientation, ending in a next-page
//! break, so page n of the document is page n of the PDF. On each page:
//!
//! - **Text.** Letters are gathered into lines (letters on one baseline, left to right), lines
//!   into words (the PDF's spaces, or a gap) and segments (a gap of two ems or more: table cells,
//!   tabbed columns, a label and its value), and consecutive lines of one size and pitch, starting
//!   where the paragraph's lines start (or centred, or flush right with them), into paragraphs.
//!   A short line ends a paragraph; a list marker starts one. Paragraphs are written top to bottom
//!   as one flow with exact line heights, the space before each measured baseline to baseline, and
//!   indents that keep them where they were, so they read, edit and reflow like any document.
//! - **What a flow can't hold** is pinned to the page in a frame (w:framePr) at its exact place:
//!   a paragraph that starts above where the flow has reached (text beside other text) and the
//!   footer band (the bottom eighth of the page: running footers, page numbers, logos), which a
//!   stand-in font setting a little larger could otherwise push onto a page of its own.
//! - **Lines with segments** become one paragraph each, with a tab stop at every segment.
//! - **Pictures** are anchored to the page where they were, behind the text when they sit under
//!   it or fill a quarter of the page; **vector drawing** (rules, table fills, panels, diagrams) is
//!   anchored behind the text as shapes with the PDF's own geometry and colours.
//! - **Fonts** the reader doesn't have are shown in a stand-in: the size is corrected so the
//!   stand-in's capitals are as tall as the original's (fonts draw at very different sizes within
//!   one point size).
//!
//! Not rebuilt: real Word tables and columns, gradients, clipping, rotated and vertical text, and
//! text in scanned pages (OCR it first).

use crate::Error;
use crate::extract::{Letter, PageContent, PathItem, Picture};
use hayro::hayro_interpret::hayro_syntax::Pdf;
use hayro::kurbo::{PathEl, Point, Rect, Shape};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::Arc;

/// Where a line's baseline sits in an exact-height line, as a share of the height.
const BASELINE_SHARE: f64 = 0.8;
const MIN_MARGIN: f64 = 18.0;
const MAX_MARGIN: f64 = 144.0;

/// Convert a PDF to a Word document (.docx package bytes).
pub fn to_docx(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let pdf = Pdf::new(bytes.to_vec()).map_err(|e| Error(format!("not a PDF this reader understands ({e:?})")))?;
    let pages = pdf.pages();
    let count = pages.len().min(crate::MAX_PAGES);
    if count == 0 {
        return Err(Error("the PDF has no pages".into()));
    }
    let mut out = Package::default();
    let mut body = String::new();
    for (i, page) in pages.iter().take(count).enumerate() {
        let content = crate::extract::page_content(&pdf, page);
        body.push_str(&page_xml(&content, &mut out, i + 1 == count));
    }
    out.write(&body)
}

// ── The package ───────────────────────────────────────────────────────

#[derive(Default)]
struct Package {
    /// word/media/<name>, with its bytes.
    media: Vec<(String, Vec<u8>)>,
    next_id: u32,
}

impl Package {
    fn id(&mut self) -> u32 {
        self.next_id = self.next_id.saturating_add(1);
        self.next_id
    }

    /// Adds a picture; its relationship id.
    fn add_media(&mut self, data: Vec<u8>, ext: &str) -> String {
        let n = self.media.len() + 1;
        self.media.push((format!("image{n}.{ext}"), data));
        format!("rIdImg{n}")
    }

    fn write(self, body: &str) -> Result<Vec<u8>, Error> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let err = |e: &dyn std::fmt::Display| Error(format!("couldn't write the document: {e}"));
        let put = |zip: &mut zip::ZipWriter<std::io::Cursor<Vec<u8>>>, name: &str, data: &[u8], o| -> Result<(), Error> {
            zip.start_file(name, o).map_err(|e| err(&e))?;
            zip.write_all(data).map_err(|e| err(&e))
        };
        let mut types = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
             <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
             <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
             <Default Extension=\"png\" ContentType=\"image/png\"/>\
             <Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>",
        );
        types.push_str(
            "<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
             <Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
             </Types>",
        );
        put(&mut zip, "[Content_Types].xml", types.as_bytes(), opts)?;
        put(
            &mut zip,
            "_rels/.rels",
            b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
              <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
              <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
              </Relationships>",
            opts,
        )?;
        let mut rels = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
             <Relationship Id=\"rIdStyles\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>",
        );
        for (i, (name, _)) in self.media.iter().enumerate() {
            let _ = write!(
                rels,
                "<Relationship Id=\"rIdImg{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/{name}\"/>",
                i + 1
            );
        }
        rels.push_str("</Relationships>");
        put(&mut zip, "word/_rels/document.xml.rels", rels.as_bytes(), opts)?;
        put(&mut zip, "word/styles.xml", STYLES.as_bytes(), opts)?;
        let doc = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
             xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
             xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" \
             xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
             xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\" \
             xmlns:wps=\"http://schemas.microsoft.com/office/word/2010/wordprocessingShape\">\
             <w:body>{body}</w:body></w:document>"
        );
        put(&mut zip, "word/document.xml", doc.as_bytes(), opts)?;
        for (name, data) in &self.media {
            // Pictures are compressed already.
            put(&mut zip, &format!("word/media/{name}"), data, stored)?;
        }
        let cursor = zip.finish().map_err(|e| err(&e))?;
        Ok(cursor.into_inner())
    }
}

const STYLES: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Times New Roman\" w:hAnsi=\"Times New Roman\" w:cs=\"Times New Roman\" w:eastAsia=\"Times New Roman\"/>\
<w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/><w:lang w:val=\"en-US\"/></w:rPr></w:rPrDefault>\
<w:pPrDefault><w:pPr><w:spacing w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault></w:docDefaults>\
<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style>\
<w:style w:type=\"character\" w:default=\"1\" w:styleId=\"DefaultParagraphFont\"><w:name w:val=\"Default Paragraph Font\"/><w:uiPriority w:val=\"1\"/><w:semiHidden/></w:style>\
</w:styles>";

// ── A page ─────────────────────────────────────────────────────────────

fn page_xml(c: &PageContent, pkg: &mut Package, last: bool) -> String {
    let (w, h) = (c.width, c.height);
    let paras = build_paras(build_lines(&c.letters));
    let (mut left, mut right, mut top) = (MIN_MARGIN, MIN_MARGIN, MIN_MARGIN);
    if !paras.is_empty() {
        left = clamp(paras.iter().map(Para::left).fold(f64::INFINITY, f64::min), MIN_MARGIN, MAX_MARGIN);
        right = clamp(w - paras.iter().map(Para::right).fold(f64::NEG_INFINITY, f64::max), MIN_MARGIN, MAX_MARGIN);
        top = clamp(paras.iter().map(|p| h - p.first() - BASELINE_SHARE * p.pitch()).fold(f64::INFINITY, f64::min), MIN_MARGIN, MAX_MARGIN);
    }
    // Small, and never below the page's lowest text (footers sit near the edge), so a page
    // whose text runs a little longer than in the PDF still fits on it.
    let bottom =
        if paras.is_empty() { MIN_MARGIN } else { clamp(paras.iter().map(Para::bottom).fold(f64::INFINITY, f64::min) - 4.0, 0.0, MIN_MARGIN) };
    if left + right > w - 72.0 {
        left = ((w - 72.0) / 2.0).max(MIN_MARGIN);
        right = left;
    }
    let text_width = (w - left - right).max(36.0);
    let sect = |kind: Option<&str>| sect_pr(w, h, top, right, bottom, left, kind);

    // Pictures and drawing ride in the page's first paragraph, positioned against the page.
    // Stacking: page-sized pictures (backgrounds) at the back, then the drawing, then the rest.
    let mut anchors = String::new();
    for pic in &c.pictures {
        let large = pic.bounds.area() > w * h * 0.25;
        let behind = large || paras.iter().any(|p| overlaps(&p.bounds(), &pic.bounds));
        anchors.push_str(&picture_xml(pic, pkg, h, behind, if large { 1 } else { 3 }));
    }
    for path in &c.paths {
        anchors.push_str(&shape_xml(path, pkg, h));
    }

    let mut sb = String::new();
    if paras.is_empty() {
        sb.push_str("<w:p><w:pPr><w:spacing w:before=\"0\" w:after=\"0\"/>");
        if !last {
            sb.push_str(&sect(Some("nextPage")));
        }
        sb.push_str("</w:pPr>");
        sb.push_str(&anchors);
        sb.push_str("</w:p>");
        if last {
            sb.push_str(&sect(None));
        }
        return sb;
    }

    let footer_band = h * 0.125;
    let (mut prev_last, mut prev_pitch, mut flow_started) = (0.0, 0.0, false);
    let mut flow: Vec<String> = Vec::new();
    for (i, para) in paras.iter().enumerate() {
        let first = para.first();
        let pitch = para.pitch();
        let before = if flow_started {
            (prev_last - first) - (1.0 - BASELINE_SHARE) * prev_pitch - BASELINE_SHARE * pitch
        } else {
            (h - first) - top - BASELINE_SHARE * pitch
        };
        let footer = para.bottom() < footer_band && i > 0;
        let overlap = flow_started && before < -1.0;
        if footer || overlap {
            let frame = Frame { x: para.left(), y: (h - first) - BASELINE_SHARE * pitch, width: para.right() - para.left() };
            let inner = paragraph_xml(para, 0.0, left, text_width, "", Some(&frame));
            anchors.push_str(&text_box_xml(&inner, &frame, para.lines.len() as f64 * pitch, pkg));
            continue;
        }
        flow.push(paragraph_xml(para, before.max(0.0), left, text_width, ANCHORS, None));
        prev_last = para.last();
        prev_pitch = pitch;
        flow_started = true;
    }
    // The page's pictures, drawing and pinned text ride in its first paragraph.
    let mut anchors = Some(anchors);
    for p in flow {
        match anchors.take() {
            Some(a) => sb.push_str(&p.replacen(ANCHORS, &a, 1).replace(ANCHORS, "")),
            None => sb.push_str(&p.replace(ANCHORS, "")),
        }
    }
    // The section ends in a hairline empty paragraph of its own, which adds no height worth
    // speaking of (and holds the anchors when no paragraph flows on the page).
    sb.push_str("<w:p><w:pPr><w:spacing w:before=\"0\" w:after=\"0\" w:line=\"20\" w:lineRule=\"exact\"/>");
    if !last {
        sb.push_str(&sect(Some("nextPage")));
    }
    sb.push_str("<w:rPr><w:sz w:val=\"2\"/></w:rPr></w:pPr>");
    if let Some(a) = anchors {
        sb.push_str(&a);
    }
    sb.push_str("</w:p>");
    if last {
        sb.push_str(&sect(None));
    }
    sb
}

fn sect_pr(w: f64, h: f64, top: f64, right: f64, bottom: f64, left: f64, kind: Option<&str>) -> String {
    let mut s = String::from("<w:sectPr>");
    if let Some(k) = kind {
        let _ = write!(s, "<w:type w:val=\"{k}\"/>");
    }
    let _ = write!(s, "<w:pgSz w:w=\"{}\" w:h=\"{}\"", tw(w), tw(h));
    if w > h {
        s.push_str(" w:orient=\"landscape\"");
    }
    let _ = write!(
        s,
        "/><w:pgMar w:top=\"{}\" w:right=\"{}\" w:bottom=\"{}\" w:left=\"{}\" w:header=\"0\" w:footer=\"0\" w:gutter=\"0\"/></w:sectPr>",
        tw(top),
        tw(right),
        tw(bottom),
        tw(left)
    );
    s
}

fn overlaps(a: &Rect, b: &Rect) -> bool {
    a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1
}

// ── Text: words, lines, paragraphs ────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
struct Style {
    font: Arc<str>,
    /// Half points, as w:sz counts.
    half_points: u32,
    bold: bool,
    italic: bool,
    color: [u8; 3],
    /// Character spacing, twentieths of a point (w:spacing): what makes the font the document
    /// shows the text in set as wide as the PDF's did.
    spacing: i32,
}

#[derive(Debug, Clone)]
struct Word {
    text: String,
    style: Style,
    left: f64,
    right: f64,
}

/// Words on one line with ordinary spaces between them.
#[derive(Debug, Clone, Default)]
struct Seg {
    words: Vec<Word>,
}

impl Seg {
    fn left(&self) -> f64 {
        self.words.first().map_or(0.0, |w| w.left)
    }
    fn right(&self) -> f64 {
        self.words.last().map_or(0.0, |w| w.right)
    }
}

#[derive(Debug, Clone)]
struct Line {
    base: f64,
    size: f64,
    segs: Vec<Seg>,
}

impl Line {
    fn left(&self) -> f64 {
        self.segs.first().map_or(0.0, Seg::left)
    }
    fn right(&self) -> f64 {
        self.segs.last().map_or(0.0, Seg::right)
    }
    fn is_row(&self) -> bool {
        self.segs.len() > 1
    }
    fn first_word(&self) -> &str {
        self.segs.first().and_then(|s| s.words.first()).map_or("", |w| w.text.as_str())
    }
}

#[derive(Debug, Clone, Default)]
struct Para {
    lines: Vec<Line>,
}

impl Para {
    fn first(&self) -> f64 {
        self.lines.first().map_or(0.0, |l| l.base)
    }
    fn last(&self) -> f64 {
        self.lines.last().map_or(0.0, |l| l.base)
    }
    fn left(&self) -> f64 {
        self.lines.iter().map(Line::left).fold(f64::INFINITY, f64::min)
    }
    fn right(&self) -> f64 {
        self.lines.iter().map(Line::right).fold(f64::NEG_INFINITY, f64::max)
    }
    fn size(&self) -> f64 {
        self.lines.iter().map(|l| l.size).fold(0.0, f64::max)
    }
    fn bottom(&self) -> f64 {
        self.last() - self.lines.last().map_or(0.0, |l| l.size) * 0.25
    }
    fn bounds(&self) -> Rect {
        Rect::new(self.left(), self.bottom(), self.right(), self.first() + self.lines.first().map_or(0.0, |l| l.size) * 0.8)
    }
    /// Line height: the median baseline distance, or 1.2 × the font size for a single line.
    fn pitch(&self) -> f64 {
        if self.lines.len() > 1 {
            let mut gaps: Vec<f64> = self.lines.windows(2).filter_map(|w| Some(w.first()?.base - w.get(1)?.base)).collect();
            gaps.sort_by(f64::total_cmp);
            if let Some(g) = gaps.get(gaps.len() / 2).copied()
                && g > 2.0
                && g < 240.0
            {
                return g;
            }
        }
        clamp(self.size() * 1.2, 4.0, 240.0)
    }
}

/// Letters → lines: letters whose baselines lie within a third of their size of the line's top
/// baseline, left to right, split into words (a space, or a gap) and segments (two ems or more).
fn build_lines(letters: &[Letter]) -> Vec<Line> {
    let scales = font_scales(letters);
    let mut sorted: Vec<&Letter> = letters.iter().filter(|l| l.base.is_finite() && l.x0.is_finite() && l.x1.is_finite()).collect();
    sorted.sort_by(|a, b| b.base.total_cmp(&a.base));
    let mut lines = Vec::new();
    let mut cluster: Vec<&Letter> = Vec::new();
    let mut top = f64::NAN;
    for l in sorted {
        if !cluster.is_empty() && top - l.base > (l.size * 0.3).max(1.0) {
            add_line(&mut lines, &cluster, &scales);
            cluster.clear();
        }
        if cluster.is_empty() {
            top = l.base;
        }
        cluster.push(l);
    }
    add_line(&mut lines, &cluster, &scales);
    lines
}

/// A size correction per font the reader doesn't have (see the module docs), measured from the
/// PDF's own capitals and assuming a typical cap height of 0.7 em.
fn font_scales(letters: &[Letter]) -> HashMap<Arc<str>, f64> {
    let mut ratios: HashMap<Arc<str>, Vec<f64>> = HashMap::new();
    for l in letters.iter().filter(|l| l.ink_height > 0.0 && l.size > 0.0) {
        let r = l.ink_height / l.size;
        if r > 0.2 && r < 1.2 {
            ratios.entry(l.font.clone()).or_default().push(r);
        }
    }
    let db = wordcraft_fonts::FontDb::global();
    let mut out = HashMap::new();
    for (font, mut rs) in ratios {
        if rs.len() < 2 || font.is_empty() || db.has_family(&font) {
            continue;
        }
        rs.sort_by(f64::total_cmp);
        let Some(ratio) = rs.get(rs.len() / 2).copied() else { continue };
        if !(0.62..=0.78).contains(&ratio) {
            out.insert(font, clamp(ratio / 0.7, 0.5, 1.4));
        }
    }
    out
}

fn style_of(l: &Letter, scales: &HashMap<Arc<str>, f64>) -> Style {
    let k = scales.get(&l.font).copied().unwrap_or(1.0);
    let font: Arc<str> = if l.font.is_empty() { Arc::from("Times New Roman") } else { l.font.clone() };
    Style { font, half_points: (clamp(l.size * k, 1.0, 1638.0) * 2.0).round() as u32, bold: l.bold, italic: l.italic, color: l.color, spacing: 0 }
}

fn add_line(lines: &mut Vec<Line>, cluster: &[&Letter], scales: &HashMap<Arc<str>, f64>) {
    let visible: Vec<&&Letter> = cluster.iter().filter(|l| !l.whitespace).collect();
    if visible.is_empty() {
        return;
    }
    // The baseline most letters sit on (a superscript doesn't move the line).
    let mut counts: Vec<(i64, usize, f64)> = Vec::new();
    for l in &visible {
        let key = (l.base * 2.0).round() as i64;
        match counts.iter_mut().find(|c| c.0 == key) {
            Some(c) => c.1 += 1,
            None => counts.push((key, 1, l.base)),
        }
    }
    let base = counts.iter().max_by_key(|c| c.1).map_or(0.0, |c| c.2);
    let mut sizes: Vec<f64> = visible.iter().map(|l| l.size).collect();
    sizes.sort_by(f64::total_cmp);
    let size = sizes.get(sizes.len() / 2).copied().unwrap_or(10.0);
    let mut line = Line { base, size, segs: Vec::new() };

    let mut ordered: Vec<&&Letter> = cluster.iter().collect();
    ordered.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    let mut prev_right = f64::NAN;
    let mut prev_left = f64::NAN;
    let mut prev_text = "";
    let mut space = false;
    for l in ordered {
        if l.whitespace {
            space = true;
            continue;
        }
        // The same glyph printed twice a hair apart (a "fake bold") is one letter.
        if l.text == prev_text && (l.x0 - prev_left).abs() < 0.6 {
            continue;
        }
        let em = l.size.max(line.size);
        let gap = if prev_right.is_nan() { 0.0 } else { l.x0 - prev_right };
        if line.segs.is_empty() || gap > em * 2.0 {
            line.segs.push(Seg::default());
            space = true;
        }
        let new_word = space || gap > em * 0.22;
        if let Some(seg) = line.segs.last_mut() {
            if new_word || seg.words.is_empty() {
                seg.words.push(Word { text: String::new(), style: style_of(l, scales), left: l.x0, right: l.x1 });
            }
            if let Some(word) = seg.words.last_mut() {
                if word.text.len() < 4096 {
                    word.text.push_str(&l.text);
                }
                word.right = word.right.max(l.x1);
            }
        }
        prev_right = if prev_right.is_nan() { l.x1 } else { prev_right.max(l.x1) };
        prev_left = l.x0;
        prev_text = &l.text;
        space = false;
    }
    for seg in &mut line.segs {
        fit_width(seg);
    }
    if !line.segs.is_empty() {
        lines.push(line);
    }
}

/// How wide `text` sets in the face the document will show `style` in, points.
fn set_width(text: &str, style: &Style) -> f64 {
    let face = wordcraft_fonts::word::resolve(&style.font, style.bold, style.italic).face;
    let upem = face.units_per_em();
    if upem.is_nan() || upem <= 0.0 {
        return 0.0;
    }
    let units: i64 = wordcraft_fonts::shape(&face, text, &[], |c| c).iter().map(|g| i64::from(g.x_advance)).sum();
    units as f64 * f64::from(style.half_points) / 2.0 / upem
}

/// Character spacing for a segment, so its words set as wide as they were in the PDF in
/// whatever font the document shows them in (a stand-in sets wider or narrower than the
/// original): lines then break where the PDF's did, and table cells stay in their columns.
fn fit_width(seg: &mut Seg) {
    let target = seg.right() - seg.left();
    let mut natural = 0.0;
    let mut chars = 0usize;
    for (i, w) in seg.words.iter().enumerate() {
        natural += set_width(&w.text, &w.style);
        chars += w.text.chars().count();
        if i > 0 {
            natural += set_width(" ", &w.style);
            chars += 1;
        }
    }
    if chars < 2 || natural.is_nan() || natural <= 0.0 || target.is_nan() || target <= 0.0 {
        return;
    }
    // Every character gets the spacing, the last one too; rounded toward narrower, and a point
    // short, so a line never comes out a hair too wide for its width and wraps.
    let per = clamp((target - 1.0 - natural) / chars as f64, -3.0, 3.0);
    let twips = (per * 20.0).floor() as i32;
    if twips != 0 {
        for w in &mut seg.words {
            w.style.spacing = twips;
        }
    }
}

/// "•", "–", "1.", "(a)", "iv)"…: a word that starts a list item.
pub(crate) fn is_list_marker(word: &str) -> bool {
    let mut chars = word.chars();
    let (Some(first), rest) = (chars.next(), chars.as_str()) else { return false };
    if rest.is_empty() && "•◦▪▫■□●○‣⁃–—-*".contains(first) {
        return true;
    }
    let w = word.strip_prefix('(').unwrap_or(word);
    let Some(core) = w.strip_suffix('.').or_else(|| w.strip_suffix(')')) else { return false };
    (!core.is_empty() && core.len() <= 3 && core.chars().all(|c| c.is_ascii_digit()))
        || (core.chars().count() == 1 && core.chars().all(|c| c.is_ascii_alphabetic()))
}

/// Lines → paragraphs (see the module docs). A line with several segments is a paragraph of its own.
fn build_paras(lines: Vec<Line>) -> Vec<Para> {
    let mut paras: Vec<Para> = Vec::new();
    for line in lines {
        if let Some(cur) = paras.last_mut()
            && continues(cur, &line)
        {
            cur.lines.push(line);
            continue;
        }
        paras.push(Para { lines: vec![line] });
    }
    paras
}

fn continues(p: &Para, line: &Line) -> bool {
    let Some(prev) = p.lines.last() else { return false };
    if prev.is_row() || line.is_row() {
        return false;
    }
    let size = line.size.max(prev.size);
    if (line.size - prev.size).abs() > 0.15 * size {
        return false;
    }
    let gap = prev.base - line.base;
    if gap < size * 0.9 || gap > size * 1.8 {
        return false;
    }
    if p.lines.len() >= 2
        && let Some(before) = p.lines.get(p.lines.len() - 2)
    {
        let pitch = before.base - prev.base;
        if (gap - pitch).abs() > (pitch * 0.12).max(1.0) {
            return false;
        }
    }
    if line.left() >= prev.right() || prev.left() >= line.right() {
        return false;
    }
    if is_list_marker(line.first_word()) {
        return false;
    }
    let tol = (size * 0.3).max(3.0);
    let same_left = (line.left() - prev.left()).abs() <= tol;
    let same_centre = ((line.left() + line.right()) / 2.0 - (prev.left() + prev.right()) / 2.0).abs() <= tol;
    let same_right = (line.right() - prev.right()).abs() <= tol;
    if p.lines.len() >= 2 {
        if !same_left && !same_centre && !same_right {
            return false;
        }
        // A left-aligned line well short of the paragraph's right edge ended it.
        if same_left && !same_centre && prev.right() < p.right() - size * 3.0 {
            return false;
        }
    } else {
        // The second line may sit left of the first (a first-line indent) or right of it (a
        // hanging indent after a list marker), by a few ems at most.
        if !same_centre && !same_right && (line.left() - prev.left()).abs() > size * 4.0 {
            return false;
        }
        // A first line much shorter than the next one ended its paragraph (a heading).
        if !same_centre && prev.right() < line.right() - size * 3.0 {
            return false;
        }
    }
    true
}

/// Where the page's anchors go in its first flowing paragraph.
const ANCHORS: &str = "\u{1}anchors\u{1}";

/// Text pinned to the page, in a text box without fill or border: its left edge, the top of its
/// first line, and its width, in points from the page's top left.
struct Frame {
    x: f64,
    y: f64,
    width: f64,
}

fn paragraph_xml(para: &Para, before: f64, margin_left: f64, text_width: f64, anchors: &str, frame: Option<&Frame>) -> String {
    let lines = &para.lines;
    let pitch = para.pitch();
    let (min_l, max_r) = (para.left(), para.right());
    let n = lines.len();
    let row = n == 1 && lines.first().is_some_and(Line::is_row);

    // Alignment from the line edges (the last line of a justified paragraph is short).
    const TOL: f64 = 2.5;
    let body: &[Line] = if n > 1 { lines.get(..n - 1).unwrap_or(lines) } else { lines };
    let second_left = lines.get(1).map_or(0.0, Line::left);
    let lefts = n == 1 || lines.iter().skip(1).all(|l| (l.left() - second_left).abs() < TOL);
    let rights = body.iter().all(|l| (l.right() - max_r).abs() < TOL);
    let centre = (min_l + max_r) / 2.0;
    let centred = lines.iter().all(|l| ((l.left() + l.right()) / 2.0 - centre).abs() < TOL);
    let mut jc = "left";
    if n > 1 && centred && !lefts {
        jc = "center";
    } else if n > 1 && rights && !lefts {
        jc = "right";
    } else if n > 2 && rights && lefts {
        jc = "both";
    } else if n == 1 && !row && frame.is_none() {
        let page_centre = margin_left + text_width / 2.0;
        if (centre - page_centre).abs() < 6.0 && max_r - min_l < text_width * 0.8 {
            jc = "center";
        }
    }

    // Indents keep the paragraph where it was (a frame is placed by its own x). The right indent
    // leaves a little slack, so a stand-in font that sets a hair wider doesn't wrap a line more.
    let right_edge = margin_left + text_width;
    let pinned = jc == "center" || frame.is_some();
    let ind_left = if pinned { 0.0 } else { (min_l - margin_left).max(0.0) };
    let mut ind_right = if pinned { 0.0 } else { (right_edge - max_r - (max_r - min_l) * 0.03 - 1.0).max(0.0) };
    if n == 1 && jc == "left" {
        // A single line may grow when edited, and may run a little past the margin as it did
        // in the PDF (the widest line sets the margin) rather than wrap.
        ind_right = -6.0;
    }
    let first_line = if n > 1 && (jc == "left" || jc == "both") {
        lines.first().map_or(0.0, Line::left) - lines.iter().skip(1).map(Line::left).fold(f64::INFINITY, f64::min)
    } else {
        0.0
    };

    let mut p = String::from("<w:p><w:pPr>");
    // Tab stops at each segment of a row, measured from the text column's (or frame's) left.
    if row && let Some(line) = lines.first() {
        let origin = frame.map_or(margin_left, |f| f.x);
        p.push_str("<w:tabs>");
        for seg in line.segs.iter().skip(1) {
            let _ = write!(p, "<w:tab w:val=\"left\" w:pos=\"{}\"/>", tw(seg.left() - origin));
        }
        p.push_str("</w:tabs>");
    }
    let _ = write!(p, "<w:spacing w:before=\"{}\" w:after=\"0\" w:line=\"{}\" w:lineRule=\"exact\"/>", tw(before), tw(pitch));
    if ind_left > 0.5 || ind_right.abs() > 0.5 || first_line.abs() > 0.5 {
        let _ = write!(p, "<w:ind w:left=\"{}\" w:right=\"{}\"", tw(ind_left), tw(ind_right));
        if first_line > 0.5 {
            let _ = write!(p, " w:firstLine=\"{}\"", tw(first_line));
        } else if first_line < -0.5 {
            let _ = write!(p, " w:hanging=\"{}\"", tw(-first_line));
        }
        p.push_str("/>");
    }
    let _ = write!(p, "<w:jc w:val=\"{jc}\"/></w:pPr>");
    p.push_str(anchors);

    // Runs: consecutive words with the same look make one run. Words join with a space,
    // segments with a tab, lines with a space (a word hyphenated across lines rejoins).
    let mut current: Option<&Style> = None;
    let mut text = String::new();
    let mut sep = "";
    for (li, line) in lines.iter().enumerate() {
        for (si, seg) in line.segs.iter().enumerate() {
            if si > 0 {
                sep = "\t";
            }
            for word in &seg.words {
                if current != Some(&word.style) {
                    text.push_str(sep);
                    sep = "";
                    if let Some(st) = current {
                        p.push_str(&run_xml(st, &text));
                    }
                    text.clear();
                    current = Some(&word.style);
                }
                text.push_str(sep);
                text.push_str(&word.text);
                sep = " ";
            }
        }
        // "bound-" + "ary" → "boundary"; "well-" + "Known" keeps its hyphen.
        if let Some(next) = lines.get(li + 1) {
            let mut tail = text.chars().rev();
            if tail.next() == Some('-')
                && tail.next().is_some_and(char::is_alphabetic)
                && next.first_word().chars().next().is_some_and(char::is_lowercase)
            {
                text.pop();
                sep = "";
            }
        }
    }
    if let Some(st) = current {
        p.push_str(&run_xml(st, &text));
    }
    p.push_str("</w:p>");
    p
}

fn run_xml(st: &Style, text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let font = xml(&st.font);
    let mut r = format!("<w:r><w:rPr><w:rFonts w:ascii=\"{font}\" w:hAnsi=\"{font}\" w:cs=\"{font}\" w:eastAsia=\"{font}\"/>");
    if st.bold {
        r.push_str("<w:b/><w:bCs/>");
    }
    if st.italic {
        r.push_str("<w:i/><w:iCs/>");
    }
    let [cr, cg, cb] = st.color;
    if st.color != [0, 0, 0] {
        let _ = write!(r, "<w:color w:val=\"{cr:02X}{cg:02X}{cb:02X}\"/>");
    }
    if st.spacing != 0 {
        let _ = write!(r, "<w:spacing w:val=\"{}\"/>", st.spacing);
    }
    let _ = write!(r, "<w:sz w:val=\"{0}\"/><w:szCs w:val=\"{0}\"/></w:rPr>", st.half_points.max(2));
    for (i, part) in text.split('\t').enumerate() {
        if i > 0 {
            r.push_str("<w:tab/>");
        }
        if !part.is_empty() {
            let _ = write!(r, "<w:t xml:space=\"preserve\">{}</w:t>", xml(part));
        }
    }
    r.push_str("</w:r>");
    r
}

// ── Pictures and drawing ──────────────────────────────────────────────

/// A paragraph in a borderless text box anchored to the page at `frame`, `height` tall. A little
/// wider than the PDF's text, so a stand-in font that sets a hair wider doesn't wrap a line more.
fn text_box_xml(paragraph: &str, frame: &Frame, height: f64, pkg: &mut Package) -> String {
    let (cx, cy) = (emu(frame.width * 1.04 + 4.0).max(1), emu(height + 2.0).max(1));
    let (mut s, _) = anchor_head(pkg, 400_000, false, frame.x, frame.y, cx, cy, "Text Box");
    let _ = write!(
        s,
        "<a:graphic><a:graphicData uri=\"http://schemas.microsoft.com/office/word/2010/wordprocessingShape\"><wps:wsp><wps:cNvSpPr txBox=\"1\"/>\
         <wps:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom><a:noFill/><a:ln><a:noFill/></a:ln></wps:spPr>\
         <wps:txbx><w:txbxContent>{paragraph}</w:txbxContent></wps:txbx>\
         <wps:bodyPr rot=\"0\" vert=\"horz\" wrap=\"square\" lIns=\"0\" tIns=\"0\" rIns=\"0\" bIns=\"0\" anchor=\"t\"><a:noAutofit/></wps:bodyPr>\
         </wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r>"
    );
    s
}

fn anchor_head(pkg: &mut Package, z: u64, behind: bool, x: f64, y: f64, cx: i64, cy: i64, kind: &str) -> (String, u32) {
    let id = pkg.id();
    let s = format!(
        "<w:r><w:drawing><wp:anchor distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\" simplePos=\"0\" relativeHeight=\"{}\" behindDoc=\"{}\" locked=\"0\" layoutInCell=\"1\" allowOverlap=\"1\">\
         <wp:simplePos x=\"0\" y=\"0\"/>\
         <wp:positionH relativeFrom=\"page\"><wp:posOffset>{}</wp:posOffset></wp:positionH>\
         <wp:positionV relativeFrom=\"page\"><wp:posOffset>{}</wp:posOffset></wp:positionV>\
         <wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/><wp:wrapNone/>\
         <wp:docPr id=\"{id}\" name=\"{kind} {id}\"/><wp:cNvGraphicFramePr/>",
        z.saturating_add(u64::from(id)),
        u8::from(behind),
        emu(x.max(0.0)),
        emu(y.max(0.0)),
    );
    (s, id)
}

fn picture_xml(pic: &Picture, pkg: &mut Package, page_h: f64, behind: bool, layer: u64) -> String {
    let b = pic.bounds;
    let (cx, cy) = (emu(b.width()).max(1), emu(b.height()).max(1));
    let rel = pkg.add_media(pic.data.clone(), pic.ext);
    let (mut s, id) = anchor_head(pkg, layer * 100_000, behind, b.x0, page_h - b.y1, cx, cy, "Picture");
    let _ = write!(
        s,
        "<a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic>\
         <pic:nvPicPr><pic:cNvPr id=\"{id}\" name=\"Picture {id}\"/><pic:cNvPicPr/></pic:nvPicPr>\
         <pic:blipFill><a:blip r:embed=\"{rel}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
         <pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr>\
         </pic:pic></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r>"
    );
    s
}

fn shape_xml(item: &PathItem, pkg: &mut Package, page_h: f64) -> String {
    let b = item.path.bounding_box();
    if !(b.x0.is_finite() && b.y0.is_finite() && b.x1.is_finite() && b.y1.is_finite()) {
        return String::new();
    }
    let (w, h) = (b.width().max(0.01), b.height().max(0.01));
    let (cx, cy) = (emu(w).max(1), emu(h).max(1));
    let pt = |p: Point| format!("<a:pt x=\"{}\" y=\"{}\"/>", emu(clamp(p.x - b.x0, 0.0, w)), emu(clamp(b.y1 - p.y, 0.0, h)));
    let mut geo = String::new();
    let mut last = Point::ZERO;
    for (n, el) in item.path.elements().iter().enumerate() {
        if n > 5000 {
            break;
        }
        match *el {
            PathEl::MoveTo(p) => {
                let _ = write!(geo, "<a:moveTo>{}</a:moveTo>", pt(p));
                last = p;
            }
            PathEl::LineTo(p) => {
                let _ = write!(geo, "<a:lnTo>{}</a:lnTo>", pt(p));
                last = p;
            }
            PathEl::QuadTo(c, p) => {
                // As a cubic: control points two thirds of the way to the quadratic's.
                let c1 = last + (c - last) * (2.0 / 3.0);
                let c2 = p + (c - p) * (2.0 / 3.0);
                let _ = write!(geo, "<a:cubicBezTo>{}{}{}</a:cubicBezTo>", pt(c1), pt(c2), pt(p));
                last = p;
            }
            PathEl::CurveTo(c1, c2, p) => {
                let _ = write!(geo, "<a:cubicBezTo>{}{}{}</a:cubicBezTo>", pt(c1), pt(c2), pt(p));
                last = p;
            }
            PathEl::ClosePath => geo.push_str("<a:close/>"),
        }
    }
    if geo.is_empty() {
        return String::new();
    }
    let hex = |c: [u8; 3]| format!("{:02X}{:02X}{:02X}", c[0], c[1], c[2]);
    let fill = item.fill.map_or_else(|| "<a:noFill/>".to_string(), |c| format!("<a:solidFill><a:srgbClr val=\"{}\"/></a:solidFill>", hex(c)));
    let line = item.stroke.map_or_else(
        || "<a:ln><a:noFill/></a:ln>".to_string(),
        |(c, width)| format!("<a:ln w=\"{}\"><a:solidFill><a:srgbClr val=\"{}\"/></a:solidFill></a:ln>", emu(clamp(width, 0.25, 24.0)), hex(c)),
    );
    let (mut s, _) = anchor_head(pkg, 200_000, true, b.x0, page_h - b.y1, cx, cy, "Shape");
    let _ = write!(
        s,
        "<a:graphic><a:graphicData uri=\"http://schemas.microsoft.com/office/word/2010/wordprocessingShape\"><wps:wsp><wps:cNvSpPr/>\
         <wps:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm>\
         <a:custGeom><a:avLst/><a:gdLst/><a:ahLst/><a:cxnLst/><a:rect l=\"0\" t=\"0\" r=\"r\" b=\"b\"/><a:pathLst><a:path w=\"{cx}\" h=\"{cy}\">{geo}</a:path></a:pathLst></a:custGeom>\
         {fill}{line}</wps:spPr><wps:bodyPr/></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r>"
    );
    s
}

// ── Units and text ────────────────────────────────────────────────────

fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    if v.is_nan() { lo } else { v.clamp(lo, hi) }
}

/// Points → twentieths of a point.
fn tw(points: f64) -> i64 {
    (clamp(points, -31680.0, 31680.0) * 20.0).round() as i64
}

/// Points → EMUs (12,700 to the point).
fn emu(points: f64) -> i64 {
    (clamp(points, 0.0, 31680.0) * 12700.0).round() as i64
}

/// Text for XML: escaped, and without the characters XML 1.0 can't hold.
fn xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    out
}

/// The page's text as lines of the editor's PDF tools (edit text, search, copy): one per
/// segment (a table cell or column is its own), top to bottom, in page display coordinates.
pub(crate) fn text_lines(c: &PageContent) -> Vec<crate::TextLine> {
    let mut out = Vec::new();
    for line in build_lines(&c.letters) {
        for seg in &line.segs {
            let Some(first) = seg.words.first() else { continue };
            let text = seg.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
            let size = f64::from(first.style.half_points) / 2.0;
            let base = c.height - line.base;
            out.push(crate::TextLine {
                text,
                x: seg.left() as f32,
                y: (base - line.size * 0.86) as f32,
                w: (seg.right() - seg.left()).max(1.0) as f32,
                h: (line.size * 1.16) as f32,
                base: base as f32,
                size: size as f32,
                font: first.style.font.to_string(),
                bold: first.style.bold,
                italic: first.style.italic,
                color: first.style.color,
                word_spans: seg.words.iter().map(|w| (w.left as f32, w.right as f32)).collect(),
            });
        }
    }
    out
}

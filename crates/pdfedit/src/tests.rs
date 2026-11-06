use crate::*;
use std::sync::Arc;
use wordcraft_pdfread::PdfView;

/// `pages` pages of `w` × `h` points, page n saying "Page n" (and the first "Secret plan" too),
/// the second turned by `rotate2` degrees; with a text field "name" and a check box "agree" on
/// the first page.
fn pdf(pages: usize, rotate2: i32) -> Vec<u8> {
    let mut objs: Vec<String> = Vec::new();
    let page_ids: Vec<usize> = (0..pages).map(|i| 4 + i * 2).collect();
    objs.push("<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [100 0 R 101 0 R] /DA (/Helv 0 Tf 0 g) >> >>".into());
    objs.push(format!("<< /Type /Pages /Kids [{}] /Count {pages} >>", page_ids.iter().map(|i| format!("{i} 0 R")).collect::<Vec<_>>().join(" ")));
    objs.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    for i in 0..pages {
        let text = if i == 0 {
            "BT /F1 24 Tf 72 700 Td (Page 1) Tj ET BT /F1 18 Tf 72 600 Td (Secret plan) Tj ET".to_string()
        } else {
            format!("BT /F1 24 Tf 72 700 Td (Page {}) Tj ET", i + 1)
        };
        let rot = if i == 1 { format!(" /Rotate {rotate2}") } else { String::new() };
        let annots = if i == 0 { " /Annots [100 0 R 101 0 R]" } else { "" };
        objs.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]{rot}{annots} /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
            5 + i * 2
        ));
        objs.push(format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len() + 1));
    }
    let mut s = String::from("%PDF-1.7\n");
    let mut offsets: Vec<(usize, usize)> = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push((i + 1, s.len()));
        s.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
    }
    let on = "<< /Type /XObject /Subtype /Form /BBox [0 0 12 12] /Length 24 >>\nstream\n0 0 1 rg 0 0 12 12 re f\nendstream";
    let extra = [
        (100, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [72 500 300 520] /P 4 0 R /DA (/Helv 12 Tf 0 g) >>".to_string()),
        (101, "<< /Type /Annot /Subtype /Widget /FT /Btn /T (agree) /Rect [72 460 84 472] /P 4 0 R /V /Off /AS /Off /AP << /N << /Yes 102 0 R /Off 103 0 R >> >> >>".to_string()),
        (102, on.to_string()),
        (103, "<< /Type /XObject /Subtype /Form /BBox [0 0 12 12] /Length 0 >>\nstream\n\nendstream".to_string()),
    ];
    for (id, o) in &extra {
        offsets.push((*id, s.len()));
        s.push_str(&format!("{id} 0 obj\n{o}\nendobj\n"));
    }
    let xref = s.len();
    let size = 104;
    s.push_str(&format!("xref\n0 {size}\n"));
    for id in 0..size {
        match offsets.iter().find(|(i, _)| *i == id) {
            Some((_, off)) => s.push_str(&format!("{off:010} 00000 n \n")),
            None => s.push_str("0000000000 65535 f \n"),
        }
    }
    s.push_str(&format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"));
    s.into_bytes()
}

fn texts(bytes: &[u8]) -> Vec<String> {
    let v = PdfView::open(bytes.to_vec()).unwrap();
    (0..v.page_count()).map(|i| v.page_text(i)).collect()
}

fn pixel(bytes: &[u8], page: usize, at: [f32; 2]) -> [u8; 3] {
    let v = PdfView::open(bytes.to_vec()).unwrap();
    let img = v.render(page, 1.0).unwrap();
    let (x, y) = (at[0].round() as u32, at[1].round() as u32);
    let i = ((y * img.width + x) * 4) as usize;
    [img.rgba[i], img.rgba[i + 1], img.rgba[i + 2]]
}

fn apply_ok(base: &[u8], ops: &[Op]) -> Vec<u8> {
    apply(base, ops, &SaveOptions::default()).unwrap()
}

const RED: Color = [220, 0, 0];

#[test]
fn pages_rotate_delete_move_duplicate_insert() {
    let base = pdf(3, 0);
    let out = apply_ok(
        &base,
        &[
            Op::Move { from: 2, to: 0 },
            Op::Duplicate { page: 0 },
            Op::Delete { pages: vec![3] },
            Op::InsertBlank { at: 1, width: 300.0, height: 400.0 },
            Op::Rotate { pages: vec![1], by: 90 },
        ],
    );
    let t = texts(&out);
    assert_eq!(t.len(), 4, "{t:?}");
    assert_eq!(t[0], "Page 3");
    assert_eq!(t[1], "", "the blank page");
    assert_eq!(t[2], "Page 3", "the duplicate");
    assert!(t[3].starts_with("Page 1"));
    let v = PdfView::open(out).unwrap();
    assert_eq!(v.page_size(0), Some((612.0, 792.0)));
    assert_eq!(v.page_size(1), Some((400.0, 300.0)), "the blank page, turned a quarter");
    // Deleting every page is refused, the rest still applies.
    let out = apply_ok(&base, &[Op::Delete { pages: vec![0, 1, 2] }, Op::Delete { pages: vec![1] }]);
    assert_eq!(texts(&out), vec!["Page 1\nSecret plan", "Page 3"]);
}

#[test]
fn another_pdf_inserts_extracts_and_splits() {
    let a = pdf(2, 0);
    let b = pdf(3, 0);
    let out = apply_ok(&a, &[Op::InsertPdf { at: 1, data: Arc::new(b) }]);
    let t = texts(&out);
    assert_eq!(t.len(), 5);
    assert_eq!(t[1], "Page 1\nSecret plan");
    assert_eq!(t[4], "Page 2");
    let ex = extract(&out, &[4, 0]).unwrap();
    assert_eq!(texts(&ex), vec!["Page 2", "Page 1\nSecret plan"]);
    let parts = split(&out, 2).unwrap();
    assert_eq!(parts.len(), 3);
    assert_eq!(texts(&parts[2]).len(), 1);
}

#[test]
fn marks_land_where_they_were_drawn_even_on_turned_pages() {
    for rotate in [0, 90, 180, 270] {
        let base = pdf(2, rotate);
        let r = Rect::new(100.0, 50.0, 40.0, 30.0);
        let out = apply_ok(&base, &[Op::Mark { id: 1, page: 1, mark: Mark::Rectangle { rect: r, stroke: None, fill: Some(RED), width: 0.0 } }]);
        let p = pixel(&out, 1, [120.0, 65.0]);
        assert!(p[0] > 180 && p[1] < 60, "rotate {rotate}: {p:?}");
        let outside = pixel(&out, 1, [90.0, 40.0]);
        assert_eq!(outside, [255, 255, 255], "rotate {rotate}");
    }
}

#[test]
fn every_kind_of_mark_applies() {
    let base = pdf(1, 0);
    let marks = vec![
        Mark::Highlight { rects: vec![Rect::new(70.0, 70.0, 100.0, 26.0)], color: [255, 230, 0] },
        Mark::Underline { rects: vec![Rect::new(70.0, 170.0, 100.0, 20.0)], color: RED },
        Mark::Strikeout { rects: vec![Rect::new(70.0, 200.0, 100.0, 20.0)], color: RED },
        Mark::Ink { strokes: vec![vec![[300.0, 300.0], [350.0, 320.0], [400.0, 300.0]]], color: RED, width: 3.0 },
        Mark::Line { from: [300.0, 400.0], to: [450.0, 400.0], color: RED, width: 2.0, arrow: true },
        Mark::Ellipse { rect: Rect::new(300.0, 450.0, 80.0, 40.0), stroke: Some(RED), fill: None, width: 2.0 },
        Mark::TextBox {
            rect: Rect::new(72.0, 500.0, 200.0, 60.0),
            text: "Hello नमस्ते".into(),
            size: 12.0,
            color: [0, 0, 160],
            font: "Arial".into(),
            bold: false,
            italic: false,
            fit: true,
            fill: None,
            border: Some(RED),
        },
        Mark::Note { at: [500.0, 100.0], text: "A note".into(), color: [255, 220, 0] },
        Mark::Stamp { rect: Rect::new(400.0, 600.0, 150.0, 50.0), text: "APPROVED".into(), color: [0, 140, 0] },
        Mark::Whiteout { rect: Rect::new(450.0, 700.0, 40.0, 20.0) },
    ];
    let ops: Vec<Op> = marks.into_iter().enumerate().map(|(i, mark)| Op::Mark { id: i as u64, page: 0, mark }).collect();
    let out = apply_ok(&base, &ops);
    // Real annotations, one per markup kind (whiteout goes into the page).
    let doc = lopdf::Document::load_mem(&out).unwrap();
    let page = *doc.get_pages().values().next().unwrap();
    let annots = doc.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap().len();
    assert_eq!(annots, 2 + 9, "two form widgets and nine markup annotations");
    // The highlight shows, the text under it stays readable.
    let p = pixel(&out, 0, [160.0, 75.0]);
    assert!(p[0] > 200 && p[1] > 180 && p[2] < 150, "{p:?}");
    // The stamp's frame.
    let p = pixel(&out, 0, [402.0, 625.0]);
    assert!(p[1] > 100 && p[0] < 80, "{p:?}");
}

#[test]
fn redaction_removes_the_text_under_it() {
    let base = pdf(2, 0);
    // "Secret plan" sits at y = 792 - 600 = 192 (baseline), 18 pt.
    let out = apply_ok(&base, &[Op::Mark { id: 1, page: 0, mark: Mark::Redact { rects: vec![Rect::new(60.0, 170.0, 200.0, 30.0)] } }]);
    let t = texts(&out);
    assert!(!t[0].contains("Secret"), "{t:?}");
    assert_eq!(t[1], "Page 2", "other pages keep their text");
    assert_eq!(pixel(&out, 0, [100.0, 185.0]), [0, 0, 0]);
}

#[test]
fn stamps_on_every_page_and_properties() {
    let base = pdf(3, 90);
    let out = apply_ok(
        &base,
        &[
            Op::PageText { text: "Page {n} of {N}".into(), position: Position::BottomCenter, size: 10.0, color: RED, start: 1, pages: None },
            Op::Watermark { text: "DRAFT".into(), size: 60.0, color: [200, 200, 200], opacity: 0.5, angle: 45.0, pages: None },
            Op::Metadata { title: Some("Report".into()), author: Some("Ana".into()), subject: None, keywords: None },
        ],
    );
    let doc = lopdf::Document::load_mem(&out).unwrap();
    let info = doc.trailer.get(b"Info").unwrap().as_reference().unwrap();
    assert_eq!(doc.get_dictionary(info).unwrap().get(b"Title").unwrap().as_str().unwrap(), b"Report");
    // The page number is drawn at the bottom of each page, upright on the turned one too.
    let v = PdfView::open(out.clone()).unwrap();
    for page in 0..3 {
        let (w, h) = v.page_size(page).unwrap();
        let img = v.render(page, 1.0).unwrap();
        let red = (0..img.height).filter(|y| (*y as f32) > h - 40.0).any(|y| {
            (0..img.width).any(|x| {
                (x as f32 - w / 2.0).abs() < 50.0
                    && img.rgba[((y * img.width + x) * 4) as usize] > 180
                    && img.rgba[((y * img.width + x) * 4 + 1) as usize] < 80
            })
        });
        assert!(red, "page {page}");
    }
}

#[test]
fn forms_list_and_fill() {
    let base = pdf(1, 0);
    let f = fields(&base).unwrap();
    assert_eq!(f.len(), 2);
    let name = f.iter().find(|f| f.name == "name").unwrap();
    assert_eq!(name.kind, FieldKind::Text);
    assert_eq!(name.widgets[0].0, 0);
    let r = name.widgets[0].1;
    assert!((r.x - 72.0).abs() < 0.5 && (r.y - 272.0).abs() < 0.5, "{r:?}");
    let agree = f.iter().find(|f| f.name == "agree").unwrap();
    assert_eq!(agree.kind, FieldKind::CheckBox);
    assert_eq!(agree.widgets[0].2, vec!["Yes".to_string()]);
    let out =
        apply_ok(&base, &[Op::Fill { field: "name".into(), value: "Ana Silva".into() }, Op::Fill { field: "agree".into(), value: "Yes".into() }]);
    let f = fields(&out).unwrap();
    assert_eq!(f.iter().find(|f| f.name == "name").unwrap().value, "Ana Silva");
    assert_eq!(f.iter().find(|f| f.name == "agree").unwrap().value, "Yes");
    // The check box shows its on state.
    let p = pixel(&out, 0, [78.0, 326.0]);
    assert!(p[2] > 180 && p[0] < 80, "{p:?}");
}

#[test]
fn edited_text_replaces_the_line() {
    let base = pdf(1, 0);
    let v = PdfView::open(base.clone()).unwrap();
    let line = v.text_lines(0).into_iter().find(|l| l.text == "Secret plan").unwrap();
    let out = apply_ok(
        &base,
        &[Op::Mark {
            id: 1,
            page: 0,
            mark: Mark::EditText {
                rect: Rect::new(line.x, line.y, line.w, line.h),
                base: line.base,
                text: "Public plan".into(),
                size: line.size,
                color: [0, 0, 0],
                font: line.font.clone(),
                bold: false,
                italic: false,
                bg: None,
            },
        }],
    );
    // The old word is painted over: the first letter's spot shows the new "P" instead of "S".
    assert_ne!(pixel(&out, 0, [line.x + 1.0, line.y + 2.0]), [0, 0, 0]);
}

#[test]
fn encryption_and_flattening() {
    let base = pdf(2, 0);
    let out = apply(&base, &[], &SaveOptions { password: Some("s3cret".into()), ..Default::default() }).unwrap();
    assert!(is_encrypted(&out));
    assert!(decrypt(&out, "wrong").is_err());
    let plain = decrypt(&out, "s3cret").unwrap();
    assert!(!is_encrypted(&plain));
    assert_eq!(texts(&plain)[1], "Page 2");
    let flat = apply(&base, &[], &SaveOptions { flatten: true, compress: true, ..Default::default() }).unwrap();
    assert_eq!(texts(&flat), vec!["", ""], "pictures only");
}

#[test]
fn hostile_edits_never_panic() {
    let base = pdf(2, 0);
    let junk = Rect { x: f32::NAN, y: f32::INFINITY, w: -1e30, h: 1e30 };
    let ops = vec![
        Op::Mark { id: 1, page: 999, mark: Mark::Whiteout { rect: Rect::new(0.0, 0.0, 10.0, 10.0) } },
        Op::Mark { id: 2, page: 0, mark: Mark::Highlight { rects: vec![junk; 3], color: RED } },
        Op::Mark { id: 3, page: 0, mark: Mark::Ink { strokes: vec![vec![], vec![[f32::NAN, 1.0]]], color: RED, width: f32::NAN } },
        Op::Mark { id: 4, page: 0, mark: Mark::Image { rect: Rect::new(0.0, 0.0, 10.0, 10.0), data: Arc::new(b"not a picture".to_vec()) } },
        Op::Mark {
            id: 5,
            page: 0,
            mark: Mark::TextBox {
                rect: junk,
                text: "x".repeat(100),
                size: f32::NAN,
                color: RED,
                font: String::new(),
                bold: true,
                italic: true,
                fit: true,
                fill: None,
                border: None,
            },
        },
        Op::Move { from: usize::MAX, to: usize::MAX },
        Op::Rotate { pages: vec![usize::MAX], by: i32::MIN },
        Op::Crop { page: 0, rect: junk },
        Op::Transform { pages: vec![0], flip_h: true, flip_v: true, scale: f32::NAN },
        Op::InsertPdf { at: usize::MAX, data: Arc::new(b"%PDF-1.7 junk".to_vec()) },
        Op::Watermark { text: String::new(), size: f32::INFINITY, color: RED, opacity: f32::NAN, angle: f32::NAN, pages: Some(vec![usize::MAX]) },
        Op::Fill { field: "nope".into(), value: "x".into() },
        Op::Mark { id: 6, page: 0, mark: Mark::Redact { rects: vec![junk] } },
    ];
    let out = apply_ok(&base, &ops);
    assert_eq!(texts(&out).len(), 2);
    assert!(apply(b"junk", &[], &SaveOptions::default()).is_err());
    assert!(extract(&base, &[99]).is_err());
}

#[test]
fn marks_are_followed_to_their_pages() {
    let base = pdf(3, 0);
    let note = |id: u64, page: usize| Op::Mark { id, page, mark: Mark::Note { at: [10.0, 10.0], text: "n".into(), color: [255, 214, 0] } };
    let ops = vec![
        note(1, 0),
        note(2, 2),
        Op::InsertBlank { at: 0, width: 200.0, height: 200.0 },
        Op::Move { from: 3, to: 0 },
        Op::Delete { pages: vec![1] },
        note(3, 0),
        Op::Rotate { pages: vec![0], by: 90 },
        note(4, 1),
    ];
    let at = crate::mark_pages(&base, &ops);
    // Note 1: page 0 → 1 (blank before) → 2 (page 4 moved to the front) → 1 (the blank deleted).
    assert_eq!(at.get(&1), Some(&1));
    // Note 2: page 2 → 3 → 0 (moved to the front) → 0 → turned.
    assert_eq!(at.get(&2), None);
    assert_eq!(at.get(&3), None, "turned after it was put there");
    assert_eq!(at.get(&4), Some(&1));
    let edited = apply(&base, &ops, &SaveOptions::default()).unwrap();
    assert_eq!(wordcraft_pdfread::PdfView::open(edited).unwrap().page_count(), 3);
}

#[test]
fn a_duplicated_page_keeps_its_marks() {
    let base = pdf(1, 0);
    let ops = vec![
        Op::Mark { id: 1, page: 0, mark: Mark::Highlight { rects: vec![Rect::new(100.0, 100.0, 120.0, 30.0)], color: [255, 0, 0] } },
        Op::Duplicate { page: 0 },
    ];
    let out = apply_ok(&base, &ops);
    assert_eq!(pixel(&out, 1, [160.0, 115.0]), pixel(&out, 0, [160.0, 115.0]));
    assert_ne!(pixel(&out, 1, [160.0, 115.0]), [255, 255, 255]);
}

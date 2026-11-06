//! Opening PDFs: a PDF opens as an editable document, page for page, and saves as a Word
//! document (never over the PDF).

use crate::Session;
use serde_json::json;
use wordcraft_doc::StoryRef;

fn run(s: &mut Session, id: &str, v: serde_json::Value) -> serde_json::Value {
    s.run(id, &v).unwrap_or_else(|e| panic!("{id}: {e}"))
}

/// A three-page document exported by WordCraft itself.
fn exported_pdf() -> Vec<u8> {
    let mut s = Session::new(wordcraft_doc::Document::new());
    run(&mut s, "text.insert", json!({"text": "First page of the round trip"}));
    run(&mut s, "insert.pageBreak", json!({}));
    run(&mut s, "text.insert", json!({"text": "Second page"}));
    run(&mut s, "insert.pageBreak", json!({}));
    run(&mut s, "text.insert", json!({"text": "Third page"}));
    crate::io::save_bytes("x.pdf", &s.doc).unwrap()
}

#[test]
fn a_pdf_opens_page_for_page_as_a_converted_copy() {
    let pdf = exported_pdf();
    let mut s = Session::new(wordcraft_doc::Document::new());
    let data = crate::cmd::insert::base64_encode(&pdf);
    run(&mut s, "file.open", json!({"path": "/tmp/Report.pdf", "data": data}));
    let text = s.doc.plain_text(StoryRef::Body);
    assert!(text.contains("First page of the round trip"), "{text}");
    assert!(text.contains("Second page") && text.contains("Third page"), "{text}");
    assert_eq!(s.layout().pages.len(), 3, "page for page");
    assert_eq!(s.doc.sections().len(), 3, "one section per PDF page");
    // A converted copy: Save asks where, suggesting the PDF's name, and the PDF is kept.
    assert!(s.path.is_none());
    assert_eq!(s.file_name.as_deref(), Some("Report"));
    assert_eq!(s.pdf_source.as_deref().map(Vec::len), Some(pdf.len()));
    // Another document replaces it, and the PDF with it.
    run(&mut s, "file.new", json!({}));
    assert!(s.pdf_source.is_none());
}

#[test]
fn a_pdf_is_known_by_its_content_too() {
    let pdf = exported_pdf();
    let (doc, _) = crate::io::open_bytes_with("no-extension", &pdf, None).unwrap();
    assert!(doc.plain_text(StoryRef::Body).contains("Third page"));
    assert!(crate::io::open_bytes("broken.pdf", b"%PDF-1.7 nothing else").is_err());
}

fn opened_pdf() -> (Session, Vec<u8>) {
    let pdf = exported_pdf();
    let mut s = Session::new(wordcraft_doc::Document::new());
    let data = crate::cmd::insert::base64_encode(&pdf);
    run(&mut s, "file.open", json!({"path": "/tmp/Report.pdf", "data": data}));
    (s, pdf)
}

fn temp(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("wc-pdf-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.to_string_lossy().into_owned()
}

#[test]
fn pdf_edits_mark_up_organize_and_save_the_pdf() {
    let (mut s, _) = opened_pdf();
    assert_eq!(s.pdf.path.as_deref(), Some(std::path::Path::new("/tmp/Report.pdf")));
    let hl = run(&mut s, "pdf.mark", json!({"page": 1, "kind": "highlight", "rect": [70, 60, 200, 16]}));
    let id = hl["id"].as_u64().unwrap();
    run(&mut s, "pdf.mark", json!({"page": 1, "kind": "textBox", "rect": [100, 300, 200, 40], "text": "Reviewed by the studio", "fit": true}));
    run(&mut s, "pdf.updateMark", json!({"id": id, "color": "#00FF00", "move": [5, 5]}));
    run(&mut s, "pdf.deletePages", json!({"pages": [2]}));
    run(&mut s, "pdf.pageText", json!({"text": "Page {n} of {N}"}));
    run(&mut s, "pdf.watermark", json!({"text": "DRAFT"}));
    let edits = run(&mut s, "pdf.edits", json!({}));
    assert_eq!(edits["edits"].as_array().unwrap().len(), 5);
    assert!(edits["dirty"].as_bool().unwrap());
    // Added text is drawn as shapes; the PDF's own text is still there to find.
    let text = run(&mut s, "pdf.text", json!({}))["text"].as_str().unwrap().to_string();
    assert!(!text.contains("Second page") && text.contains("Third page"), "{text}");
    let hits = run(&mut s, "pdf.search", json!({"text": "third"}));
    assert_eq!(hits["hits"][0]["page"], 2);
    // Undo and redo step through the edits.
    run(&mut s, "pdf.undo", json!({}));
    run(&mut s, "pdf.undo", json!({}));
    assert_eq!(s.pdf.ops.len(), 3);
    assert!(!run(&mut s, "pdf.text", json!({}))["text"].as_str().unwrap().contains("Second page"));
    run(&mut s, "pdf.undo", json!({}));
    assert!(run(&mut s, "pdf.text", json!({}))["text"].as_str().unwrap().contains("Second page"), "the deleted page is back");
    run(&mut s, "pdf.redo", json!({}));
    run(&mut s, "pdf.redo", json!({}));
    assert_eq!(s.pdf.ops.len(), 4);
    // Saved where asked; a page out of range is an error, not a crash.
    let dir = temp("save");
    let out = format!("{dir}/out.pdf");
    run(&mut s, "pdf.save", json!({"path": out}));
    let view = wordcraft_pdfread::PdfView::open(std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(view.page_count(), 2);
    let page = view.render(0, 1.0).unwrap();
    let px = |x: u32, y: u32| page.rgba.get(((y * page.width + x) * 4) as usize..((y * page.width + x) * 4 + 3) as usize).unwrap().to_vec();
    assert_ne!(px(80, 72), vec![255, 255, 255], "the highlight is on the saved page");
    assert!(s.run("pdf.mark", &json!({"page": 0, "kind": "note", "at": [1, 1]})).is_err());
    assert!(s.run("pdf.deleteMark", &json!({"id": 999})).is_err());
    // Extract, split, images.
    run(&mut s, "pdf.extract", json!({"path": format!("{dir}/one.pdf"), "pages": [2]}));
    assert_eq!(wordcraft_pdfread::PdfView::open(std::fs::read(format!("{dir}/one.pdf")).unwrap()).unwrap().page_count(), 1);
    let parts = run(&mut s, "pdf.split", json!({"dir": dir, "every": 1}));
    assert_eq!(parts["files"].as_array().unwrap().len(), 2);
    let imgs = run(&mut s, "pdf.exportImages", json!({"dir": dir, "dpi": 36}));
    assert_eq!(imgs["files"].as_array().unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn pdf_edits_work_on_a_document_never_opened_from_a_pdf() {
    let mut s = Session::new(wordcraft_doc::Document::new());
    run(&mut s, "text.insert", json!({"text": "Made here"}));
    run(&mut s, "pdf.mark", json!({"page": 1, "kind": "stamp", "rect": [300, 40, 160, 50]}));
    let text = run(&mut s, "pdf.text", json!({}))["text"].as_str().unwrap().to_string();
    assert!(text.contains("Made here"), "{text}");
    assert_eq!(run(&mut s, "pdf.edits", json!({}))["edits"][0]["kind"], "stamp");
    // Nowhere to save yet: Save PDF asks where.
    run(&mut s, "pdf.save", json!({}));
    assert!(s.ui_requests.iter().any(|r| r["open"] == "savePdf"));
}

#[test]
fn a_password_protected_pdf_opens_with_its_password_and_keeps_it() {
    let (mut s, _) = opened_pdf();
    let dir = temp("pw");
    let locked = format!("{dir}/locked.pdf");
    run(&mut s, "pdf.save", json!({"path": locked, "password": "s3cret"}));
    let bytes = std::fs::read(&locked).unwrap();
    assert!(wordcraft_pdfedit::is_encrypted(&bytes));
    let mut t = Session::new(wordcraft_doc::Document::new());
    let e = t.run("file.open", &json!({"path": locked})).unwrap_err().to_string();
    assert!(crate::io::needs_password(&e), "{e}");
    let e = t.run("file.open", &json!({"path": locked, "password": "nope"})).unwrap_err().to_string();
    assert!(crate::io::wrong_password(&e), "{e}");
    run(&mut t, "file.open", json!({"path": locked, "password": "s3cret"}));
    assert!(t.doc.plain_text(StoryRef::Body).contains("Third page"));
    assert_eq!(t.pdf.password.as_deref(), Some("s3cret"));
    run(&mut t, "pdf.rotate", json!({"page": 1}));
    run(&mut t, "pdf.save", json!({}));
    assert!(wordcraft_pdfedit::is_encrypted(&std::fs::read(&locked).unwrap()), "saved with its password again");
    assert!(!t.pdf.dirty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn edited_text_is_covered_in_the_colour_behind_it() {
    let (mut s, _) = opened_pdf();
    // A dark panel where the text will go.
    run(&mut s, "pdf.mark", json!({"page": 1, "kind": "rectangle", "rect": [50, 400, 300, 60], "stroke": null, "fill": "#203040", "width": 0}));
    run(&mut s, "pdf.mark", json!({"page": 1, "kind": "editText", "rect": [60, 410, 200, 20], "text": "On the panel", "color": "#FFFFFF"}));
    let Some((_, wordcraft_pdfedit::Mark::EditText { bg, .. })) = s.pdf.mark(2) else { panic!("no edit") };
    let bg = bg.expect("a background");
    assert!(bg.iter().zip([0x20u8, 0x30, 0x40]).all(|(a, b)| a.abs_diff(b) < 12), "{bg:?}");
    // Blank paper stays white.
    run(&mut s, "pdf.mark", json!({"page": 1, "kind": "editText", "rect": [60, 600, 200, 20], "text": "Plain"}));
    let Some((_, wordcraft_pdfedit::Mark::EditText { bg, .. })) = s.pdf.mark(3) else { panic!("no edit") };
    assert!(bg.is_some_and(|c| c.iter().all(|v| *v > 245)), "{bg:?}");
}

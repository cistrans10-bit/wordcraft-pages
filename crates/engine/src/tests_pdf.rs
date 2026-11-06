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

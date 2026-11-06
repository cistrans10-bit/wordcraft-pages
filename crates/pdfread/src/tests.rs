use crate::convert::is_list_marker;
use crate::fonts::family_of;
use crate::{PdfView, is_pdf, to_docx};
use std::io::Read;

/// A4 page: a heading, a two-cell row, a green bar, body text, a footer. Letter page: one line.
pub(crate) fn two_page_pdf() -> Vec<u8> {
    let page1 = "0.102 0.549 0.298 rg 72 700 451 6 re f 0 g\n\
                 BT /F1 18 Tf 72 740 Td (Quarterly report) Tj ET\n\
                 BT /F1 11 Tf 72 660 Td (Key) Tj ET\n\
                 BT /F1 11 Tf 300 660 Td (Action) Tj ET\n\
                 BT /F1 11 Tf 72 620 Td (Plain body text on the first page.) Tj ET\n\
                 BT /F1 8 Tf 72 30 Td (Footer line 01) Tj ET\n";
    let page2 = "BT /F1 12 Tf 72 700 Td (Second page) Tj ET\n";
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595.3 841.9] /Resources << /Font << /F1 5 0 R >> >> /Contents 6 0 R >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 7 0 R >>".to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        format!("<< /Length {} >>\nstream\n{page1}endstream", page1.len()),
        format!("<< /Length {} >>\nstream\n{page2}endstream", page2.len()),
    ];
    let mut s = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(s.len());
        s.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
    }
    let xref = s.len();
    s.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1));
    for o in offsets {
        s.push_str(&format!("{o:010} 00000 n \n"));
    }
    s.push_str(&format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1));
    s.into_bytes()
}

fn document_xml(docx: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx)).unwrap();
    assert!(zip.by_name("[Content_Types].xml").is_ok());
    assert!(zip.by_name("word/styles.xml").is_ok());
    let mut s = String::new();
    zip.by_name("word/document.xml").unwrap().read_to_string(&mut s).unwrap();
    s
}

#[test]
fn every_page_becomes_one_section_of_its_size() {
    let xml = document_xml(&to_docx(&two_page_pdf()).unwrap());
    let sizes: Vec<&str> = xml.match_indices("<w:pgSz ").map(|(i, _)| &xml[i..i + 40]).collect();
    assert_eq!(sizes.len(), 2, "{sizes:?}");
    assert!(sizes[0].starts_with("<w:pgSz w:w=\"11906\" w:h=\"16838\""), "{}", sizes[0]); // A4
    assert!(sizes[1].starts_with("<w:pgSz w:w=\"12240\" w:h=\"15840\""), "{}", sizes[1]); // Letter
    assert_eq!(xml.matches("w:type w:val=\"nextPage\"").count(), 1);
    assert!(xml.ends_with("</w:sectPr></w:body></w:document>"));
}

#[test]
fn text_keeps_words_columns_footer_and_drawing() {
    let xml = document_xml(&to_docx(&two_page_pdf()).unwrap());
    assert!(xml.contains(">Quarterly report</w:t>"), "spaces between words survive");
    assert!(!xml.contains("\u{1}"), "no placeholder left");
    assert!(xml.contains(">Plain body text on the first page.</w:t>"));
    assert!(xml.contains(">Key</w:t>") && xml.contains(">Action</w:t>"), "a row's two cells");
    assert!(xml.contains("<w:tabs><w:tab w:val=\"left\" w:pos=\""), "a tab stop between them");
    assert!(xml.contains("<w:tab/>"));
    assert!(xml.contains("<wps:txbx><w:txbxContent>"), "the footer is pinned in a text box");
    assert!(xml.contains("<wps:wsp>") && xml.contains("<a:srgbClr val=\"1A8C4C\"/>"), "the green bar is a shape");
    assert!(xml.contains("w:rFonts w:ascii=\"Arial\""), "Helvetica is Arial in Word: {}", &xml[..xml.len().min(3000)]);
    assert!(!xml.contains("<w:color "), "black text has no colour");
}

#[test]
fn renders_pages_to_pixels() {
    let view = PdfView::open(two_page_pdf()).unwrap();
    assert_eq!(view.page_count(), 2);
    let (w, h) = view.page_size(1).unwrap();
    assert_eq!((w.round(), h.round()), (612.0, 792.0));
    let img = view.render(0, 0.5).unwrap();
    assert_eq!((img.width, img.height), (297, 420));
    assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
    // The green bar is there, and the page around it is white.
    let at = |x: u32, y: u32| {
        let i = ((y * img.width + x) * 4) as usize;
        [img.rgba[i], img.rgba[i + 1], img.rgba[i + 2]]
    };
    assert_eq!(at(5, 5), [255, 255, 255]);
    let bar_y = ((841.9 - 703.0) * 0.5) as u32;
    let [r, g, b] = at(150, bar_y);
    assert!(g > r + 60 && g > b + 60, "{:?}", [r, g, b]);
}

#[test]
fn hostile_input_is_an_error_not_a_panic() {
    assert!(to_docx(b"").is_err());
    assert!(to_docx(b"%PDF-1.7\n garbage").is_err());
    assert!(PdfView::open(b"not a pdf".to_vec()).is_err());
    let mut cut = two_page_pdf();
    cut.truncate(cut.len() / 2);
    let _ = to_docx(&cut);
    let _ = PdfView::open(cut).map(|v| v.render(0, f32::NAN));
    let view = PdfView::open(two_page_pdf()).unwrap();
    assert!(view.render(99, 1.0).is_err());
    assert!(view.render(0, 1e9).is_ok_and(|i| i.width <= 8192 && i.height <= 8192));
}

#[test]
fn pdf_signature() {
    assert!(is_pdf(b"%PDF-1.4\n..."));
    assert!(is_pdf(b"\xef\xbb\xbf  %PDF-1.7"));
    assert!(!is_pdf(b"PK\x03\x04"));
}

#[test]
fn font_names_become_families() {
    assert_eq!(family_of("ABCDEF+TimesNewRomanPS-BoldMT"), "Times New Roman");
    assert_eq!(family_of("Arial,Bold"), "Arial");
    assert_eq!(family_of("ArialMT"), "Arial");
    assert_eq!(family_of("Inter-SemiBold"), "Inter");
    assert_eq!(family_of("QWERTY+SourceSans3-Regular"), "Source Sans 3");
    assert_eq!(family_of("GaramondItalic"), "Garamond");
    assert_eq!(family_of(""), "");
}

#[test]
fn list_markers() {
    for m in ["•", "–", "1.", "12)", "(a)", "b."] {
        assert!(is_list_marker(m), "{m}");
    }
    for m in ["Word", "1234.", "ab.", ""] {
        assert!(!is_list_marker(m), "{m}");
    }
}

#[test]
fn a_view_moves_to_another_thread() {
    fn send<T: Send>(_: &T) {}
    let view = PdfView::open(two_page_pdf()).unwrap();
    send(&view);
    assert!(std::thread::spawn(move || view.render(0, 0.2).is_ok()).join().unwrap());
}

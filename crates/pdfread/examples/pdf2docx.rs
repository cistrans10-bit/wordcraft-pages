//! Convert a PDF to a Word document: `cargo run -p wordcraft-pdfread --example pdf2docx -- in.pdf out.docx`.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(output)) = (args.next(), args.next()) else {
        return Err("usage: pdf2docx in.pdf out.docx".into());
    };
    let t = std::time::Instant::now();
    let docx = wordcraft_pdfread::to_docx(&std::fs::read(&input)?)?;
    std::fs::write(&output, &docx)?;
    eprintln!("{output}: {} bytes in {:.2}s", docx.len(), t.elapsed().as_secs_f64());
    Ok(())
}

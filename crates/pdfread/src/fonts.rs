//! Fonts for PDFs that don't embed theirs: the 14 standard fonts (Helvetica, Times, Courier,
//! Symbol, ZapfDingbats) and named fonts the PDF only refers to. They come from WordCraft's font
//! database the way a Word document's fonts do (installed fonts, then metric-compatible
//! substitutes, then the bundled fallback), so nothing new is bundled for PDF.

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_interpret::font::{FontData, FontQuery, StandardFont};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use wordcraft_fonts::FaceRef;

/// The family, weight and slant each resolved face stands for, by the address of its data: a
/// glyph drawn with a font the PDF didn't embed carries no name of its own, only that data.
fn stand_ins() -> &'static Mutex<HashMap<usize, StandIn>> {
    static MAP: OnceLock<Mutex<HashMap<usize, StandIn>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Clone)]
struct StandIn {
    face: FaceRef,
    family: String,
    bold: bool,
    italic: bool,
}

/// What a face resolved for a non-embedded font stands for (see [`stand_ins`]).
pub(crate) fn stand_in(data: &[u8]) -> Option<(String, bool, bool)> {
    stand_ins().lock().ok()?.get(&(data.as_ptr() as usize)).map(|s| (s.family.clone(), s.bold, s.italic))
}

/// The stand-in a glyph of a non-embedded Type 1 font (the standard 14) was drawn with, told by
/// its outline: hayro gives such glyphs no font data to look the name up by. `bbox` is the glyph's
/// outline bounds in glyph space (1000 units to the em).
pub(crate) fn stand_in_by_outline(ch: char, bbox: hayro::kurbo::Rect) -> Option<(String, bool, bool)> {
    use hayro::kurbo::Shape;
    let map = stand_ins().lock().ok()?;
    let mut best: Option<(f64, &StandIn)> = None;
    for s in map.values() {
        let gid = s.face.glyph_for(ch);
        if gid == 0 {
            continue;
        }
        let k = 1000.0 / s.face.units_per_em().max(1.0);
        let b = s.face.glyph_outline(gid).bounding_box();
        // WordCraft's outlines run y down; glyph space runs y up.
        let d = (b.x0 * k - bbox.x0).abs() + (-b.y1 * k - bbox.y0).abs() + (b.x1 * k - bbox.x1).abs() + (-b.y0 * k - bbox.y1).abs();
        if d.is_finite() && d < 12.0 && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, s));
        }
    }
    best.map(|(_, s)| (s.family.clone(), s.bold, s.italic))
}

/// A font database face as hayro font data, without copying it.
struct FaceBytes(FaceRef);

impl AsRef<[u8]> for FaceBytes {
    fn as_ref(&self) -> &[u8] {
        self.0.data()
    }
}

/// Interpreter settings with WordCraft's font resolution.
pub(crate) fn settings() -> InterpreterSettings {
    InterpreterSettings {
        font_resolver: Arc::new(|query: &FontQuery| {
            let (family, bold, italic) = match query {
                FontQuery::Standard(s) => standard(*s),
                FontQuery::Fallback(f) => {
                    let name = f.font_family.as_deref().or(f.font_name.as_deref()).or(f.post_script_name.as_deref()).unwrap_or("");
                    let family = family_of(name);
                    let family = if family.is_empty() {
                        if f.is_fixed_pitch {
                            "Courier New".to_string()
                        } else if f.is_serif {
                            "Times New Roman".to_string()
                        } else {
                            "Arial".to_string()
                        }
                    } else {
                        family
                    };
                    let bold = f.is_bold || f.font_weight >= 600 || name.to_ascii_lowercase().contains("bold");
                    let italic = f.is_italic || name.to_ascii_lowercase().contains("italic") || name.to_ascii_lowercase().contains("oblique");
                    (family, bold, italic)
                }
            };
            let face = wordcraft_fonts::word::resolve(&family, bold, italic).face;
            let index = face.index();
            if let Ok(mut map) = stand_ins().lock()
                && map.len() < 4096
            {
                map.insert(face.data().as_ptr() as usize, StandIn { face, family, bold, italic });
            }
            let data: FontData = Arc::new(FaceBytes(face));
            Some((data, index))
        }),
        ..Default::default()
    }
}

/// The Word family standing in for a standard PDF font, with its weight and slant.
fn standard(s: StandardFont) -> (String, bool, bool) {
    use StandardFont::*;
    let (family, bold, italic) = match s {
        Helvetica => ("Arial", false, false),
        HelveticaBold => ("Arial", true, false),
        HelveticaOblique => ("Arial", false, true),
        HelveticaBoldOblique => ("Arial", true, true),
        Courier => ("Courier New", false, false),
        CourierBold => ("Courier New", true, false),
        CourierOblique => ("Courier New", false, true),
        CourierBoldOblique => ("Courier New", true, true),
        TimesRoman => ("Times New Roman", false, false),
        TimesBold => ("Times New Roman", true, false),
        TimesItalic => ("Times New Roman", false, true),
        TimesBoldItalic => ("Times New Roman", true, true),
        Symbol => ("Symbol", false, false),
        ZapfDingBats => ("Wingdings", false, false),
    };
    (family.to_string(), bold, italic)
}

/// "ABCDEF+TimesNewRomanPS-BoldMT" → "Times New Roman"; "Inter-SemiBold" → "Inter";
/// "Arial,Bold" → "Arial". A family name for a PDF font name: the subset tag and the style
/// suffix go, a few well-known PostScript names map to their families, and CamelCase splits.
pub(crate) fn family_of(postscript: &str) -> String {
    let mut name = postscript.trim();
    if let Some((tag, rest)) = name.split_once('+')
        && tag.len() == 6
        && tag.chars().all(|c| c.is_ascii_uppercase())
    {
        name = rest;
    }
    let base = name.split([',', '-']).next().unwrap_or(name).trim();
    let compact: String = base.chars().filter(|c| !c.is_whitespace()).collect();
    let lower = compact.to_ascii_lowercase();
    for (key, family) in KNOWN {
        if lower == *key || lower.strip_suffix("mt").is_some_and(|l| l == *key) || lower.strip_suffix("ps").is_some_and(|l| l == *key) {
            return (*family).to_string();
        }
    }
    // Style words glued on without a hyphen ("ArialBold", "GaramondItalic").
    let mut core = compact.as_str();
    for suffix in
        ["BoldItalic", "BoldOblique", "Bold", "Italic", "Oblique", "Regular", "Medium", "Light", "Semibold", "SemiBold", "Black", "MT", "PS"]
    {
        if let Some(c) = core.strip_suffix(suffix)
            && !c.is_empty()
        {
            core = c;
        }
    }
    let mut out = String::with_capacity(core.len() + 4);
    let mut prev: Option<char> = None;
    for c in core.chars() {
        if let Some(p) = prev
            && c.is_ascii_uppercase()
            && (p.is_ascii_lowercase() || p.is_ascii_digit())
        {
            out.push(' ');
        }
        out.push(c);
        prev = Some(c);
    }
    out.chars().take(64).collect()
}

const KNOWN: &[(&str, &str)] = &[
    ("timesnewroman", "Times New Roman"),
    ("timesnewromanps", "Times New Roman"),
    ("times", "Times New Roman"),
    ("timesroman", "Times New Roman"),
    ("arial", "Arial"),
    ("arialmt", "Arial"),
    ("helvetica", "Arial"),
    ("couriernew", "Courier New"),
    ("couriernewps", "Courier New"),
    ("courier", "Courier New"),
    ("calibri", "Calibri"),
    ("cambria", "Cambria"),
    ("segoeui", "Segoe UI"),
    ("georgia", "Georgia"),
    ("verdana", "Verdana"),
    ("tahoma", "Tahoma"),
    ("trebuchetms", "Trebuchet MS"),
    ("garamond", "Garamond"),
    ("bookantiqua", "Book Antiqua"),
    ("centurygothic", "Century Gothic"),
    ("symbolmt", "Symbol"),
    ("symbol", "Symbol"),
    ("wingdings", "Wingdings"),
    ("dejavusans", "DejaVu Sans"),
    ("dejavuserif", "DejaVu Serif"),
    ("liberationsans", "Liberation Sans"),
    ("liberationserif", "Liberation Serif"),
    ("liberationmono", "Liberation Mono"),
    ("sourcesans3", "Source Sans 3"),
    ("sourceserif4", "Source Serif 4"),
];

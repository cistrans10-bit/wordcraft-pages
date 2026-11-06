//! The PDF view: the document as a PDF, beside the page (DOCX + PDF) or in place of it (PDF).
//!
//! What it shows is a real PDF, rendered by `wordcraft_pdfread` (hayro): the PDF the document was
//! opened from while it is unchanged, and from the first edit on the document's own export, made
//! again a moment after each pause in editing. So the PDF on screen is the PDF Export › PDF would
//! write, page for page, as the document is written.
//!
//! The two sides keep to the same page: scroll the document and the PDF follows; scroll the PDF
//! (the pointer over it) and the document follows.
//!
//! Exporting and rendering run on a worker thread (in the browser, where there are no threads,
//! in the frame), so typing never waits for them; the page images on screen stay until their
//! replacements arrive, so the view never flashes blank.

use crate::WordApp;
use crate::theme::Tokens;
use egui::{Align2, Color32, Rect, Sense, Stroke, Ui, pos2, vec2};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use wordcraft_engine::pdfread::{PageImage, PdfView};

/// How the PDF view is shown: not at all (DOCX), beside the page (DOCX + PDF) or alone (PDF).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PdfMode {
    #[default]
    Docx,
    Side,
    Pdf,
}

impl PdfMode {
    pub fn id(self) -> &'static str {
        match self {
            PdfMode::Docx => "docx",
            PdfMode::Side => "side",
            PdfMode::Pdf => "pdf",
        }
    }

    pub fn parse(s: &str) -> Option<PdfMode> {
        match s {
            "docx" | "off" | "none" => Some(PdfMode::Docx),
            "side" | "both" | "sideBySide" => Some(PdfMode::Side),
            "pdf" | "full" => Some(PdfMode::Pdf),
            _ => None,
        }
    }
}

/// Seconds editing must pause before the PDF is made again.
const SETTLE: f64 = 0.6;
/// Points between pages, and around them.
const GAP: f32 = 12.0;
/// Page images kept on either side of the visible pages.
const KEEP: usize = 4;

enum Source {
    /// The PDF itself (the one the document was opened from).
    Bytes(Arc<Vec<u8>>),
    /// The document, to export first.
    Export(Box<wordcraft_engine::doc::Document>),
}

enum Job {
    Show { version: u64, source: Source },
    Render { version: u64, page: usize, scale: f32, key: u32 },
}

enum Reply {
    Opened { version: u64, sizes: Vec<(f32, f32)>, exported: bool },
    Failed { version: u64, error: String },
    Page { version: u64, page: usize, key: u32, image: PageImage },
}

/// The worker that exports and renders. It holds the open PDF; only sizes and pixels come back.
struct Worker {
    #[cfg(not(target_arch = "wasm32"))]
    jobs: std::sync::mpsc::Sender<Job>,
    #[cfg(not(target_arch = "wasm32"))]
    replies: std::sync::mpsc::Receiver<Reply>,
    #[cfg(target_arch = "wasm32")]
    state: WorkerState,
    #[cfg(target_arch = "wasm32")]
    replies: Vec<Reply>,
}

#[derive(Default)]
struct WorkerState {
    view: Option<(u64, PdfView)>,
}

impl WorkerState {
    fn run(&mut self, job: Job, reply: &mut dyn FnMut(Reply)) {
        match job {
            Job::Show { version, source } => {
                let exported = matches!(source, Source::Export(_));
                let bytes = match source {
                    Source::Bytes(b) => Ok(b.as_ref().clone()),
                    Source::Export(doc) => wordcraft_engine::io::save_bytes("view.pdf", &doc),
                };
                match bytes.and_then(|b| PdfView::open(b).map_err(|e| e.to_string())) {
                    Ok(view) => {
                        reply(Reply::Opened { version, sizes: view.page_sizes().to_vec(), exported });
                        self.view = Some((version, view));
                    }
                    Err(error) => reply(Reply::Failed { version, error }),
                }
            }
            Job::Render { version, page, scale, key } => {
                if let Some((v, view)) = &self.view
                    && *v == version
                    && let Ok(image) = view.render(page, scale)
                {
                    reply(Reply::Page { version, page, key, image });
                }
            }
        }
    }
}

impl Worker {
    #[cfg(not(target_arch = "wasm32"))]
    fn start(ctx: &egui::Context) -> Option<Worker> {
        let (jobs, job_rx) = std::sync::mpsc::channel::<Job>();
        let (reply_tx, replies) = std::sync::mpsc::channel::<Reply>();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("pdf-view".into())
            .spawn(move || {
                let mut state = WorkerState::default();
                while let Ok(first) = job_rx.recv() {
                    let mut batch = vec![first];
                    batch.extend(job_rx.try_iter());
                    // Only the newest PDF matters; renders of older ones are dropped by version.
                    let last_show = batch.iter().rposition(|j| matches!(j, Job::Show { .. }));
                    // Renders newest first: the page just scrolled to comes before the ones passed.
                    let (shows, mut renders): (Vec<_>, Vec<_>) = batch.into_iter().enumerate().partition(|(_, j)| matches!(j, Job::Show { .. }));
                    renders.reverse();
                    for (i, job) in shows.into_iter().chain(renders) {
                        if matches!(job, Job::Show { .. }) && Some(i) != last_show {
                            continue;
                        }
                        // A panic here (a hostile PDF) ends the view's worker, never the app.
                        let mut send = |r: Reply| {
                            let _ = reply_tx.send(r);
                        };
                        state.run(job, &mut send);
                        ctx.request_repaint();
                    }
                }
            })
            .ok()?;
        Some(Worker { jobs, replies })
    }

    #[cfg(target_arch = "wasm32")]
    fn start(_ctx: &egui::Context) -> Option<Worker> {
        Some(Worker { state: WorkerState::default(), replies: Vec::new() })
    }

    /// Hands a job over; false when the worker is gone.
    fn send(&mut self, job: Job) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.jobs.send(job).is_ok()
        }
        #[cfg(target_arch = "wasm32")]
        {
            let replies = &mut self.replies;
            self.state.run(job, &mut |r| replies.push(r));
            true
        }
    }

    fn take(&mut self) -> Vec<Reply> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.replies.try_iter().collect()
        }
        #[cfg(target_arch = "wasm32")]
        {
            std::mem::take(&mut self.replies)
        }
    }
}

/// The PDF on screen.
struct Shown {
    version: u64,
    sizes: Vec<(f32, f32)>,
    exported: bool,
}

/// The PDF view's state.
#[derive(Default)]
pub struct PdfPane {
    pub mode: PdfMode,
    worker: Option<Worker>,
    worker_failed: bool,
    /// The last PDF asked for.
    version: u64,
    shown: Option<Shown>,
    /// The document state (document id, revision) the PDF shows or is being made from.
    requested: Option<(u64, u64)>,
    /// The document state last seen, and when it changed (seconds).
    seen: Option<(u64, u64)>,
    changed_at: f64,
    textures: HashMap<usize, (u64, u32, egui::TextureHandle)>,
    pending: HashSet<(u64, usize, u32)>,
    pub error: Option<String>,
    /// Zoom over Fit Width (1 = the widest page fills the view).
    zoom: f32,
    /// A page to bring to the top of the view next frame.
    scroll_to: Option<usize>,
    /// The page filling most of the view, last frame.
    pub page: usize,
    /// The page both sides last agreed on.
    synced: Option<usize>,
    /// The view's rect last frame (whether the pointer is over it leads the page sync).
    rect: Option<Rect>,
    /// Width of the view beside the page (DOCX + PDF).
    side_width: Option<f32>,
}

impl PdfPane {
    pub fn set_mode(&mut self, mode: PdfMode) {
        if mode != self.mode {
            self.mode = mode;
            self.synced = None;
            // The view opens on the page it was on (the sync then follows the document).
            self.scroll_to = Some(self.page);
        }
    }

    /// The PDF on screen: its pages and whether it is the document's export (or the PDF it was
    /// opened from).
    pub fn info(&self) -> serde_json::Value {
        match &self.shown {
            Some(s) => json!({"mode": self.mode.id(), "pages": s.sizes.len(), "page": self.page + 1, "live": s.exported, "error": self.error}),
            None => json!({"mode": self.mode.id(), "pages": 0, "error": self.error}),
        }
    }
}

fn now(ctx: &egui::Context) -> f64 {
    ctx.input(|i| i.time)
}

/// Each frame, before drawing: start the worker, ask for a new PDF when the document changed
/// (once editing pauses), and take in what the worker made.
pub fn logic(app: &mut WordApp, ctx: &egui::Context) {
    let pane = &mut app.pdf;
    if pane.mode == PdfMode::Docx || !app_has_document(app) {
        return;
    }
    let pane = &mut app.pdf;
    if pane.worker.is_none() && !pane.worker_failed {
        pane.worker = Worker::start(ctx);
        pane.worker_failed = pane.worker.is_none();
        if pane.zoom <= 0.0 {
            pane.zoom = 1.0;
        }
    }
    let want = (app.session.document_id(), app.session.rev());
    let t = now(ctx);
    if pane.seen != Some(want) {
        pane.seen = Some(want);
        pane.changed_at = t;
    }
    if pane.requested != Some(want) {
        let original = app.session.pdf_source.clone().filter(|_| app.session.pdf_source_rev == want.1);
        let new_document = pane.requested.map(|r| r.0) != Some(want.0);
        let settled = t - pane.changed_at >= SETTLE;
        if original.is_some() || new_document || settled {
            let source = match original {
                Some(bytes) => Source::Bytes(bytes),
                None => Source::Export(Box::new(app.session.doc.clone())),
            };
            pane.version += 1;
            let version = pane.version;
            if let Some(w) = pane.worker.as_mut()
                && w.send(Job::Show { version, source })
            {
                pane.requested = Some(want);
            } else {
                pane.worker = None;
                pane.worker_failed = true;
                pane.error = Some(tl!("The PDF view stopped working; restart WordCraft to bring it back.").to_string());
            }
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64((SETTLE - (t - pane.changed_at)).max(0.05)));
        }
    }
    let replies = pane.worker.as_mut().map(Worker::take).unwrap_or_default();
    for r in replies {
        match r {
            Reply::Opened { version, sizes, exported } => {
                // Only a newer PDF replaces the one on screen.
                if pane.shown.as_ref().is_none_or(|s| s.version < version) {
                    pane.shown = Some(Shown { version, sizes, exported });
                    pane.error = None;
                    pane.pending.retain(|p| p.0 >= version);
                }
            }
            Reply::Failed { version, error } => {
                if pane.shown.as_ref().is_none_or(|s| s.version < version) {
                    pane.error = Some(error);
                }
            }
            Reply::Page { version, page, key, image } => {
                pane.pending.remove(&(version, page, key));
                if pane.shown.as_ref().is_some_and(|s| s.version == version) {
                    let size = [image.width as usize, image.height as usize];
                    let ci = egui::ColorImage::from_rgba_unmultiplied(size, &image.rgba);
                    let tex = ctx.load_texture(format!("pdf-page{page}"), ci, egui::TextureOptions::LINEAR);
                    pane.textures.insert(page, (version, key, tex));
                }
            }
        }
    }
}

fn app_has_document(app: &WordApp) -> bool {
    app.has_document()
}

/// The document page filling most of the editing view, from last frame's layout.
fn doc_page(app: &WordApp) -> Option<usize> {
    let vp = app.canvas.viewport?;
    let mut best: Option<(f32, usize)> = None;
    for (i, r) in app.canvas.page_rects.iter().enumerate() {
        let h = (r.max.y.min(vp.max.y) - r.min.y.max(vp.min.y)).max(0.0);
        let w = (r.max.x.min(vp.max.x) - r.min.x.max(vp.min.x)).max(0.0);
        if h * w > best.map_or(0.0, |b| b.0) {
            best = Some((h * w, i));
        }
    }
    best.map(|b| b.1)
}

/// Scroll the editing view so page `page` is at its top.
fn scroll_doc_to(app: &mut WordApp, page: usize) {
    let (Some(vp), Some(r)) = (app.canvas.viewport, app.canvas.page_rects.get(page)) else { return };
    let y = app.canvas.scroll_offset.y + (r.min.y - vp.min.y) - 16.0;
    app.canvas.scroll_request = Some(vec2(app.canvas.scroll_offset.x, y.max(0.0)));
}

/// The view beside the page (DOCX + PDF): a resizable panel on the right.
pub fn show_side(app: &mut WordApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let full = ui.available_width();
    let width = app.pdf.side_width.unwrap_or(full * 0.45).clamp(240.0, (full - 320.0).max(240.0));
    let resp = egui::Panel::right("pdf_view")
        .default_size(width)
        .min_size(240.0)
        .max_size((full - 320.0).max(240.0))
        .resizable(true)
        .frame(egui::Frame::NONE.fill(t.canvas).stroke(Stroke::new(1.0, t.border)))
        .show(ui, |ui| view(app, ui));
    app.pdf.side_width = Some(resp.response.rect.width());
}

/// The view in place of the page (PDF).
pub fn show_full(app: &mut WordApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| view(app, ui));
}

fn view(app: &mut WordApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    toolbar(app, ui, &t);
    let Some(shown) = app.pdf.shown.as_ref() else {
        let msg = app.pdf.error.clone().unwrap_or_else(|| tl!("Making the PDF…").to_string());
        ui.centered_and_justified(|ui| ui.label(egui::RichText::new(msg).color(t.text_dim)));
        return;
    };
    let version = shown.version;
    let sizes = shown.sizes.clone();
    let pane_rect = ui.available_rect_before_wrap();
    app.pdf.rect = Some(pane_rect);
    let widest = sizes.iter().map(|s| s.0).fold(1.0f32, f32::max);
    let zoom = if app.pdf.zoom > 0.0 { app.pdf.zoom } else { 1.0 };
    // Beside the page: the width fills the view. Alone: the whole page fits (as PDF readers open).
    let fit_w = (pane_rect.width() - 2.0 * GAP - 14.0).max(80.0) / widest;
    let tallest = sizes.iter().map(|s| s.1).fold(1.0f32, f32::max);
    let fit = if app.pdf.mode == PdfMode::Pdf { fit_w.min((pane_rect.height() - 2.0 * GAP).max(80.0) / tallest) } else { fit_w };
    let scale = (fit * zoom).clamp(0.05, 8.0);
    let ppp = ui.ctx().pixels_per_point();
    // Page images at the nearest 2% step, so resizing the view doesn't re-render every frame.
    let key = ((scale * ppp * 50.0).round() as u32).max(1);
    let render_scale = key as f32 / 50.0;
    let mut tops = Vec::with_capacity(sizes.len());
    let mut y = GAP;
    for (w, h) in &sizes {
        tops.push(y);
        let _ = w;
        y += h * scale + GAP;
    }
    let content_h = y;
    let content_w = widest * scale + 2.0 * GAP;

    // Who leads the page sync: the side the pointer is over.
    let pointer_over_pdf = ui.ctx().pointer_hover_pos().is_some_and(|p| pane_rect.contains(p));
    let side = app.pdf.mode == PdfMode::Side;
    if side
        && !pointer_over_pdf
        && let Some(p) = doc_page(app)
        && app.pdf.synced != Some(p)
    {
        app.pdf.synced = Some(p);
        if p != app.pdf.page {
            app.pdf.scroll_to = Some(p.min(sizes.len().saturating_sub(1)));
        }
    }

    let mut area = egui::ScrollArea::both().id_salt("pdf_scroll").auto_shrink([false, false]);
    if let Some(p) = app.pdf.scroll_to.take()
        && let Some(top) = tops.get(p)
    {
        area = area.vertical_scroll_offset((top - GAP).max(0.0));
    }
    let mut wanted: Vec<(usize, f32)> = Vec::new();
    let mut visible_best: Option<(f32, usize)> = None;
    let mut visible_range = (usize::MAX, 0usize);
    area.show_viewport(ui, |ui, viewport| {
        let origin = ui.min_rect().min;
        let avail_w = ui.available_width().max(content_w);
        ui.allocate_space(vec2(avail_w, content_h));
        let painter = ui.painter();
        for (i, (w, h)) in sizes.iter().enumerate() {
            let top = tops.get(i).copied().unwrap_or(0.0);
            let (pw, ph) = (w * scale, h * scale);
            let local = Rect::from_min_size(pos2(((avail_w - pw) / 2.0).max(GAP), top), vec2(pw, ph));
            if local.max.y < viewport.min.y - ph || local.min.y > viewport.max.y + ph {
                continue;
            }
            let r = local.translate(origin.to_vec2());
            let seen = (local.max.y.min(viewport.max.y) - local.min.y.max(viewport.min.y)).max(0.0);
            if seen > 0.0 {
                visible_range = (visible_range.0.min(i), visible_range.1.max(i));
                if seen > visible_best.map_or(0.0, |b| b.0) {
                    visible_best = Some((seen, i));
                }
            }
            painter.rect_filled(r.translate(vec2(0.0, 2.0)).expand(1.0), 2.0, t.page_shadow);
            painter.rect_filled(r, 0.0, Color32::WHITE);
            match app.pdf.textures.get(&i) {
                Some((v, k, tex)) => {
                    painter.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    if *v != version || *k != key {
                        wanted.push((i, seen));
                    }
                }
                None => wanted.push((i, seen)),
            }
        }
    });
    if let Some((_, p)) = visible_best {
        app.pdf.page = p;
    }
    // The PDF leads while the pointer is over it.
    if side && pointer_over_pdf && app.pdf.synced != Some(app.pdf.page) {
        app.pdf.synced = Some(app.pdf.page);
        let p = app.pdf.page;
        scroll_doc_to(app, p);
    }
    // Ask for the pages in view (most visible last, so the worker draws it first).
    wanted.sort_by(|a, b| a.1.total_cmp(&b.1));
    for (page, _) in wanted {
        if app.pdf.pending.insert((version, page, key))
            && let Some(w) = app.pdf.worker.as_mut()
        {
            w.send(Job::Render { version, page, scale: render_scale, key });
        }
    }
    // Pages far from view give their images back.
    let (lo, hi) = visible_range;
    if lo != usize::MAX {
        app.pdf.textures.retain(|p, _| *p + KEEP >= lo && *p <= hi + KEEP);
    }
}

fn toolbar(app: &mut WordApp, ui: &mut Ui, t: &Tokens) {
    let (pages, live) = app.pdf.shown.as_ref().map_or((0, false), |s| (s.sizes.len(), s.exported));
    egui::Frame::NONE.fill(t.ribbon).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 0.0);
            let title = if pages == 0 {
                tl!("PDF").to_string()
            } else if live {
                tl!("PDF · updates as you edit").to_string()
            } else {
                tl!("PDF · as opened").to_string()
            };
            ui.label(egui::RichText::new(title).strong().color(t.text));
            if pages > 0 {
                ui.label(egui::RichText::new(format!("{} {} / {pages}", tl!("Page"), app.pdf.page + 1)).color(t.text_dim));
            }
            if let Some(e) = &app.pdf.error {
                ui.label(egui::RichText::new(e).color(Color32::from_rgb(0xC4, 0x2B, 0x1C))).on_hover_text(e.as_str());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tl!("Export PDF…")).on_hover_text(tl!("Save the document as a PDF")).clicked() {
                    let _ = app.run("file.exportPdf", json!({}));
                }
                let z = if app.pdf.zoom > 0.0 { app.pdf.zoom } else { 1.0 };
                if ui.button("+").on_hover_text(tl!("Zoom In")).clicked() {
                    app.pdf.zoom = (z * 1.25).min(6.0);
                }
                if ui.button(tl!("Fit")).on_hover_text(tl!("Fit the page")).clicked() {
                    app.pdf.zoom = 1.0;
                }
                if ui.button("−").on_hover_text(tl!("Zoom Out")).clicked() {
                    app.pdf.zoom = (z / 1.25).max(0.2);
                }
                ui.label(egui::RichText::new(format!("{:.0}%", z * 100.0)).color(t.text_dim));
            });
        });
    });
    let r = ui.max_rect();
    ui.painter().hline(r.x_range(), ui.min_rect().max.y, Stroke::new(1.0, t.border));
}

/// The DOCX · PDF · DOCX + PDF switch (title bar): `height` tall, drawn right to left.
pub fn mode_switch(app: &mut WordApp, ui: &mut Ui, height: f32) {
    let t = Tokens::get(ui.ctx());
    let items = [
        (PdfMode::Side, tl!("DOCX + PDF"), tl!("The document and its PDF side by side, page for page")),
        (PdfMode::Pdf, tl!("PDF"), tl!("The document as a PDF")),
        (PdfMode::Docx, tl!("DOCX"), tl!("The document")),
    ];
    let font = egui::FontId::proportional(12.0);
    for (mode, label, tip) in items {
        let w = ui.painter().layout_no_wrap(label.to_string(), font.clone(), t.text).size().x + 18.0;
        let (r, resp) = ui.allocate_exact_size(vec2(w, height), Sense::click());
        let on = app.pdf.mode == mode;
        let fill = if on {
            t.accent
        } else if resp.hovered() {
            t.hover
        } else {
            t.ribbon
        };
        ui.painter().rect(r, 5.0, fill, Stroke::new(1.0, if on { t.accent } else { t.border }), egui::StrokeKind::Inside);
        ui.painter().text(r.center(), Align2::CENTER_CENTER, label, font.clone(), if on { t.on_accent } else { t.text });
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, on, label));
        if resp.on_hover_text(tip).clicked() {
            let _ = app.run("ui.pdfView", json!({"mode": mode.id()}));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Services, WordApp};
    use egui_kittest::kittest::Queryable;
    use wordcraft_engine::Session;

    fn harness() -> egui_kittest::Harness<'static, WordApp> {
        let mut s = Session::new(wordcraft_doc::Document::new());
        s.run("text.insert", &json!({"text": "Page one"})).unwrap();
        s.run("insert.pageBreak", &json!({})).unwrap();
        s.run("text.insert", &json!({"text": "Page two"})).unwrap();
        egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 900.0)).build_ui_state(
            |ui, app: &mut WordApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            WordApp::new(s, Services::default()),
        )
    }

    /// Frames until `done` holds (the worker runs on its own thread), at most a few seconds.
    fn until(h: &mut egui_kittest::Harness<'static, WordApp>, done: impl Fn(&WordApp) -> bool) {
        for _ in 0..600 {
            h.step();
            if done(h.state()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("timed out: {}", h.state().pdf.info());
    }

    #[test]
    fn the_pdf_view_shows_the_document_and_follows_its_edits() {
        let mut h = harness();
        h.run_steps(4);
        assert_eq!(h.state_mut().run("ui.pdfView", json!({})).unwrap()["mode"], "docx");
        // The title bar's switch.
        h.get_by_label("DOCX + PDF").click();
        until(&mut h, |a| a.pdf.shown.is_some());
        let info = h.state().pdf.info();
        assert_eq!(info["mode"], "side");
        assert_eq!(info["pages"], 2, "page for page: {info}");
        assert_eq!(info["live"], true, "a new document's PDF is its export");
        // An edit makes the PDF again once editing pauses.
        let before = h.state().pdf.version;
        h.state_mut().run("insert.pageBreak", json!({})).unwrap();
        until(&mut h, |a| a.pdf.shown.as_ref().is_some_and(|s| s.version > before && s.sizes.len() == 3));
        // PDF alone, and back.
        assert_eq!(h.state_mut().run("ui.pdfView", json!({"mode": "pdf"})).unwrap()["mode"], "pdf");
        h.run_steps(4);
        assert!(h.query_by_label("Export PDF…").is_some());
        assert_eq!(h.state_mut().run("ui.pdfView", json!({"mode": "docx"})).unwrap()["mode"], "docx");
        for junk in [json!({"mode": "nope"}), json!({"mode": 3}), json!({"mode": null})] {
            assert!(h.state_mut().run("ui.pdfView", junk).is_err());
        }
    }
}

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
//! PDF edits (`pdf.*`, [`crate::pdf_edit`]: markup, pages, stamps, forms) are applied over that
//! PDF as it is shown, the moment they are made.
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
use wordcraft_engine::pdfedit::{Field, Op};
use wordcraft_engine::pdfread::{PageImage, PdfView, TextLine};

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

pub(crate) enum Source {
    /// The PDF itself (the one the document was opened from).
    Bytes(Arc<Vec<u8>>),
    /// The document, to export first.
    Export(Box<wordcraft_engine::doc::Document>),
}

pub(crate) enum Job {
    /// Show the PDF of `source` (known by `base`: the document state it is) with `ops` applied.
    Show {
        version: u64,
        base: (u64, u64),
        source: Source,
        ops: Arc<Vec<Op>>,
    },
    Render {
        version: u64,
        page: usize,
        scale: f32,
        key: u32,
        thumb: bool,
    },
    /// A page's text lines (for marking up and editing text).
    Lines {
        version: u64,
        page: usize,
    },
    /// Where `query` is found.
    Search {
        version: u64,
        query: String,
    },
    /// The pages' text, for copying.
    Text {
        version: u64,
    },
}

pub(crate) enum Reply {
    Opened { version: u64, sizes: Vec<(f32, f32)>, exported: bool, fields: Vec<Field>, marks: HashMap<u64, usize> },
    Failed { version: u64, error: String },
    Page { version: u64, page: usize, key: u32, image: PageImage, thumb: bool },
    Lines { version: u64, page: usize, lines: Vec<TextLine> },
    Hits { version: u64, query: String, hits: Vec<(usize, [f32; 4])> },
    Text { text: String },
}

/// The worker that exports and renders. It holds the open PDF; only sizes and pixels come back.
pub(crate) struct Worker {
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
    /// The last PDF made from the document (its state, the bytes): edits to the PDF alone
    /// don't export it again.
    base: Option<((u64, u64), Arc<Vec<u8>>)>,
}

impl WorkerState {
    fn run(&mut self, job: Job, reply: &mut dyn FnMut(Reply)) {
        fn current(v: u64, s: &WorkerState) -> Option<&PdfView> {
            s.view.as_ref().filter(|(sv, _)| *sv == v).map(|(_, view)| view)
        }
        match job {
            Job::Show { version, base, source, ops } => {
                let exported = matches!(source, Source::Export(_));
                let bytes = match (&self.base, source) {
                    (Some((k, b)), _) if *k == base => Ok(b.clone()),
                    (_, Source::Bytes(b)) => Ok(b),
                    (_, Source::Export(doc)) => wordcraft_engine::io::save_bytes("view.pdf", &doc).map(Arc::new),
                };
                let made = bytes.and_then(|b| {
                    self.base = Some((base, b.clone()));
                    if ops.is_empty() {
                        return Ok((b.as_ref().clone(), HashMap::new()));
                    }
                    let opts = wordcraft_engine::pdfedit::SaveOptions { compress: false, ..Default::default() };
                    let edited = wordcraft_engine::pdfedit::apply(&b, &ops, &opts).map_err(|e| e.to_string())?;
                    Ok((edited, wordcraft_engine::pdfedit::mark_pages(&b, &ops)))
                });
                match made.and_then(|(b, marks)| {
                    let fields = wordcraft_engine::pdfedit::fields(&b).unwrap_or_default();
                    PdfView::open(b).map(|v| (v, fields, marks)).map_err(|e| e.to_string())
                }) {
                    Ok((view, fields, marks)) => {
                        reply(Reply::Opened { version, sizes: view.page_sizes().to_vec(), exported, fields, marks });
                        self.view = Some((version, view));
                    }
                    Err(error) => reply(Reply::Failed { version, error }),
                }
            }
            Job::Render { version, page, scale, key, thumb } => {
                if let Some(view) = current(version, self)
                    && let Ok(image) = view.render(page, scale)
                {
                    reply(Reply::Page { version, page, key, image, thumb });
                }
            }
            Job::Lines { version, page } => {
                if let Some(view) = current(version, self) {
                    reply(Reply::Lines { version, page, lines: view.text_lines(page) });
                }
            }
            Job::Search { version, query } => {
                if let Some(view) = current(version, self) {
                    reply(Reply::Hits { version, hits: view.search(&query, 5000), query });
                }
            }
            Job::Text { version } => {
                if let Some(view) = current(version, self) {
                    let text: Vec<String> = (0..view.page_count()).map(|p| view.page_text(p)).collect();
                    reply(Reply::Text { text: text.join("\n\n") });
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
    pub(crate) fn send(&mut self, job: Job) -> bool {
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
pub(crate) struct Shown {
    pub version: u64,
    pub sizes: Vec<(f32, f32)>,
    exported: bool,
    /// Its form fields.
    pub fields: Vec<Field>,
    /// The page each mark is on (by mark id), where it can still be picked.
    pub marks: HashMap<u64, usize>,
}

/// The PDF view's state.
#[derive(Default)]
pub struct PdfPane {
    pub mode: PdfMode,
    pub(crate) worker: Option<Worker>,
    worker_failed: bool,
    /// The last PDF asked for.
    version: u64,
    pub(crate) shown: Option<Shown>,
    /// The document state (document id, revision, PDF edits' revision) the PDF shows or is being
    /// made from.
    requested: Option<(u64, u64, u64)>,
    /// The document state last seen, and when it changed (seconds).
    seen: Option<(u64, u64)>,
    changed_at: f64,
    textures: HashMap<usize, (u64, u32, egui::TextureHandle)>,
    pending: HashSet<(u64, usize, u32)>,
    /// Page thumbnails (Pages panel): version, texture.
    pub(crate) thumbs: HashMap<usize, (u64, egui::TextureHandle)>,
    pub(crate) thumbs_pending: HashSet<(u64, usize)>,
    pub error: Option<String>,
    /// Zoom over Fit Width (1 = the widest page fills the view).
    pub(crate) zoom: f32,
    /// A page to bring to the top of the view next frame.
    pub(crate) scroll_to: Option<usize>,
    /// The page filling most of the view, last frame.
    pub page: usize,
    /// The page both sides last agreed on.
    synced: Option<usize>,
    /// The view's rect last frame (whether the pointer is over it leads the page sync).
    pub(crate) rect: Option<Rect>,
    /// Width of the view beside the page (DOCX + PDF).
    side_width: Option<f32>,
    /// The pages on screen last frame (index, rect) and their scale (screen points per point).
    pub(crate) on_screen: (Vec<(usize, Rect)>, f32),
    /// The editing tools' state.
    pub ed: crate::pdf_edit::Editor,
    /// The last document seen: one opened from a PDF opens in PDF mode.
    seen_document: Option<u64>,
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
    /// The PDF on screen's version (`None`: none yet).
    pub(crate) fn shown_version(&self) -> Option<u64> {
        self.shown.as_ref().map(|s| s.version)
    }

    pub fn info(&self) -> serde_json::Value {
        match &self.shown {
            Some(s) => {
                json!({"mode": self.mode.id(), "pages": s.sizes.len(), "page": self.page + 1, "live": s.exported, "error": self.error, "tool": self.ed.tool.id(), "fields": s.fields.len(), "version": s.version})
            }
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
    // A document opened from a PDF opens as the PDF, ready to edit.
    let id = app.session.document_id();
    if app.pdf.seen_document != Some(id) {
        app.pdf.seen_document = Some(id);
        app.pdf.ed = Default::default();
        if app.session.pdf_source.is_some() && app.pdf.mode == PdfMode::Docx {
            app.pdf.set_mode(PdfMode::Pdf);
        }
    }
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
    let doc_state = (app.session.document_id(), app.session.rev());
    let want = (doc_state.0, doc_state.1, app.session.pdf.rev);
    let t = now(ctx);
    if pane.seen != Some(doc_state) {
        pane.seen = Some(doc_state);
        pane.changed_at = t;
    }
    if pane.requested != Some(want) {
        let original = app.session.pdf_source.clone().filter(|_| app.session.pdf_source_rev == want.1);
        let new_document = pane.requested.map(|r| r.0) != Some(want.0);
        let settled = t - pane.changed_at >= SETTLE;
        // Only the PDF's edits changed: shown at once.
        let same_document = pane.requested.map(|r| (r.0, r.1)) == Some(doc_state);
        if original.is_some() || new_document || settled || same_document {
            // The PDF the edits apply to, known by the document state it shows.
            let base = if original.is_some() { (doc_state.0, app.session.pdf_source_rev) } else { doc_state };
            let source = match original {
                Some(bytes) => Source::Bytes(bytes),
                None => Source::Export(Box::new(app.session.doc.clone())),
            };
            let ops = Arc::new(app.session.pdf.ops.clone());
            pane.version += 1;
            let version = pane.version;
            if let Some(w) = pane.worker.as_mut()
                && w.send(Job::Show { version, base, source, ops })
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
            Reply::Opened { version, sizes, exported, fields, marks } => {
                // Only a newer PDF replaces the one on screen.
                if pane.shown.as_ref().is_none_or(|s| s.version < version) {
                    pane.shown = Some(Shown { version, sizes, exported, fields, marks });
                    pane.ed.new_version(version);
                    pane.error = None;
                    pane.pending.retain(|p| p.0 >= version);
                }
            }
            Reply::Failed { version, error } => {
                if pane.shown.as_ref().is_none_or(|s| s.version < version) {
                    pane.error = Some(error);
                }
            }
            Reply::Page { version, page, key, image, thumb } => {
                if thumb {
                    pane.thumbs_pending.remove(&(version, page));
                } else {
                    pane.pending.remove(&(version, page, key));
                }
                if pane.shown.as_ref().is_some_and(|s| s.version == version) {
                    let size = [image.width as usize, image.height as usize];
                    let ci = egui::ColorImage::from_rgba_unmultiplied(size, &image.rgba);
                    if thumb {
                        let tex = ctx.load_texture(format!("pdf-thumb{page}"), ci, egui::TextureOptions::LINEAR);
                        pane.thumbs.insert(page, (version, tex));
                    } else {
                        let tex = ctx.load_texture(format!("pdf-page{page}"), ci, egui::TextureOptions::LINEAR);
                        pane.textures.insert(page, (version, key, tex));
                    }
                }
            }
            Reply::Lines { version, page, lines } => pane.ed.got_lines(version, page, lines),
            Reply::Hits { version, query, hits } => pane.ed.got_hits(version, query, hits),
            Reply::Text { text } => {
                ctx.copy_text(text);
                pane.ed.status = Some(tl!("Copied the PDF's text").to_string());
            }
        }
    }
    if let Some(msg) = app.pdf.ed.status.take() {
        app.status(msg);
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
    if app.pdf.ed.pages_panel {
        egui::Panel::left("pdf_pages")
            .default_size(150.0)
            .min_size(110.0)
            .max_size(320.0)
            .resizable(true)
            .frame(egui::Frame::NONE.fill(t.ribbon).stroke(Stroke::new(1.0, t.border)))
            .show(ui, |ui| crate::pdf_edit::pages_panel(app, ui));
    }
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| view(app, ui));
}

fn view(app: &mut WordApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    toolbar(app, ui, &t);
    crate::pdf_edit::toolbar(app, ui, &t);
    crate::pdf_edit::keys(app, ui.ctx());
    crate::pdf_edit::dialogs(app, ui.ctx());
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

    // Dragging on a page draws (or selects), so it doesn't scroll.
    let mut area = egui::ScrollArea::both().id_salt("pdf_scroll").auto_shrink([false, false]).scroll_source(egui::scroll_area::ScrollSource {
        scroll_bar: true,
        drag: egui::scroll_area::DragScroll::Never,
        mouse_wheel: true,
    });
    if let Some(p) = app.pdf.scroll_to.take()
        && let Some(top) = tops.get(p)
    {
        area = area.vertical_scroll_offset((top - GAP).max(0.0));
    }
    let mut wanted: Vec<(usize, f32)> = Vec::new();
    let mut visible_best: Option<(f32, usize)> = None;
    let mut visible_range = (usize::MAX, 0usize);
    let mut on_screen: Vec<(usize, Rect)> = Vec::new();
    area.show_viewport(ui, |ui, viewport| {
        let origin = ui.min_rect().min;
        let avail_w = ui.available_width().max(content_w);
        ui.allocate_space(vec2(avail_w, content_h));
        let painter = ui.painter().clone();
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
            on_screen.push((i, r));
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
        // Marking up, selecting and filling in, over the pages.
        crate::pdf_edit::overlay(app, ui, &on_screen, scale);
        app.pdf.on_screen = (on_screen, scale);
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
            w.send(Job::Render { version, page, scale: render_scale, key, thumb: false });
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

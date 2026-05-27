mod api;
mod export;
mod model3d;
mod preview;
mod settings;

use api::{Component, Pin, SearchResult};
use eframe::egui;
use egui::TextureHandle;
use settings::Settings;

fn setup_fonts(ctx: &egui::Context) {
    // Try to load a system font with full Unicode coverage (degree signs, CJK, etc.)
    let paths: &[&str] = if cfg!(target_os = "windows") {
        &[
            "C:\\Windows\\Fonts\\segoeui.ttf",
            "C:\\Windows\\Fonts\\arial.ttf",
        ]
    } else if cfg!(target_os = "macos") {
        &[
            "/System/Library/Fonts/Helvetica.ttc",
            "/Library/Fonts/Arial.ttf",
        ]
    } else {
        &[
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",        // Arch
            "/usr/share/fonts/truetype/freefont/FreeSans.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans.ttf",      // RPi OS
        ]
    };

    if let Some(data) = paths.iter().find_map(|p| std::fs::read(p).ok()) {
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "system".to_owned(),
            egui::FontData::from_owned(data).into(),
        );
        // Insert as first fallback so it fills missing glyphs before the built-in font
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .insert(0, "system".to_owned());
        fonts
            .families
            .get_mut(&egui::FontFamily::Monospace)
            .unwrap()
            .push("system".to_owned());
        ctx.set_fonts(fonts);
    }
}

#[derive(Clone, Copy, PartialEq, Default)]
enum SortCol {
    Class, Lcsc, Value, Package, Mfr,
    #[default]
    Stock,
    Price,
}

#[derive(Clone)]
struct PanZoom {
    pan: egui::Vec2,  // UV offset from center
    zoom: f32,
}

impl Default for PanZoom {
    fn default() -> Self { Self { pan: egui::Vec2::ZERO, zoom: 1.0 } }
}

impl PanZoom {
    fn reset(&mut self) { *self = Self::default(); }
}

enum BgMsg {
    SearchDone(Vec<SearchResult>),
    SearchErr(String),
    DetailDone(Component, Option<Vec<u8>>, Option<Vec<u8>>),  // comp, wrl_vrml, step
    DetailErr(String),
}

#[derive(Default)]
struct AppState {
    // Search input
    search_input: String,

    // Search results list
    search_results: Vec<SearchResult>,
    selected_idx: Option<usize>,

    // Full component detail
    component: Option<Component>,
    wrl_bytes: Option<Vec<u8>>,   // VRML 2.0 bytes (converted from EasyEDA OBJ)
    step_bytes: Option<Vec<u8>>,  // raw STEP binary
    symbol_texture: Option<TextureHandle>,
    footprint_texture: Option<TextureHandle>,

    // SVG preview pan/zoom
    sym_pz: PanZoom,
    fp_pz: PanZoom,

    // Symbol text position adjustment
    ref_pos: [f32; 2],  // Reference label X/Y
    val_pos: [f32; 2],  // Value label X/Y

    // 3D viewer + adjustment
    model_viewer: model3d::ModelViewer,
    model_offset:   [f32; 3],
    model_rotation: [f32; 3],
    model_scale:    [f32; 3],
    model_unified_scale: bool,

    // Settings
    settings: Settings,
    show_settings: bool,

    // Filters
    basic_only: bool,

    // Table sort state
    sort_col: Option<SortCol>,
    sort_asc: bool,

    // Deferred row click (set inside table closure, handled at top of update)
    pending_select: Option<usize>,

    // Status / loading
    status: String,
    loading: bool,

    bg_rx: Option<std::sync::mpsc::Receiver<BgMsg>>,
}

struct App {
    state: AppState,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        setup_fonts(&cc.egui_ctx);
        let mut state = AppState::default();
        state.settings = settings::load();
        state.model_scale = [1.0, 1.0, 1.0];
        App { state }
    }

    fn spawn<F>(&mut self, f: F)
    where
        F: FnOnce(std::sync::mpsc::Sender<BgMsg>) + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel();
        self.state.bg_rx = Some(rx);
        self.state.loading = true;
        std::thread::spawn(move || f(tx));
    }

    fn do_search(&mut self) {
        let query = self.state.search_input.trim().to_string();
        if query.is_empty() { return; }
        self.state.search_results.clear();
        self.state.selected_idx = None;
        self.state.component = None;
        self.state.symbol_texture = None;
        self.state.footprint_texture = None;
        self.state.status = format!("Searching for \"{}\"…", query);
        let basic_only = self.state.basic_only;
        self.spawn(move |tx| {
            match api::search_components(&query, 100, basic_only) {
                Ok(results) => { let _ = tx.send(BgMsg::SearchDone(results)); }
                Err(e) => { let _ = tx.send(BgMsg::SearchErr(e.to_string())); }
            }
        });
    }

    fn do_detail(&mut self, lcsc_id: String, ctx: &egui::Context) {
        self.state.component = None;
        self.state.symbol_texture = None;
        self.state.footprint_texture = None;
        self.state.wrl_bytes = None;
        self.state.step_bytes = None;
        self.state.status = format!("Loading {}…", lcsc_id);
        let ctx = ctx.clone();
        self.spawn(move |tx| {
            match api::fetch_component(&lcsc_id) {
                Err(e) => { let _ = tx.send(BgMsg::DetailErr(e.to_string())); }
                Ok(comp) => {
                    // Download WRL (EasyEDA OBJ → converted to VRML 2.0 in memory)
                    let wrl = comp.wrl_url.as_deref()
                        .and_then(|url| api::download_wrl(url));
                    // Download STEP (raw binary for KiCad export)
                    let step = comp.step_url.as_deref()
                        .and_then(|url| api::download_bytes(url).ok());
                    let _ = tx.send(BgMsg::DetailDone(comp, wrl, step));
                    ctx.request_repaint();
                }
            }
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Handle deferred row click from inside table closure
        if let Some(i) = self.state.pending_select.take() {
            if self.state.selected_idx != Some(i) {
                self.state.selected_idx = Some(i);
                let lcsc = self.state.search_results[i].lcsc_id.clone();
                self.do_detail(lcsc, ctx);
            }
        }

        // Poll background channel
        let msg = self.state.bg_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(msg) = msg {
            self.state.loading = false;
            self.state.bg_rx = None;
            match msg {
                BgMsg::SearchDone(results) => {
                    self.state.status = format!("{} results", results.len());
                    self.state.search_results = results;
                }
                BgMsg::SearchErr(e) => {
                    self.state.status = format!("Search error: {}", e);
                }
                BgMsg::DetailDone(comp, wrl, step) => {
                    if let Some(svg) = &comp.symbol_svg {
                        if let Ok(img) = preview::svg_to_image(svg, 400, 300) {
                            self.state.symbol_texture =
                                Some(ctx.load_texture("symbol", img, Default::default()));
                        }
                    }
                    if let Some(svg) = &comp.footprint_svg {
                        if let Ok(img) = preview::svg_to_image(svg, 400, 300) {
                            self.state.footprint_texture =
                                Some(ctx.load_texture("footprint", img, Default::default()));
                        }
                    }
                    self.state.sym_pz.reset();
                    self.state.fp_pz.reset();
                    // Default ref/val positions just outside the symbol body bounds
                    let (body_max_y, body_min_y): (f32, f32) = {
                        let ys: Vec<f32> = comp.pins.iter().flat_map(|p| {
                            let by = match p.angle {
                                90  => p.y + 2.54,
                                270 => p.y - 2.54,
                                _   => p.y,
                            };
                            [p.y, by]
                        }).collect();
                        if ys.is_empty() { (1.27, -1.27) }
                        else {
                            (ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
                             ys.iter().cloned().fold(f32::INFINITY,     f32::min))
                        }
                    };
                    self.state.ref_pos = [0.0, body_max_y + 2.54];
                    self.state.val_pos = [0.0, body_min_y - 2.54];
                    // Init 3D model placement. Bake EasyEDA c_rotation into the mesh so the
                    // viewer starts at 0,0,0 (matching KiCad's STEP display at 0,0,0 rotation).
                    self.state.model_offset   = [0.0; 3];
                    self.state.model_rotation = [0.0; 3];
                    self.state.model_scale    = [1.0, 1.0, 1.0];
                    self.state.model_viewer.reset_view();
                    if let Some(ref bytes) = wrl {
                        let pads: Vec<model3d::PadInfo> = comp.pads.iter()
                            .map(|p| model3d::PadInfo { cx: p.cx, cz: p.cy, w: p.w, h: p.h })
                            .collect();
                        let drawings: Vec<model3d::PcbDrawing> = comp.fp_drawings.iter()
                            .map(|d| model3d::PcbDrawing { tris: d.tris.clone(), color: d.color })
                            .collect();
                        self.state.model_viewer.load(bytes, &pads, &drawings, comp.model_init_rotation);
                    } else {
                        self.state.model_viewer.has_model = false;
                    }
                    let model_status = match (&wrl, &step) {
                        (Some(_), Some(_)) => " (WRL+STEP)",
                        (Some(_), None)    => " (WRL only)",
                        (None,    Some(_)) => " (STEP only)",
                        (None,    None)    => " (no 3D model)",
                    };
                    self.state.wrl_bytes  = wrl;
                    self.state.step_bytes = step;
                    self.state.status = format!("Loaded: {} ({}){}", comp.value, comp.lcsc_id, model_status);
                    self.state.component = Some(comp);
                }
                BgMsg::DetailErr(e) => {
                    self.state.status = format!("Error: {}", e);
                }
            }
            ctx.request_repaint();
        } else if self.state.loading {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        // Settings window
        if self.state.show_settings {
            egui::Window::new("Settings")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Library path:");
                    ui.text_edit_singleline(&mut self.state.settings.lib_path);
                    if ui.button("Browse…").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.state.settings.lib_path = path.to_string_lossy().to_string();
                        }
                    }
                    ui.add_space(4.0);
                    ui.label("Library name:");
                    ui.text_edit_singleline(&mut self.state.settings.lib_name);
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            let _ = settings::save(&self.state.settings);
                            self.state.show_settings = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.state.show_settings = false;
                        }
                    });
                });
        }

        // Top bar
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("JLCPCB → KiCad");
                ui.add_space(16.0);
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.state.search_input)
                        .hint_text("Any query: C7512, ULN2003ADR, relay 5V, 100nF 0402…")
                        .desired_width(400.0),
                );
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let clicked = ui.add_enabled(!self.state.loading, egui::Button::new("Search")).clicked();
                ui.checkbox(&mut self.state.basic_only, "Basic only");
                if (enter || clicked) && !self.state.loading {
                    self.do_search();
                }
                if self.state.loading { ui.spinner(); }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Settings").clicked() {
                        self.state.show_settings = true;
                    }
                });
            });
        });

        // Bottom status bar
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.label(&self.state.status);
        });

        // Left panel: search results table
        egui::SidePanel::left("results")
            .min_width(200.0)
            .default_width(600.0)
            .show(ctx, |ui| {
                ui.add_space(2.0);
                ui.label(egui::RichText::new(
                    format!("{} results  (click header to sort)", self.state.search_results.len())
                ).small());
                ui.separator();

                if self.state.search_results.is_empty() && !self.state.loading {
                    ui.label("No results.");
                    return;
                }

                let results = &self.state.search_results;

                // Compute sorted row order
                let mut sorted: Vec<usize> = (0..results.len()).collect();
                if let Some(col) = self.state.sort_col {
                    let asc = self.state.sort_asc;
                    sorted.sort_by(|&a, &b| {
                        let ra = &results[a];
                        let rb = &results[b];
                        let ord = match col {
                            SortCol::Class => ra.class.cmp(&rb.class),
                            SortCol::Lcsc  => ra.lcsc_id.cmp(&rb.lcsc_id),
                            SortCol::Value => ra.value.cmp(&rb.value),
                            SortCol::Package => ra.package.cmp(&rb.package),
                            SortCol::Mfr   => ra.manufacturer.cmp(&rb.manufacturer),
                            SortCol::Stock => ra.stock.cmp(&rb.stock),
                            SortCol::Price => ra.price.partial_cmp(&rb.price)
                                .unwrap_or(std::cmp::Ordering::Equal),
                        };
                        if asc { ord } else { ord.reverse() }
                    });
                }

                let row_even = egui::Color32::from_gray(28);
                let row_odd  = egui::Color32::from_gray(40);
                let sel_col  = ui.visuals().selection.bg_fill;
                let sort_col = self.state.sort_col;
                let sort_asc = self.state.sort_asc;

                // Capture header clicks without borrow conflict
                use std::cell::Cell;
                let hdr_click: Cell<Option<SortCol>> = Cell::new(None);

                use egui_extras::{Column, TableBuilder};

                egui::ScrollArea::horizontal().show(ui, |ui| {
                    TableBuilder::new(ui)
                    .striped(false)
                    .resizable(true)
                    .sense(egui::Sense::click())
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::exact(26.0))                        // B/E
                    .column(Column::exact(72.0))                        // LCSC
                    .column(Column::initial(110.0).at_least(80.0))      // Model
                    .column(Column::initial(80.0).at_least(60.0))       // Package
                    .column(Column::remainder().at_least(80.0))         // Manufacturer
                    .column(Column::exact(68.0))                        // Stock
                    .column(Column::exact(62.0))                        // Price
                    .header(20.0, |mut h| {
                        let headers: &[(&str, SortCol)] = &[
                            ("B/E",          SortCol::Class),
                            ("LCSC",         SortCol::Lcsc),
                            ("Model",        SortCol::Value),
                            ("Package",      SortCol::Package),
                            ("Manufacturer", SortCol::Mfr),
                            ("Stock",        SortCol::Stock),
                            ("Price",        SortCol::Price),
                        ];
                        for &(title, col) in headers {
                            h.col(|ui| {
                                let active = sort_col == Some(col);
                                let label = if active {
                                    format!("{} {}", title, if sort_asc { "▲" } else { "▼" })
                                } else {
                                    title.to_string()
                                };
                                let btn = ui.add_sized(
                                    ui.available_size(),
                                    egui::Button::new(
                                        egui::RichText::new(label).small().strong()
                                    ).frame(false),
                                );
                                if btn.clicked() {
                                    hdr_click.set(Some(col));
                                }
                                if btn.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(22.0, sorted.len(), |mut row| {
                            let row_i = row.index();
                            let real_i = sorted[row_i];
                            let r = &results[real_i];
                            let selected = self.state.selected_idx == Some(real_i);

                            row.set_selected(selected);

                            let bg = if selected { sel_col }
                                     else if row_i % 2 == 0 { row_even }
                                     else { row_odd };

                            let lbl = |ui: &mut egui::Ui, text: egui::RichText| {
                                ui.add(egui::Label::new(text).selectable(false));
                            };

                            // B/E badge
                            row.col(|ui| {
                                ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                let badge = if r.class == "Basic" {
                                    egui::RichText::new("B").small()
                                        .color(egui::Color32::from_rgb(100, 220, 100))
                                } else {
                                    egui::RichText::new("E").small()
                                        .color(egui::Color32::GRAY)
                                };
                                lbl(ui, badge);
                            });
                            row.col(|ui| { ui.painter().rect_filled(ui.max_rect(), 0.0, bg); lbl(ui, egui::RichText::new(&r.lcsc_id).small()); });
                            row.col(|ui| { ui.painter().rect_filled(ui.max_rect(), 0.0, bg); lbl(ui, egui::RichText::new(&r.value).small()); });
                            row.col(|ui| { ui.painter().rect_filled(ui.max_rect(), 0.0, bg); lbl(ui, egui::RichText::new(&r.package).small()); });
                            row.col(|ui| { ui.painter().rect_filled(ui.max_rect(), 0.0, bg); lbl(ui, egui::RichText::new(&r.manufacturer).small()); });
                            row.col(|ui| { ui.painter().rect_filled(ui.max_rect(), 0.0, bg); lbl(ui, egui::RichText::new(r.stock.to_string()).small()); });
                            row.col(|ui| { ui.painter().rect_filled(ui.max_rect(), 0.0, bg); lbl(ui, egui::RichText::new(format!("${:.4}", r.price)).small()); });

                            if row.response().clicked() {
                                self.state.pending_select = Some(real_i);
                            }
                        });
                    });
                });

                // Process header click (after table — avoids borrow conflict)
                if let Some(col) = hdr_click.get() {
                    if self.state.sort_col == Some(col) {
                        self.state.sort_asc = !self.state.sort_asc;
                    } else {
                        self.state.sort_col = Some(col);
                        // Numeric cols default descending, text cols ascending
                        self.state.sort_asc =
                            !matches!(col, SortCol::Stock | SortCol::Price);
                    }
                }
            });

        // Central panel: component detail
        egui::CentralPanel::default().show(ctx, |ui| {
            let Some(comp) = self.state.component.clone() else {
                ui.centered_and_justified(|ui| {
                    ui.label("Pick a component from the list.");
                });
                return;
            };

            egui::ScrollArea::both().show(ui, |ui| {
                // Prevent the two-column layout from squeezing below usable size
                ui.set_min_width(900.0);
                // Header
                ui.heading(&comp.value);
                ui.label(format!("{} | {} | {}", comp.lcsc_id, comp.package, comp.manufacturer));
                ui.label(egui::RichText::new(&comp.description).italics());
                ui.add_space(8.0);

                ui.columns(2, |cols| {
                    // Left: previews (interactive pan/zoom)
                    if comp.pins.is_empty() {
                        // No pin data → show EasyEDA SVG with Reference/Value overlay
                        cols[0].label(egui::RichText::new("Symbol  (EasyEDA SVG — no pin data)").strong());
                        if let Some(tex) = &self.state.symbol_texture {
                            let rect = show_panzoom_image(&mut cols[0], tex, egui::Vec2::new(360.0, 300.0), &mut self.state.sym_pz, "sym");
                            // Overlay Reference "U?" and Value labels like KiCad does
                            let painter = cols[0].painter().with_clip_rect(rect);
                            let lbl_font  = egui::FontId::proportional(13.0);
                            let lbl_color = egui::Color32::from_rgb(0, 0, 200);
                            painter.text(
                                rect.center_top() + egui::vec2(0.0, 6.0),
                                egui::Align2::CENTER_TOP,
                                "U?",
                                lbl_font.clone(),
                                lbl_color,
                            );
                            painter.text(
                                rect.center_bottom() + egui::vec2(0.0, -6.0),
                                egui::Align2::CENTER_BOTTOM,
                                &comp.value,
                                lbl_font,
                                lbl_color,
                            );
                        } else {
                            cols[0].label("(no symbol data)");
                        }
                    } else {
                        cols[0].label(egui::RichText::new("Symbol  (drag: pan  scroll: zoom)").strong());
                        show_symbol_preview(
                            &mut cols[0],
                            &comp.pins,
                            &comp.value,
                            self.state.ref_pos,
                            self.state.val_pos,
                            &mut self.state.sym_pz,
                            egui::Vec2::new(360.0, 300.0),
                        );
                    }
                    cols[0].add_space(8.0);
                    cols[0].label(egui::RichText::new("Footprint  (drag: pan  scroll: zoom)").strong());
                    if let Some(tex) = &self.state.footprint_texture {
                        show_panzoom_image(&mut cols[0], tex, egui::Vec2::new(320.0, 220.0), &mut self.state.fp_pz, "fp");
                    } else {
                        cols[0].label("(no preview)");
                    }

                    // Right: attributes table
                    cols[1].label(egui::RichText::new("Attributes").strong());
                    {
                        let stock_s  = comp.stock.to_string();
                        let minqty_s = comp.min_qty.to_string();
                        let mut rows: Vec<(&str, &str)> = vec![
                            ("LCSC",         &comp.lcsc_id),
                            ("Value",        &comp.value),
                            ("Manufacturer", &comp.manufacturer),
                            ("Package",      &comp.package),
                            ("Category",     &comp.category),
                            ("Description",  &comp.description),
                            ("Datasheet",    &comp.datasheet),
                            ("Stock",        &stock_s),
                            ("Price",        &comp.price),
                            ("Min Qty",      &minqty_s),
                            ("Process",      &comp.process),
                            ("Class",        &comp.class),
                        ];
                        let extra: Vec<(&str, &str)> = comp.extra_attrs.iter()
                            .map(|a| (a.name.as_str(), a.value.as_str()))
                            .collect();
                        rows.extend_from_slice(&extra);

                        {
                            use egui_extras::{Column, TableBuilder};
                            TableBuilder::new(&mut cols[1])
                                .striped(true)
                                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                                .column(Column::exact(110.0))
                                .column(Column::remainder().at_least(80.0))
                                .header(22.0, |mut h| {
                                    h.col(|ui| { ui.strong("Property"); });
                                    h.col(|ui| { ui.strong("Value"); });
                                })
                                .body(|mut body| {
                                    for (k, v) in &rows {
                                        let h = if k == &"Description" || k == &"Datasheet" { 36.0 } else { 20.0 };
                                        body.row(h, |mut row| {
                                            row.col(|ui| {
                                                ui.add(egui::Label::new(
                                                    egui::RichText::new(*k).strong()
                                                ).selectable(false));
                                            });
                                            row.col(|ui| {
                                                ui.add(egui::Label::new(*v)
                                                    .selectable(false)
                                                    .wrap())
                                                    .on_hover_text(*v);
                                            });
                                        });
                                    }
                                });
                        }
                    }
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                // Symbol text position
                ui.label(egui::RichText::new("Symbol Text Positions (mm)").strong());
                egui::Grid::new("sym_adj").spacing([8.0, 4.0]).show(ui, |ui| {
                    ui.label("Reference X:");
                    ui.add(egui::DragValue::new(&mut self.state.ref_pos[0]).speed(0.1));
                    ui.label("Y:");
                    ui.add(egui::DragValue::new(&mut self.state.ref_pos[1]).speed(0.1));
                    if ui.button("Reset").clicked() { self.state.ref_pos = [0.0, 3.81]; }
                    ui.end_row();
                    ui.label("Value X:");
                    ui.add(egui::DragValue::new(&mut self.state.val_pos[0]).speed(0.1));
                    ui.label("Y:");
                    ui.add(egui::DragValue::new(&mut self.state.val_pos[1]).speed(0.1));
                    if ui.button("Reset").clicked() { self.state.val_pos = [0.0, 2.54]; }
                    ui.end_row();
                });

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // 3D viewer + adjustment
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("3D Model").strong());
                    ui.add_space(8.0);
                    let mv = &mut self.state.model_viewer;
                    let small = egui::TextStyle::Small;
                    let views: &[(&str, f32, f32)] = &[
                        ("Top",    0.0,                  std::f32::consts::FRAC_PI_2 * 0.99),
                        ("Front",  0.0,                  0.0),
                        ("Back",   std::f32::consts::PI, 0.0),
                        ("Left",  -std::f32::consts::FRAC_PI_2, 0.0),
                        ("Right",  std::f32::consts::FRAC_PI_2, 0.0),
                        ("Bottom", 0.0,                 -std::f32::consts::FRAC_PI_2 * 0.99),
                        ("Iso",    0.5,                  0.4),
                    ];
                    for &(label, yaw, pitch) in views {
                        if ui.add(egui::Button::new(
                            egui::RichText::new(label).text_style(small.clone())
                        )).clicked() {
                            mv.yaw   = yaw;
                            mv.pitch = pitch;
                            ui.ctx().request_repaint();
                        }
                    }
                    ui.add_space(8.0);
                    ui.checkbox(&mut mv.ortho, egui::RichText::new("Ortho").text_style(small));
                });
                self.state.model_viewer.show(
                    ui,
                    egui::Vec2::new(ui.available_width().min(560.0), 360.0),
                    self.state.model_offset,
                    self.state.model_rotation,
                    self.state.model_scale,
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("3D Model Adjustment").strong());
                    ui.add_space(12.0);
                    if ui.checkbox(&mut self.state.model_unified_scale, "Unified scale").changed()
                        && self.state.model_unified_scale
                    {
                        let s = self.state.model_scale[0];
                        self.state.model_scale = [s, s, s];
                    }
                });
                egui::Grid::new("model_adj").spacing([8.0, 4.0]).show(ui, |ui| {
                    ui.label(egui::RichText::new("Offset (mm)").strong());
                    ui.label(egui::RichText::new("Rotation (°)").strong());
                    ui.label(egui::RichText::new("Scale").strong());
                    ui.end_row();

                    let unified = self.state.model_unified_scale;
                    let scale_drag = |ui: &mut egui::Ui, v: &mut f32| -> egui::Response {
                        ui.add(egui::DragValue::new(v).speed(0.01).range(0.01..=10.0_f32))
                    };

                    ui.label("X:");
                    ui.add(egui::DragValue::new(&mut self.state.model_offset[0]).speed(0.1));
                    ui.label("X:");
                    ui.add(egui::DragValue::new(&mut self.state.model_rotation[0]).speed(1.0));
                    ui.label("X:");
                    if scale_drag(ui, &mut self.state.model_scale[0]).changed() && unified {
                        let s = self.state.model_scale[0];
                        self.state.model_scale[1] = s;
                        self.state.model_scale[2] = s;
                    }
                    ui.end_row();

                    ui.label("Y:");
                    ui.add(egui::DragValue::new(&mut self.state.model_offset[1]).speed(0.1));
                    ui.label("Y:");
                    ui.add(egui::DragValue::new(&mut self.state.model_rotation[1]).speed(1.0));
                    ui.label("Y:");
                    ui.add_enabled_ui(!unified, |ui| {
                        scale_drag(ui, &mut self.state.model_scale[1]);
                    });
                    ui.end_row();

                    ui.label("Z (height):");
                    ui.add(egui::DragValue::new(&mut self.state.model_offset[2]).speed(0.01));
                    ui.label("Z:");
                    ui.add(egui::DragValue::new(&mut self.state.model_rotation[2]).speed(1.0));
                    ui.label("Z:");
                    ui.add_enabled_ui(!unified, |ui| {
                        scale_drag(ui, &mut self.state.model_scale[2]);
                    });
                    if ui.button("Reset all").clicked() {
                        self.state.model_offset   = [0.0; 3];
                        self.state.model_rotation = [0.0; 3];
                        self.state.model_scale    = [1.0, 1.0, 1.0];
                    }
                    ui.end_row();
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                // Import
                ui.label(egui::RichText::new("Import to KiCad Library").strong());
                ui.horizontal(|ui| {
                    ui.label("Path:");
                    ui.monospace(&self.state.settings.lib_path);
                    ui.label("  Name:");
                    ui.monospace(&self.state.settings.lib_name);
                });
                ui.add_space(4.0);
                if ui.button("  Import  ").clicked() {
                    let lib_name = self.state.settings.lib_name.clone();
                    let paths = export::LibPaths::new(&self.state.settings.lib_path, &lib_name);
                    let has_step = self.state.step_bytes.is_some();
                    let model_ext = if has_step { "step" } else { "wrl" };
                    // STEP files sit correctly in KiCad at 0,0,0 (native Z-up convention).
                    // WRL from EasyEDA OBJ needs c_rotation to orient correctly in KiCad's Y-up viewer.
                    let export_rotation = if has_step {
                        [0.0_f32; 3]
                    } else {
                        comp.model_init_rotation
                    };
                    let result: anyhow::Result<()> = (|| {
                        paths.ensure_dirs()?;
                        export::write_symbol(&paths, &comp, &lib_name,
                            self.state.ref_pos, self.state.val_pos)?;
                        export::write_footprint(&paths, &comp, &lib_name,
                            self.state.model_offset, export_rotation, model_ext)?;
                        if let Some(step) = &self.state.step_bytes {
                            export::write_step_model(&paths, &comp, step)?;
                        }
                        if let Some(wrl) = &self.state.wrl_bytes {
                            export::write_wrl_model(&paths, &comp, wrl)?;
                        }
                        Ok(())
                    })();
                    self.state.status = match result {
                        Ok(()) => format!("Imported {} ({}) → {}/{}",
                            comp.value, comp.lcsc_id,
                            self.state.settings.lib_path, self.state.settings.lib_name),
                        Err(e) => format!("Import error: {e}"),
                    };
                }
            });
        });
    }
}

fn show_symbol_preview(
    ui: &mut egui::Ui,
    pins: &[Pin],
    value: &str,
    ref_pos: [f32; 2],
    val_pos: [f32; 2],
    pz: &mut PanZoom,
    display_size: egui::Vec2,
) {
    const PIN_LEN: f32 = 2.54;

    let (rect, response) = ui.allocate_exact_size(display_size, egui::Sense::click_and_drag());

    if response.dragged() {
        pz.pan += response.drag_delta();
    }
    // Only zoom if pointer is directly over this specific widget
    let pointer_over_widget = ui.input(|i| {
        i.pointer.hover_pos().map_or(false, |pos| rect.contains(pos))
    });
    if pointer_over_widget {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.1 {
            // Consume scroll for zoom - prevent page scroll
            ui.input_mut(|i| {
                i.smooth_scroll_delta = egui::Vec2::ZERO;
                i.raw_scroll_delta    = egui::Vec2::ZERO;
            });
            pz.zoom = (pz.zoom * (1.0 + scroll * 0.005)).clamp(0.1, 30.0);
        }
    }
    if response.double_clicked() { pz.reset(); }

    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::WHITE);

    // body-end helper (KiCad mm, Y-up)
    let body_end = |p: &Pin| -> (f32, f32) {
        match p.angle {
            0   => (p.x + PIN_LEN, p.y),
            90  => (p.x,           p.y + PIN_LEN),
            180 => (p.x - PIN_LEN, p.y),
            _   => (p.x,           p.y - PIN_LEN),
        }
    };

    // Body rectangle bounds
    let (bmin_x, bmax_x, bmin_y, bmax_y) = if pins.is_empty() {
        (-5.08f32, 5.08, -1.27, 1.27)
    } else {
        let pts: Vec<(f32, f32)> = pins.iter().map(|p| body_end(p)).collect();
        let m = 0.5_f32;
        (
            pts.iter().map(|(x, _)| *x).fold(f32::MAX, f32::min) - m,
            pts.iter().map(|(x, _)| *x).fold(f32::MIN, f32::max) + m,
            pts.iter().map(|(_, y)| *y).fold(f32::MAX, f32::min) - m,
            pts.iter().map(|(_, y)| *y).fold(f32::MIN, f32::max) + m,
        )
    };

    // Full extents for auto-fit (body + pin tips + label positions)
    let label_pad = (value.len() as f32 * 0.9).max(5.0);
    let mut min_x = bmin_x.min(ref_pos[0] - 2.0).min(val_pos[0] - 2.0);
    let mut max_x = bmax_x.max(ref_pos[0] + label_pad).max(val_pos[0] + label_pad);
    let mut min_y = bmin_y.min(ref_pos[1] - 2.0).min(val_pos[1] - 2.0);
    let mut max_y = bmax_y.max(ref_pos[1] + 2.0).max(val_pos[1] + 2.0);
    for p in pins {
        min_x = min_x.min(p.x - PIN_LEN - 0.5);
        max_x = max_x.max(p.x + PIN_LEN + 0.5);
        min_y = min_y.min(p.y - PIN_LEN - 0.5);
        max_y = max_y.max(p.y + PIN_LEN + 0.5);
    }
    min_x -= 2.0; max_x += 2.0; min_y -= 2.0; max_y += 2.0;

    let sym_w = (max_x - min_x).max(1.0);
    let sym_h = (max_y - min_y).max(1.0);
    let base_scale = (display_size.x / sym_w).min(display_size.y / sym_h) * 0.85;
    let scale = base_scale * pz.zoom;

    let cx_mm = (min_x + max_x) * 0.5;
    let cy_mm = (min_y + max_y) * 0.5;
    let center = rect.center() + pz.pan;

    // KiCad mm (Y-up) → screen px (Y-down)
    let ts = |mx: f32, my: f32| egui::pos2(
        center.x + (mx - cx_mm) * scale,
        center.y - (my - cy_mm) * scale,
    );

    // Body rectangle — light yellow fill, dark red border
    painter.rect(
        egui::Rect::from_two_pos(ts(bmin_x, bmax_y), ts(bmax_x, bmin_y)),
        0.0,
        egui::Color32::from_rgb(255, 255, 204),
        egui::Stroke::new(1.5, egui::Color32::from_rgb(160, 0, 0)),
        egui::StrokeKind::Middle,
    );

    // Pins
    let font_sz = (scale * 1.27).clamp(8.0, 14.0);
    let pin_col  = egui::Color32::from_gray(80);
    let name_col = egui::Color32::from_rgb(0, 0, 160);
    let num_col  = egui::Color32::from_rgb(130, 0, 0);

    for pin in pins {
        let tip      = ts(pin.x, pin.y);
        let (bx, by) = body_end(pin);
        let body_pt  = ts(bx, by);

        painter.line_segment([tip, body_pt], egui::Stroke::new(1.0, pin_col));
        painter.circle_filled(tip, 2.0, pin_col);

        // Pin name — just inside body end
        let (na, no) = match pin.angle {
            0   => (egui::Align2::LEFT_CENTER,   egui::vec2( 3.0,  0.0)),
            180 => (egui::Align2::RIGHT_CENTER,  egui::vec2(-3.0,  0.0)),
            90  => (egui::Align2::CENTER_BOTTOM, egui::vec2( 0.0, -3.0)),
            _   => (egui::Align2::CENTER_TOP,    egui::vec2( 0.0,  3.0)),
        };
        painter.text(body_pt + no, na, &pin.name,
            egui::FontId::proportional(font_sz), name_col);

        // Pin number — near midpoint, offset to avoid the line
        let mid = egui::pos2((tip.x + body_pt.x) * 0.5, (tip.y + body_pt.y) * 0.5);
        let (na2, no2) = match pin.angle {
            0 | 180 => (egui::Align2::CENTER_BOTTOM, egui::vec2( 0.0, -2.0)),
            _       => (egui::Align2::RIGHT_CENTER,  egui::vec2(-2.0,  0.0)),
        };
        painter.text(mid + no2, na2, &pin.number,
            egui::FontId::proportional((font_sz * 0.8).clamp(7.0, 11.0)), num_col);
    }

    // Reference "U" and Value labels (blue, like KiCad)
    let lbl_font  = egui::FontId::proportional((scale * 1.27).clamp(10.0, 16.0));
    let lbl_color = egui::Color32::from_rgb(0, 0, 160);
    painter.text(ts(ref_pos[0], ref_pos[1]), egui::Align2::CENTER_CENTER,
        "U", lbl_font.clone(), lbl_color);
    painter.text(ts(val_pos[0], val_pos[1]), egui::Align2::CENTER_CENTER,
        value, lbl_font, lbl_color);

    if pins.is_empty() {
        painter.text(
            rect.left_bottom() + egui::vec2(4.0, -4.0),
            egui::Align2::LEFT_BOTTOM,
            "⚠ no pin data from EasyEDA",
            egui::FontId::proportional(10.0),
            egui::Color32::from_rgb(200, 150, 50),
        );
    }
    if pz.zoom != 1.0 || pz.pan != egui::Vec2::ZERO {
        painter.text(
            rect.right_bottom() + egui::vec2(-4.0, -4.0),
            egui::Align2::RIGHT_BOTTOM,
            "dbl-click: reset",
            egui::FontId::proportional(10.0),
            egui::Color32::from_gray(150),
        );
    }
}

fn show_panzoom_image(
    ui: &mut egui::Ui,
    tex: &egui::TextureHandle,
    display_size: egui::Vec2,
    pz: &mut PanZoom,
    id: &str,
) -> egui::Rect {
    let (rect, response) = ui.allocate_exact_size(display_size, egui::Sense::click_and_drag());

    if response.dragged() {
        // drag delta in screen pixels → UV offset
        let delta_uv = response.drag_delta() / (display_size * pz.zoom);
        pz.pan -= delta_uv;
    }
    // Only zoom if pointer is directly over this specific widget
    let pointer_over_widget = ui.input(|i| {
        i.pointer.hover_pos().map_or(false, |pos| rect.contains(pos))
    });
    if pointer_over_widget {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.1 {
            // Consume scroll for zoom - prevent page scroll
            ui.input_mut(|i| {
                i.smooth_scroll_delta = egui::Vec2::ZERO;
                i.raw_scroll_delta    = egui::Vec2::ZERO;
            });
            pz.zoom = (pz.zoom * (1.0 + scroll * 0.003)).clamp(0.5, 20.0);
        }
    }
    if response.double_clicked() {
        pz.reset();
    }

    // UV rect: center + pan, half-size = 0.5/zoom
    let half = egui::Vec2::splat(0.5 / pz.zoom);
    let center_uv = egui::Vec2::splat(0.5) + pz.pan;
    let uv_min = center_uv - half;
    let uv_max = center_uv + half;

    ui.painter().image(
        tex.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(uv_min.x, uv_min.y), egui::pos2(uv_max.x, uv_max.y)),
        egui::Color32::WHITE,
    );

    // "double-click to reset" hint
    if pz.zoom != 1.0 || pz.pan != egui::Vec2::ZERO {
        ui.painter().text(
            rect.right_bottom() + egui::vec2(-4.0, -4.0),
            egui::Align2::RIGHT_BOTTOM,
            "dbl-click: reset",
            egui::FontId::proportional(10.0),
            egui::Color32::from_gray(120),
        );
    }
    let _ = id;
    rect
}


fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("JLCPCB → KiCad")
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "jlcpcb-kicad",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

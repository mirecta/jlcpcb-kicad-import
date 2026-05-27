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
    RefreshProgress(usize, usize),  // current, total
    RefreshDone(usize, usize),      // updated_count, failed_count
    RefreshErr(String),
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

    // Icon state
    icon_set: bool,
}

struct App {
    state: AppState,
}

impl App {
    fn new(cc: &eframe::CreationContext, lib_path_override: Option<String>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        setup_fonts(&cc.egui_ctx);
        let mut state = AppState::default();
        state.settings = settings::load();

        // Override lib_path from command line if provided
        if let Some(path) = lib_path_override {
            state.settings.lib_path = path;
        }

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

    fn do_refresh_library(&mut self) {
        let lib_path = self.state.settings.lib_path.clone();
        let lib_name = self.state.settings.lib_name.clone();
        self.state.status = "Starting library refresh...".to_string();

        self.spawn(move |tx| {
            use std::fs;
            use std::path::Path;

            let sym_file = Path::new(&lib_path).join(format!("{}.kicad_sym", lib_name));

            if !sym_file.exists() {
                let _ = tx.send(BgMsg::RefreshErr(format!("Library file not found: {}", sym_file.display())));
                return;
            }

            let content = match fs::read_to_string(&sym_file) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(BgMsg::RefreshErr(format!("Failed to read library: {}", e)));
                    return;
                }
            };

            // Extract all LCSC IDs from the library
            let lcsc_ids = extract_lcsc_ids(&content);
            let total = lcsc_ids.len();

            if total == 0 {
                let _ = tx.send(BgMsg::RefreshErr("No components with LCSC IDs found".to_string()));
                return;
            }

            let mut updated_content = content.clone();
            let mut updated_count = 0;
            let mut failed_count = 0;

            for (i, lcsc_id) in lcsc_ids.iter().enumerate() {
                let _ = tx.send(BgMsg::RefreshProgress(i + 1, total));

                match api::fetch_component(lcsc_id) {
                    Ok(comp) => {
                        updated_content = update_component_in_lib(&updated_content, lcsc_id, &comp);
                        updated_count += 1;
                    }
                    Err(_) => {
                        failed_count += 1;
                    }
                }
            }

            // Write back the updated library
            if let Err(e) = fs::write(&sym_file, updated_content) {
                let _ = tx.send(BgMsg::RefreshErr(format!("Failed to write library: {}", e)));
                return;
            }

            let _ = tx.send(BgMsg::RefreshDone(updated_count, failed_count));
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Set icon at runtime (some platforms require this instead of NativeOptions)
        if !self.state.icon_set {
            let icon = create_app_icon();
            eprintln!("Setting window icon at runtime...");
            ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(std::sync::Arc::new(icon))));
            self.state.icon_set = true;
            eprintln!("Icon set successfully");
        }

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
            // Don't reset loading/bg_rx yet - RefreshProgress needs to keep the channel open
            let is_progress = matches!(msg, BgMsg::RefreshProgress(_, _));
            if !is_progress {
                self.state.loading = false;
                self.state.bg_rx = None;
            }
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
                            .map(|p| {
                                // If pad is rotated ~90°, swap width and height
                                let rotated = (p.rotation % 180.0 - 90.0).abs() < 45.0;
                                let (w, h) = if rotated { (p.h, p.w) } else { (p.w, p.h) };
                                model3d::PadInfo { cx: p.cx, cz: p.cy, w, h, shape: p.shape.clone() }
                            })
                            .collect();
                        let drawings: Vec<model3d::PcbDrawing> = comp.fp_drawings.iter()
                            .map(|d| model3d::PcbDrawing { tris: d.tris.clone(), color: d.color })
                            .collect();
                        // VRML is Y-up, convert to Z-up like STEP: rotate +90° around X
                        self.state.model_viewer.load(bytes, &pads, &drawings, [90.0, 0.0, 0.0]);
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
                    self.state.status = format!("⚠ Error loading component: {}", e);
                    // Clear stale data so UI doesn't show broken mix of old+new
                    self.state.component = None;
                    self.state.symbol_texture = None;
                    self.state.footprint_texture = None;
                    self.state.wrl_bytes = None;
                    self.state.step_bytes = None;
                    self.state.model_viewer.has_model = false;
                }
                BgMsg::RefreshProgress(current, total) => {
                    self.state.status = format!("Refreshing library... {}/{}", current, total);
                    // Keep loading=true and bg_rx open for more progress messages
                    ctx.request_repaint();
                }
                BgMsg::RefreshDone(updated, failed) => {
                    self.state.status = if failed == 0 {
                        format!("✓ Refreshed {} components", updated)
                    } else {
                        format!("✓ Refreshed {} components ({} failed)", updated, failed)
                    };
                }
                BgMsg::RefreshErr(e) => {
                    self.state.status = format!("Refresh error: {}", e);
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
                    if ui.add_enabled(!self.state.loading, egui::Button::new("🔄 Refresh Library")).clicked() {
                        self.do_refresh_library();
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
                    // Force minimum width so table doesn't shrink - triggers horizontal scroll
                    ui.set_min_width(500.0);
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
                                .resizable(true)
                                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                                .column(Column::initial(150.0).at_least(80.0))
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

                // Helper function to create DragValue that responds to scroll wheel
                let scrollable_drag_helper = |ui: &mut egui::Ui, value: &mut f32, speed: f32, scroll_mult: f32, range: Option<std::ops::RangeInclusive<f32>>| {
                    let mut drag = egui::DragValue::new(value).speed(speed);
                    if let Some(ref r) = range {
                        drag = drag.range(r.clone());
                    }
                    let response = ui.add(drag);

                    // If hovering over this widget, consume scroll and change value
                    if response.hovered() {
                        let scroll_delta = ui.input(|i| i.smooth_scroll_delta.y);
                        if scroll_delta.abs() > 0.1 {
                            *value += scroll_delta * scroll_mult;
                            if let Some(ref r) = range {
                                *value = value.clamp(*r.start(), *r.end());
                            }
                            ui.input_mut(|i| {
                                i.smooth_scroll_delta = egui::Vec2::ZERO;
                                i.raw_scroll_delta = egui::Vec2::ZERO;
                            });
                        }
                    }
                    response
                };

                // Symbol text position
                ui.label(egui::RichText::new("Symbol Text Positions (mm)").strong());
                egui::Grid::new("sym_adj").spacing([8.0, 4.0]).show(ui, |ui| {
                    ui.label("Reference X:");
                    scrollable_drag_helper(ui, &mut self.state.ref_pos[0], 0.1, 0.0025, None);
                    ui.label("Y:");
                    scrollable_drag_helper(ui, &mut self.state.ref_pos[1], 0.1, 0.0025, None);
                    if ui.button("Reset").clicked() { self.state.ref_pos = [0.0, 3.81]; }
                    ui.end_row();
                    ui.label("Value X:");
                    scrollable_drag_helper(ui, &mut self.state.val_pos[0], 0.1, 0.0025, None);
                    ui.label("Y:");
                    scrollable_drag_helper(ui, &mut self.state.val_pos[1], 0.1, 0.0025, None);
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
                ui.horizontal(|ui| {
                    let unified = self.state.model_unified_scale;

                    // Offset column
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("Offset (mm)").strong());
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label("X");
                                scrollable_drag_helper(ui, &mut self.state.model_offset[0], 0.1, 0.0025, None);
                            });
                            ui.vertical(|ui| {
                                ui.label("Y");
                                scrollable_drag_helper(ui, &mut self.state.model_offset[1], 0.1, 0.0025, None);
                            });
                            ui.vertical(|ui| {
                                ui.label("Z");
                                scrollable_drag_helper(ui, &mut self.state.model_offset[2], 0.01, 0.00025, None);
                            });
                        });
                    });

                    ui.add_space(10.0);

                    // Rotation column
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("Rotation (°)").strong());
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label("X");
                                scrollable_drag_helper(ui, &mut self.state.model_rotation[0], 1.0, 0.25, None);
                            });
                            ui.vertical(|ui| {
                                ui.label("Y");
                                scrollable_drag_helper(ui, &mut self.state.model_rotation[1], 1.0, 0.25, None);
                            });
                            ui.vertical(|ui| {
                                ui.label("Z");
                                scrollable_drag_helper(ui, &mut self.state.model_rotation[2], 1.0, 0.25, None);
                            });
                        });
                    });

                    ui.add_space(10.0);

                    // Scale column
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("Scale").strong());
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label("X");
                                let resp = scrollable_drag_helper(ui, &mut self.state.model_scale[0], 0.01, 0.0025, Some(0.01..=10.0));
                                if resp.changed() && unified {
                                    let s = self.state.model_scale[0];
                                    self.state.model_scale[1] = s;
                                    self.state.model_scale[2] = s;
                                }
                            });
                            ui.add_enabled_ui(!unified, |ui| {
                                ui.vertical(|ui| {
                                    ui.label("Y");
                                    scrollable_drag_helper(ui, &mut self.state.model_scale[1], 0.01, 0.0025, Some(0.01..=10.0));
                                });
                            });
                            ui.add_enabled_ui(!unified, |ui| {
                                ui.vertical(|ui| {
                                    ui.label("Z");
                                    scrollable_drag_helper(ui, &mut self.state.model_scale[2], 0.01, 0.0025, Some(0.01..=10.0));
                                });
                            });
                        });
                    });

                    ui.add_space(10.0);

                    if ui.button("Reset all").clicked() {
                        self.state.model_offset   = [0.0; 3];
                        self.state.model_rotation = [0.0; 3];
                        self.state.model_scale    = [1.0, 1.0, 1.0];
                    }
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
                    let result: anyhow::Result<()> = (|| {
                        paths.ensure_dirs()?;
                        export::write_symbol(&paths, &comp, &lib_name,
                            self.state.ref_pos, self.state.val_pos)?;

                        // Convert viewer coordinates (X, Y, Z) to KiCad model coordinates (X, Z, -Y)
                        // In viewer: Y is vertical (height), Z is depth
                        // In KiCad: Z is vertical (height), Y is depth, BUT Y-axis is inverted
                        let kicad_offset = [
                            self.state.model_offset[0],   // X unchanged
                            self.state.model_offset[2],   // KiCad Y = viewer Z
                            -self.state.model_offset[1],  // KiCad Z = -viewer Y (negated!)
                        ];

                        // Convert rotation (X is negated, Y/Z swapped with Y negated)
                        let kicad_rotation = [
                            -self.state.model_rotation[0],  // X negated
                            self.state.model_rotation[2],   // KiCad Y = viewer Z
                            -self.state.model_rotation[1],  // KiCad Z = -viewer Y
                        ];

                        // Convert scale (swap Y/Z but no negation since it's a multiplier)
                        let kicad_scale = [
                            self.state.model_scale[0],  // X unchanged
                            self.state.model_scale[2],  // KiCad Y = viewer Z
                            self.state.model_scale[1],  // KiCad Z = viewer Y
                        ];

                        export::write_footprint(&paths, &comp, &lib_name,
                            kicad_offset, kicad_rotation, kicad_scale, model_ext)?;
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
                ui.add_space(12.0);
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
            "⚠ No pin data from EasyEDA - cannot generate KiCad symbol",
            egui::FontId::proportional(12.0),
            egui::Color32::from_rgb(220, 120, 50),
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

// ── App icon ──────────────────────────────────────────────────────────────────

fn create_app_icon() -> egui::IconData {
    // Create a 32x32 IC chip icon with solid background
    let size = 32;
    let mut rgba = vec![0u8; size * size * 4];
    eprintln!("Creating app icon: {}x{} ({} bytes)", size, size, rgba.len());

    for y in 0..size {
        for x in 0..size {
            let idx = (y * size + x) * 4;

            // IC chip body (center square)
            let is_body = x >= 8 && x < 24 && y >= 8 && y < 24;

            // Pins on left side (4 pins)
            let is_left_pins = x < 8 && (
                (y >= 6 && y < 9) ||   // pin 1
                (y >= 11 && y < 14) ||  // pin 2
                (y >= 18 && y < 21) ||  // pin 3
                (y >= 23 && y < 26)     // pin 4
            );

            // Pins on right side (4 pins)
            let is_right_pins = x >= 24 && (
                (y >= 6 && y < 9) ||   // pin 8
                (y >= 11 && y < 14) ||  // pin 7
                (y >= 18 && y < 21) ||  // pin 6
                (y >= 23 && y < 26)     // pin 5
            );

            // Pin 1 indicator (small circle top-left)
            let dx = x as f32 - 10.0;
            let dy = y as f32 - 10.0;
            let is_pin1_dot = (dx * dx + dy * dy) < 4.0;

            if is_body {
                if is_pin1_dot {
                    // White dot for pin 1
                    rgba[idx] = 255;
                    rgba[idx + 1] = 255;
                    rgba[idx + 2] = 255;
                    rgba[idx + 3] = 255;
                } else {
                    // Dark gray IC body
                    rgba[idx] = 40;
                    rgba[idx + 1] = 40;
                    rgba[idx + 2] = 40;
                    rgba[idx + 3] = 255;
                }
            } else if is_left_pins || is_right_pins {
                // Silver pins
                rgba[idx] = 192;
                rgba[idx + 1] = 192;
                rgba[idx + 2] = 192;
                rgba[idx + 3] = 255;
            } else {
                // Green PCB background (solid, not transparent)
                rgba[idx] = 40;
                rgba[idx + 1] = 120;
                rgba[idx + 2] = 60;
                rgba[idx + 3] = 255;
            }
        }
    }

    egui::IconData {
        rgba,
        width: size as u32,
        height: size as u32,
    }
}

// ── Library refresh helpers ──────────────────────────────────────────────────

fn extract_lcsc_ids(content: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for line in content.lines() {
        if line.contains("(property \"LCSC\"") {
            // Extract LCSC ID from: (property "LCSC" "C7512" ...
            if let Some(start) = line.find("\"LCSC\" \"") {
                let after = &line[start + 9..];
                if let Some(end) = after.find('"') {
                    ids.push(after[..end].to_string());
                }
            }
        }
    }
    ids
}

fn update_component_in_lib(content: &str, lcsc_id: &str, comp: &Component) -> String {
    // Find the symbol containing this LCSC ID and update its Stock and Price properties
    let mut result = String::new();
    let mut in_target_symbol = false;
    let mut in_property = false;
    let mut skip_until_paren_close = false;
    let mut paren_depth = 0;

    for line in content.lines() {
        // Check if we found the LCSC property with our ID
        if line.contains(&format!("(property \"LCSC\" \"{}\"", lcsc_id)) {
            in_target_symbol = true;
        }

        // If we're in the target symbol and found Stock or Price property, replace it
        if in_target_symbol && !skip_until_paren_close {
            if line.contains("(property \"Stock\" \"") {
                // Get the indentation
                let indent = line.chars().take_while(|c| c.is_whitespace()).collect::<String>();
                // Write new stock property value (just update the value line)
                let new_line = line.replace(
                    &format!("(property \"Stock\" \""),
                    &format!("(property \"Stock\" \"{}", comp.stock).as_str()
                ).split('"').take(3).collect::<Vec<_>>().join("\"") + "\"";
                result.push_str(&new_line);
                result.push('\n');
                skip_until_paren_close = true;
                paren_depth = 1;
                continue;
            } else if line.contains("(property \"Price\" \"") {
                // Get the indentation
                let indent = line.chars().take_while(|c| c.is_whitespace()).collect::<String>();
                // Write new price property value (just update the value line)
                let new_line = line.replace(
                    &format!("(property \"Price\" \""),
                    &format!("(property \"Price\" \"{}", comp.price).as_str()
                ).split('"').take(3).collect::<Vec<_>>().join("\"") + "\"";
                result.push_str(&new_line);
                result.push('\n');
                skip_until_paren_close = true;
                paren_depth = 1;
                continue;
            }
        }

        // If we're skipping lines in a property, track parens to know when to stop
        if skip_until_paren_close {
            for ch in line.chars() {
                match ch {
                    '(' => paren_depth += 1,
                    ')' => {
                        paren_depth -= 1;
                        if paren_depth == 0 {
                            skip_until_paren_close = false;
                            result.push_str(line);
                            result.push('\n');
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if skip_until_paren_close {
                continue; // Skip this line, still inside the property
            } else {
                continue; // We just wrote the closing line
            }
        }

        result.push_str(line);
        result.push('\n');

        // Check if we're exiting the symbol
        if in_target_symbol && line.trim() == ")" && line.len() <= 4 {
            in_target_symbol = false;
        }
    }

    result
}


fn main() -> eframe::Result<()> {
    // Parse command-line arguments
    let args: Vec<String> = std::env::args().collect();
    let lib_path_override = if args.len() > 1 {
        Some(args[1].clone())
    } else {
        None
    };

    let icon = create_app_icon();
    eprintln!("Icon created, setting in NativeOptions...");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("JLCPCB → KiCad")
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([900.0, 600.0])
            .with_icon(icon),
        depth_buffer: 24,  // Request 24-bit depth buffer for 3D rendering
        ..Default::default()
    };
    eframe::run_native(
        "jlcpcb-kicad",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, lib_path_override)))),
    )
}

use std::sync::{Arc, Mutex};
use eframe::glow::{self, HasContext};
use egui::Sense;
use glam::{Mat3, Mat4, Vec3};

// ── GLSL ─────────────────────────────────────────────────────────────────────

const VS: &str = "
#version 140
uniform mat4 u_vp;
uniform mat4 u_model;
uniform mat3 u_nrm;
in vec3 a_pos;
in vec3 a_nrm;
in vec3 a_col;
out vec3 v_col;
out vec3 v_nrm;
void main() {
    gl_Position = u_vp * u_model * vec4(a_pos, 1.0);
    v_nrm = normalize(u_nrm * a_nrm);
    v_col = a_col;
}
";

const FS: &str = "
#version 140
uniform float u_alpha;
in vec3 v_col;
in vec3 v_nrm;
out vec4 o_col;
void main() {
    vec3 n  = normalize(v_nrm);
    vec3 L1 = normalize(vec3(1.0, 2.0, 1.5));
    vec3 L2 = normalize(vec3(-1.0, 1.5, -0.5));
    vec3 L3 = normalize(vec3(0.0, -1.0, 0.5));
    float d = abs(dot(n, L1))
            + 0.45 * abs(dot(n, L2))
            + 0.20 * abs(dot(n, L3));
    o_col = vec4(v_col * (0.45 + 0.55 * d), u_alpha);
}
";

// ── Pad descriptor (world XZ coords in mm, Y is PCB surface = 0) ──────────────

pub struct PadInfo {
    pub cx: f32,  // centre X in mm
    pub cz: f32,  // centre Z in mm (PCB Y-axis → viewer Z-axis)
    pub w:  f32,
    pub h:  f32,
    pub shape: String,  // "circle", "rect", "oval"
    pub drill: f32,     // 0 = SMD; >0 = through-hole drill diameter in mm
}

// ── Footprint drawing (silkscreen / fab layer flat geometry) ──────────────────

pub struct PcbDrawing {
    pub tris:  Vec<[f32; 2]>,  // (x_mm, z_mm) triangle verts in viewer coords
    pub color: [f32; 3],
}

// ── CPU mesh (pos + nrm + col, 9 f32 per vertex) ─────────────────────────────

struct Mesh {
    data: Vec<f32>,
    count: i32,
    center: Vec3,
    radius: f32,
    xz_half: f32,  // max(half-extent X, half-extent Z) — used to size the PCB board
}

// ── GPU state ─────────────────────────────────────────────────────────────────

struct GlState {
    program:    glow::Program,
    comp_vao:   glow::VertexArray,
    comp_vbo:   glow::Buffer,
    comp_count: i32,
    pcb_vao:    glow::VertexArray,
    pcb_vbo:    glow::Buffer,
    pcb_count:  i32,
    edge_vao:   glow::VertexArray,
    edge_vbo:   glow::Buffer,
    edge_count: i32,
}

impl GlState {
    unsafe fn new(
        gl: &glow::Context,
        mesh: &Mesh,
        pads: &[PadInfo],
        drawings: &[PcbDrawing],
    ) -> Option<Self> {
        // ── compile shaders ──────────────────────────────────────────────────
        let vs = gl.create_shader(glow::VERTEX_SHADER).ok()?;
        gl.shader_source(vs, VS);
        gl.compile_shader(vs);
        if !gl.get_shader_compile_status(vs) {
            eprintln!("[3d] VS: {}", gl.get_shader_info_log(vs));
            gl.delete_shader(vs);
            return None;
        }
        let fs = gl.create_shader(glow::FRAGMENT_SHADER).ok()?;
        gl.shader_source(fs, FS);
        gl.compile_shader(fs);
        if !gl.get_shader_compile_status(fs) {
            eprintln!("[3d] FS: {}", gl.get_shader_info_log(fs));
            gl.delete_shader(fs);
            gl.delete_shader(vs);
            return None;
        }
        let program = gl.create_program().ok()?;
        gl.attach_shader(program, vs);
        gl.attach_shader(program, fs);
        gl.bind_attrib_location(program, 0, "a_pos");
        gl.bind_attrib_location(program, 1, "a_nrm");
        gl.bind_attrib_location(program, 2, "a_col");
        gl.link_program(program);
        gl.detach_shader(program, vs);
        gl.detach_shader(program, fs);
        gl.delete_shader(vs);
        gl.delete_shader(fs);
        if !gl.get_program_link_status(program) {
            eprintln!("[3d] Link: {}", gl.get_program_info_log(program));
            gl.delete_program(program);
            return None;
        }

        // ── component VAO ────────────────────────────────────────────────────
        let comp_vao = gl.create_vertex_array().ok()?;
        let comp_vbo = gl.create_buffer().ok()?;
        upload_verts(gl, comp_vao, comp_vbo, &mesh.data);

        // ── PCB body VAO (opaque) ────────────────────────────────────────────
        let pcb_data = build_pcb_body(mesh.radius, mesh.xz_half, pads, drawings);
        let pcb_vao = gl.create_vertex_array().ok()?;
        let pcb_vbo = gl.create_buffer().ok()?;
        upload_verts(gl, pcb_vao, pcb_vbo, &pcb_data);

        // ── PCB edge VAO (transparent) ───────────────────────────────────────
        let edge_data = build_pcb_edges(mesh.radius, mesh.xz_half, &pads);
        let edge_vao = gl.create_vertex_array().ok()?;
        let edge_vbo = gl.create_buffer().ok()?;
        upload_verts(gl, edge_vao, edge_vbo, &edge_data);

        Some(GlState {
            program,
            comp_vao, comp_vbo, comp_count: mesh.count,
            pcb_vao,  pcb_vbo,  pcb_count: (pcb_data.len() / 9) as i32,
            edge_vao, edge_vbo, edge_count: (edge_data.len() / 9) as i32,
        })
    }

    unsafe fn paint(
        &self,
        gl: &glow::Context,
        info: &egui::PaintCallbackInfo,
        center: Vec3,
        radius: f32,
        yaw: f32, pitch: f32, zoom: f32,
        cam_pan:  Vec3,
        offset:   [f32; 3],
        rotation: [f32; 3],
        scale:    [f32; 3],
        ortho:    bool,
    ) {
        let ppp = info.pixels_per_point;
        let vp  = &info.viewport;
        let sh  = info.screen_size_px[1] as f32;
        let x = (vp.min.x * ppp).round() as i32;
        let y = (sh - vp.max.y * ppp).round() as i32;
        let w = (vp.width()  * ppp).round() as i32;
        let h = (vp.height() * ppp).round() as i32;

        gl.viewport(x, y, w, h);
        gl.enable(glow::SCISSOR_TEST);
        gl.scissor(x, y, w, h);

        // Set up depth buffer (z-buffer) properly
        gl.clear_depth_f32(1.0);       // Clear depth to far plane
        gl.depth_mask(true);           // Enable depth writes
        gl.enable(glow::DEPTH_TEST);   // Enable depth testing
        gl.depth_func(glow::LESS);     // Closer fragments win

        // Force opaque rendering - no blending at all
        gl.blend_func(glow::ONE, glow::ZERO);  // Even if blend is on, dst = src (no blend)
        gl.disable(glow::BLEND);
        gl.color_mask(true, true, true, true); // Ensure RGBA writes enabled

        gl.disable(glow::CULL_FACE);

        gl.clear_color(1.0, 1.0, 1.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);

        // Component model matrix uses the user-chosen scale (component grows/shrinks).
        // Orbit target is computed with unit scale so changing scale doesn't shift
        // the camera focus — the PCB/pads stay framed identically.
        let comp_model = build_model_mat(offset, rotation, scale,         center);
        let view_model = build_model_mat(offset, rotation, [1.0, 1.0, 1.0], center);
        // With the new matrix the component center maps to offset_world + center,
        // so the orbit target is simply that plus the camera pan.
        let offset_world = Vec3::new(offset[0], offset[2], offset[1]);
        let orbit_target = offset_world + center + cam_pan;

        let dist = radius * zoom;
        let eye = orbit_target + Vec3::new(
            dist * pitch.cos() * yaw.sin(),
            dist * pitch.sin(),
            dist * pitch.cos() * yaw.cos(),
        );
        let aspect = (w as f32) / (h.max(1) as f32);
        // When looking nearly straight up/down, Y is parallel to the view direction —
        // use Z as world-up instead to avoid a degenerate view matrix.
        let world_up = if pitch.abs() > std::f32::consts::FRAC_PI_2 - 0.05 {
            Vec3::Z
        } else {
            Vec3::Y
        };
        let view = Mat4::look_at_rh(eye, orbit_target, world_up);
        let proj = if ortho {
            let half_h = radius * zoom;
            let half_w = half_h * aspect;
            Mat4::orthographic_rh_gl(-half_w, half_w, -half_h, half_h, -dist * 100.0, dist * 100.0)
        } else {
            Mat4::perspective_rh_gl(0.7, aspect, dist * 0.001, dist * 200.0)
        };
        let vp_mat = proj * view;

        gl.use_program(Some(self.program));

        if let Some(loc) = gl.get_uniform_location(self.program, "u_vp") {
            gl.uniform_matrix_4_f32_slice(Some(&loc), false, &vp_mat.to_cols_array());
        }

        // Set u_alpha = 1.0 for opaque geometry
        if let Some(loc) = gl.get_uniform_location(self.program, "u_alpha") {
            gl.uniform_1_f32(Some(&loc), 1.0);
        }

        // Draw PCB body (opaque — tessellated surfaces + pads + barrel)
        set_model(gl, self.program, &Mat4::IDENTITY, &Mat3::IDENTITY);
        gl.bind_vertex_array(Some(self.pcb_vao));
        gl.draw_arrays(glow::TRIANGLES, 0, self.pcb_count);

        // Draw component
        let m3 = Mat3::from_mat4(comp_model);
        let nrm = if m3.determinant().abs() > 1e-6 {
            m3.inverse().transpose()
        } else {
            Mat3::IDENTITY
        };
        set_model(gl, self.program, &comp_model, &nrm);
        gl.bind_vertex_array(Some(self.comp_vao));
        gl.draw_arrays(glow::TRIANGLES, 0, self.comp_count);

        // Draw PCB edges (semi-transparent) — rendered last after all opaque geometry
        if self.edge_count > 0 {
            gl.enable(glow::BLEND);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            gl.depth_mask(false); // don't write depth for transparent geometry
            if let Some(loc) = gl.get_uniform_location(self.program, "u_alpha") {
                gl.uniform_1_f32(Some(&loc), 0.80);
            }
            set_model(gl, self.program, &Mat4::IDENTITY, &Mat3::IDENTITY);
            gl.bind_vertex_array(Some(self.edge_vao));
            gl.draw_arrays(glow::TRIANGLES, 0, self.edge_count);
            gl.depth_mask(true);
            gl.disable(glow::BLEND);
        }

        // Restore GL state for egui
        gl.bind_vertex_array(None);
        gl.use_program(None);
        gl.disable(glow::DEPTH_TEST);
        gl.disable(glow::SCISSOR_TEST);
        gl.depth_mask(false);
        gl.enable(glow::BLEND);
    }

    unsafe fn destroy(&self, gl: &glow::Context) {
        gl.delete_program(self.program);
        gl.delete_vertex_array(self.comp_vao);
        gl.delete_buffer(self.comp_vbo);
        gl.delete_vertex_array(self.pcb_vao);
        gl.delete_buffer(self.pcb_vbo);
        gl.delete_vertex_array(self.edge_vao);
        gl.delete_buffer(self.edge_vbo);
    }
}

// ── Shared GL data (UI thread ↔ render callback) ──────────────────────────────

struct GlData {
    state:            Option<GlState>,
    pending_mesh:     Option<Mesh>,
    pending_pads:     Option<Vec<PadInfo>>,
    pending_drawings: Option<Vec<PcbDrawing>>,
    center:           Vec3,
    radius:           f32,
}

impl Default for GlData {
    fn default() -> Self {
        Self { state: None, pending_mesh: None, pending_pads: None,
               pending_drawings: None,
               center: Vec3::ZERO, radius: 1.0 }
    }
}

// ── Public viewer widget ──────────────────────────────────────────────────────

pub struct ModelViewer {
    gl_data:   Arc<Mutex<GlData>>,
    pub yaw:   f32,
    pub pitch: f32,
    pub zoom:  f32,
    pub cam_pan: Vec3,
    pub has_model: bool,
    pub ortho: bool,
}

impl Default for ModelViewer {
    fn default() -> Self {
        Self {
            gl_data: Arc::new(Mutex::new(GlData::default())),
            yaw: 0.5, pitch: 0.4, zoom: 2.5,
            cam_pan: Vec3::ZERO,
            has_model: false,
            ortho: true,
        }
    }
}

impl ModelViewer {
    pub fn load(&mut self, wrl: &[u8], pads: &[PadInfo], drawings: &[PcbDrawing], pre_rotation: [f32; 3]) {
        match parse_wrl(wrl, pre_rotation) {
            Some(mesh) => {
                let mut gd = self.gl_data.lock().unwrap();
                gd.center = mesh.center;
                gd.radius = mesh.radius;
                gd.pending_pads = Some(pads.iter()
                    .map(|p| PadInfo { cx: p.cx, cz: p.cz, w: p.w, h: p.h, shape: p.shape.clone(), drill: p.drill })
                    .collect());
                gd.pending_drawings = Some(drawings.iter()
                    .map(|d| PcbDrawing { tris: d.tris.clone(), color: d.color })
                    .collect());
                gd.pending_mesh = Some(mesh);
                self.has_model = true;
            }
            None => {
                eprintln!("[3d] VRML parse: no geometry");
                self.has_model = false;
            }
        }
    }

    pub fn load_stl(&mut self, stl: &[u8], pads: &[PadInfo], drawings: &[PcbDrawing], pre_rotation: [f32; 3]) {
        match parse_stl(stl, pre_rotation) {
            Some(mesh) => {
                let mut gd = self.gl_data.lock().unwrap();
                gd.center = mesh.center;
                gd.radius = mesh.radius;
                gd.pending_pads = Some(pads.iter()
                    .map(|p| PadInfo { cx: p.cx, cz: p.cz, w: p.w, h: p.h, shape: p.shape.clone(), drill: p.drill })
                    .collect());
                gd.pending_drawings = Some(drawings.iter()
                    .map(|d| PcbDrawing { tris: d.tris.clone(), color: d.color })
                    .collect());
                gd.pending_mesh = Some(mesh);
                self.has_model = true;
            }
            None => {
                eprintln!("[3d] STL parse: no geometry");
                self.has_model = false;
            }
        }
    }

    pub fn load_step(&mut self, step: &[u8], pads: &[PadInfo], drawings: &[PcbDrawing], pre_rotation: [f32; 3]) {
        match parse_step(step, pre_rotation) {
            Some(mesh) => {
                let mut gd = self.gl_data.lock().unwrap();
                gd.center = mesh.center;
                gd.radius = mesh.radius;
                gd.pending_pads = Some(pads.iter()
                    .map(|p| PadInfo { cx: p.cx, cz: p.cz, w: p.w, h: p.h, shape: p.shape.clone(), drill: p.drill })
                    .collect());
                gd.pending_drawings = Some(drawings.iter()
                    .map(|d| PcbDrawing { tris: d.tris.clone(), color: d.color })
                    .collect());
                gd.pending_mesh = Some(mesh);
                self.has_model = true;
            }
            None => {
                eprintln!("[3d] STEP parse: no geometry");
                self.has_model = false;
            }
        }
    }

    pub fn reset_view(&mut self) {
        self.yaw     = 0.5;
        self.pitch   = 0.4;
        self.zoom    = 2.5;
        self.cam_pan = Vec3::ZERO;
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        size: egui::Vec2,
        offset:   [f32; 3],
        rotation: [f32; 3],
        scale:    [f32; 3],
    ) {
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());

        // Left-drag: orbit
        if response.dragged_by(egui::PointerButton::Primary) {
            let d = response.drag_delta();
            self.yaw   += d.x * 0.008;
            self.pitch  = (self.pitch - d.y * 0.008).clamp(-1.5, 1.5);
            ui.ctx().request_repaint();
        }

        // Middle-drag: pan in camera plane
        if response.contains_pointer() {
            let mid_delta = ui.input(|i| {
                if i.pointer.button_down(egui::PointerButton::Middle) {
                    i.pointer.delta()
                } else {
                    egui::Vec2::ZERO
                }
            });
            if mid_delta.length_sq() > 0.0 {
                let gd = self.gl_data.lock().unwrap();
                let world_per_px = gd.radius * self.zoom / size.x;
                drop(gd);
                // Camera right and up vectors from yaw/pitch
                let right = Vec3::new(self.yaw.cos(), 0.0, -self.yaw.sin());
                let fwd   = Vec3::new(
                    -self.pitch.cos() * self.yaw.sin(),
                    -self.pitch.sin(),
                    -self.pitch.cos() * self.yaw.cos(),
                );
                let up = right.cross(fwd).normalize();

                // For top/bottom views, flip vertical pan direction to match screen orientation
                let y_sign = if self.pitch.abs() > 1.4 { -1.0 } else { 1.0 };

                self.cam_pan -= right * (mid_delta.x * world_per_px)
                              - up    * (mid_delta.y * world_per_px * y_sign);
                ui.ctx().request_repaint();
            }
        }

        // Scroll: zoom — consume event so the parent ScrollArea doesn't also move.
        if response.contains_pointer() {
            let scroll = ui.input_mut(|i| {
                let s = i.smooth_scroll_delta.y;
                if s.abs() > 0.1 {
                    i.smooth_scroll_delta = egui::Vec2::ZERO;
                    i.raw_scroll_delta    = egui::Vec2::ZERO;
                }
                s
            });
            if scroll.abs() > 0.1 {
                self.zoom = (self.zoom * (1.0 - scroll * 0.003)).clamp(0.3, 20.0);
                ui.ctx().request_repaint();
            }
        }

        ui.painter().rect_filled(rect, 4.0, egui::Color32::WHITE);

        if !self.has_model {
            ui.painter().text(
                rect.center(), egui::Align2::CENTER_CENTER,
                "⚠ No 3D model available from JLCPCB",
                egui::FontId::proportional(16.0),
                egui::Color32::from_rgb(200, 100, 50),
            );
            return;
        }

        let gl_data = self.gl_data.clone();
        let (yaw, pitch, zoom, cam_pan, ortho) = (self.yaw, self.pitch, self.zoom, self.cam_pan, self.ortho);

        ui.painter().add(egui::PaintCallback {
            rect,
            callback: Arc::new(eframe::egui_glow::CallbackFn::new(
                move |info, painter| {
                    let gl = painter.gl();
                    let mut gd = gl_data.lock().unwrap();

                    if let Some(mesh) = gd.pending_mesh.take() {
                        let pads     = gd.pending_pads.take().unwrap_or_default();
                        let drawings = gd.pending_drawings.take().unwrap_or_default();
                        if let Some(old) = gd.state.take() {
                            unsafe { old.destroy(gl); }
                        }
                        gd.state = unsafe { GlState::new(gl, &mesh, &pads, &drawings) };
                    }

                    if let Some(state) = &gd.state {
                        unsafe {
                            state.paint(
                                gl, &info,
                                gd.center, gd.radius,
                                yaw, pitch, zoom,
                                cam_pan,
                                offset, rotation, scale,
                                ortho,
                            );
                        }
                    }
                },
            )),
        });

        ui.painter().text(
            rect.left_bottom() + egui::vec2(4.0, -4.0),
            egui::Align2::LEFT_BOTTOM,
            "L-drag: orbit   M-drag: pan   scroll: zoom",
            egui::FontId::proportional(10.0),
            egui::Color32::from_gray(120),
        );
    }
}

// ── GL helpers ────────────────────────────────────────────────────────────────

unsafe fn upload_verts(
    gl: &glow::Context,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    data: &[f32],
) {
    gl.bind_vertex_array(Some(vao));
    gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
    let bytes = std::slice::from_raw_parts(
        data.as_ptr() as *const u8,
        data.len() * 4,
    );
    gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::STATIC_DRAW);
    let stride = 9 * 4_i32;
    gl.enable_vertex_attrib_array(0);
    gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, stride, 0);
    gl.enable_vertex_attrib_array(1);
    gl.vertex_attrib_pointer_f32(1, 3, glow::FLOAT, false, stride, 12);
    gl.enable_vertex_attrib_array(2);
    gl.vertex_attrib_pointer_f32(2, 3, glow::FLOAT, false, stride, 24);
    gl.bind_vertex_array(None);
    gl.bind_buffer(glow::ARRAY_BUFFER, None);
}

unsafe fn set_model(
    gl: &glow::Context,
    prog: glow::Program,
    model: &Mat4,
    nrm: &Mat3,
) {
    if let Some(loc) = gl.get_uniform_location(prog, "u_model") {
        gl.uniform_matrix_4_f32_slice(Some(&loc), false, &model.to_cols_array());
    }
    if let Some(loc) = gl.get_uniform_location(prog, "u_nrm") {
        gl.uniform_matrix_3_f32_slice(Some(&loc), false, &nrm.to_cols_array());
    }
}

fn build_model_mat(offset: [f32; 3], rotation: [f32; 3], scale: [f32; 3], center: Vec3) -> Mat4 {
    // Rotate and scale around the component's own bounding-box center, not the world origin.
    // Matrix order: translate(offset + center) * R * S * translate(-center)
    // Coordinate swap: KiCad Y→viewer Z, KiCad Z→viewer Y
    let offset_world = Vec3::new(offset[0], offset[2], offset[1]);
    let t     = Mat4::from_translation(offset_world + center);
    let t_neg = Mat4::from_translation(-center);
    let r = Mat4::from_euler(
        glam::EulerRot::ZYX,
        rotation[1].to_radians(),  // Y from UI → Z axis in viewer
        rotation[2].to_radians(),  // Z from UI → Y axis in viewer
        rotation[0].to_radians(),  // X stays X
    );
    let s = Mat4::from_scale(Vec3::from(scale));
    t * r * s * t_neg
}

// ── PCB + pad geometry ────────────────────────────────────────────────────────

fn build_pcb_body(radius: f32, mesh_xz_half: f32, pads: &[PadInfo], drawings: &[PcbDrawing]) -> Vec<f32> {
    const MARGIN: f32 = 3.0;
    let pad_half = if pads.is_empty() { 0.0_f32 } else {
        let mx = pads.iter().map(|p| p.cx.abs() + p.w * 0.5).fold(0.0_f32, f32::max);
        let mz = pads.iter().map(|p| p.cz.abs() + p.h * 0.5).fold(0.0_f32, f32::max);
        mx.max(mz)
    };
    let half  = pad_half.max(mesh_xz_half).max(radius * 0.5) + MARGIN;
    let thick = 1.6_f32;
    let mut v: Vec<f32> = Vec::new();

    // Collect drill holes for surface tessellation
    let holes: Vec<(f32, f32, f32)> = pads.iter()
        .filter(|p| p.drill > 0.0)
        .map(|p| (p.cx, p.cz, p.drill * 0.5))
        .collect();

    // Top surface (Y=0) and bottom surface (Y=-thick) — tessellated with real holes
    let top_green = [0.10_f32, 0.42, 0.12];
    let bot_green = [0.07_f32, 0.30, 0.09];
    quad_y_holed(&mut v, half, &holes, 0.0,    top_green);
    quad_y_holed(&mut v, half, &holes, -thick, bot_green);

    let pc  = [0.85_f32, 0.68, 0.08]; // copper

    // Pads and through-hole barrels
    let draw_pad = |v: &mut Vec<f32>, pad: &PadInfo, hw: f32, hh: f32, y: f32, c: [f32; 3]| {
        match pad.shape.as_str() {
            "circle" => circle_y(v, pad.cx, pad.cz, hw.max(hh), y, c),
            "oval" => {
                if (hw - hh).abs() < 0.01 {
                    circle_y(v, pad.cx, pad.cz, hw, y, c);
                } else if hw > hh {
                    quad_y(v, pad.cx - hw + hh, pad.cx + hw - hh, pad.cz - hh, pad.cz + hh, y, c);
                    circle_y(v, pad.cx - hw + hh, pad.cz, hh, y, c);
                    circle_y(v, pad.cx + hw - hh, pad.cz, hh, y, c);
                } else {
                    quad_y(v, pad.cx - hw, pad.cx + hw, pad.cz - hh + hw, pad.cz + hh - hw, y, c);
                    circle_y(v, pad.cx, pad.cz - hh + hw, hw, y, c);
                    circle_y(v, pad.cx, pad.cz + hh - hw, hw, y, c);
                }
            }
            _ => quad_y(v, pad.cx - hw, pad.cx + hw, pad.cz - hh, pad.cz + hh, y, c),
        }
    };

    for pad in pads {
        let hw = pad.w * 0.5;
        let hh = pad.h * 0.5;
        if pad.drill > 0.0 {
            let dr  = pad.drill * 0.5;
            let r_o = hw.max(hh); // outer pad radius
            // Copper barrel — open cylinder, no caps so hole is see-through
            cylinder_hole(&mut v, pad.cx, pad.cz, dr, 0.0, -thick, pc);
            // Annular ring pads: inner=drill radius, outer=pad radius
            // This keeps the hole center genuinely open for see-through
            ring_y(&mut v, pad.cx, pad.cz, dr, r_o,  0.05,         pc);
            ring_y(&mut v, pad.cx, pad.cz, dr, r_o, -thick - 0.05, pc);
        } else {
            draw_pad(&mut v, pad, hw, hh, 0.05, pc);
        }
    }

    // Silkscreen / fab drawings at Y=0.08
    let draw_y = 0.08_f32;
    let draw_n = [0.0_f32, 1.0, 0.0];
    for draw in drawings {
        for tri in draw.tris.chunks(3) {
            if tri.len() < 3 { continue; }
            for vert in tri {
                v.extend_from_slice(&[vert[0], draw_y, vert[1]]);
                v.extend_from_slice(&draw_n);
                v.extend_from_slice(&draw.color);
            }
        }
    }

    v
}

fn build_pcb_edges(radius: f32, mesh_xz_half: f32, pads: &[PadInfo]) -> Vec<f32> {
    const MARGIN: f32 = 3.0;
    let pad_half = if pads.is_empty() { 0.0_f32 } else {
        let mx = pads.iter().map(|p| p.cx.abs() + p.w * 0.5).fold(0.0_f32, f32::max);
        let mz = pads.iter().map(|p| p.cz.abs() + p.h * 0.5).fold(0.0_f32, f32::max);
        mx.max(mz)
    };
    let half  = pad_half.max(mesh_xz_half).max(radius * 0.5) + MARGIN;
    let thick = 1.6_f32;
    let mut v: Vec<f32> = Vec::new();
    let ec = [0.08_f32, 0.32, 0.10]; // edge green
    rect_xz_face(&mut v, -half, -thick, -half,  half, 0.0, -half, [0.0, 0.0, -1.0], ec);
    rect_xz_face(&mut v,  half, -thick,  half, -half, 0.0,  half, [0.0, 0.0,  1.0], ec);
    rect_xz_face(&mut v,  half, -thick, -half,  half, 0.0,  half, [1.0, 0.0,  0.0], ec);
    rect_xz_face(&mut v, -half, -thick,  half, -half, 0.0, -half, [-1.0, 0.0, 0.0], ec);
    v
}

// Flat circle in XZ plane at given Y, centered at (cx, cz) with radius r
fn circle_y(v: &mut Vec<f32>, cx: f32, cz: f32, r: f32, y: f32, c: [f32; 3]) {
    let n = [0.0_f32, 1.0, 0.0];
    let segments = 24; // Number of triangles
    let center = [cx, y, cz];

    for i in 0..segments {
        let a0 = (i as f32) * 2.0 * std::f32::consts::PI / (segments as f32);
        let a1 = ((i + 1) as f32) * 2.0 * std::f32::consts::PI / (segments as f32);

        let p0 = [cx + r * a0.cos(), y, cz + r * a0.sin()];
        let p1 = [cx + r * a1.cos(), y, cz + r * a1.sin()];

        // Triangle: center -> p0 -> p1
        v.extend_from_slice(&center);
        v.extend_from_slice(&n);
        v.extend_from_slice(&c);

        v.extend_from_slice(&p0);
        v.extend_from_slice(&n);
        v.extend_from_slice(&c);

        v.extend_from_slice(&p1);
        v.extend_from_slice(&n);
        v.extend_from_slice(&c);
    }
}

// Annular ring (washer) in XZ plane — inner radius r_i, outer radius r_o
fn ring_y(v: &mut Vec<f32>, cx: f32, cz: f32, r_i: f32, r_o: f32, y: f32, c: [f32; 3]) {
    let n: [f32; 3] = [0.0, if y >= 0.0 { 1.0 } else { -1.0 }, 0.0];
    let segments = 32;
    for i in 0..segments {
        let a0 = (i as f32) * 2.0 * std::f32::consts::PI / segments as f32;
        let a1 = ((i + 1) as f32) * 2.0 * std::f32::consts::PI / segments as f32;
        let pi0 = [cx + r_i * a0.cos(), y, cz + r_i * a0.sin()];
        let pi1 = [cx + r_i * a1.cos(), y, cz + r_i * a1.sin()];
        let po0 = [cx + r_o * a0.cos(), y, cz + r_o * a0.sin()];
        let po1 = [cx + r_o * a1.cos(), y, cz + r_o * a1.sin()];
        if y >= 0.0 {
            for pt in [pi0, po0, po1] { v.extend_from_slice(&pt); v.extend_from_slice(&n); v.extend_from_slice(&c); }
            for pt in [pi0, po1, pi1] { v.extend_from_slice(&pt); v.extend_from_slice(&n); v.extend_from_slice(&c); }
        } else {
            for pt in [pi0, po1, po0] { v.extend_from_slice(&pt); v.extend_from_slice(&n); v.extend_from_slice(&c); }
            for pt in [pi0, pi1, po1] { v.extend_from_slice(&pt); v.extend_from_slice(&n); v.extend_from_slice(&c); }
        }
    }
}

// Same as circle_y but normal faces DOWN (for bottom-facing surfaces)
fn circle_y_down(v: &mut Vec<f32>, cx: f32, cz: f32, r: f32, y: f32, c: [f32; 3]) {
    let n = [0.0_f32, -1.0, 0.0];
    let segments = 24;
    for i in 0..segments {
        let a0 = (i as f32) * 2.0 * std::f32::consts::PI / segments as f32;
        let a1 = ((i + 1) as f32) * 2.0 * std::f32::consts::PI / segments as f32;
        let center = [cx, y, cz];
        let p0 = [cx + r * a0.cos(), y, cz + r * a0.sin()];
        let p1 = [cx + r * a1.cos(), y, cz + r * a1.sin()];
        // Reversed winding for downward normal
        for pt in [center, p1, p0] {
            v.extend_from_slice(&pt); v.extend_from_slice(&n); v.extend_from_slice(&c);
        }
    }
}

// Copper-plated through-hole barrel: cylinder from y_top to y_bot, inward normals
fn cylinder_hole(v: &mut Vec<f32>, cx: f32, cz: f32, r: f32, y_top: f32, y_bot: f32, c: [f32; 3]) {
    let segments = 24;
    for i in 0..segments {
        let a0 = (i as f32) * 2.0 * std::f32::consts::PI / segments as f32;
        let a1 = ((i + 1) as f32) * 2.0 * std::f32::consts::PI / segments as f32;
        let (c0, s0) = (a0.cos(), a0.sin());
        let (c1, s1) = (a1.cos(), a1.sin());
        let n0 = [-c0, 0.0_f32, -s0]; // inward normal
        let n1 = [-c1, 0.0_f32, -s1];
        let t0 = [cx + r*c0, y_top, cz + r*s0];
        let t1 = [cx + r*c1, y_top, cz + r*s1];
        let b0 = [cx + r*c0, y_bot,  cz + r*s0];
        let b1 = [cx + r*c1, y_bot,  cz + r*s1];
        // Two triangles per segment
        for (p, n) in [(t0,n0),(b0,n0),(b1,n1)] { v.extend_from_slice(&p); v.extend_from_slice(&n); v.extend_from_slice(&c); }
        for (p, n) in [(t0,n0),(b1,n1),(t1,n1)] { v.extend_from_slice(&p); v.extend_from_slice(&n); v.extend_from_slice(&c); }
    }
}

// PCB surface quad in XZ plane with circular drill holes cut out (grid tessellation)
fn quad_y_holed(v: &mut Vec<f32>, half: f32, holes: &[(f32, f32, f32)], y: f32, c: [f32; 3]) {
    const N: i32 = 96;
    let step = (2.0 * half) / N as f32;
    let nrm: [f32; 3] = [0.0, if y >= 0.0 { 1.0 } else { -1.0 }, 0.0];
    for i in 0..N {
        for j in 0..N {
            let x0 = -half + i as f32 * step;
            let x1 = x0 + step;
            let z0 = -half + j as f32 * step;
            let z1 = z0 + step;
            let cx = (x0 + x1) * 0.5;
            let cz = (z0 + z1) * 0.5;
            let in_hole = holes.iter().any(|(hx, hz, hr)| {
                let dx = cx - hx; let dz = cz - hz;
                dx * dx + dz * dz < hr * hr
            });
            if in_hole { continue; }
            let pts = [[x0,y,z0],[x1,y,z0],[x1,y,z1],[x0,y,z1]];
            let order: [(usize,usize,usize); 2] = if y >= 0.0 { [(0,1,2),(0,2,3)] } else { [(0,2,1),(0,3,2)] };
            for (a,b,ci) in order {
                for &k in &[a, b, ci] {
                    v.extend_from_slice(&pts[k]);
                    v.extend_from_slice(&nrm);
                    v.extend_from_slice(&c);
                }
            }
        }
    }
}

// Flat quad in XZ plane at given Y, x0..x1, z0..z1 (CCW from above = Y-up)
fn quad_y(v: &mut Vec<f32>, x0: f32, x1: f32, z0: f32, z1: f32, y: f32, c: [f32; 3]) {
    let n = if y >= 0.0 { [0.0_f32, 1.0, 0.0] } else { [0.0_f32, -1.0, 0.0] };
    let pts = [[x0,y,z0],[x1,y,z0],[x1,y,z1],[x0,y,z1]];
    let order = if y >= 0.0 { [(0,1,2),(0,2,3)] } else { [(0,2,1),(0,3,2)] };
    for (a,b,c_i) in order {
        for &i in &[a, b, c_i] {
            v.extend_from_slice(&pts[i]);
            v.extend_from_slice(&n);
            v.extend_from_slice(&c);
        }
    }
}

// Side face of PCB board — two triangles forming a vertical rect
fn rect_xz_face(
    v: &mut Vec<f32>,
    x0: f32, y0: f32, z0: f32,
    x1: f32, y1: f32, z1: f32,  // ignored — we use x0/z0 and x1/z1 for two corners
    n: [f32; 3],
    c: [f32; 3],
) {
    // We receive the "far" corner as (x1,y1,z1) but need 4 corners of the face.
    // Call site passes: near-bottom corner and far-bottom corner; we extend Y.
    // Actually let's do it differently: the 4 corners are determined by the normal.
    // For simplicity, let's treat (x0, y0, z0)..(x1, y1, z1) as two diagonal points
    // of the face. This only works for axis-aligned faces.
    let pts = [
        [x0, y0, z0],
        [x1, y0, z1],
        [x1, y1, z1],
        [x0, y1, z0],
    ];
    for (a,b,ci) in [(0,1,2),(0,2,3)] {
        for &i in &[a, b, ci] {
            v.extend_from_slice(&pts[i]);
            v.extend_from_slice(&n);
            v.extend_from_slice(&c);
        }
    }
}

// ── VRML 2.0 parser ───────────────────────────────────────────────────────────

fn parse_wrl(data: &[u8], pre_rotation: [f32; 3]) -> Option<Mesh> {
    let raw = String::from_utf8_lossy(data);
    let text: String = raw.lines()
        .map(|l| if let Some(i) = l.find('#') { &l[..i] } else { l })
        .collect::<Vec<_>>()
        .join(" ");

    let point_arrays = collect_float_arrays(&text, "point");
    let index_arrays = collect_int_arrays(&text, "coordIndex");
    let diffuse      = collect_diffuse_colors(&text);

    if point_arrays.is_empty() || index_arrays.is_empty() {
        return None;
    }

    let mut verts: Vec<f32> = Vec::new();

    for (i, (pts_raw, idx_raw)) in
        point_arrays.iter().zip(index_arrays.iter()).enumerate()
    {
        let color = diffuse.get(i).copied().unwrap_or([0.75, 0.75, 0.75]);
        // KiCad VRML uses 0.1-inch units (1 unit = 2.54 mm); convert to mm for the viewer.
        let pts: Vec<[f32; 3]> = pts_raw.chunks_exact(3)
            .map(|c| [c[0] * 2.54, c[1] * 2.54, c[2] * 2.54])
            .collect();
        if pts.is_empty() { continue; }

        let mut face: Vec<usize> = Vec::new();
        for &idx in idx_raw {
            if idx < 0 {
                if face.len() >= 3 {
                    let a = pts[face[0]];
                    for j in 1..face.len() - 1 {
                        let b = pts[face[j]];
                        let c = pts[face[j + 1]];
                        let n = flat_normal(a, b, c);
                        for &p in &[a, b, c] {
                            verts.extend_from_slice(&p);
                            verts.extend_from_slice(&n);
                            verts.extend_from_slice(&color);
                        }
                    }
                }
                face.clear();
            } else {
                let ui = idx as usize;
                if ui < pts.len() { face.push(ui); }
            }
        }
    }

    if verts.is_empty() { return None; }

    // Bake pre_rotation (c_rotation from EasyEDA) into the mesh.
    // KiCad has a rotation sign bug - negatives match positive footprint rotation!
    // Use XYZ order with NEGATED angles to match KiCad's 3D viewer behavior.
    let any_rot = pre_rotation.iter().any(|&v| v.abs() > 1e-4);
    if any_rot {
        let mat = Mat3::from_euler(
            glam::EulerRot::XYZ,
            -pre_rotation[0].to_radians(),
            -pre_rotation[1].to_radians(),
            -pre_rotation[2].to_radians(),
        );
        for chunk in verts.chunks_mut(9) {
            let p = Vec3::new(chunk[0], chunk[1], chunk[2]);
            let n = Vec3::new(chunk[3], chunk[4], chunk[5]);
            let rp = mat * p;
            let rn = mat * n;
            chunk[0] = rp.x; chunk[1] = rp.y; chunk[2] = rp.z;
            chunk[3] = rn.x; chunk[4] = rn.y; chunk[5] = rn.z;
        }
    }

    // Shift Y so the model bottom sits 0.1mm above the PCB surface (prevents z-fighting at Y=0).
    let min_y = verts.chunks(9).map(|c| c[1]).fold(f32::MAX, f32::min);
    for chunk in verts.chunks_mut(9) { chunk[1] += 0.1 - min_y; }

    // Center the model at XZ=(0,0) so it appears over the pad centroid.
    // The VRML from EasyEDA uses its own origin which may differ from the footprint centroid.
    let (pre_center, _, _) = compute_bounds(&verts);
    for chunk in verts.chunks_mut(9) {
        chunk[0] -= pre_center.x;
        chunk[2] -= pre_center.z;
    }

    let (center, radius, xz_half) = compute_bounds(&verts);
    Some(Mesh { count: (verts.len() / 9) as i32, data: verts, center, radius, xz_half })
}

// ── STL parser ────────────────────────────────────────────────────────────────

fn parse_stl(data: &[u8], pre_rotation: [f32; 3]) -> Option<Mesh> {
    use std::io::Cursor;

    // Try binary STL first, fall back to ASCII
    let mut cursor = Cursor::new(data);
    let stl = stl_io::read_stl(&mut cursor).ok()?;

    let mut verts: Vec<f32> = Vec::new();
    let color = [0.2, 0.2, 0.2]; // Dark gray default color

    for tri in stl.faces {
        // STL uses indexed vertices
        let idx = tri.vertices;
        if idx[0] >= stl.vertices.len() || idx[1] >= stl.vertices.len() || idx[2] >= stl.vertices.len() {
            continue; // Invalid indices
        }

        let a: [f32; 3] = stl.vertices[idx[0]].into();
        let b: [f32; 3] = stl.vertices[idx[1]].into();
        let c: [f32; 3] = stl.vertices[idx[2]].into();

        // Use provided normal from STL, or compute if zero
        let n: [f32; 3] = if tri.normal[0].abs() < 1e-6 && tri.normal[1].abs() < 1e-6 && tri.normal[2].abs() < 1e-6 {
            flat_normal(a, b, c)
        } else {
            tri.normal.into()
        };

        // Add triangle: 3 vertices × (pos + normal + color)
        for &p in &[a, b, c] {
            verts.extend_from_slice(&p);
            verts.extend_from_slice(&n);
            verts.extend_from_slice(&color);
        }
    }

    if verts.is_empty() { return None; }

    // Apply pre_rotation if needed (same as WRL)
    let any_rot = pre_rotation.iter().any(|&v| v.abs() > 1e-4);
    if any_rot {
        let mat = Mat3::from_euler(
            glam::EulerRot::XYZ,
            -pre_rotation[0].to_radians(),
            -pre_rotation[1].to_radians(),
            -pre_rotation[2].to_radians(),
        );
        for chunk in verts.chunks_mut(9) {
            let p = Vec3::new(chunk[0], chunk[1], chunk[2]);
            let n = Vec3::new(chunk[3], chunk[4], chunk[5]);
            let rp = mat * p;
            let rn = mat * n;
            chunk[0] = rp.x; chunk[1] = rp.y; chunk[2] = rp.z;
            chunk[3] = rn.x; chunk[4] = rn.y; chunk[5] = rn.z;
        }
    }

    // Shift Y so model bottom sits 0.1mm above PCB surface
    let min_y = verts.chunks(9).map(|c| c[1]).fold(f32::MAX, f32::min);
    for chunk in verts.chunks_mut(9) { chunk[1] += 0.1 - min_y; }

    // Center at XZ=(0,0)
    let (pre_center, _, _) = compute_bounds(&verts);
    for chunk in verts.chunks_mut(9) {
        chunk[0] -= pre_center.x;
        chunk[2] -= pre_center.z;
    }

    let (center, radius, xz_half) = compute_bounds(&verts);
    Some(Mesh { count: (verts.len() / 9) as i32, data: verts, center, radius, xz_half })
}

// ── STEP color extraction ─────────────────────────────────────────────────────

fn step_refs(s: &str) -> Vec<u64> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(p) = rest.find('#') {
        rest = &rest[p + 1..];
        let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        if end > 0 { if let Ok(id) = rest[..end].parse::<u64>() { out.push(id); } }
        rest = &rest[end.min(rest.len())..];
    }
    out
}

fn step_floats(s: &str) -> Vec<f64> {
    let mut out = Vec::new();
    let mut i = 0;
    let b = s.as_bytes();
    while i < b.len() {
        // skip to a digit or minus that starts a number
        if b[i] == b'-' || b[i].is_ascii_digit() {
            let start = i;
            if b[i] == b'-' { i += 1; }
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.' || b[i] == b'e' || b[i] == b'E' || ((b[i] == b'+' || b[i] == b'-') && i > 0 && (b[i-1] == b'e' || b[i-1] == b'E'))) {
                i += 1;
            }
            if let Ok(f) = s[start..i].parse::<f64>() { out.push(f); }
        } else {
            i += 1;
        }
    }
    out
}

/// Parse the STEP text and return a map: shell_entity_id → [r, g, b]
/// Follows the full AP214 color chain:
/// STYLED_ITEM → PRESENTATION_STYLE_ASSIGNMENT → SURFACE_STYLE_USAGE →
/// SURFACE_SIDE_STYLE → SURFACE_STYLE_FILL_AREA → FILL_AREA_STYLE →
/// FILL_AREA_STYLE_COLOUR → COLOUR_RGB
fn step_shell_colors(text: &str) -> std::collections::HashMap<u64, [f32; 3]> {
    use std::collections::HashMap;

    let mut entity_name: HashMap<u64, String> = HashMap::new();
    let mut entity_refs: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut id_to_color: HashMap<u64, [f32; 3]> = HashMap::new();
    let mut shell_faces: HashMap<u64, Vec<u64>> = HashMap::new();

    for line in text.lines() {
        let line = line.trim();
        let Some(line) = line.strip_prefix('#') else { continue };
        let Some(eq) = line.find('=') else { continue };
        let Ok(id) = line[..eq].trim().parse::<u64>() else { continue };
        let content = line[eq + 1..].trim().trim_end_matches(';').trim();

        let name_end = content.find(|c: char| !c.is_alphanumeric() && c != '_').unwrap_or(content.len());
        let name = content[..name_end].to_uppercase();
        let refs = step_refs(content);

        if name == "COLOUR_RGB" {
            let nums = step_floats(content);
            if nums.len() >= 3 {
                id_to_color.insert(id, [nums[0] as f32, nums[1] as f32, nums[2] as f32]);
            }
        } else if name == "CLOSED_SHELL" || name == "OPEN_SHELL" {
            shell_faces.insert(id, refs.clone());
        }

        entity_name.insert(id, name);
        entity_refs.insert(id, refs);
    }

    // Propagate colors up through the chain (multiple passes until stable)
    let chain = [
        "FILL_AREA_STYLE_COLOUR",
        "FILL_AREA_STYLE",
        "SURFACE_STYLE_FILL_AREA",
        "SURFACE_SIDE_STYLE",
        "SURFACE_STYLE_USAGE",
        "CURVE_STYLE",
        "PRESENTATION_STYLE_ASSIGNMENT",
    ];
    for _ in 0..2 {
        for ty in &chain {
            let ids: Vec<u64> = entity_name.iter()
                .filter(|(_, n)| n.as_str() == *ty)
                .map(|(&id, _)| id)
                .collect();
            for id in ids {
                if id_to_color.contains_key(&id) { continue; }
                let refs = entity_refs.get(&id).cloned().unwrap_or_default();
                for r in refs {
                    if let Some(&c) = id_to_color.get(&r) {
                        id_to_color.insert(id, c);
                        break;
                    }
                }
            }
        }
    }

    // Build face_id → color from STYLED_ITEM and OVER_RIDING_STYLED_ITEM
    let mut face_colors: HashMap<u64, [f32; 3]> = HashMap::new();
    for (&_id, name) in &entity_name {
        if name != "STYLED_ITEM" && name != "OVER_RIDING_STYLED_ITEM" { continue; }
        let refs = entity_refs.get(&_id).cloned().unwrap_or_default();
        if refs.len() < 2 { continue; }
        let shape_ref = *refs.last().unwrap();
        for &r in &refs[..refs.len() - 1] {
            if let Some(&c) = id_to_color.get(&r) {
                face_colors.insert(shape_ref, c);
                break;
            }
        }
    }

    // Build shell_id → dominant face color
    let mut shell_colors: HashMap<u64, [f32; 3]> = HashMap::new();
    for (&shell_id, face_ids) in &shell_faces {
        // Pick the most frequent color among this shell's faces
        let mut color_count: HashMap<[u32; 3], (usize, [f32; 3])> = HashMap::new();
        for &fid in face_ids {
            if let Some(&c) = face_colors.get(&fid) {
                let key = [
                    (c[0] * 1000.0) as u32,
                    (c[1] * 1000.0) as u32,
                    (c[2] * 1000.0) as u32,
                ];
                color_count.entry(key).and_modify(|(cnt, _)| *cnt += 1).or_insert((1, c));
            }
        }
        if let Some((_, c)) = color_count.values().max_by_key(|(cnt, _)| *cnt) {
            shell_colors.insert(shell_id, *c);
        }
    }

    shell_colors
}

// ── STEP parser (via truck) ───────────────────────────────────────────────────

fn parse_step(data: &[u8], pre_rotation: [f32; 3]) -> Option<Mesh> {
    use truck_stepio::r#in::{ruststep, Table};
    use truck_meshalgo::prelude::*;

    let s = String::from_utf8_lossy(data);
    let shell_colors = step_shell_colors(s.as_ref());
    let exchange = ruststep::parser::parse(s.as_ref()).ok()?;
    let data_section = exchange.data.first()?;
    let table = Table::from_data_section(data_section);

    let mut verts: Vec<f32> = Vec::new();
    const DEFAULT_COLOR: [f32; 3] = [0.75, 0.75, 0.75];

    for (shell_id, shell_holder) in table.shell.iter() {
        let color = shell_colors.get(shell_id).copied().unwrap_or(DEFAULT_COLOR);
        let shell = match table.to_compressed_shell(shell_holder) {
            Ok(s) => s,
            Err(_) => continue,
        };
        // Two-pass adaptive triangulation: first pass computes bounding box for tolerance
        let bdd = shell.robust_triangulation(0.01).to_polygon().bounding_box();
        let tol = (bdd.diameter() * 0.001).max(1e-4);
        let poly = shell.robust_triangulation(tol).to_polygon();

        let positions = poly.positions();
        let normals   = poly.normals();
        for tri in poly.tri_faces() {
            let p0 = positions[tri[0].pos];
            let p1 = positions[tri[1].pos];
            let p2 = positions[tri[2].pos];
            let e1 = [p1.x-p0.x, p1.y-p0.y, p1.z-p0.z];
            let e2 = [p2.x-p0.x, p2.y-p0.y, p2.z-p0.z];
            let fn_ = [
                e1[1]*e2[2] - e1[2]*e2[1],
                e1[2]*e2[0] - e1[0]*e2[2],
                e1[0]*e2[1] - e1[1]*e2[0],
            ];
            let flen = (fn_[0]*fn_[0]+fn_[1]*fn_[1]+fn_[2]*fn_[2]).sqrt().max(1e-9);
            let face_n = [(fn_[0]/flen) as f32, (fn_[1]/flen) as f32, (fn_[2]/flen) as f32];

            for v in tri {
                let p = positions[v.pos];
                let (nx, ny, nz) = match v.nor {
                    Some(ni) => {
                        let n = normals[ni];
                        (n.x as f32, n.y as f32, n.z as f32)
                    }
                    None => (face_n[0], face_n[1], face_n[2]),
                };
                verts.extend_from_slice(&[
                    p.x as f32, p.y as f32, p.z as f32,
                    nx, ny, nz,
                    color[0], color[1], color[2],
                ]);
            }
        }
    }

    if verts.is_empty() { return None; }

    let any_rot = pre_rotation.iter().any(|&v| v.abs() > 1e-4);
    if any_rot {
        let mat = Mat3::from_euler(
            glam::EulerRot::XYZ,
            -pre_rotation[0].to_radians(),
            -pre_rotation[1].to_radians(),
            -pre_rotation[2].to_radians(),
        );
        for chunk in verts.chunks_mut(9) {
            let p = Vec3::new(chunk[0], chunk[1], chunk[2]);
            let n = Vec3::new(chunk[3], chunk[4], chunk[5]);
            let rp = mat * p;
            let rn = mat * n;
            chunk[0] = rp.x; chunk[1] = rp.y; chunk[2] = rp.z;
            chunk[3] = rn.x; chunk[4] = rn.y; chunk[5] = rn.z;
        }
    }

    let min_y = verts.chunks(9).map(|c| c[1]).fold(f32::MAX, f32::min);
    for chunk in verts.chunks_mut(9) { chunk[1] += 0.1 - min_y; }

    let (pre_center, _, _) = compute_bounds(&verts);
    for chunk in verts.chunks_mut(9) {
        chunk[0] -= pre_center.x;
        chunk[2] -= pre_center.z;
    }

    let (center, radius, xz_half) = compute_bounds(&verts);
    Some(Mesh { count: (verts.len() / 9) as i32, data: verts, center, radius, xz_half })
}

fn flat_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [f32; 3] {
    let ab = [b[0]-a[0], b[1]-a[1], b[2]-a[2]];
    let ac = [c[0]-a[0], c[1]-a[1], c[2]-a[2]];
    let n  = [ab[1]*ac[2]-ab[2]*ac[1], ab[2]*ac[0]-ab[0]*ac[2], ab[0]*ac[1]-ab[1]*ac[0]];
    let len = (n[0]*n[0]+n[1]*n[1]+n[2]*n[2]).sqrt().max(1e-9);
    [n[0]/len, n[1]/len, n[2]/len]
}

fn compute_bounds(verts: &[f32]) -> (Vec3, f32, f32) {
    let mut mn = Vec3::splat(f32::MAX);
    let mut mx = Vec3::splat(f32::MIN);
    for chunk in verts.chunks(9) {
        let p = Vec3::new(chunk[0], chunk[1], chunk[2]);
        mn = mn.min(p);
        mx = mx.max(p);
    }
    let center = (mn + mx) * 0.5;
    let radius = ((mx - mn).length() * 0.5).max(0.001);
    let xz_half = ((mx.x - mn.x).max(mx.z - mn.z) * 0.5).max(0.001);
    (center, radius, xz_half)
}

fn collect_float_arrays(text: &str, tag: &str) -> Vec<Vec<f32>> {
    let mut out = Vec::new();
    let mut s = text;
    while let Some(pos) = s.find(tag) {
        s = &s[pos + tag.len()..];
        let trimmed = s.trim_start();
        if !trimmed.starts_with('[') { continue; }
        s = &trimmed[1..];
        let end = match s.find(']') { Some(e) => e, None => break };
        let nums: Vec<f32> = s[..end]
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|t| !t.is_empty())
            .filter_map(|t| t.parse().ok())
            .collect();
        s = &s[end + 1..];
        if !nums.is_empty() { out.push(nums); }
    }
    out
}

fn collect_int_arrays(text: &str, tag: &str) -> Vec<Vec<i32>> {
    let mut out = Vec::new();
    let mut s = text;
    while let Some(pos) = s.find(tag) {
        s = &s[pos + tag.len()..];
        let trimmed = s.trim_start();
        if !trimmed.starts_with('[') { continue; }
        s = &trimmed[1..];
        let end = match s.find(']') { Some(e) => e, None => break };
        let nums: Vec<i32> = s[..end]
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|t| !t.is_empty())
            .filter_map(|t| t.parse().ok())
            .collect();
        s = &s[end + 1..];
        if !nums.is_empty() { out.push(nums); }
    }
    out
}

fn collect_diffuse_colors(text: &str) -> Vec<[f32; 3]> {
    let mut out = Vec::new();
    let mut s = text;
    while let Some(pos) = s.find("diffuseColor") {
        s = &s[pos + 12..];
        let nums: Vec<f32> = s
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|t| !t.is_empty())
            .take(3)
            .filter_map(|t| t.parse().ok())
            .collect();
        if nums.len() == 3 { out.push([nums[0], nums[1], nums[2]]); }
    }
    out
}

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
in vec3 v_col;
in vec3 v_nrm;
out vec4 o_col;
void main() {
    vec3 n  = normalize(v_nrm);
    vec3 L1 = normalize(vec3(1.0, 2.0, 1.5));
    vec3 L2 = normalize(vec3(-1.0, 1.5, -0.5));
    vec3 L3 = normalize(vec3(0.0, -1.0, 0.5));  // soft fill from below
    // Two-sided lighting: use abs() to light both sides of faces
    float d = abs(dot(n, L1))
            + 0.45 * abs(dot(n, L2))
            + 0.20 * abs(dot(n, L3));
    o_col = vec4(v_col * (0.45 + 0.55 * d), 1.0);
}
";

// ── Pad descriptor (world XZ coords in mm, Y is PCB surface = 0) ──────────────

pub struct PadInfo {
    pub cx: f32,  // centre X in mm
    pub cz: f32,  // centre Z in mm (PCB Y-axis → viewer Z-axis)
    pub w:  f32,
    pub h:  f32,
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

        // ── PCB + pads VAO ───────────────────────────────────────────────────
        let pcb_data = build_pcb_and_pads(mesh.radius, mesh.xz_half, pads, drawings);
        let pcb_vao = gl.create_vertex_array().ok()?;
        let pcb_vbo = gl.create_buffer().ok()?;
        upload_verts(gl, pcb_vao, pcb_vbo, &pcb_data);

        Some(GlState {
            program,
            comp_vao, comp_vbo, comp_count: mesh.count,
            pcb_vao,  pcb_vbo,  pcb_count: (pcb_data.len() / 9) as i32,
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

        // Draw PCB (model = identity, normal = identity)
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

        // Restore GL state for egui
        gl.bind_vertex_array(None);
        gl.use_program(None);
        gl.disable(glow::DEPTH_TEST);
        gl.disable(glow::SCISSOR_TEST);
        gl.depth_mask(false);  // egui doesn't use depth writes
        gl.enable(glow::BLEND); // egui needs blending for UI
    }

    unsafe fn destroy(&self, gl: &glow::Context) {
        gl.delete_program(self.program);
        gl.delete_vertex_array(self.comp_vao);
        gl.delete_buffer(self.comp_vbo);
        gl.delete_vertex_array(self.pcb_vao);
        gl.delete_buffer(self.pcb_vbo);
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
                    .map(|p| PadInfo { cx: p.cx, cz: p.cz, w: p.w, h: p.h })
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
                self.cam_pan -= right * (mid_delta.x * world_per_px)
                              - up    * (mid_delta.y * world_per_px);
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
                "No 3D model",
                egui::FontId::proportional(13.0),
                egui::Color32::from_gray(160),
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
    let offset_world = Vec3::new(offset[0], offset[2], offset[1]);
    let t     = Mat4::from_translation(offset_world + center);
    let t_neg = Mat4::from_translation(-center);
    let r = Mat4::from_euler(
        glam::EulerRot::ZYX,
        rotation[2].to_radians(),
        rotation[1].to_radians(),
        rotation[0].to_radians(),
    );
    let s = Mat4::from_scale(Vec3::from(scale));
    t * r * s * t_neg
}

// ── PCB + pad geometry ────────────────────────────────────────────────────────

fn build_pcb_and_pads(radius: f32, mesh_xz_half: f32, pads: &[PadInfo], drawings: &[PcbDrawing]) -> Vec<f32> {
    // PCB must contain both the pad extents and the 3D body XZ footprint.
    const MARGIN: f32 = 3.0; // mm of board visible around the outermost feature
    let pad_half = if pads.is_empty() {
        0.0_f32
    } else {
        let max_x = pads.iter().map(|p| p.cx.abs() + p.w * 0.5).fold(0.0_f32, f32::max);
        let max_z = pads.iter().map(|p| p.cz.abs() + p.h * 0.5).fold(0.0_f32, f32::max);
        max_x.max(max_z)
    };
    let half = pad_half.max(mesh_xz_half).max(radius * 0.5) + MARGIN;
    let thick = 1.6_f32;
    let mut v: Vec<f32> = Vec::new();

    // top surface at Y=0 — KiCad-style PCB green
    quad_y(&mut v, -half, half, -half, half, 0.0, [0.10, 0.42, 0.12]);
    // bottom surface at Y=-thick
    quad_y(&mut v, half, -half, -half, half, -thick, [0.07, 0.30, 0.09]);
    // 4 edge faces
    let ec = [0.08, 0.32, 0.10_f32];
    // front (z = -half): (x0,y0,z0) → (x1,y0,z1) → (x1,y1,z1) → (x0,y1,z0)
    rect_xz_face(&mut v, -half, -thick, -half,  half, 0.0, -half, [0.0, 0.0, -1.0], ec);
    rect_xz_face(&mut v,  half, -thick,  half, -half, 0.0,  half, [0.0, 0.0,  1.0], ec);
    rect_xz_face(&mut v,  half, -thick, -half,  half, 0.0,  half, [1.0, 0.0,  0.0], ec);
    rect_xz_face(&mut v, -half, -thick,  half, -half, 0.0, -half, [-1.0, 0.0, 0.0], ec);

    // Pads: flat quads at Y=0.05 (above PCB to avoid z-fight)
    let py = 0.05_f32;
    let pc = [0.85, 0.68, 0.08_f32];
    for pad in pads {
        let hw = pad.w * 0.5;
        let hh = pad.h * 0.5;
        quad_y(&mut v,
            pad.cx - hw, pad.cx + hw,
            pad.cz - hh, pad.cz + hh,
            py, pc);
    }

    // Drawing shapes (silkscreen / fab): flat tris at Y=0.08, just above pads
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

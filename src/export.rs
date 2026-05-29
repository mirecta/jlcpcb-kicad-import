use crate::api::{Component, FpGraphic, Pin, SymGraphic};
use anyhow::Result;
use std::path::{Path, PathBuf};

pub struct LibPaths {
    pub sym_file: PathBuf,
    pub fp_dir: PathBuf,
    pub model_dir: PathBuf,
}

impl LibPaths {
    pub fn new(lib_path: &str, lib_name: &str) -> Self {
        let base = Path::new(lib_path);
        Self {
            sym_file: base.join(format!("{}.kicad_sym", lib_name)),
            fp_dir: base.join(format!("{}.pretty", lib_name)),
            model_dir: base.join(format!("{}.3dshapes", lib_name)),
        }
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.fp_dir)?;
        std::fs::create_dir_all(&self.model_dir)?;
        Ok(())
    }
}

pub fn write_symbol(
    paths: &LibPaths,
    component: &Component,
    lib_name: &str,
    ref_pos: [f32; 2],
    val_pos: [f32; 2],
) -> Result<()> {
    let sym = build_symbol(component, lib_name, ref_pos, val_pos);
    let name = sanitize_name(&component.value);

    if paths.sym_file.exists() {
        let existing = std::fs::read_to_string(&paths.sym_file)?;
        if existing.contains(&format!(r#"(symbol "{}""#, name)) {
            let updated = replace_symbol_in_lib(&existing, &name, &sym);
            std::fs::write(&paths.sym_file, updated)?;
        } else {
            let updated = existing.trim_end().trim_end_matches(')').to_string()
                + "\n"
                + &sym
                + "\n)\n";
            std::fs::write(&paths.sym_file, updated)?;
        }
    } else {
        let lib = format!(
            "(kicad_symbol_lib (version 20220914) (generator jlcpcb-kicad)\n{}\n)\n",
            sym
        );
        std::fs::write(&paths.sym_file, lib)?;
    }
    Ok(())
}

/// Package-based filename: package_detail if set, else package. Matches the footprint reference
/// written into the symbol so the two stay in sync.
pub fn package_name(component: &Component) -> String {
    let raw = if component.package_detail.is_empty() {
        &component.package
    } else {
        &component.package_detail
    };
    sanitize_name(raw)
}

pub fn write_footprint(
    paths: &LibPaths,
    component: &Component,
    lib_name: &str,
    model_offset: [f32; 3],
    model_rotation: [f32; 3],
    model_scale: [f32; 3],
    model_ext: &str,   // "step" or "wrl"
) -> Result<()> {
    let name = package_name(component);
    let model_path = format!(
        "${{{lib_name}_3D}}/{name}.{ext}",
        lib_name = lib_name.to_uppercase().replace('-', "_"),
        name = name,
        ext  = model_ext,
    );
    let content = build_footprint(component, &model_path, model_offset, model_rotation, model_scale);
    let fp_file = paths.fp_dir.join(format!("{}.kicad_mod", name));
    std::fs::write(&fp_file, content)?;
    Ok(())
}

pub fn write_wrl_model(paths: &LibPaths, component: &Component, wrl_bytes: &[u8]) -> Result<()> {
    let f = paths.model_dir.join(format!("{}.wrl", package_name(component)));
    std::fs::write(&f, wrl_bytes)?;
    Ok(())
}

pub fn write_step_model(paths: &LibPaths, component: &Component, step_bytes: &[u8]) -> Result<()> {
    let f = paths.model_dir.join(format!("{}.step", package_name(component)));
    std::fs::write(&f, step_bytes)?;
    Ok(())
}

pub fn write_stl_model(paths: &LibPaths, component: &Component, stl_bytes: &[u8]) -> Result<()> {
    let f = paths.model_dir.join(format!("{}.stl", package_name(component)));
    std::fs::write(&f, stl_bytes)?;
    Ok(())
}

// ── Symbol builder ────────────────────────────────────────────────────────────

fn prop(name: &str, value: &str, at_y: f32, show: bool, _hide_name: bool) -> String {
    // KiCad 7/8: `hide` goes inside (effects ...), not after (at ...)
    let hide = if !show { " hide" } else { "" };
    format!(
        r#"    (property "{name}" "{value}"
      (at 0 {at_y} 0)
      (effects (font (size 1.27 1.27)){hide})
    )"#,
        name  = esc(name),
        value = esc(value),
        at_y  = at_y,
        hide  = hide,
    )
}

fn build_symbol(c: &Component, lib_name: &str, ref_pos: [f32; 2], val_pos: [f32; 2]) -> String {
    let name = sanitize_name(&c.value);
    let footprint_ref = format!(
        "{}:{}",
        lib_name,
        if c.package_detail.is_empty() { &c.package } else { &c.package_detail }
    );

    let mut props = Vec::new();
    let mut y = (val_pos[1] - 1.27f32).min(ref_pos[1] - 1.27);

    // Reference and Value with user-adjusted positions
    props.push(format!(
        r#"    (property "Reference" "U"
      (at {} {} 0)
      (effects (font (size 1.27 1.27)))
    )"#,
        ref_pos[0], ref_pos[1]
    ));
    props.push(format!(
        r#"    (property "Value" "{}"
      (at {} {} 0)
      (effects (font (size 1.27 1.27)))
    )"#,
        esc(&c.value), val_pos[0], val_pos[1]
    ));
    y = val_pos[1] - 1.27;
    props.push(prop("Footprint", &footprint_ref, y, false, false));
    y -= 1.27;
    props.push(prop("Datasheet", &c.datasheet, y, false, false));
    y -= 1.27;

    // JLCPCB-specific mandatory fields
    let mandatory = [
        ("Description", c.description.as_str()),
        ("LCSC", c.lcsc_id.as_str()),
        ("Stock", &c.stock.to_string()),
        ("Price", c.price.as_str()),
        ("Process", c.process.as_str()),
        ("Minimum Qty", &c.min_qty.to_string()),
        ("Attrition Qty", &c.attrition_qty.to_string()),
        ("Class", c.class.as_str()),
        ("Category", c.category.as_str()),
        ("Manufacturer", c.manufacturer.as_str()),
        ("Part", c.value.as_str()),
    ];
    for (field, value) in &mandatory {
        props.push(prop(field, value, y, false, false));
        y -= 1.27;
    }

    // Electrical parameters from JLCPCB attributes
    for attr in &c.extra_attrs {
        props.push(prop(&attr.name, &attr.value, y, false, false));
        y -= 1.27;
    }

    let props_str = props.join("\n");
    let sym_name = &name;

    let body = build_symbol_body(sym_name, &c.pins, &c.sym_graphics);

    format!(
        "  (symbol \"{sym_name}\"\n    (pin_names (offset 1.016))\n    (in_bom yes) (on_board yes)\n{props_str}\n{body}\n  )"
    )
}

const PIN_LEN: f32 = 2.54;

fn build_symbol_body(sym_name: &str, pins: &[Pin], graphics: &[SymGraphic]) -> String {
    let mut out = format!("    (symbol \"{sym_name}_0_1\"\n");

    if graphics.is_empty() {
        // Auto-generate rectangle body from pin endpoints
        let (rx0, ry0, rx1, ry1) = if pins.is_empty() {
            (-5.08_f32, 1.27, 5.08, -1.27)
        } else {
            let pts: Vec<(f32, f32)> = pins.iter().map(|p| body_end(p)).collect();
            let m = 0.5_f32;
            (pts.iter().map(|(x,_)| *x).fold(f32::MAX, f32::min) - m,
             pts.iter().map(|(_,y)| *y).fold(f32::MIN, f32::max) + m,
             pts.iter().map(|(x,_)| *x).fold(f32::MIN, f32::max) + m,
             pts.iter().map(|(_,y)| *y).fold(f32::MAX, f32::min) - m)
        };
        out.push_str(&format!(
            "      (rectangle (start {:.3} {:.3}) (end {:.3} {:.3})\n        (stroke (width 0.254) (type default))\n        (fill (type background))\n      )\n",
            rx0, ry0, rx1, ry1,
        ));
    } else {
        // Emit EasyEDA-derived graphical elements
        let mut prev_arc_end: Option<[f32; 2]> = None;
        for g in graphics {
            match g {
                SymGraphic::Arc { start, mid, end, width } => {
                    // Close the small gap between consecutive arcs
                    if let Some(prev) = prev_arc_end {
                        let dx = start[0] - prev[0];
                        let dy = start[1] - prev[1];
                        if (dx*dx + dy*dy).sqrt() < 0.5 {
                            out.push_str(&format!(
                                "      (polyline (pts (xy {:.3} {:.3}) (xy {:.3} {:.3}))\n        (stroke (width {:.3}) (type default))\n        (fill (type none))\n      )\n",
                                prev[0], prev[1], start[0], start[1], width
                            ));
                        }
                    }
                    out.push_str(&format!(
                        "      (arc (start {:.3} {:.3}) (mid {:.3} {:.3}) (end {:.3} {:.3})\n        (stroke (width {:.3}) (type default))\n        (fill (type none))\n      )\n",
                        start[0], start[1], mid[0], mid[1], end[0], end[1], width
                    ));
                    prev_arc_end = Some(*end);
                }
                SymGraphic::Poly { pts, width, fill } => {
                    prev_arc_end = None;
                    let pts_str: String = pts.iter()
                        .map(|p| format!("(xy {:.3} {:.3})", p[0], p[1]))
                        .collect::<Vec<_>>().join(" ");
                    let fill_str = if *fill { "outline" } else { "none" };
                    out.push_str(&format!(
                        "      (polyline (pts {pts_str})\n        (stroke (width {:.3}) (type default))\n        (fill (type {fill_str}))\n      )\n",
                        width
                    ));
                }
                SymGraphic::Circle { cx, cy, r, width, fill } => {
                    let fill_str = if *fill { "outline" } else { "none" };
                    out.push_str(&format!(
                        "      (circle (center {:.3} {:.3}) (radius {:.3})\n        (stroke (width {:.3}) (type default))\n        (fill (type {fill_str}))\n      )\n",
                        cx, cy, r, width
                    ));
                }
                SymGraphic::Rect { x0, y0, x1, y1, width, fill } => {
                    let fill_str = if *fill { "background" } else { "none" };
                    out.push_str(&format!(
                        "      (rectangle (start {:.3} {:.3}) (end {:.3} {:.3})\n        (stroke (width {:.3}) (type default))\n        (fill (type {fill_str}))\n      )\n",
                        x0, y0, x1, y1, width
                    ));
                }
            }
        }
    }

    for pin in pins {
        out.push_str(&format!(
            "      (pin {ptype} line\n        (at {x:.3} {y:.3} {angle})\n        (length {PIN_LEN:.3})\n        (name \"{name}\" (effects (font (size 1.27 1.27))))\n        (number \"{num}\" (effects (font (size 1.27 1.27))))\n      )\n",
            ptype  = pin.pin_type,
            x      = pin.x,
            y      = pin.y,
            angle  = pin.angle,
            name   = esc_pin(&pin.name),
            num    = esc_pin(&pin.number),
        ));
    }

    out.push_str("    )");
    out
}

fn body_end(p: &Pin) -> (f32, f32) {
    match p.angle {
        0   => (p.x + PIN_LEN, p.y),
        90  => (p.x,           p.y + PIN_LEN),
        180 => (p.x - PIN_LEN, p.y),
        _   => (p.x,           p.y - PIN_LEN),
    }
}

fn esc_pin(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// ── Footprint builder ─────────────────────────────────────────────────────────

fn build_footprint(
    c: &Component,
    model_path: &str,
    offset: [f32; 3],
    rotation: [f32; 3],
    scale: [f32; 3],
) -> String {
    let name = package_name(c);

    let mut pads = String::new();
    for pad in &c.pads {
        let rot_field = if pad.rotation.abs() > 0.01 {
            format!(" {:.4}", pad.rotation)
        } else {
            String::new()
        };

        if pad.drill > 0.0 {
            pads.push_str(&format!(
                "  (pad \"{}\" thru_hole {} (at {:.4} {:.4}{}) (size {:.4} {:.4}) (drill {:.4}) (layers \"*.Cu\" \"*.Mask\"))\n",
                esc_pad(&pad.number), pad.shape,
                pad.cx, pad.cy, rot_field,
                pad.w, pad.h, pad.drill,
            ));
        } else {
            pads.push_str(&format!(
                "  (pad \"{}\" smd {} (at {:.4} {:.4}{}) (size {:.4} {:.4}) (layers \"F.Cu\" \"F.Paste\" \"F.Mask\"))\n",
                esc_pad(&pad.number), pad.shape,
                pad.cx, pad.cy, rot_field,
                pad.w, pad.h,
            ));
        }
    }

    let mut graphics = String::new();
    for g in &c.fp_graphics {
        match g {
            FpGraphic::Line { x1, y1, x2, y2, width, layer } => {
                graphics.push_str(&format!(
                    "  (fp_line (start {x1:.4} {y1:.4}) (end {x2:.4} {y2:.4}) (layer \"{layer}\") (width {width:.4}))\n"
                ));
            }
            FpGraphic::Circle { cx, cy, r, width, layer } => {
                graphics.push_str(&format!(
                    "  (fp_circle (center {cx:.4} {cy:.4}) (end {ex:.4} {cy:.4}) (layer \"{layer}\") (width {width:.4}) (fill none))\n",
                    ex = cx + r
                ));
            }
            FpGraphic::Poly { pts, width, layer, fill } => {
                let pts_str: String = pts.iter()
                    .map(|p| format!(" (xy {:.4} {:.4})", p[0], p[1]))
                    .collect::<Vec<_>>().join("");
                let fill_str = if *fill { "solid" } else { "none" };
                graphics.push_str(&format!(
                    "  (fp_poly (pts{pts_str}) (layer \"{layer}\") (width {width:.4}) (fill {fill_str}))\n"
                ));
            }
        }
    }

    format!(
        r#"(footprint "{name}"
  (version 20221018) (generator jlcpcb-kicad)
  (layer "F.Cu")
  (descr "{desc}")
  (property "Reference" "REF**" (at 0 -3 0) (layer "F.SilkS")
    (effects (font (size 1 1) (thickness 0.15))))
  (property "Value" "{name}" (at 0 3 0) (layer "F.Fab")
    (effects (font (size 1 1) (thickness 0.15))))
  (property "LCSC" "{lcsc}" (at 0 0 0) (layer "F.Fab") (hide yes)
    (effects (font (size 1.27 1.27))))
{pads}{graphics}  (model "{model}"
    (offset (xyz {ox} {oy} {oz}))
    (scale (xyz {sx} {sy} {sz}))
    (rotate (xyz {rx} {ry} {rz}))
  )
)
"#,
        name = name,
        desc = esc(&c.description),
        lcsc = c.lcsc_id,
        pads = pads,
        graphics = graphics,
        model = model_path,
        ox = offset[0], oy = offset[2], oz = offset[1],
        sx = scale[0], sy = scale[1], sz = scale[2],
        rx = rotation[0], ry = rotation[1], rz = rotation[2],
    )
}

fn esc_pad(s: &str) -> String {
    s.replace('"', "\\\"")
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn replace_symbol_in_lib(lib: &str, name: &str, new_sym: &str) -> String {
    let marker = format!(r#"  (symbol "{}""#, name);
    if let Some(start) = lib.find(&marker) {
        let tail = &lib[start..];
        let mut depth = 0usize;
        let mut end = start;
        for (i, ch) in tail.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    if depth == 0 { break; }
                    depth -= 1;
                    if depth == 0 {
                        end = start + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        format!("{}{}{}", &lib[..start], new_sym, &lib[end..])
    } else {
        lib.to_string()
    }
}

pub fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

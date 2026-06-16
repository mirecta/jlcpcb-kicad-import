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
    hide_pin_numbers: bool,
    hide_pin_names: bool,
) -> Result<()> {
    let sym = build_symbol(component, lib_name, ref_pos, val_pos, hide_pin_numbers, hide_pin_names);
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

// ── Symbol builder ────────────────────────────────────────────────────────────

fn prop(name: &str, value: &str, at_y: f32, show: bool, _hide_name: bool) -> String {
    // KiCad 8+: (hide yes) is a sibling of (effects), not inside it
    let hide_line = if !show { "\n      (hide yes)" } else { "" };
    format!(
        r#"    (property "{name}" "{value}"
      (at 0 {at_y} 0)
      (effects (font (size 1.27 1.27))){hide_line}
    )"#,
        name      = esc(name),
        value     = esc(value),
        at_y      = at_y,
        hide_line = hide_line,
    )
}

/// Extract the electrical value (capacitance, resistance, inductance) from component attributes.
/// Falls back to the component's part name for ICs and other types without a simple value.
pub fn electrical_value(c: &Component) -> String {
    let cat = c.category.to_lowercase();
    let find = |key: &str| -> Option<&str> {
        c.extra_attrs.iter()
            .find(|a| a.name.eq_ignore_ascii_case(key))
            .map(|a| a.value.as_str())
            .filter(|v| !v.is_empty())
    };

    if cat.contains("capacitor") {
        match (find("Capacitance"), find("Voltage Rating")) {
            (Some(cap), Some(v)) => return format!("{cap}/{v}"),
            (Some(cap), None)    => return cap.to_string(),
            _ => {}
        }
    } else if cat.contains("resistor") {
        if let Some(v) = find("Resistance") { return v.to_string(); }
    } else if cat.contains("inductor") || cat.contains("ferrite") || cat.contains("choke") {
        if let Some(v) = find("Inductance") { return v.to_string(); }
    } else if cat.contains("crystal") || cat.contains("oscillator") || cat.contains("resonator") {
        if let Some(v) = find("Frequency") { return v.to_string(); }
    }
    c.value.clone()
}

pub fn ref_letter(category: &str) -> &'static str {
    let cat = category.to_lowercase();
    if cat.contains("capacitor") { return "C"; }
    if cat.contains("resistor")  { return "R"; }
    if cat.contains("inductor") || cat.contains("ferrite") || cat.contains("choke") { return "L"; }
    if cat.contains("transistor") || cat.contains("mosfet") || cat.contains("bjt") { return "Q"; }
    if cat.contains("diode") || cat.contains("rectifier") || cat.contains("zener") || cat.contains("tvs") { return "D"; }
    if cat.contains("led") || cat.contains("light emitting") { return "D"; }
    if cat.contains("crystal") || cat.contains("oscillator") || cat.contains("resonator") { return "Y"; }
    if cat.contains("fuse") { return "F"; }
    if cat.contains("switch") || cat.contains("button") || cat.contains("relay") { return "SW"; }
    if cat.contains("connector") || cat.contains("socket") || cat.contains("header") { return "J"; }
    if cat.contains("transformer") { return "T"; }
    if cat.contains("optocoupler") || cat.contains("opto") { return "U"; }
    "U"
}

pub fn build_symbol(c: &Component, lib_name: &str, ref_pos: [f32; 2], val_pos: [f32; 2], hide_pin_numbers: bool, hide_pin_names: bool) -> String {
    let name = sanitize_name(&c.value);
    let footprint_ref = format!("{}:{}", lib_name, package_name(c));

    let mut props = Vec::new();
    let mut y = (val_pos[1] - 1.27f32).min(ref_pos[1] - 1.27);

    // Reference and Value with user-adjusted positions
    props.push(format!(
        r#"    (property "Reference" "{}"
      (at {} {} 0)
      (effects (font (size 1.27 1.27)))
    )"#,
        ref_letter(&c.category), ref_pos[0], ref_pos[1]
    ));
    props.push(format!(
        r#"    (property "Value" "{}"
      (at {} {} 0)
      (effects (font (size 1.27 1.27)))
    )"#,
        esc(&electrical_value(c)), val_pos[0], val_pos[1]
    ));
    y = val_pos[1] - 1.27;
    props.push(prop("Footprint", &footprint_ref, y, false, false));
    y -= 1.27;
    props.push(prop("Part Name", &c.value, y, false, false));
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

    let pin_num_clause = if hide_pin_numbers {
        "\n    (pin_numbers\n      (hide yes)\n    )"
    } else { "" };
    let is_passive = {
        let cat = c.category.to_lowercase();
        cat.contains("capacitor") || cat.contains("resistor")
            || cat.contains("inductor") || cat.contains("ferrite") || cat.contains("choke")
            || cat.contains("crystal") || cat.contains("oscillator") || cat.contains("resonator")
    };
    let pin_names_clause = if is_passive || hide_pin_names {
        "(pin_names (offset 1.016) (hide yes))"
    } else {
        "(pin_names (offset 1.016))"
    };
    format!(
        "  (symbol \"{sym_name}\"{pin_num_clause}\n    {pin_names_clause}\n    (in_bom yes) (on_board yes)\n{props_str}\n{body}\n  )"
    )
}

const PIN_LEN: f32 = 2.54;

fn build_symbol_body(sym_name: &str, pins: &[Pin], graphics: &[SymGraphic]) -> String {
    let mut out = format!("    (symbol \"{sym_name}_0_1\"\n");

    for g in graphics {
        match g {
            SymGraphic::Arc { start, mid, end, width } => {
                out.push_str(&format!(
                    "      (arc (start {:.3} {:.3}) (mid {:.3} {:.3}) (end {:.3} {:.3})\n        (stroke (width {:.3}) (type default))\n        (fill (type none))\n      )\n",
                    start[0], start[1], mid[0], mid[1], end[0], end[1], width
                ));
            }
            SymGraphic::Poly { pts, width, fill } => {
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

    for pin in pins {
        let pin_len = pin.stub_len;
        out.push_str(&format!(
            "      (pin {ptype} line\n        (at {x:.3} {y:.3} {angle})\n        (length {pin_len:.3})\n        (name \"{name}\" (effects (font (size 1.27 1.27))))\n        (number \"{num}\" (effects (font (size 1.27 1.27))))\n      )\n",
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

    // Compute bounding box from pads + SilkS/Fab graphics to place labels outside the footprint
    let mut all_ys: Vec<f32> = c.pads.iter()
        .flat_map(|p| [p.cy - p.h * 0.5, p.cy + p.h * 0.5])
        .collect();
    for g in &c.fp_graphics {
        let silkfab = match g {
            FpGraphic::Line   { layer, .. } => layer.contains("SilkS") || layer.contains("Fab"),
            FpGraphic::Circle { layer, .. } => layer.contains("SilkS") || layer.contains("Fab"),
            FpGraphic::Poly   { layer, .. } => layer.contains("SilkS") || layer.contains("Fab"),
            FpGraphic::Arc    { layer, .. } => layer.contains("SilkS") || layer.contains("Fab"),
        };
        if !silkfab { continue; }
        match g {
            FpGraphic::Line   { y1, y2, .. } => { all_ys.push(*y1); all_ys.push(*y2); }
            FpGraphic::Circle { cy, r, .. }  => { all_ys.push(cy - r); all_ys.push(cy + r); }
            FpGraphic::Poly   { pts, .. }    => all_ys.extend(pts.iter().map(|p| p[1])),
            FpGraphic::Arc    { start, mid, end, .. } => {
                all_ys.push(start[1]); all_ys.push(mid[1]); all_ys.push(end[1]);
            }
        }
    }
    let (body_min_y, body_max_y) = if all_ys.is_empty() {
        (-2.0_f32, 2.0_f32)
    } else {
        (all_ys.iter().cloned().fold(f32::INFINITY, f32::min),
         all_ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max))
    };
    let pad_mid_y = {
        let pad_ys: Vec<f32> = c.pads.iter().map(|p| p.cy).collect();
        if pad_ys.is_empty() { 0.0 }
        else { pad_ys.iter().sum::<f32>() / pad_ys.len() as f32 }
    };
    let ref_y  = body_min_y - 2.0;   // 2mm above full body outline
    let val_y  = body_max_y + 2.0;   // 2mm below full body outline
    let fab_y  = pad_mid_y;          // centroid of pads (for ${REFERENCE} marker)

    let mut pads = String::new();
    for pad in &c.pads {
        let rot_field = if pad.rotation.abs() > 0.01 {
            format!(" {:.4}", pad.rotation)
        } else {
            String::new()
        };

        if pad.npth {
            // Non-plated through hole
            pads.push_str(&format!(
                "  (pad \"\" np_thru_hole circle (at {:.4} {:.4}{}) (size {:.4} {:.4}) (drill {:.4}) (layers \"*.Cu\" \"*.Mask\"))\n",
                pad.cx, pad.cy, rot_field, pad.w, pad.h, pad.drill,
            ));
            continue;
        }

        if pad.drill > 0.0 && pad.shape == "polygon" && pad.poly_pts.len() >= 3 {
            // THT custom polygon pad — same as Python SHAPE_CUSTOM with gr_poly + drill
            let hw = pad.w * 0.5;
            let hh = pad.h * 0.5;
            let drill_field = if pad.drill_slot > 0.0 {
                let (sw, sh) = if hw >= hh { (pad.drill_slot, pad.drill) } else { (pad.drill, pad.drill_slot) };
                format!("oval {:.4} {:.4}", sw, sh)
            } else {
                format!("{:.4}", pad.drill)
            };
            let pts_str: String = pad.poly_pts.iter()
                .map(|p| format!("        (xy {:.4} {:.4})", p[0] - pad.cx, p[1] - pad.cy))
                .collect::<Vec<_>>().join("\n");
            pads.push_str(&format!(
                "  (pad \"{}\" thru_hole custom (at {:.4} {:.4}{}) (size 0.1 0.1) (drill {}) (layers \"*.Cu\" \"*.Mask\")\n    (primitives\n      (gr_poly (pts\n{}\n      ) (width 0) (fill yes))\n    )\n  )\n",
                esc_pad(&pad.number), pad.cx, pad.cy, rot_field, drill_field, pts_str,
            ));
        } else if pad.drill > 0.0 {
            let hw = pad.w * 0.5;
            let hh = pad.h * 0.5;
            let drill_field = if pad.drill_slot > 0.0 {
                let minor = pad.drill;
                let major = pad.drill_slot;
                let (sw, sh) = if hw >= hh { (major, minor) } else { (minor, major) };
                format!("oval {:.4} {:.4}", sw, sh)
            } else if pad.shape == "oval" && (hw - hh).abs() > 0.01 {
                let slot_minor = pad.drill;
                let slot_major = if hw <= hh { pad.drill * hh / hw } else { pad.drill * hw / hh };
                let (sw, sh) = if hw <= hh { (slot_minor, slot_major) } else { (slot_major, slot_minor) };
                format!("oval {:.4} {:.4}", sw, sh)
            } else {
                format!("{:.4}", pad.drill)
            };
            pads.push_str(&format!(
                "  (pad \"{}\" thru_hole {} (at {:.4} {:.4}{}) (size {:.4} {:.4}) (drill {}) (layers \"*.Cu\" \"*.Mask\"))\n",
                esc_pad(&pad.number), pad.shape,
                pad.cx, pad.cy, rot_field,
                pad.w, pad.h, drill_field,
            ));
        } else if pad.shape == "polygon" && pad.poly_pts.len() >= 3 {
            // SMD custom polygon pad — Python SHAPE_CUSTOM, size 0.1×0.1
            let pts_str: String = pad.poly_pts.iter()
                .map(|p| format!("        (xy {:.4} {:.4})", p[0] - pad.cx, p[1] - pad.cy))
                .collect::<Vec<_>>().join("\n");
            pads.push_str(&format!(
                "  (pad \"{}\" smd custom (at {:.4} {:.4}{}) (size 0.1 0.1)\n    (layers \"F.Cu\" \"F.Paste\" \"F.Mask\")\n    (primitives\n      (gr_poly (pts\n{}\n      ) (width 0) (fill yes))\n    )\n  )\n",
                esc_pad(&pad.number), pad.cx, pad.cy, rot_field, pts_str,
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
                // Only copper layers need explicit (fill solid); paste/fab/comments/courtyard
                // use KiCad's layer-default behavior — omitting fill matches JLC2KiCadLib output
                let fill_part = if *fill { " (fill solid)" } else { "" };
                graphics.push_str(&format!(
                    "  (fp_poly (pts{pts_str}) (layer \"{layer}\") (width {width:.4}){fill_part})\n"
                ));
            }
            FpGraphic::Arc { start, mid, end, width, layer } => {
                graphics.push_str(&format!(
                    "  (fp_arc (start {:.4} {:.4}) (mid {:.4} {:.4}) (end {:.4} {:.4}) (layer \"{layer}\") (width {width:.4}))\n",
                    start[0], start[1], mid[0], mid[1], end[0], end[1]
                ));
            }
        }
    }

    format!(
        r#"(footprint "{name}"
  (version 20221018) (generator jlcpcb-kicad)
  (layer "F.Cu")
  (descr "{desc}")
  (fp_text reference REF** (at 0 {ref_y:.6}) (layer "F.SilkS")
    (effects (font (size 1 1) (thickness 0.15))))
  (fp_text value {name} (at 0 {val_y:.6}) (layer "F.Fab")
    (effects (font (size 1 1) (thickness 0.15))))
  (fp_text user "${{REFERENCE}}" (at 0 {fab_y:.6}) (layer "F.Fab")
    (effects (font (size 1 1) (thickness 0.15))))
{pads}{graphics}  (model "{model}"
    (offset (xyz {ox} {oy} {oz}))
    (scale (xyz {sx} {sy} {sz}))
    (rotate (xyz {rx} {ry} {rz}))
  )
)
"#,
        name = name,
        desc = esc(&c.description),
        ref_y = ref_y,
        val_y = val_y,
        fab_y = fab_y,
        pads = pads,
        graphics = graphics,
        model = model_path,
        ox = offset[0], oy = offset[1], oz = offset[2],
        sx = scale[0], sy = scale[1], sz = scale[2],
        rx = rotation[0], ry = rotation[1], rz = rotation[2],
    )
}

fn esc_pad(s: &str) -> String {
    s.replace('"', "\\\"")
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn replace_symbol_in_lib(lib: &str, name: &str, new_sym: &str) -> String {
    // Search without leading-whitespace assumption so both tab and space indent work
    let search = format!(r#"(symbol "{}""#, name);
    if let Some(sym_offset) = lib.find(&search) {
        // Walk back to the start of this line (include leading indent)
        let line_start = lib[..sym_offset].rfind('\n').map_or(0, |i| i + 1);
        // Depth-scan from the `(` to find the matching closing `)`
        let tail = &lib[sym_offset..];
        let mut depth = 0usize;
        let mut sym_end = sym_offset;
        for (i, ch) in tail.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    if depth == 0 { break; }
                    depth -= 1;
                    if depth == 0 { sym_end = sym_offset + i + 1; break; }
                }
                _ => {}
            }
        }
        format!("{}{}{}", &lib[..line_start], new_sym, &lib[sym_end..])
    } else {
        lib.to_string()
    }
}

/// Extract (ref_pos, val_pos) from an existing .kicad_sym library string for a given LCSC ID.
/// Returns None if the symbol or positions can't be found.
pub fn extract_label_positions(lib: &str, lcsc_id: &str) -> Option<([f32;2],[f32;2])> {
    // Find the symbol block that contains this LCSC ID
    let lcsc_marker = format!("(property \"LCSC\" \"{}\"", lcsc_id);
    let lcsc_pos = lib.find(&lcsc_marker)?;
    // Walk back to the start of the enclosing symbol block
    let before = &lib[..lcsc_pos];
    let sym_start = before.rfind("(symbol \"")?;
    // Find the end of this symbol block by paren matching
    let tail = &lib[sym_start..];
    let mut depth = 0usize;
    let mut sym_end = sym_start;
    for (i, ch) in tail.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => { depth -= 1; if depth == 0 { sym_end = sym_start + i + 1; break; } }
            _ => {}
        }
    }
    let sym_block = &lib[sym_start..sym_end];

    let parse_at = |prop: &str| -> Option<[f32;2]> {
        let marker = format!("(property \"{}\"", prop);
        let p = sym_block.find(&marker)?;
        let at_p = sym_block[p..].find("(at ")?;
        let after = &sym_block[p + at_p + 4..];
        let end = after.find(')')?;
        let nums: Vec<f32> = after[..end].split_whitespace()
            .filter_map(|s| s.parse().ok()).collect();
        if nums.len() >= 2 { Some([nums[0], nums[1]]) } else { None }
    };

    let ref_pos = parse_at("Reference")?;
    let val_pos = parse_at("Value")?;
    Some((ref_pos, val_pos))
}

pub fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

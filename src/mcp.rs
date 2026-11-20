//! MCP (Model Context Protocol) server: `jlcpcb-kicad --mcp`.
//!
//! Speaks JSON-RPC 2.0 over stdio (one JSON message per line) and exposes the
//! app's features as tools: search JLCPCB, inspect parts, browse the local
//! library, import / refresh parts, edit and render 3D model placement, and
//! download datasheets. Uses the same settings file as the GUI.

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use crate::api::{self, Component};
use crate::export::{self, LibPaths};
use crate::library::{self, LibEntry, ModelTransform};
use crate::{model3d, ops, preview, settings};

const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "\
JLCPCB → KiCad library manager. Typical flow: search_parts → get_part → import_part.
The target library comes from the app settings (get_library_config / set_library_config);
every tool also accepts lib_path + lib_name to override it for one call. The footprint
library may have a different name/folder than the symbol library (fp_lib_name, fp_dir,
model_dir); by default it has the same name.
3D placement: render_3d renders exactly like the app's 3D viewer (same camera presets as its
buttons: iso, top, bottom, front, back, left, right; red/green/blue axes are X/Y/Z). Preview changes with render_3d's
offset/rotation/scale arguments, then persist with set_3d_transform. Rotation values are
the same numbers KiCad shows in footprint properties (degrees); offset in mm.
JLCPCB data is fetched over the network and rate-limited — avoid bulk loops.";

struct Server {
    out: Box<dyn Write>,
    /// Fetched components + model bytes (wrl, step), to avoid re-downloading.
    cache: HashMap<String, (Component, Option<Vec<u8>>, Option<Vec<u8>>, Option<String>)>,
}

pub fn run() -> Result<()> {
    let out = protocol_stdout();
    let mut srv = Server { out, cache: HashMap::new() };
    eprintln!("[mcp] jlcpcb-kicad MCP server ready");
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() { continue; }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                srv.send(&json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":format!("parse error: {e}")}}));
                continue;
            }
        };
        let msgs = if let Value::Array(a) = msg { a } else { vec![msg] };
        for m in msgs {
            if let Some(resp) = srv.handle(&m) {
                srv.send(&resp);
            }
        }
    }
    Ok(())
}

/// Keep the real stdout for the protocol and point fd 1 at stderr, so stray
/// prints from libraries (e.g. OpenCASCADE) can't corrupt the JSON stream.
fn protocol_stdout() -> Box<dyn Write> {
    #[cfg(unix)]
    unsafe {
        use std::os::unix::io::FromRawFd;
        let fd = libc::dup(1);
        if fd >= 0 && libc::dup2(2, 1) >= 0 {
            return Box::new(std::fs::File::from_raw_fd(fd));
        }
    }
    Box::new(std::io::stdout())
}

impl Server {
    fn send(&mut self, v: &Value) {
        let _ = writeln!(self.out, "{}", v);
        let _ = self.out.flush();
    }

    fn handle(&mut self, m: &Value) -> Option<Value> {
        let id = m.get("id").cloned();
        let method = m.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = m.get("params").cloned().unwrap_or(json!({}));
        let id = id?; // notifications get no response
        let result: Result<Value, (i64, String)> = match method {
            "initialize" => {
                let req = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("");
                let ver = if PROTOCOL_VERSIONS.contains(&req) { req } else { PROTOCOL_VERSIONS[0] };
                Ok(json!({
                    "protocolVersion": ver,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "jlcpcb-kicad", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tool_list()})),
            "tools/call" => {
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                eprintln!("[mcp] call {name} {args}");
                Ok(match self.call(&name, &args) {
                    Ok(content) => json!({"content": content}),
                    Err(e) => json!({"content": [text(&format!("Error: {e:#}"))], "isError": true}),
                })
            }
            "resources/list" => Ok(json!({"resources": []})),
            "prompts/list" => Ok(json!({"prompts": []})),
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        Some(match result {
            Ok(r) => json!({"jsonrpc":"2.0","id":id,"result":r}),
            Err((code, msg)) => json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":msg}}),
        })
    }

    fn call(&mut self, name: &str, a: &Value) -> Result<Vec<Value>> {
        match name {
            "get_library_config" => self.get_library_config(a),
            "set_library_config" => self.set_library_config(a),
            "search_parts" => self.search_parts(a),
            "get_part" => self.get_part(a),
            "list_library" => self.list_library(a),
            "import_part" => self.import_part(a),
            "refresh_parts" => self.refresh_parts(a),
            "get_3d_transform" => self.get_3d_transform(a),
            "set_3d_transform" => self.set_3d_transform(a),
            "render_3d" => self.render_3d(a),
            "render_part" => self.render_part(a),
            "save_datasheet" => self.save_datasheet(a),
            _ => bail!("unknown tool: {name}"),
        }
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    fn fetch(&mut self, lcsc: &str, with_models: bool) -> Result<&(Component, Option<Vec<u8>>, Option<Vec<u8>>, Option<String>)> {
        let lcsc = normalize_lcsc(lcsc)?;
        let need_models = with_models
            && self.cache.get(&lcsc).map_or(true, |c| c.1.is_none() && c.2.is_none());
        if !self.cache.contains_key(&lcsc) || need_models {
            let (comp, warn) = ops::fetch_component(&lcsc)?;
            let (wrl, step) = if with_models { ops::download_models(&comp) } else { (None, None) };
            self.cache.insert(lcsc.clone(), (comp, wrl, step, warn));
        }
        Ok(&self.cache[&lcsc])
    }

    // ── tools ────────────────────────────────────────────────────────────────

    fn get_library_config(&mut self, a: &Value) -> Result<Vec<Value>> {
        let paths = lib_target(a);
        let (lib_path, lib_name) = (paths.lib_path.to_string_lossy().to_string(), paths.sym_name.clone());
        let scan = settings::scan_library_dir(&lib_path);
        let mut warnings = Vec::new();
        if let Some((dir, name)) = &scan.suggested_parent {
            warnings.push(format!("path is inside library '{name}' — library folder is probably {dir}"));
        }
        if !scan.exists { warnings.push("folder does not exist yet (created on first import)".into()); }
        if scan.exists && !scan.libs.iter().any(|l| l.name == lib_name) {
            warnings.push(format!("no library named '{lib_name}' in this folder — import would create a new one"));
        }
        for (dir, names) in &scan.child_dirs {
            warnings.push(format!("libraries found in sub-folder {dir}: {}", names.join(", ")));
        }
        let layout = layout_info(&paths);
        Ok(vec![json_text(&json!({
            "lib_path": lib_path,
            "lib_name": lib_name,
            "symbol_file": paths.sym_file,
            "footprint_library": {
                "name": paths.fp_nick,
                "folder": paths.fp_dir,
                "model_dir": paths.model_dir,
                "model_path_in_footprint": paths.model_ref("<name>", "step"),
                "kicad_path_variable": paths.model_var(),
            },
            "detected_footprint_library": library::detect_layout(&lib_path, &lib_name).map(|d| json!({
                "name": d.nick, "folder": d.fp_dir, "model_dir": d.model_dir, "symbols": d.symbols
            })),
            "libraries_in_folder": scan.libs.iter().map(|l| json!({
                "name": l.name, "symbols": l.has_sym, "footprints": l.has_fp, "models_3d": l.has_3d
            })).collect::<Vec<_>>(),
            "footprint_libraries_used": layout,
            "datasheet_dir": settings::load().datasheet_dir,
            "warnings": warnings,
        }))])
    }

    fn set_library_config(&mut self, a: &Value) -> Result<Vec<Value>> {
        let mut s = settings::load_effective();
        apply_lib_args(&mut s, a);
        if let Some(d) = a.get("datasheet_dir").and_then(|v| v.as_str()) { s.datasheet_dir = d.trim().to_string(); }
        if s.lib_name.is_empty() { bail!("library name is empty"); }
        settings::save(&s)?;
        self.get_library_config(&json!({}))
    }

    fn search_parts(&mut self, a: &Value) -> Result<Vec<Value>> {
        let q = req_str(a, "query")?;
        let basic = a.get("basic_only").and_then(|v| v.as_bool()).unwrap_or(false);
        let limit = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(20).clamp(1, 100) as usize;
        let res = api::search_components(q, limit.max(10), basic)?;
        let rows: Vec<Value> = res.iter().take(limit).map(|r| json!({
            "lcsc": r.lcsc_id, "part": r.value, "manufacturer": r.manufacturer,
            "package": r.package, "category": r.category, "description": r.description,
            "stock": r.stock, "price_usd": r.price, "min_qty": r.min_qty, "class": r.class,
        })).collect();
        Ok(vec![json_text(&json!({"count": rows.len(), "results": rows}))])
    }

    fn get_part(&mut self, a: &Value) -> Result<Vec<Value>> {
        let lcsc = req_str(a, "lcsc")?.to_string();
        let paths = lib_target(a);
        let (c, _, _, warn) = self.fetch(&lcsc, false)?.clone();
        let in_lib = library::load(&paths).ok()
            .and_then(|l| l.entries.into_iter().find(|e| e.lcsc == c.lcsc_id));
        Ok(vec![json_text(&json!({
            "lcsc": c.lcsc_id, "part": c.value, "value": export::electrical_value(&c),
            "manufacturer": c.manufacturer, "package": c.package, "package_detail": c.package_detail,
            "category": c.category, "description": c.description, "datasheet": c.datasheet,
            "stock": c.stock, "price": c.price, "min_qty": c.min_qty, "class": c.class, "process": c.process,
            "attributes": c.extra_attrs.iter().map(|x| json!({"name": x.name, "value": x.value})).collect::<Vec<_>>(),
            "pins": c.pins.iter().map(|p| json!({"number": p.number, "name": p.name, "type": p.pin_type})).collect::<Vec<_>>(),
            "pad_count": c.pads.len(),
            "has_3d_model": {"wrl": c.wrl_url.is_some(), "step": c.step_url.is_some()},
            "footprint_name_on_import": export::package_name(&c),
            "symbol_name_on_import": export::sanitize_name(&c.value),
            "warning": warn,
            "in_library": in_lib.map(|e| entry_json(&e)),
        }))])
    }

    fn list_library(&mut self, a: &Value) -> Result<Vec<Value>> {
        let paths = lib_target(a);
        let lib = library::load(&paths)?;
        if !lib.sym_exists {
            bail!("symbol library {} does not exist", paths.clone().sym_file.display());
        }
        let f = a.get("filter").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
        let only_problems = a.get("only_problems").and_then(|v| v.as_bool()).unwrap_or(false);
        let entries: Vec<Value> = lib.entries.iter()
            .filter(|e| f.is_empty() || [&e.name, &e.value, &e.lcsc, &e.footprint, &e.description, &e.manufacturer, &e.category]
                .iter().any(|s| s.to_lowercase().contains(&f)))
            .filter(|e| !only_problems || e.fp_file.is_none() || (!e.has_step && !e.has_wrl))
            .map(entry_json)
            .collect();
        Ok(vec![json_text(&json!({
            "library": paths.clone().sym_file,
            "project_dir": lib.project_dir,
            "total": lib.entries.len(),
            "shown": entries.len(),
            "missing_footprint": lib.entries.iter().filter(|e| e.fp_file.is_none() && e.fp_mismatch.is_none()).count(),
            "broken_footprint_links": lib.entries.iter().filter(|e| e.fp_mismatch.is_some()).count(),
            "missing_3d": lib.entries.iter().filter(|e| !e.has_step && !e.has_wrl).count(),
            "unused_footprints": lib.orphan_footprints,
            "entries": entries,
        }))])
    }

    fn import_part(&mut self, a: &Value) -> Result<Vec<Value>> {
        let lcsc = req_str(a, "lcsc")?.to_string();
        let paths = lib_target(a);
        let b = |k: &str, d: bool| a.get(k).and_then(|v| v.as_bool()).unwrap_or(d);
        let (c, wrl, step, warn) = self.fetch(&lcsc, true)?.clone();
        let cat = c.category.to_lowercase();
        let passive = ["capacitor", "resistor", "inductor", "ferrite", "choke", "crystal", "oscillator", "resonator"]
            .iter().any(|k| cat.contains(k));
        let opts = ops::ImportOptions {
            symbol: b("symbol", true),
            footprint: b("footprint", true),
            model_3d: b("model_3d", true),
            hide_pin_numbers: b("hide_pin_numbers", false),
            hide_pin_names: b("hide_pin_names", passive),
            transform: transform_arg(a, ModelTransform::default())?,
        };
        ops::import_component(&paths, &c, wrl.as_deref(), step.as_deref(), &opts)?;
        let paths = paths.clone();
        let fp = export::find_fp_by_lcsc(&paths.fp_dir, &c.lcsc_id);
        Ok(vec![json_text(&json!({
            "imported": c.lcsc_id, "part": c.value,
            "symbol": opts.symbol.then(|| paths.sym_file.clone()),
            "footprint": if opts.footprint { fp } else { None },
            "model_3d": opts.model_3d.then(|| json!({"wrl": wrl.is_some(), "step": step.is_some(), "dir": paths.model_dir})),
            "transform": transform_json(&opts.transform),
            "warning": warn,
            "next": "Check placement with render_3d (source=library); adjust with set_3d_transform.",
        }))])
    }

    fn refresh_parts(&mut self, a: &Value) -> Result<Vec<Value>> {
        let paths = lib_target(a);
        let ids: Vec<String> = a.get("lcsc").and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("'lcsc' must be an array of LCSC ids"))?
            .iter().filter_map(|v| v.as_str().map(String::from)).collect();
        if ids.is_empty() { bail!("no LCSC ids given"); }
        if ids.len() > 10 { bail!("at most 10 parts per call (JLCPCB rate limits)"); }
        let b = |k: &str, d: bool| a.get(k).and_then(|v| v.as_bool()).unwrap_or(d);
        let opts = ops::RefreshOptions {
            symbols: b("symbol", true),
            footprints: b("footprint", true),
            models_3d: b("model_3d", true),
            keep_3d_settings: b("keep_3d_settings", true),
        };
        let paths = paths.clone();
        let mut sym = std::fs::read_to_string(&paths.sym_file)
            .map_err(|e| anyhow!("cannot read {}: {e}", paths.sym_file.display()))?;
        let mut written = HashSet::new();
        let mut report = Vec::new();
        for (i, id) in ids.iter().enumerate() {
            if i > 0 { std::thread::sleep(std::time::Duration::from_secs(3)); }
            let id = normalize_lcsc(id)?;
            self.cache.remove(&id);
            let r = ops::fetch_component(&id).and_then(|(c, _)| {
                ops::refresh_component(&paths, &c, &opts, b("hide_pin_numbers", false), b("hide_pin_names", false), &mut sym, &mut written)?;
                if opts.symbols { std::fs::write(&paths.sym_file, &sym)?; }
                Ok(c.value)
            });
            report.push(match r {
                Ok(v) => json!({"lcsc": id, "ok": true, "part": v}),
                Err(e) => json!({"lcsc": id, "ok": false, "error": e.to_string()}),
            });
        }
        Ok(vec![json_text(&json!({"results": report}))])
    }

    fn get_3d_transform(&mut self, a: &Value) -> Result<Vec<Value>> {
        let e = find_entry(a)?;
        let fp = entry_fp_file(&e)?;
        let t = std::fs::read_to_string(&fp).ok().and_then(|s| library::read_model_transform(&s))
            .ok_or_else(|| anyhow!("footprint {} has no 3D model entry", fp.display()))?;
        Ok(vec![json_text(&json!({
            "lcsc": e.lcsc, "footprint_file": fp, "model": e.model_ref, "model_file": e.model_file,
            "transform": transform_json(&t),
        }))])
    }

    fn set_3d_transform(&mut self, a: &Value) -> Result<Vec<Value>> {
        let e = find_entry(a)?;
        let fp = entry_fp_file(&e)?;
        let old = std::fs::read_to_string(&fp).ok().and_then(|s| library::read_model_transform(&s))
            .ok_or_else(|| anyhow!("footprint {} has no 3D model entry", fp.display()))?;
        let relative = a.get("mode").and_then(|v| v.as_str()) == Some("relative");
        let new = if relative {
            let d = transform_arg(a, ModelTransform { offset: [0.0; 3], rotation: [0.0; 3], scale: [1.0; 3] })?;
            ModelTransform {
                offset: add3(old.offset, d.offset),
                rotation: add3(old.rotation, d.rotation).map(norm_angle),
                scale: [old.scale[0] * d.scale[0], old.scale[1] * d.scale[1], old.scale[2] * d.scale[2]],
            }
        } else {
            let mut t = transform_arg(a, old)?;
            t.rotation = t.rotation.map(norm_angle);
            t
        };
        library::save_model_transform(&fp, &new)?;
        let mut out = vec![json_text(&json!({
            "saved": fp, "lcsc": e.lcsc, "before": transform_json(&old), "after": transform_json(&new),
        }))];
        if let Some(v) = a.get("render_view").and_then(|v| v.as_str()) {
            let mut ra = a.clone();
            ra["views"] = json!([v]);
            ra["source"] = json!("library");
            if let Some(o) = ra.as_object_mut() { o.remove("offset"); o.remove("rotation"); o.remove("scale"); }
            out.extend(self.render_3d(&ra)?);
        }
        Ok(out)
    }

    fn render_3d(&mut self, a: &Value) -> Result<Vec<Value>> {
        let lcsc = req_str(a, "lcsc")?.to_string();
        let paths = lib_target(a);
        let lib_name = paths.sym_name.clone();
        let entry = library::load(&paths).ok()
            .and_then(|l| l.entries.into_iter().find(|e| e.lcsc.eq_ignore_ascii_case(&lcsc)));
        let source = a.get("source").and_then(|v| v.as_str())
            .unwrap_or(if entry.is_some() { "library" } else { "jlcpcb" }).to_string();
        let views: Vec<String> = a.get("views").and_then(|v| v.as_array())
            .map(|v| v.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_else(|| vec!["iso".into(), "front".into()]);
        // Default = the app's 3D viewer size (560 × 360)
        let width = a.get("width").and_then(|v| v.as_u64()).unwrap_or(560).clamp(128, 1600) as u32;
        let height = a.get("height").and_then(|v| v.as_u64()).unwrap_or(360).clamp(128, 1200) as u32;
        let ortho = a.get("ortho").and_then(|v| v.as_bool()).unwrap_or(true);

        let (comp, wrl, step, _) = self.fetch(&lcsc, source == "jlcpcb")?.clone();
        let (mut pads, mut drawings) = ops::pads_3d(&comp);
        let mut pad_src = "JLCPCB footprint";
        if source == "library" {
            if let Some(lp) = entry.as_ref().and_then(|e| entry_fp_file(e).ok()).and_then(|f| ops::local_pads_3d(&f)) {
                pads = lp;
                drawings.clear();
                pad_src = "library footprint pads";
            }
        }

        let (model_bytes, is_wrl, base, desc): (Option<Vec<u8>>, bool, ModelTransform, String) = if source == "library" {
            let e = entry.ok_or_else(|| anyhow!("{lcsc} is not in library {lib_name} — use source=jlcpcb"))?;
            let fp = entry_fp_file(&e)?;
            let t = std::fs::read_to_string(&fp).ok().and_then(|s| library::read_model_transform(&s)).unwrap_or_default();
            let mf = e.model_file.clone();
            let bytes = mf.as_ref().and_then(|p| std::fs::read(p).ok());
            let wrl = mf.as_ref().map_or(false, |p| p.extension().map_or(false, |x| x.eq_ignore_ascii_case("wrl")));
            (bytes, wrl, t, format!("library model {}", mf.map(|p| p.display().to_string()).unwrap_or("(not found)".into())))
        } else if let Some(s) = step {
            (Some(s), false, ModelTransform::default(), "JLCPCB STEP model".into())
        } else if let Some(w) = wrl {
            (Some(w), true, ModelTransform::default(), "JLCPCB WRL model".into())
        } else {
            (None, false, ModelTransform::default(), "no 3D model available".into())
        };
        let t = transform_arg(a, base)?;

        let mut out = vec![text(&format!(
            "{} ({}) — {} on {} — offset {:?} mm, rotation {:?}°, scale {:?}{}. Same camera and views as the app's 3D viewer buttons. Axes from the footprint origin: X red, Y green, Z blue (PCB top = +Z).",
            comp.value, comp.lcsc_id, desc, pad_src, r4(t.offset), r4(t.rotation), r4(t.scale),
            if t != base { " (PREVIEW, not saved)" } else { "" }
        ))];
        for v in &views {
            let (yaw, pitch) = model3d::view_angles(v).ok_or_else(|| anyhow!("unknown view '{v}'"))?;
            let (img, has_model) = model3d::render_offscreen(
                model_bytes.as_deref().map(|b| (b, is_wrl)), &pads, &drawings,
                t.offset, t.rotation, t.scale, yaw, pitch, ortho, width, height);
            if !has_model && v == &views[0] {
                out.push(text("⚠ model could not be loaded — showing footprint only"));
            }
            out.push(text(&format!("view: {v}")));
            out.push(image_png(&img)?);
        }
        Ok(out)
    }

    fn render_part(&mut self, a: &Value) -> Result<Vec<Value>> {
        let lcsc = req_str(a, "lcsc")?.to_string();
        let what = a.get("what").and_then(|v| v.as_str()).unwrap_or("both").to_string();
        let (c, _, _, _) = self.fetch(&lcsc, false)?.clone();
        let mut out = Vec::new();
        if what == "symbol" || what == "both" {
            match c.symbol_svg.as_deref().map(|s| preview::svg_to_image(s, 800, 600)) {
                Some(Ok(ci)) => {
                    let img = image::RgbaImage::from_raw(ci.size[0] as u32, ci.size[1] as u32,
                        ci.pixels.iter().flat_map(|p| p.to_array()).collect()).ok_or_else(|| anyhow!("bad image"))?;
                    out.push(text("symbol (JLCPCB/EasyEDA preview)"));
                    out.push(image_png(&img)?);
                }
                _ => out.push(text("no symbol preview available")),
            }
        }
        if what == "footprint" || what == "both" {
            let (pw, ph) = (800u32, 600u32);
            let (mut x0, mut x1, mut y0, mut y1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
            for p in &c.pads {
                let r = p.w.max(p.h) * 0.5;
                x0 = x0.min(p.cx - r); x1 = x1.max(p.cx + r); y0 = y0.min(p.cy - r); y1 = y1.max(p.cy + r);
            }
            if x0 > x1 { x0 = -5.0; x1 = 5.0; y0 = -5.0; y1 = 5.0; }
            let (x0, x1, y0, y1) = (x0 - 2.0, x1 + 2.0, y0 - 2.0, y1 + 2.0);
            let scale = (pw as f32 / (x1 - x0)).min(ph as f32 / (y1 - y0)) * 0.85;
            let pix = crate::render_fp_pixmap(&c.pads, &c.fp_graphics, pw, ph, scale,
                (x0 + x1) / 2.0, (y0 + y1) / 2.0, pw as f32 / 2.0, ph as f32 / 2.0)
                .ok_or_else(|| anyhow!("footprint render failed"))?;
            let png = pix.encode_png().map_err(|e| anyhow!("{e}"))?;
            out.push(text(&format!("footprint (top view, {} pads; pads: {})", c.pads.len(),
                c.pads.iter().take(200).map(|p| format!("{}@({:.2},{:.2})", p.number, p.cx, p.cy)).collect::<Vec<_>>().join(" "))));
            out.push(json!({"type": "image", "mimeType": "image/png", "data": base64(&png)}));
        }
        Ok(out)
    }

    fn save_datasheet(&mut self, a: &Value) -> Result<Vec<Value>> {
        let lcsc = req_str(a, "lcsc")?.to_string();
        let (c, _, _, _) = self.fetch(&lcsc, false)?.clone();
        let fname = format!("{}_{}.pdf", export::sanitize_name(&c.value), c.lcsc_id);
        let path = match a.get("path").and_then(|v| v.as_str()) {
            Some(p) if Path::new(p).is_dir() => Path::new(p).join(&fname),
            Some(p) => PathBuf::from(p),
            None => {
                let s = settings::load();
                let dir = if Path::new(&s.datasheet_dir).is_dir() { PathBuf::from(&s.datasheet_dir) }
                    else { dirs::download_dir().ok_or_else(|| anyhow!("no download folder — give 'path'"))? };
                dir.join(&fname)
            }
        };
        let bytes = api::download_datasheet(&c.datasheet)?;
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
        std::fs::write(&path, &bytes)?;
        Ok(vec![json_text(&json!({"saved": path, "bytes": bytes.len(), "source": c.datasheet}))])
    }
}

// ── argument / result helpers ────────────────────────────────────────────────

fn text(s: &str) -> Value { json!({"type": "text", "text": s}) }
fn json_text(v: &Value) -> Value { text(&serde_json::to_string_pretty(v).unwrap_or_default()) }

fn image_png(img: &image::RgbaImage) -> Result<Value> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png)?;
    Ok(json!({"type": "image", "mimeType": "image/png", "data": base64(buf.get_ref())}))
}

fn req_str<'a>(a: &'a Value, k: &str) -> Result<&'a str> {
    a.get(k).and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow!("missing required argument '{k}'"))
}

fn normalize_lcsc(s: &str) -> Result<String> {
    let s = s.trim().to_uppercase();
    let s = if s.chars().all(|c| c.is_ascii_digit()) { format!("C{s}") } else { s };
    if !(s.starts_with('C') && s.len() > 1 && s[1..].chars().all(|c| c.is_ascii_digit())) {
        bail!("'{s}' is not an LCSC id (expected e.g. C7512)");
    }
    Ok(s)
}

/// Library targeted by a call: app settings, with per-call overrides.
/// Overriding lib_path/lib_name auto-detects that library's footprint layout
/// (like picking it in the GUI); fp_lib_name / fp_dir / model_dir override it.
fn lib_target(a: &Value) -> LibPaths {
    let mut s = settings::load_effective();
    apply_lib_args(&mut s, a);
    s.paths()
}

fn apply_lib_args(s: &mut settings::Settings, a: &Value) {
    let st = |k: &str| a.get(k).and_then(|v| v.as_str()).map(|v| v.trim().to_string());
    let mut relocated = false;
    if let Some(p) = st("pick") {
        let (dir, name) = settings::resolve_pick(Path::new(&p));
        s.lib_path = dir.to_string_lossy().to_string();
        if let Some(n) = name { s.lib_name = n; }
        relocated = true;
    }
    if let Some(p) = st("lib_path") { s.lib_path = p; relocated = true; }
    if let Some(n) = st("lib_name") { s.lib_name = n; relocated = true; }
    if relocated {
        s.apply_layout(library::detect_layout(&s.lib_path, &s.lib_name).as_ref());
    }
    if a.get("same_as_symbol_library").and_then(|v| v.as_bool()) == Some(true) {
        s.apply_layout(None);
    }
    if let Some(v) = st("fp_lib_name") { s.fp_lib_name = v; }
    if let Some(v) = st("fp_dir") { s.fp_dir = v; }
    if let Some(v) = st("model_dir") { s.model_dir = v; }
}

/// Footprint libraries referenced by the symbols, with the folder they resolve to.
fn layout_info(paths: &LibPaths) -> Vec<Value> {
    let Ok(l) = library::load(paths) else { return vec![] };
    let mut seen: Vec<(String, Option<PathBuf>, usize)> = Vec::new();
    for e in &l.entries {
        let nick = e.footprint.split_once(':').map(|(n, _)| n.to_string()).unwrap_or_default();
        match seen.iter_mut().find(|s| s.0 == nick) {
            Some(s) => s.2 += 1,
            None => seen.push((nick, e.fp_lib_dir.clone(), 1)),
        }
    }
    seen.into_iter().map(|(n, d, c)| json!({"nickname": n, "folder": d, "symbols": c})).collect()
}

fn find_entry(a: &Value) -> Result<LibEntry> {
    let paths = lib_target(a);
        let lib_name = paths.sym_name.clone();
    let l = library::load(&paths)?;
    if let Some(id) = a.get("lcsc").and_then(|v| v.as_str()) {
        let id = normalize_lcsc(id)?;
        return l.entries.into_iter().find(|e| e.lcsc == id)
            .ok_or_else(|| anyhow!("{id} not found in library {lib_name}"));
    }
    if let Some(sym) = a.get("symbol").and_then(|v| v.as_str()) {
        return l.entries.into_iter().find(|e| e.name == sym)
            .ok_or_else(|| anyhow!("symbol '{sym}' not found in library {lib_name}"));
    }
    bail!("give 'lcsc' or 'symbol'")
}

fn entry_fp_file(e: &LibEntry) -> Result<PathBuf> {
    e.fp_file.clone()
        .or_else(|| Some(e.fp_lib_dir.as_ref()?.join(format!("{}.kicad_mod", e.fp_mismatch.as_ref()?))))
        .ok_or_else(|| anyhow!("footprint '{}' of {} not found", e.footprint, e.name))
}

fn entry_json(e: &LibEntry) -> Value {
    json!({
        "lcsc": e.lcsc, "symbol": e.name, "value": e.value, "footprint": e.footprint,
        "footprint_status": if e.fp_file.is_some() { "ok" }
            else if e.fp_mismatch.is_some() { "broken_link" }
            else if e.fp_lib_dir.is_none() { "unknown_library" } else { "missing" },
        "footprint_file": e.fp_file,
        "actual_footprint_file": e.fp_mismatch,
        "model_3d": match (e.has_step, e.has_wrl) { (true, true) => "step+wrl", (true, false) => "step", (false, true) => "wrl", _ => "none" },
        "model_file": e.model_file,
        "description": e.description, "manufacturer": e.manufacturer,
    })
}

fn vec3_arg(a: &Value, k: &str) -> Result<Option<[f32; 3]>> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(v)) if v.len() == 3 => {
            let mut o = [0.0f32; 3];
            for (i, x) in v.iter().enumerate() {
                o[i] = x.as_f64().ok_or_else(|| anyhow!("'{k}' must contain numbers"))? as f32;
            }
            Ok(Some(o))
        }
        Some(Value::Number(n)) if k == "scale" => { let s = n.as_f64().unwrap_or(1.0) as f32; Ok(Some([s; 3])) }
        _ => bail!("'{k}' must be [x, y, z]"),
    }
}

fn transform_arg(a: &Value, base: ModelTransform) -> Result<ModelTransform> {
    Ok(ModelTransform {
        offset: vec3_arg(a, "offset")?.unwrap_or(base.offset),
        rotation: vec3_arg(a, "rotation")?.unwrap_or(base.rotation),
        scale: vec3_arg(a, "scale")?.unwrap_or(base.scale),
    })
}

fn r4(v: [f32; 3]) -> [f64; 3] {
    v.map(|x| { let r = (x as f64 * 10000.0).round() / 10000.0; if r == 0.0 { 0.0 } else { r } })
}

fn transform_json(t: &ModelTransform) -> Value {
    json!({"offset_mm": r4(t.offset), "rotation_deg": r4(t.rotation), "scale": r4(t.scale)})
}

fn add3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] { [a[0] + b[0], a[1] + b[1], a[2] + b[2]] }

fn norm_angle(v: f32) -> f32 {
    let mut r = v % 360.0;
    if r > 180.0 { r -= 360.0; }
    if r <= -180.0 { r += 360.0; }
    if r == -0.0 { 0.0 } else { r }
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity((data.len() + 2) / 3 * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

// ── tool definitions ─────────────────────────────────────────────────────────

fn tool_list() -> Value {
    let lib = json!({
        "lib_path": {"type": "string", "description": "Library folder (default: app settings)"},
        "lib_name": {"type": "string", "description": "Symbol library name (default: app settings)"},
        "fp_lib_name": {"type": "string", "description": "Footprint library name if different from the symbol library (default: settings, or auto-detected when lib_path/lib_name is given)"},
        "fp_dir": {"type": "string", "description": "Footprint library folder (default: <lib_path>/<fp_lib_name>.pretty)"},
        "model_dir": {"type": "string", "description": "3D model folder (default: <lib_path>/<fp_lib_name>.3dshapes)"},
    });
    let vec3 = |d: &str| json!({"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3, "description": d});
    let with_lib = |mut props: Value| { for (k, v) in lib.as_object().unwrap() { props[k] = v.clone(); } props };
    let transform = |mut props: Value| {
        props["offset"] = vec3("3D model offset [x, y, z] in mm");
        props["rotation"] = vec3("3D model rotation [x, y, z] in degrees (as in KiCad footprint properties)");
        props["scale"] = vec3("3D model scale [x, y, z]");
        props
    };
    let lcsc = json!({"type": "string", "description": "LCSC part number, e.g. C7512"});
    let ro = json!({"readOnlyHint": true, "openWorldHint": true});
    let tools = vec![
        json!({"name": "get_library_config", "title": "Get library configuration",
            "description": "Show the target KiCad library (folder + name), libraries detected in that folder, which footprint libraries its symbols use, and warnings (wrong folder, typo in name…).",
            "inputSchema": {"type": "object", "properties": with_lib(json!({}))},
            "annotations": {"readOnlyHint": true}}),
        json!({"name": "set_library_config", "title": "Set library configuration",
            "description": "Change the target library saved in the app settings (shared with the GUI). Symbol and footprint libraries may differ in name and folder (default: same). 'pick' accepts any path related to a library — a .kicad_sym file, a .pretty/.3dshapes folder or the folder containing them — and detects folder + name automatically.",
            "inputSchema": {"type": "object", "properties": {
                "pick": {"type": "string", "description": "Path to a .kicad_sym / .pretty / library folder — auto-detects lib_path, lib_name and the footprint library"},
                "lib_path": {"type": "string"}, "lib_name": {"type": "string"},
                "fp_lib_name": {"type": "string", "description": "Footprint library name (nickname used in symbols)"},
                "fp_dir": {"type": "string", "description": "Footprint library folder"},
                "model_dir": {"type": "string", "description": "3D model folder"},
                "same_as_symbol_library": {"type": "boolean", "description": "Reset footprints/3D to the default: same name and folder as the symbol library"},
                "datasheet_dir": {"type": "string", "description": "Default folder for save_datasheet"}}}}),
        json!({"name": "search_parts", "title": "Search JLCPCB parts",
            "description": "Search the JLCPCB catalog by keyword, value, MPN or LCSC id (e.g. 'ULN2003', '100nF 0402', 'C7512').",
            "inputSchema": {"type": "object", "required": ["query"], "properties": {
                "query": {"type": "string"},
                "basic_only": {"type": "boolean", "description": "Only JLCPCB Basic parts (no extra assembly fee)"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 20}}},
            "annotations": ro}),
        json!({"name": "get_part", "title": "Get part details",
            "description": "Full details of a JLCPCB part: attributes, pins, pad count, 3D model availability, datasheet URL, and whether it is already in the library.",
            "inputSchema": {"type": "object", "required": ["lcsc"], "properties": with_lib(json!({"lcsc": lcsc}))},
            "annotations": ro}),
        json!({"name": "render_part", "title": "Render symbol / footprint",
            "description": "Images of a JLCPCB part's schematic symbol and/or PCB footprint (top view) as they will be imported.",
            "inputSchema": {"type": "object", "required": ["lcsc"], "properties": {
                "lcsc": lcsc, "what": {"type": "string", "enum": ["symbol", "footprint", "both"], "default": "both"}}},
            "annotations": ro}),
        json!({"name": "list_library", "title": "List library contents",
            "description": "Components in the local KiCad library with footprint and 3D model status (footprint libraries resolved via KiCad fp-lib-table), plus unused footprints.",
            "inputSchema": {"type": "object", "properties": with_lib(json!({
                "filter": {"type": "string", "description": "Substring match on name, value, LCSC, footprint, manufacturer, description"},
                "only_problems": {"type": "boolean", "description": "Only entries with missing footprint or 3D model"}}))},
            "annotations": {"readOnlyHint": true}}),
        json!({"name": "import_part", "title": "Import part into library",
            "description": "Download a JLCPCB part and write its symbol into the symbol library, its footprint into the footprint library and 3D models into the 3D folder (see get_library_config). Existing symbol/footprint for the same part is replaced. Optional initial 3D transform.",
            "inputSchema": {"type": "object", "required": ["lcsc"], "properties": with_lib(transform(json!({
                "lcsc": lcsc,
                "symbol": {"type": "boolean", "default": true},
                "footprint": {"type": "boolean", "default": true},
                "model_3d": {"type": "boolean", "default": true},
                "hide_pin_numbers": {"type": "boolean", "default": false},
                "hide_pin_names": {"type": "boolean", "description": "Default: true for passives/crystals"}})))},
            "annotations": {"destructiveHint": true, "openWorldHint": true}}),
        json!({"name": "refresh_parts", "title": "Refresh parts from JLCPCB",
            "description": "Regenerate already-imported parts from fresh JLCPCB data (max 10 per call). Keeps the 3D transform by default.",
            "inputSchema": {"type": "object", "required": ["lcsc"], "properties": with_lib(json!({
                "lcsc": {"type": "array", "items": {"type": "string"}, "maxItems": 10},
                "symbol": {"type": "boolean", "default": true},
                "footprint": {"type": "boolean", "default": true},
                "model_3d": {"type": "boolean", "default": true},
                "keep_3d_settings": {"type": "boolean", "default": true},
                "hide_pin_numbers": {"type": "boolean", "default": false},
                "hide_pin_names": {"type": "boolean", "default": false}}))},
            "annotations": {"destructiveHint": true, "openWorldHint": true}}),
        json!({"name": "get_3d_transform", "title": "Get 3D model transform",
            "description": "Read the 3D model offset / rotation / scale stored in a library footprint.",
            "inputSchema": {"type": "object", "properties": with_lib(json!({
                "lcsc": lcsc, "symbol": {"type": "string", "description": "Symbol name (for parts without LCSC id)"}}))},
            "annotations": {"readOnlyHint": true}}),
        json!({"name": "set_3d_transform", "title": "Set 3D model transform",
            "description": "Write the 3D model offset / rotation / scale into a library footprint (only the (model …) block changes). mode=relative adds offset/rotation and multiplies scale. Optionally returns a render.",
            "inputSchema": {"type": "object", "properties": with_lib(transform(json!({
                "lcsc": lcsc, "symbol": {"type": "string"},
                "mode": {"type": "string", "enum": ["absolute", "relative"], "default": "absolute"},
                "render_view": {"type": "string", "enum": ["iso", "top", "bottom", "front", "back", "left", "right"], "description": "Return a render after saving"}})))},
            "annotations": {"destructiveHint": false, "idempotentHint": true}}),
        json!({"name": "render_3d", "title": "Render 3D model on footprint",
            "description": "Render the 3D model on its footprint/PCB exactly like the app's 3D viewer (same camera, view buttons, lighting, axes). source=library uses the model file and transform from the library footprint; source=jlcpcb uses the downloaded model. offset/rotation/scale preview a transform without saving.",
            "inputSchema": {"type": "object", "required": ["lcsc"], "properties": with_lib(transform(json!({
                "lcsc": lcsc,
                "source": {"type": "string", "enum": ["library", "jlcpcb"], "description": "Default: library if the part is in it"},
                "views": {"type": "array", "items": {"type": "string", "enum": ["iso", "top", "bottom", "front", "back", "left", "right"]}, "default": ["iso", "front"]},
                "width": {"type": "integer", "minimum": 128, "maximum": 1600, "default": 560},
                "height": {"type": "integer", "minimum": 128, "maximum": 1200, "default": 360},
                "ortho": {"type": "boolean", "default": true, "description": "Orthographic like the app's 'Ortho' checkbox"}})))},
            "annotations": ro}),
        json!({"name": "save_datasheet", "title": "Save datasheet PDF",
            "description": "Download the part's datasheet PDF (resolves LCSC's viewer page to the real PDF). Default folder: datasheet_dir from settings or Downloads.",
            "inputSchema": {"type": "object", "required": ["lcsc"], "properties": {
                "lcsc": lcsc, "path": {"type": "string", "description": "Target file or folder"}}},
            "annotations": {"openWorldHint": true}}),
    ];
    Value::Array(tools)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_reference() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn angles_and_ids() {
        assert_eq!(norm_angle(270.0), -90.0);
        assert_eq!(norm_angle(-180.0), 180.0);
        assert_eq!(normalize_lcsc(" c7512 ").unwrap(), "C7512");
        assert_eq!(normalize_lcsc("7512").unwrap(), "C7512");
        assert!(normalize_lcsc("ULN2003").is_err());
    }

    #[test]
    fn protocol_basics() {
        let mut s = Server { out: Box::new(std::io::sink()), cache: HashMap::new() };
        let r = s.handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}})).unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert!(s.handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})).is_none());
        let r = s.handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).unwrap();
        assert!(r["result"]["tools"].as_array().unwrap().len() >= 12);
        let r = s.handle(&json!({"jsonrpc":"2.0","id":3,"method":"nope"})).unwrap();
        assert_eq!(r["error"]["code"], -32601);
        let r = s.handle(&json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_part","arguments":{}}})).unwrap();
        assert_eq!(r["result"]["isError"], true);
    }
}

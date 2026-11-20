//! Library operations shared by the GUI and the MCP server.

use anyhow::Result;
use std::collections::HashSet;
use std::path::PathBuf;

use crate::api::{self, Component};
use crate::export::{self, LibPaths};
use crate::library::{self, ModelTransform};
use crate::model3d;

/// Pads / board drawings in the 3D viewer's coordinate system (KiCad Y-down → Y-up).
pub fn pads_3d(comp: &Component) -> (Vec<model3d::PadInfo>, Vec<model3d::PcbDrawing>) {
    let pads = comp.pads.iter()
        .map(|p| model3d::PadInfo {
            cx: p.cx, cy: -p.cy,
            w: p.w, h: p.h,
            shape: p.shape.clone(),
            drill: p.drill,
            drill_slot: p.drill_slot,
            npth: p.npth,
            rotation: -p.rotation,
            poly_pts: p.poly_pts.iter().map(|pt| [pt[0], -pt[1]]).collect(),
        })
        .collect();
    let drawings = comp.fp_drawings.iter()
        .map(|d| model3d::PcbDrawing {
            tris: d.tris.iter().map(|pt| [pt[0], -pt[1]]).collect(),
            color: d.color,
        })
        .collect();
    (pads, drawings)
}

/// Pads of a local library footprint in viewer coordinates (no board graphics).
pub fn local_pads_3d(fp_file: &std::path::Path) -> Option<Vec<model3d::PadInfo>> {
    let text = std::fs::read_to_string(fp_file).ok()?;
    let pads: Vec<model3d::PadInfo> = library::footprint_pads(&text).into_iter()
        .map(|p| model3d::PadInfo {
            cx: p.x, cy: -p.y,
            w: p.w, h: p.h,
            shape: p.shape,
            drill: p.drill,
            drill_slot: p.drill_slot,
            npth: p.npth,
            rotation: -p.rotation,
            poly_pts: Vec::new(),
        })
        .collect();
    (!pads.is_empty()).then_some(pads)
}

#[derive(Clone, Copy, Debug)]
pub struct ImportOptions {
    pub symbol: bool,
    pub footprint: bool,
    pub model_3d: bool,
    pub hide_pin_numbers: bool,
    pub hide_pin_names: bool,
    pub transform: ModelTransform,
}

/// Write symbol / footprint / 3D model of `comp` into the library.
pub fn import_component(
    paths: &LibPaths,
    comp: &Component,
    wrl: Option<&[u8]>,
    step: Option<&[u8]>,
    o: &ImportOptions,
) -> Result<()> {
    // STEP preferred over WRL
    let model_ext = if step.is_some() { "step" } else if wrl.is_some() { "wrl" } else { "step" };
    paths.ensure_dirs()?;
    if o.symbol {
        export::write_symbol(paths, comp, o.hide_pin_numbers, o.hide_pin_names)?;
    }
    if o.footprint {
        let t = &o.transform;
        export::write_footprint(paths, comp, t.offset, t.rotation, t.scale, model_ext)?;
    }
    if o.model_3d {
        if let Some(s) = step { export::write_step_model(paths, comp, s)?; }
        if let Some(w) = wrl { export::write_wrl_model(paths, comp, w)?; }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
pub struct RefreshOptions {
    pub symbols: bool,
    pub footprints: bool,
    pub models_3d: bool,
    pub keep_3d_settings: bool,
}

/// Regenerate one already-imported component from fresh JLCPCB data.
/// `sym_text` is the symbol library content (updated in place, caller writes it);
/// `written_fps` avoids rewriting a footprint shared by several components.
pub fn refresh_component(
    paths: &LibPaths,
    comp: &Component,
    o: &RefreshOptions,
    hide_pin_numbers: bool,
    hide_pin_names: bool,
    sym_text: &mut String,
    written_fps: &mut HashSet<PathBuf>,
) -> Result<()> {
    if o.symbols {
        *sym_text = update_component_in_lib(sym_text, comp, paths, hide_pin_numbers, hide_pin_names);
    }
    if o.footprints {
        // Locate the existing file by LCSC id — package_name() may change between API calls
        let fp_path = export::find_fp_by_lcsc(&paths.fp_dir, &comp.lcsc_id)
            .unwrap_or_else(|| paths.fp_dir.join(format!("{}.kicad_mod", export::package_name(comp))));
        let t = if o.keep_3d_settings {
            std::fs::read_to_string(&fp_path).ok()
                .and_then(|s| library::read_model_transform(&s))
                .unwrap_or_default()
        } else {
            ModelTransform::default()
        };
        let stem = fp_path.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let model_ext = if paths.model_dir.join(format!("{stem}.wrl")).exists() { "wrl" } else { "step" };
        if !written_fps.contains(&fp_path) {
            let model_path = paths.model_ref(&stem, model_ext);
            let content = export::build_footprint_content(comp, &stem, &model_path, t.offset, t.rotation, t.scale);
            std::fs::write(&fp_path, content)?;
            written_fps.insert(fp_path);
        }
    }
    if o.models_3d {
        if let Some(wrl) = comp.wrl_url.as_deref().and_then(api::download_wrl) {
            let _ = export::write_wrl_model(paths, comp, &wrl);
        }
        if let Some(step) = comp.step_url.as_deref().and_then(|u| api::download_bytes(u).ok()) {
            let _ = export::write_step_model(paths, comp, &step);
        }
    }
    Ok(())
}

pub fn update_component_in_lib(
    content: &str,
    comp: &Component,
    paths: &LibPaths,
    hide_pin_numbers: bool,
    hide_pin_names: bool,
) -> String {
    let name = export::sanitize_name(&comp.value);
    let fp_name = export::find_fp_by_lcsc(&paths.fp_dir, &comp.lcsc_id)
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| export::package_name(comp));
    let new_sym = export::build_symbol(comp, &paths.fp_nick, &fp_name, hide_pin_numbers, hide_pin_names);
    export::replace_symbol_in_lib(content, &name, &new_sym)
}

/// Fetch a component, retrying once if the API returned incomplete data.
pub fn fetch_component(lcsc_id: &str) -> Result<(Component, Option<String>)> {
    let mut comp = api::fetch_component(lcsc_id)?;
    if comp.pins.is_empty() || comp.pads.is_empty() {
        std::thread::sleep(std::time::Duration::from_secs(2));
        if let Ok(retry) = api::fetch_component(lcsc_id) {
            if !retry.pins.is_empty() || !retry.pads.is_empty() {
                comp = retry;
            }
        }
    }
    let warn = match (comp.pins.is_empty(), comp.pads.is_empty()) {
        (true, true) => Some("⚠ No pin data and no footprint data from API".into()),
        (true, false) => Some("⚠ No pin data from API".into()),
        (false, true) => Some("⚠ No footprint data from API".into()),
        _ => None,
    };
    Ok((comp, warn))
}

/// Download both 3D model formats (if available).
pub fn download_models(comp: &Component) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let wrl = comp.wrl_url.as_deref().and_then(api::download_wrl);
    let step = comp.step_url.as_deref().and_then(|u| api::download_bytes(u).ok());
    (wrl, step)
}

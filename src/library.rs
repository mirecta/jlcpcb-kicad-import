//! Reading the contents of a local KiCad library for the library browser tab.
//!
//! Footprint libraries are resolved the same way KiCad does it — through the
//! project's `fp-lib-table` (found by walking up from the library folder) and
//! the global `~/.config/kicad/<ver>/fp-lib-table` — so libraries whose
//! footprints live under a different nickname or folder are handled too.

use anyhow::{bail, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::export::LibPaths;

/// One symbol from `<lib>.kicad_sym` plus the state of its footprint / 3D files.
#[derive(Debug, Clone, Default)]
pub struct LibEntry {
    pub name: String,
    pub value: String,
    pub lcsc: String,
    pub footprint: String,      // full "lib:fp" reference
    pub description: String,
    pub manufacturer: String,
    pub category: String,
    /// Resolved folder of the footprint library nickname (None = nickname unknown).
    pub fp_lib_dir: Option<PathBuf>,
    pub fp_file: Option<PathBuf>,
    /// Footprint reference points to a different library nickname.
    pub fp_foreign_lib: bool,
    /// Footprint file found by LCSC id, but its name differs from the symbol's
    /// Footprint reference (broken link in KiCad — fix with Refresh Library).
    pub fp_mismatch: Option<String>,
    /// First (model …) path written in the footprint.
    pub model_ref: String,
    pub has_step: bool,
    pub has_wrl: bool,
    /// Resolved local 3D model file (STEP preferred over WRL).
    pub model_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct LibContents {
    pub entries: Vec<LibEntry>,
    /// .kicad_mod files (in the libraries used by this symbol lib) not referenced by any symbol.
    pub orphan_footprints: Vec<String>,
    pub sym_exists: bool,
    /// Project folder (where fp-lib-table was found), used for ${KIPRJMOD}.
    pub project_dir: Option<PathBuf>,
}

fn mtime(p: &Path) -> u128 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Change-detection key: symbol file + mtimes of the related folders.
pub fn cache_key(lib_path: &str, lib_name: &str) -> String {
    let p = LibPaths::new(lib_path, lib_name);
    let proj = find_project_dir(Path::new(lib_path));
    let proj_mt = proj.as_ref().map(|d| mtime(&d.join("fp-lib-table"))).unwrap_or(0);
    format!("{}|{}|{}|{}|{}", p.sym_file.display(), mtime(&p.sym_file), mtime(&p.fp_dir), mtime(&p.model_dir), proj_mt)
}

/// Walk up from `start` to the first folder containing `fp-lib-table` or a `.kicad_pro`.
pub fn find_project_dir(start: &Path) -> Option<PathBuf> {
    start.ancestors().find(|d| {
        d.join("fp-lib-table").is_file()
            || std::fs::read_dir(d).map_or(false, |rd| {
                rd.flatten().any(|e| e.path().extension().map_or(false, |x| x == "kicad_pro"))
            })
    }).map(|d| d.to_path_buf())
}

/// Highest-version KiCad config folder (`~/.config/kicad/10.0`, …).
fn kicad_config_dir() -> Option<PathBuf> {
    let base = dirs::config_dir()?.join("kicad");
    let mut vers: Vec<(f32, PathBuf)> = std::fs::read_dir(&base).ok()?
        .flatten()
        .filter_map(|e| {
            let v: f32 = e.file_name().to_string_lossy().parse().ok()?;
            Some((v, e.path()))
        })
        .collect();
    vers.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    vers.pop().map(|(_, p)| p)
}

/// Path variables: KiCad's kicad_common.json, then process env, then KIPRJMOD.
fn path_vars(project_dir: Option<&Path>) -> HashMap<String, String> {
    let mut vars: HashMap<String, String> = HashMap::new();
    if let Some(cfg) = kicad_config_dir() {
        if let Ok(t) = std::fs::read_to_string(cfg.join("kicad_common.json")) {
            if let Ok(j) = serde_json::from_str::<serde_json::Value>(&t) {
                if let Some(obj) = j.pointer("/environment/vars").and_then(|v| v.as_object()) {
                    for (k, v) in obj {
                        if let Some(s) = v.as_str() { vars.insert(k.clone(), s.to_string()); }
                    }
                }
            }
        }
    }
    for (k, v) in std::env::vars() {
        if k.starts_with("KICAD") || k.ends_with("_3D") || k.ends_with("_DIR") {
            vars.entry(k).or_insert(v);
        }
    }
    if let Some(p) = project_dir {
        vars.insert("KIPRJMOD".into(), p.to_string_lossy().to_string());
    }
    vars
}

/// Expand `${VAR}` / `$(VAR)`; None if a variable is unknown.
fn expand(s: &str, vars: &HashMap<String, String>) -> Option<String> {
    let mut out = String::new();
    let mut rest = s;
    loop {
        let Some(i) = rest.find('$') else { out.push_str(rest); return Some(out) };
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let close = match after.chars().next() { Some('{') => '}', Some('(') => ')', _ => {
            out.push('$'); rest = after; continue;
        }};
        let end = after.find(close)?;
        out.push_str(vars.get(&after[1..end])?);
        rest = &after[end + 1..];
    }
}

/// nickname → folder, from a fp-lib-table file.
fn read_fp_lib_table(file: &Path, vars: &HashMap<String, String>, into: &mut HashMap<String, PathBuf>) {
    let Ok(t) = std::fs::read_to_string(file) else { return };
    let Ok(root) = parse_root(&t, "fp_lib_table") else { return };
    for lib in root.children("lib") {
        let field = |h: &str| lib.children(h).next().map(|c| c.arg(0).to_string()).unwrap_or_default();
        let (name, uri) = (field("name"), field("uri"));
        if name.is_empty() || into.contains_key(&name) { continue; }
        if let Some(u) = expand(&uri, vars) {
            into.insert(name, PathBuf::from(u));
        }
    }
}

/// Index of a footprint folder: LCSC id → file.
fn index_fp_dir(dir: &Path) -> HashMap<String, PathBuf> {
    let mut m = HashMap::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for p in rd.flatten().map(|e| e.path()) {
            if p.extension().map_or(false, |x| x == "kicad_mod") {
                if let Ok(t) = std::fs::read_to_string(&p) {
                    if let Some(i) = t.find("\"LCSC\" \"") {
                        let rest = &t[i + 8..];
                        if let Some(end) = rest.find('"') {
                            m.insert(rest[..end].to_string(), p.clone());
                        }
                    }
                }
            }
        }
    }
    m
}

/// First `(model "…")` path inside a footprint file.
fn model_ref(fp_file: &Path) -> Option<String> {
    let t = std::fs::read_to_string(fp_file).ok()?;
    let root = parse_root(&t, "footprint").or_else(|_| parse_root(&t, "module")).ok()?;
    let r = root.children("model").next().map(|m| m.arg(0).to_string());
    r
}

pub fn load(lib_path: &str, lib_name: &str) -> Result<LibContents> {
    let paths = LibPaths::new(lib_path, lib_name);
    let mut out = LibContents::default();
    if !paths.sym_file.is_file() {
        return Ok(out);
    }
    out.sym_exists = true;

    // Resolve footprint library nicknames like KiCad does
    out.project_dir = find_project_dir(Path::new(lib_path));
    let vars = path_vars(out.project_dir.as_deref());
    let mut fp_libs: HashMap<String, PathBuf> = HashMap::new();
    if let Some(p) = &out.project_dir { read_fp_lib_table(&p.join("fp-lib-table"), &vars, &mut fp_libs); }
    if let Some(c) = kicad_config_dir() { read_fp_lib_table(&c.join("fp-lib-table"), &vars, &mut fp_libs); }
    // This app's own convention: <lib_path>/<lib_name>.pretty
    fp_libs.entry(lib_name.to_string()).or_insert_with(|| paths.fp_dir.clone());

    let mut dir_index: HashMap<PathBuf, HashMap<String, PathBuf>> = HashMap::new();

    let text = std::fs::read_to_string(&paths.sym_file)?;
    let root = parse_root(&text, "kicad_symbol_lib")?;
    for sym in root.children("symbol") {
        let mut e = LibEntry { name: sym.arg(0).to_string(), ..Default::default() };
        for p in sym.children("property") {
            let (k, v) = (p.arg(0), p.arg(1).to_string());
            match k {
                "Value" => e.value = v,
                "LCSC" => e.lcsc = v,
                "Footprint" => e.footprint = v,
                "Description" => e.description = v,
                "Manufacturer" => e.manufacturer = v,
                "Category" => e.category = v,
                _ => {}
            }
        }
        if let Some((lib, fp)) = e.footprint.split_once(':') {
            e.fp_foreign_lib = lib != lib_name;
            if let Some(dir) = fp_libs.get(lib) {
                e.fp_lib_dir = Some(dir.clone());
                let f = dir.join(format!("{fp}.kicad_mod"));
                if f.is_file() {
                    e.fp_file = Some(f);
                } else if !e.lcsc.is_empty() {
                    let idx = dir_index.entry(dir.clone()).or_insert_with(|| index_fp_dir(dir));
                    if let Some(actual) = idx.get(&e.lcsc) {
                        e.fp_mismatch = Some(actual.file_stem().unwrap_or_default().to_string_lossy().to_string());
                        e.fp_file = None;
                        // still inspect the real file for its 3D model
                        let actual = actual.clone();
                        check_model(&mut e, &actual, dir, out.project_dir.as_deref(), &vars, &paths);
                    }
                }
                if let Some(f) = e.fp_file.clone() {
                    check_model(&mut e, &f, dir, out.project_dir.as_deref(), &vars, &paths);
                }
            }
        }
        out.entries.push(e);
    }

    // Unused footprints in every footprint folder this symbol library points to
    let used_dirs: std::collections::BTreeSet<PathBuf> =
        out.entries.iter().filter_map(|e| e.fp_lib_dir.clone()).collect();
    let mut orphans = Vec::new();
    for dir in used_dirs {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for p in rd.flatten().map(|e| e.path()) {
            if !p.extension().map_or(false, |x| x == "kicad_mod") { continue; }
            let stem = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
            let used = out.entries.iter().any(|e| {
                e.fp_file.as_deref() == Some(p.as_path())
                    || (e.fp_lib_dir.as_deref() == Some(dir.as_path()) && e.fp_mismatch.as_deref() == Some(stem.as_str()))
            });
            if !used { orphans.push(stem); }
        }
    }
    orphans.sort();
    out.orphan_footprints = orphans;
    Ok(out)
}

/// Resolve the footprint's 3D model path and record which of STEP / WRL exist.
fn check_model(
    e: &mut LibEntry,
    fp_file: &Path,
    fp_dir: &Path,
    project_dir: Option<&Path>,
    vars: &HashMap<String, String>,
    paths: &LibPaths,
) {
    let stem = fp_file.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(r) = model_ref(fp_file) {
        e.model_ref = r.clone();
        if let Some(x) = expand(&r, vars) {
            let p = PathBuf::from(&x);
            if p.is_absolute() {
                candidates.push(p);
            } else {
                // KiCad resolves relative model paths against the project and the footprint lib
                if let Some(pd) = project_dir { candidates.push(pd.join(&p)); }
                candidates.push(fp_dir.join(&p));
            }
        }
    }
    // This app's convention as a fallback
    candidates.push(paths.model_dir.join(format!("{stem}.step")));
    for c in &candidates {
        // Strip only a known model extension — names like "L2.9-W1.3" contain dots
        let s = c.to_string_lossy();
        let base = [".step", ".stp", ".wrl", ".STEP", ".STP", ".WRL"].iter()
            .find_map(|x| s.strip_suffix(x))
            .unwrap_or(&s)
            .to_string();
        let find = |exts: &[&str]| exts.iter()
            .map(|x| PathBuf::from(format!("{base}.{x}")))
            .find(|p| p.is_file());
        let step = find(&["step", "stp", "STEP", "STP"]);
        let wrl = find(&["wrl", "WRL"]);
        e.has_step = step.is_some();
        e.has_wrl = wrl.is_some();
        e.model_file = step.or(wrl);
        if e.has_step || e.has_wrl { break; }
    }
}

// ── 3D model transform in a footprint ────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelTransform {
    pub offset: [f32; 3],   // mm
    pub rotation: [f32; 3], // degrees
    pub scale: [f32; 3],
}

impl Default for ModelTransform {
    fn default() -> Self {
        Self { offset: [0.0; 3], rotation: [0.0; 3], scale: [1.0; 3] }
    }
}

/// Read offset / rotate / scale of the first `(model …)` in a footprint.
/// Legacy `(at (xyz …))` is in inches and is converted to mm.
pub fn read_model_transform(fp_text: &str) -> Option<ModelTransform> {
    let root = parse_root(fp_text, "footprint").or_else(|_| parse_root(fp_text, "module")).ok()?;
    let model = root.children("model").next()?;
    let xyz = |head: &str| -> Option<[f32; 3]> {
        let x = model.children(head).next()?.children("xyz").next()?;
        Some([x.arg(0).parse().ok()?, x.arg(1).parse().ok()?, x.arg(2).parse().ok()?])
    };
    let mut t = ModelTransform::default();
    if let Some(o) = xyz("offset") {
        t.offset = o;
    } else if let Some(a) = xyz("at") {
        t.offset = a.map(|v| v * 25.4);
    }
    if let Some(r) = xyz("rotate") { t.rotation = r; }
    if let Some(s) = xyz("scale") { t.scale = s; }
    Some(t)
}

/// Byte span of the list starting at `start` (which must be '('), honouring strings.
fn list_end(text: &str, start: usize) -> Option<usize> {
    let b = text.as_bytes();
    let (mut depth, mut i, mut in_str) = (0i32, start, false);
    while i < b.len() {
        match b[i] {
            b'\\' if in_str => i += 1,
            b'"' => in_str = !in_str,
            b'(' if !in_str => depth += 1,
            b')' if !in_str => {
                depth -= 1;
                if depth == 0 { return Some(i + 1); }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Start of the first `(model` list outside of strings.
fn find_model_start(text: &str) -> Option<usize> {
    let b = text.as_bytes();
    let mut in_str = false;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' if in_str => i += 1,
            b'"' => in_str = !in_str,
            b'(' if !in_str => {
                let rest = &text[i + 1..];
                if rest.starts_with("model") && rest[5..].starts_with(|c: char| c.is_whitespace()) {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn fmt_num(v: f32) -> String {
    let s = format!("{:.4}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() { "0".into() } else { s.to_string() }
}

/// Replace offset / rotate / scale of the first `(model …)` block, keeping the
/// model path and any other settings (hide, opacity, …) untouched.
pub fn write_model_transform(fp_text: &str, t: &ModelTransform) -> Result<String> {
    let Some(start) = find_model_start(fp_text) else { bail!("footprint has no (model …) entry") };
    let end = list_end(fp_text, start).ok_or_else(|| anyhow::anyhow!("unbalanced (model …) block"))?;
    let block = &fp_text[start..end];

    // Indentation of the (model line
    let line_start = fp_text[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let ind = &fp_text[line_start..start];
    let ind = if ind.trim().is_empty() { ind.to_string() } else { String::new() };
    let unit = if ind.contains('\t') || fp_text.contains("\n\t(") { "\t" } else { "  " };
    let ind1 = format!("{ind}{unit}");
    let ind2 = format!("{ind1}{unit}");

    // Split the block into head (path) and child lists
    let inner = &block[1..block.len() - 1];
    let mut head_end = inner.len();
    let mut kept: Vec<&str> = Vec::new();
    let (mut i, mut in_str) = (0usize, false);
    let ib = inner.as_bytes();
    while i < ib.len() {
        match ib[i] {
            b'\\' if in_str => i += 1,
            b'"' => in_str = !in_str,
            b'(' if !in_str => {
                if head_end == inner.len() { head_end = i; }
                let e = list_end(inner, i).ok_or_else(|| anyhow::anyhow!("unbalanced model child"))?;
                let child = &inner[i..e];
                let name: String = child[1..].chars().take_while(|c| !c.is_whitespace() && *c != '(' && *c != ')').collect();
                if !matches!(name.as_str(), "at" | "offset" | "scale" | "rotate") {
                    kept.push(child.trim());
                }
                i = e;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    let head = inner[..head_end].trim();

    let xyz = |v: [f32; 3]| format!("(xyz {} {} {})", fmt_num(v[0]), fmt_num(v[1]), fmt_num(v[2]));
    let mut out = format!("({head}\n");
    for (name, v) in [("offset", t.offset), ("scale", t.scale), ("rotate", t.rotation)] {
        out += &format!("{ind1}({name}\n{ind2}{}\n{ind1})\n", xyz(v));
    }
    for k in kept {
        out += &format!("{ind1}{k}\n");
    }
    out += &format!("{ind})");

    Ok(format!("{}{}{}", &fp_text[..start], out, &fp_text[end..]))
}

/// Read, update and write back the 3D transform of a footprint file.
pub fn save_model_transform(fp_file: &Path, t: &ModelTransform) -> Result<()> {
    let text = std::fs::read_to_string(fp_file)?;
    let new = write_model_transform(&text, t)?;
    // sanity check: result must parse and read back the same values
    let back = read_model_transform(&new).ok_or_else(|| anyhow::anyhow!("re-read failed"))?;
    if back.offset.iter().zip(t.offset).any(|(a, b)| (a - b).abs() > 1e-3) {
        bail!("verification of written transform failed");
    }
    let tmp = fp_file.with_extension("kicad_mod.tmp");
    std::fs::write(&tmp, new)?;
    std::fs::rename(&tmp, fp_file)?;
    Ok(())
}

// ── Pads of a local footprint (for 3D rendering of library parts) ─────────────

/// A pad as written in a .kicad_mod (KiCad coordinates: mm, Y down).
#[derive(Debug, Clone)]
pub struct FpPad {
    pub number: String,
    pub x: f32,
    pub y: f32,
    pub rotation: f32,
    pub w: f32,
    pub h: f32,
    pub shape: String, // rect | oval | circle
    pub drill: f32,
    pub drill_slot: f32,
    pub npth: bool,
}

/// Copper pads of a footprint (front-side SMD and all through-hole pads).
pub fn footprint_pads(fp_text: &str) -> Vec<FpPad> {
    let Ok(root) = parse_root(fp_text, "footprint").or_else(|_| parse_root(fp_text, "module")) else { return vec![] };
    let num = |s: &Sx, i: usize| s.arg(i).parse::<f32>().unwrap_or(0.0);
    root.children("pad").filter_map(|p| {
        let kind = p.arg(1);
        let shape = match p.arg(2) {
            "circle" => "circle",
            "oval" => "oval",
            _ => "rect",
        };
        let at = p.children("at").next()?;
        let size = p.children("size").next()?;
        let (mut drill, mut drill_slot) = (0.0, 0.0);
        if let Some(d) = p.children("drill").next() {
            if d.arg(0) == "oval" {
                let (a, b) = (num(d, 1), num(d, 2));
                drill = a.min(b);
                drill_slot = a.max(b);
            } else {
                drill = num(d, 0);
            }
        }
        // skip back-side-only SMD pads
        let layers: Vec<&str> = p.children("layers").next()
            .map(|l| (0..l.items().len().saturating_sub(1)).map(|i| l.arg(i)).collect())
            .unwrap_or_default();
        if kind == "smd" && !layers.is_empty() && !layers.iter().any(|l| l.starts_with("F.") || l.starts_with("*.")) {
            return None;
        }
        Some(FpPad {
            number: p.arg(0).to_string(),
            x: num(at, 0),
            y: num(at, 1),
            rotation: num(at, 2),
            w: num(size, 0),
            h: num(size, 1),
            shape: shape.into(),
            drill,
            drill_slot,
            npth: kind == "np_thru_hole",
        })
    }).collect()
}

// ── Minimal S-expression parser ──────────────────────────────────────────────

#[derive(Debug)]
enum Sx {
    Atom(String),
    List(Vec<Sx>),
}

impl Sx {
    fn items(&self) -> &[Sx] {
        match self {
            Sx::List(v) => v,
            Sx::Atom(_) => &[],
        }
    }
    fn head(&self) -> &str {
        match self.items().first() {
            Some(Sx::Atom(s)) => s,
            _ => "",
        }
    }
    /// n-th argument after the head, as a string ("" if missing or a list).
    fn arg(&self, n: usize) -> &str {
        match self.items().get(n + 1) {
            Some(Sx::Atom(s)) => s,
            _ => "",
        }
    }
    fn children<'a>(&'a self, head: &'a str) -> impl Iterator<Item = &'a Sx> + 'a {
        self.items().iter().filter(move |c| c.head() == head)
    }
}

fn parse_root(text: &str, root_head: &str) -> Result<Sx> {
    let mut stack: Vec<Vec<Sx>> = vec![Vec::new()];
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '(' => stack.push(Vec::new()),
            ')' => {
                let list = stack.pop().unwrap_or_default();
                match stack.last_mut() {
                    Some(parent) => parent.push(Sx::List(list)),
                    None => bail!("unbalanced ')' in symbol library"),
                }
            }
            '"' => {
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            if let Some(n) = chars.next() {
                                s.push(match n { 'n' => '\n', 't' => '\t', o => o });
                            }
                        }
                        '"' => break,
                        o => s.push(o),
                    }
                }
                if let Some(top) = stack.last_mut() { top.push(Sx::Atom(s)); }
            }
            c if c.is_whitespace() => {}
            c => {
                let mut s = String::from(c);
                while let Some(&n) = chars.peek() {
                    if n.is_whitespace() || n == '(' || n == ')' || n == '"' { break; }
                    s.push(n);
                    chars.next();
                }
                if let Some(top) = stack.last_mut() { top.push(Sx::Atom(s)); }
            }
        }
    }
    let mut top = stack.pop().unwrap_or_default();
    if !stack.is_empty() {
        bail!("unbalanced '(' in KiCad file");
    }
    // Return the (kicad_symbol_lib …) list
    let found = top.drain(..).find(|s| s.head() == root_head);
    found.ok_or_else(|| anyhow::anyhow!("not a KiCad file ({root_head})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pads_both_formats() {
        let legacy = "(module X (layer F.Cu)\n  (pad 1 smd oval (at -1.9 2.7 90) (size 0.6 2.0) (layers F.Cu F.Mask))\n  (pad \"\" np_thru_hole circle (at 0 0) (size 1 1) (drill 1) (layers *.Cu))\n  (pad 9 smd rect (at 0 0) (size 1 1) (layers B.Cu))\n)";
        let p = footprint_pads(legacy);
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].number.as_str(), p[0].x, p[0].y, p[0].rotation, p[0].shape.as_str()), ("1", -1.9, 2.7, 90.0, "oval"));
        assert!(p[1].npth && p[1].drill == 1.0);
        let modern = "(footprint \"X\"\n\t(pad \"2\" thru_hole oval\n\t\t(at 1 2)\n\t\t(size 1.5 2)\n\t\t(drill oval 0.8 1.2)\n\t\t(layers \"*.Cu\" \"*.Mask\")\n\t)\n)";
        let p = footprint_pads(modern);
        assert_eq!(p.len(), 1);
        assert_eq!((p[0].drill, p[0].drill_slot), (0.8, 1.2));
    }

    #[test]
    fn model_transform_roundtrip() {
        // KiCad 10 multi-line format with tabs and an extra child
        let new_fmt = "(footprint \"C0603\"\n\t(pad \"1\")\n\t(model \"My_model_dir/C0603.step\"\n\t\t(offset\n\t\t\t(xyz 0 0 0.35)\n\t\t)\n\t\t(hide yes)\n\t\t(scale\n\t\t\t(xyz 1 1 1)\n\t\t)\n\t\t(rotate\n\t\t\t(xyz -0 -0 -0)\n\t\t)\n\t)\n)\n";
        let t = read_model_transform(new_fmt).unwrap();
        assert_eq!(t.offset, [0.0, 0.0, 0.35]);
        let t2 = ModelTransform { offset: [0.1, -0.2, 0.5], rotation: [0.0, 0.0, 90.0], scale: [1.0; 3] };
        let out = write_model_transform(new_fmt, &t2).unwrap();
        assert_eq!(read_model_transform(&out).unwrap(), t2);
        assert!(out.contains("(hide yes)"));
        assert!(out.contains("\t(model \"My_model_dir/C0603.step\"\n\t\t(offset\n\t\t\t(xyz 0.1 -0.2 0.5)"));
        assert!(out.starts_with("(footprint \"C0603\"\n\t(pad \"1\")\n"));
        assert!(out.ends_with("\t)\n)\n"));

        // Legacy format: unquoted path, (at …) in inches
        let old_fmt = "(module SOT23 (layer F.Cu)\n  (model My_lib/SOT23.step\n    (at (xyz 0 0.019685 0))\n    (scale (xyz 1 1 1))\n    (rotate (xyz 0 0 -180))\n  )\n)\n";
        let t = read_model_transform(old_fmt).unwrap();
        assert!((t.offset[1] - 0.5).abs() < 1e-4);
        assert_eq!(t.rotation, [0.0, 0.0, -180.0]);
        let out = write_model_transform(old_fmt, &t).unwrap();
        assert!(!out.contains("(at "));
        assert!(out.contains("  (model My_lib/SOT23.step\n    (offset\n      (xyz 0 0.5 0)"));
        assert_eq!(read_model_transform(&out).unwrap().rotation, [0.0, 0.0, -180.0]);
    }

    #[test]
    fn model_with_dots_in_name() {
        let d = std::env::temp_dir().join(format!("jlc_model_test_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("SOT-23_L2.9-W1.3.step"), "").unwrap();
        let fp = d.join("SOT-23_L2.9-W1.3.kicad_mod");
        std::fs::write(&fp, "(footprint \"x\" (model \"SOT-23_L2.9-W1.3.step\"))").unwrap();
        let mut e = LibEntry::default();
        let paths = LibPaths::new(&d.to_string_lossy(), "none");
        check_model(&mut e, &fp, &d, None, &HashMap::new(), &paths);
        assert!(e.has_step && !e.has_wrl);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn parses_symbols_and_properties() {
        let txt = "(kicad_symbol_lib (version 1)\n\t(symbol \"R_1k\"\n\t\t(property \"Value\" \"1k \\\"x\\\"\")\n\t\t(property \"LCSC\" \"C21190\")\n\t\t(property \"Footprint\" \"mylib:R0603\")\n\t\t(symbol \"R_1k_0_1\" (property \"LCSC\" \"bad\"))\n\t)\n)";
        let root = parse_root(txt, "kicad_symbol_lib").unwrap();
        let syms: Vec<_> = root.children("symbol").collect();
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].arg(0), "R_1k");
        let props: Vec<_> = syms[0].children("property").map(|p| (p.arg(0), p.arg(1))).collect();
        assert_eq!(props, vec![("Value", "1k \"x\""), ("LCSC", "C21190"), ("Footprint", "mylib:R0603")]);
    }
}

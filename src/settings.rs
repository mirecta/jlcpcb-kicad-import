use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub lib_path: String,
    pub lib_name: String,
    /// Footprint library nickname — empty = same as lib_name.
    #[serde(default)]
    pub fp_lib_name: String,
    /// Footprint library folder — empty = <lib_path>/<footprint name>.pretty
    #[serde(default)]
    pub fp_dir: String,
    /// 3D model folder — empty = <lib_path>/<footprint name>.3dshapes
    #[serde(default)]
    pub model_dir: String,
    /// Last folder a datasheet was saved to.
    #[serde(default)]
    pub datasheet_dir: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            lib_path: dirs::document_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("KiCad")
                .join("libraries")
                .to_string_lossy()
                .to_string(),
            lib_name: "local_lib".to_string(),
            fp_lib_name: String::new(),
            fp_dir: String::new(),
            model_dir: String::new(),
            datasheet_dir: String::new(),
        }
    }
}

impl Settings {
    pub fn paths(&self) -> crate::export::LibPaths {
        crate::export::LibPaths::with_footprints(&self.lib_path, &self.lib_name, &self.fp_lib_name, &self.fp_dir, &self.model_dir)
    }

    /// True when footprints/3D use the default name and location.
    pub fn fp_is_default(&self) -> bool {
        self.fp_lib_name.trim().is_empty() && self.fp_dir.trim().is_empty() && self.model_dir.trim().is_empty()
    }

    /// Adopt a detected footprint layout (or reset to defaults with None).
    pub fn apply_layout(&mut self, l: Option<&crate::library::FpLayout>) {
        match l {
            Some(l) => {
                self.fp_lib_name = l.nick.clone();
                self.fp_dir = l.fp_dir.to_string_lossy().to_string();
                self.model_dir = l.model_dir.as_ref().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
            }
            None => {
                self.fp_lib_name.clear();
                self.fp_dir.clear();
                self.model_dir.clear();
            }
        }
    }
}

fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("jlcpcb-kicad")
        .join("settings.json")
}

fn history_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("jlcpcb-kicad")
        .join("history.json")
}

pub fn load_history() -> Vec<String> {
    std::fs::read_to_string(history_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_history(history: &[String]) {
    let path = history_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(history) {
        let _ = std::fs::write(&path, json);
    }
}

pub fn load() -> Settings {
    let path = config_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Settings with the footprint layout filled in: when footprints are left at the
/// default but the symbol library already references another footprint library
/// (resolved via fp-lib-table), use that one.
pub fn load_effective() -> Settings {
    let mut s = load();
    s.fill_detected_layout();
    s
}

impl Settings {
    pub fn fill_detected_layout(&mut self) {
        if self.fp_is_default() {
            if let Some(l) = crate::library::detect_layout(&self.lib_path, &self.lib_name) {
                self.apply_layout(Some(&l));
            }
        }
    }
}

pub fn save(settings: &Settings) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(settings)?;
    std::fs::write(&path, json)?;
    Ok(())
}

// ── Library detection ────────────────────────────────────────────────────────

/// One KiCad library found in a directory, made of up to three parts:
/// `<name>.kicad_sym`, `<name>.pretty/`, `<name>.3dshapes/`.
#[derive(Debug, Clone, Default)]
pub struct LibInfo {
    pub name: String,
    pub has_sym: bool,
    pub has_fp: bool,
    pub has_3d: bool,
}

/// Result of scanning a candidate library folder.
#[derive(Debug, Clone, Default)]
pub struct LibScan {
    pub exists: bool,
    /// Libraries directly inside the folder.
    pub libs: Vec<LibInfo>,
    /// Set when the folder itself is a library part (`X.pretty`, `X.3dshapes`)
    /// or lies inside one — the folder the user most likely meant.
    pub suggested_parent: Option<(String, String)>,
    /// Sub-folders (one level down) that contain libraries — the user probably
    /// stopped one level too shallow.
    pub child_dirs: Vec<(String, Vec<String>)>,
}

/// Library part suffixes recognised in a directory.
fn lib_part(file_name: &str, is_dir: bool) -> Option<(&str, u8)> {
    if is_dir {
        if let Some(n) = file_name.strip_suffix(".pretty") { return Some((n, 1)); }
        if let Some(n) = file_name.strip_suffix(".3dshapes") { return Some((n, 2)); }
    } else if let Some(n) = file_name.strip_suffix(".kicad_sym") {
        return Some((n, 0));
    }
    None
}

/// List all libraries directly inside `dir`, sorted by name.
pub fn list_libraries(dir: &Path) -> Vec<LibInfo> {
    let mut map: std::collections::BTreeMap<String, LibInfo> = Default::default();
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    for e in rd.flatten() {
        let fname = e.file_name().to_string_lossy().to_string();
        let is_dir = e.path().is_dir();
        if let Some((name, kind)) = lib_part(&fname, is_dir) {
            if name.is_empty() { continue; }
            let li = map.entry(name.to_string()).or_insert_with(|| LibInfo {
                name: name.to_string(),
                ..Default::default()
            });
            match kind {
                0 => li.has_sym = true,
                1 => li.has_fp = true,
                _ => li.has_3d = true,
            }
        }
    }
    map.into_values().collect()
}

/// If `path` is (or is inside) a `.pretty` / `.3dshapes` folder, or is a
/// `.kicad_sym` file, return `(library_dir, library_name)`.
pub fn library_from_inner_path(path: &Path) -> Option<(PathBuf, String)> {
    for anc in path.ancestors() {
        let Some(fname) = anc.file_name().map(|f| f.to_string_lossy().to_string()) else { continue };
        let is_dir = !(anc == path && path.is_file());
        if let Some((name, _)) = lib_part(&fname, is_dir) {
            if name.is_empty() { continue; }
            let parent = anc.parent()?.to_path_buf();
            return Some((parent, name.to_string()));
        }
    }
    None
}

pub fn scan_library_dir(path_str: &str) -> LibScan {
    let path = Path::new(path_str.trim());
    let mut scan = LibScan {
        exists: path.is_dir(),
        ..Default::default()
    };
    if let Some((dir, name)) = library_from_inner_path(path) {
        scan.suggested_parent = Some((dir.to_string_lossy().to_string(), name));
    }
    if !scan.exists {
        return scan;
    }
    scan.libs = list_libraries(path);
    if scan.libs.is_empty() {
        if let Ok(rd) = std::fs::read_dir(path) {
            let mut dirs: Vec<_> = rd
                .flatten()
                .filter(|e| e.path().is_dir())
                .filter(|e| lib_part(&e.file_name().to_string_lossy(), true).is_none())
                .collect();
            dirs.sort_by_key(|e| e.file_name());
            for e in dirs {
                let libs = list_libraries(&e.path());
                if !libs.is_empty() {
                    scan.child_dirs.push((
                        e.path().to_string_lossy().to_string(),
                        libs.into_iter().map(|l| l.name).collect(),
                    ));
                }
            }
        }
    }
    scan
}

/// Normalise a folder or file picked in a dialog into `(lib_path, lib_name)`.
/// The name is `None` when it cannot be determined unambiguously.
pub fn resolve_pick(picked: &Path) -> (PathBuf, Option<String>) {
    if let Some((dir, name)) = library_from_inner_path(picked) {
        return (dir, Some(name));
    }
    let dir = if picked.is_file() {
        picked.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| picked.to_path_buf())
    } else {
        picked.to_path_buf()
    };
    let libs = list_libraries(&dir);
    let name = if libs.len() == 1 { Some(libs[0].name.clone()) } else { None };
    (dir, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_library_from_any_pick() {
        let root = std::env::temp_dir().join(format!("jlc_lib_test_{}", std::process::id()));
        let lib = root.join("kicad");
        std::fs::create_dir_all(lib.join("mylib.pretty")).unwrap();
        std::fs::create_dir_all(lib.join("mylib.3dshapes")).unwrap();
        std::fs::write(lib.join("mylib.kicad_sym"), "").unwrap();

        // Correct folder
        assert_eq!(resolve_pick(&lib), (lib.clone(), Some("mylib".into())));
        // Too deep: .pretty folder
        assert_eq!(resolve_pick(&lib.join("mylib.pretty")), (lib.clone(), Some("mylib".into())));
        // Picked the .kicad_sym file
        assert_eq!(resolve_pick(&lib.join("mylib.kicad_sym")), (lib.clone(), Some("mylib".into())));
        // Too shallow: child dir hint
        let scan = scan_library_dir(&root.to_string_lossy());
        assert!(scan.libs.is_empty());
        assert_eq!(scan.child_dirs[0].1, vec!["mylib".to_string()]);
        // Inside .3dshapes
        let scan = scan_library_dir(&lib.join("mylib.3dshapes").to_string_lossy());
        assert_eq!(scan.suggested_parent.unwrap().1, "mylib");

        std::fs::remove_dir_all(&root).unwrap();
    }
}

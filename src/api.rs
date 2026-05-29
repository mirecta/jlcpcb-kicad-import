use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

const JLCPCB_SEARCH_API: &str =
    "https://jlcpcb.com/api/overseas-pcb-order/v1/shoppingCart/smtGood/selectSmtComponentList";
const EASYEDA_COMPONENT_API: &str = "https://easyeda.com/api/products/{id}/components";
const EASYEDA_SVG_API: &str = "https://easyeda.com/api/products/{id}/svgs";

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attribute {
    pub name: String,
    pub value: String,
}

/// Lightweight result shown in the search list
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub lcsc_id: String,
    pub value: String,
    pub manufacturer: String,
    pub package: String,
    pub category: String,
    pub description: String,
    pub stock: u64,
    pub price: f64,
    pub min_qty: u32,
    pub class: String,
}

/// Pre-tessellated footprint drawing for the 3D PCB viewer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FpDrawing {
    /// Triangle vertices as (x_mm, z_mm) pairs in board coords, already centred on pad centroid.
    pub tris:  Vec<[f32; 2]>,
    pub color: [f32; 3],
}

/// Raw graphic element written into the .kicad_mod (silkscreen, courtyard, fab).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FpGraphic {
    Line { x1: f32, y1: f32, x2: f32, y2: f32, width: f32, layer: String },
    Circle { cx: f32, cy: f32, r: f32, width: f32, layer: String },
    Poly { pts: Vec<[f32; 2]>, width: f32, layer: String, fill: bool },
}

/// Footprint pad extracted from EasyEDA package shape data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pad {
    pub cx:     f32,    // centre X in mm (PCB X-axis)
    pub cy:     f32,    // centre Y in mm (PCB Y-axis → viewer Z)
    pub w:      f32,
    pub h:      f32,
    pub number: String, // pad number ("1", "2", "A1", …)
    pub shape:  String, // KiCad shape: "oval", "rect", "circle"
    pub rotation: f32,  // degrees
    pub drill:  f32,    // 0 = SMD, >0 = through-hole drill diameter in mm
}

/// Graphical element extracted from EasyEDA schematic data for KiCad symbol body
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SymGraphic {
    Arc     { start: [f32; 2], mid: [f32; 2], end: [f32; 2], width: f32 },
    Poly    { pts: Vec<[f32; 2]>, width: f32, fill: bool },
    Circle  { cx: f32, cy: f32, r: f32, width: f32, fill: bool },
    Rect    { x0: f32, y0: f32, x1: f32, y1: f32, width: f32, fill: bool },
}

/// One pin extracted from EasyEDA schematic shape data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pin {
    pub name: String,
    pub number: String,
    pub pin_type: String,  // KiCad type: input/output/bidirectional/power_in/…
    pub x: f32,            // wire-connection end in mm, KiCad coords
    pub y: f32,
    pub angle: i32,        // KiCad angle (0=right, 90=up, 180=left, 270=down)
}

/// Full component data loaded after the user picks a result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Component {
    pub lcsc_id: String,
    pub value: String,
    pub manufacturer: String,
    pub description: String,
    pub package: String,
    pub package_detail: String,
    pub category: String,
    pub datasheet: String,
    pub stock: u64,
    pub price: String,
    pub min_qty: u32,
    pub process: String,
    pub class: String,
    pub attrition_qty: u32,
    pub extra_attrs: Vec<Attribute>,
    pub pins: Vec<Pin>,
    pub pads: Vec<Pad>,
    pub symbol_svg: Option<String>,
    pub footprint_svg: Option<String>,
    pub wrl_url: Option<String>,
    pub step_url: Option<String>,
    pub fp_drawings: Vec<FpDrawing>,
    pub fp_graphics: Vec<FpGraphic>,
    // Initial 3D model placement from EasyEDA SVGNODE data (mm / degrees)
    pub model_init_offset:   [f32; 3],
    pub model_init_rotation: [f32; 3],
    pub sym_graphics: Vec<SymGraphic>,
}

// ── HTTP helpers ──────────────────────────────────────────────────────────────

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/120.0.0.0 Safari/537.36")
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap()
}

fn get_json(url: &str) -> Result<serde_json::Value> {
    let resp = client()
        .get(url)
        .header("Accept", "application/json")
        .header("Referer", "https://easyeda.com/")
        .send()?;
    if !resp.status().is_success() {
        return Err(anyhow!("{} → HTTP {}", url, resp.status()));
    }
    Ok(resp.json()?)
}

fn post_json(url: &str, body: &serde_json::Value) -> Result<serde_json::Value> {
    let resp = client()
        .post(url)
        .header("Content-Type", "application/json")
        .header("Origin", "https://jlcpcb.com")
        .header("Referer", "https://jlcpcb.com/parts")
        .json(body)
        .send()?;
    if !resp.status().is_success() {
        return Err(anyhow!("{} → HTTP {}", url, resp.status()));
    }
    Ok(resp.json()?)
}

pub fn download_bytes(url: &str) -> Result<Vec<u8>> {
    let resp = reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0")
        .timeout(std::time::Duration::from_secs(30))
        .build()?
        .get(url)
        .send()?;
    Ok(resp.bytes()?.to_vec())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Replace unsupported Unicode characters (box drawing, special separators) with ASCII
fn sanitize_description(s: &str) -> String {
    s.chars()
        .map(|c| {
            // Replace box drawing characters and other Unicode separators with " | "
            if c > '\u{007F}' && !c.is_alphanumeric() {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// ── Search (list) ─────────────────────────────────────────────────────────────

pub fn search_components(query: &str, page_size: usize, basic_only: bool) -> Result<Vec<SearchResult>> {
    let mut body = serde_json::json!({
        "keyword": query,
        "currentPage": 1,
        "pageSize": page_size
    });
    if basic_only {
        body["componentLibraryType"] = serde_json::json!("base");
    }
    let data = post_json(JLCPCB_SEARCH_API, &body)?;
    let list = data
        .pointer("/data/componentPageInfo/list")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("No results for {}", query))?;

    let mut results: Vec<SearchResult> = list
        .iter()
        .filter_map(|item| {
            let lcsc_id = item["componentCode"].as_str()?.to_string();
            let prices = item["componentPrices"].as_array();
            let price = prices
                .and_then(|arr| arr.first())
                .and_then(|p| p["productPrice"].as_f64())
                .unwrap_or(0.0);
            let class = if item["componentLibraryType"].as_str() == Some("base") {
                "Basic"
            } else {
                "Extended"
            };
            Some(SearchResult {
                lcsc_id,
                value: item["componentModelEn"].as_str().unwrap_or("").to_string(),
                manufacturer: item["componentBrandEn"].as_str().unwrap_or("").to_string(),
                package: item["componentSpecificationEn"].as_str().unwrap_or("").to_string(),
                category: item["componentTypeEn"].as_str().unwrap_or("").to_string(),
                description: sanitize_description(item["describe"].as_str().unwrap_or("")),
                stock: item["stockCount"].as_u64().unwrap_or(0),
                price,
                min_qty: item["minPurchaseNum"].as_u64().unwrap_or(1) as u32,
                class: class.to_string(),
            })
        })
        .collect();

    // Sort: Basic first, then by stock desc, then price asc
    results.sort_by(|a, b| {
        let class_ord = b.class.cmp(&a.class); // "Basic" > "Extended"
        if class_ord != std::cmp::Ordering::Equal {
            return class_ord;
        }
        b.stock.cmp(&a.stock).then(a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal))
    });

    Ok(results)
}

// ── Full component fetch (by LCSC C-number) ───────────────────────────────────

pub fn fetch_component(lcsc_id: &str) -> Result<Component> {
    let id = lcsc_id.to_uppercase();

    // 1) JLCPCB for description, datasheet, price, attributes — search by exact LCSC ID
    let jlcpcb = fetch_jlcpcb_by_lcsc(&id)?;

    // 2) EasyEDA CAD data — needs C-number
    let easyeda = fetch_easyeda_info(&id).unwrap_or_default();

    // 3) SVG previews — needs C-number
    let svgs = fetch_svgs(&id).unwrap_or_default();

    let package_detail = easyeda
        .pointer("/packageDetail/title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let class_raw = easyeda
        .pointer("/dataStr/head/c_para/JLCPCB Part Class")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let class = if class_raw.to_lowercase().contains("basic") || jlcpcb.class == "Basic" {
        "Basic Component"
    } else {
        "Extended Component"
    }
    .to_string();

    let process = if easyeda["SMT"].as_bool().unwrap_or(false) {
        "SMT"
    } else {
        "THT"
    }
    .to_string();

    let model_info = extract_model_info(&easyeda);

    let wrl_url = model_info.as_ref()
        .map(|m| format!("https://easyeda.com/analyzer/api/3dmodel/{}", m.uuid));
    let step_url = model_info.as_ref()
        .map(|m| format!("https://modules.easyeda.com/qAxj6KHrDKw4blvCG8QJPs7Y/{}", m.uuid));

    let model_init_offset   = model_info.as_ref().map(|m| m.offset).unwrap_or([0.0; 3]);
    let model_init_rotation = model_info.as_ref().map(|m| m.rotation).unwrap_or([0.0; 3]);

    let pins = extract_pins(&easyeda);
    let sym_graphics = extract_sym_graphics(&easyeda);
    let pads = extract_pads(&easyeda);
    let fp_drawings = extract_fp_drawings(&easyeda);
    let fp_graphics = extract_fp_graphics(&easyeda);

    Ok(Component {
        lcsc_id: id,
        value: jlcpcb.value,
        manufacturer: jlcpcb.manufacturer,
        description: jlcpcb.description,
        package: jlcpcb.package,
        package_detail,
        category: jlcpcb.category,
        datasheet: jlcpcb.datasheet,
        stock: jlcpcb.stock,
        price: format!("{}USD", jlcpcb.price),
        min_qty: jlcpcb.min_qty,
        process,
        class,
        attrition_qty: 0,
        extra_attrs: jlcpcb.attributes,
        pins,
        pads,
        symbol_svg: svgs.symbol,
        footprint_svg: svgs.footprint,
        wrl_url,
        step_url,
        fp_drawings,
        fp_graphics,
        model_init_offset,
        model_init_rotation,
        sym_graphics,
    })
}

// ── EasyEDA pin extraction ────────────────────────────────────────────────────

fn ee_shape_array(data: &serde_json::Value) -> Vec<serde_json::Value> {
    // dataStr can be a nested JSON object OR a JSON-encoded string
    let ds = match data.get("dataStr") {
        Some(v) => v,
        None => return vec![],
    };
    let parsed: serde_json::Value = if ds.is_string() {
        match serde_json::from_str(ds.as_str().unwrap_or("")) {
            Ok(v) => v,
            Err(_) => return vec![],
        }
    } else {
        ds.clone()
    };
    parsed.get("shape")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default()
}

fn extract_pins(easyeda: &serde_json::Value) -> Vec<Pin> {
    let shapes = ee_shape_array(easyeda);
    if shapes.is_empty() { return vec![]; }

    // 1 EasyEDA schematic unit = 10 mil = 0.254 mm
    const SCALE: f32 = 0.254;

    let mut raw: Vec<(f32, f32, i32, String, String, String)> = Vec::new();

    for shape in &shapes {
        let s = match shape.as_str() { Some(s) => s, None => continue };

        if s.starts_with("PIN~") {
            // ── Legacy EasyEDA schematic format ──────────────────────────────
            // PIN~x~y~rotation~?~name~type~?~number~...
            let p: Vec<&str> = s.split('~').collect();
            if p.len() < 9 { continue; }

            let x: f32 = p[1].parse().unwrap_or(0.0);
            let y: f32 = p[2].parse().unwrap_or(0.0);
            let rot: i32 = p[3].parse::<i32>().unwrap_or(0).rem_euclid(360);
            let name   = p[5].to_string();
            let number = p[8].to_string();

            let pin_type = match p[6].to_uppercase().as_str() {
                "IN"      => "input",
                "OUT"     => "output",
                "IO"      => "bidirectional",
                "PWR"     => "power_in",
                "PWROUT"  => "power_out",
                "NC"      => "no_connect",
                "PASSIVE" => "passive",
                _         => "unspecified",
            }.to_string();

            // EE Y-down rotation → KiCad Y-up angle
            let kicad_angle = match rot {
                0 => 0, 90 => 270, 180 => 180, 270 => 90, _ => 0,
            };

            raw.push((x, y, kicad_angle, name, number, pin_type));

        } else if s.starts_with("P~") {
            // ── EasyEDA Pro schematic format ──────────────────────────────────
            // P~show~?~pinnum~x~y~rotation~id~...
            //   ^^cx~cy
            //   ^^path (M{x},{y}h±len...)
            //   ^^1~tx~ty~rot~NAME~align~...   (name label)
            //   ^^1~tx~ty~rot~NUM~align~...    (number label)
            //   ^^...
            let segs: Vec<&str> = s.split("^^").collect();

            let p0: Vec<&str> = segs[0].split('~').collect();
            if p0.len() < 7 { continue; }

            let pin_number = p0[3].to_string();
            let x: f32 = p0[4].parse().unwrap_or(0.0);
            let y: f32 = p0[5].parse().unwrap_or(0.0);
            let rot: i32 = p0[6].parse::<i32>().unwrap_or(0).rem_euclid(360);

            // P~ rotation = direction the pin wire points outward (away from body).
            // KiCad angle = direction toward body = (rot + 180) % 360.
            let kicad_angle = (rot + 180) % 360;

            // Segments 3 and 4 are text labels. Identify name vs number by matching
            // against pin_number: the segment whose text == pin_number is the number label.
            let text3 = segs.get(3)
                .and_then(|seg| seg.split('~').nth(4))
                .unwrap_or("").to_string();
            let text4 = segs.get(4)
                .and_then(|seg| seg.split('~').nth(4))
                .unwrap_or("").to_string();

            let pin_name = if text4 == pin_number {
                text3
            } else if text3 == pin_number {
                text4
            } else {
                text3
            };

            raw.push((x, y, kicad_angle, pin_name, pin_number, "passive".to_string()));
        }
    }

    if raw.is_empty() { return vec![]; }

    // Centre on centroid of all wire-connection points
    let cx = raw.iter().map(|(x, ..)| *x).sum::<f32>() / raw.len() as f32;
    let cy = raw.iter().map(|(_, y, ..)| *y).sum::<f32>() / raw.len() as f32;

    raw.into_iter().map(|(x, y, angle, name, number, pin_type)| Pin {
        name,
        number,
        pin_type,
        x:  (x - cx) * SCALE,
        y: -(y - cy) * SCALE,
        angle,
    }).collect()
}

// ── EasyEDA schematic graphic extraction ──────────────────────────────────────

/// Return the centroid of all pin positions in EasyEDA units (used to centre graphics).
fn ee_pin_centroid(shapes: &[serde_json::Value]) -> (f32, f32) {
    let mut xs: Vec<f32> = Vec::new();
    let mut ys: Vec<f32> = Vec::new();
    for shape in shapes {
        let s = match shape.as_str() { Some(s) => s, None => continue };
        if s.starts_with("PIN~") {
            let p: Vec<&str> = s.split('~').collect();
            if p.len() >= 3 {
                if let (Ok(x), Ok(y)) = (p[1].parse::<f32>(), p[2].parse::<f32>()) {
                    xs.push(x); ys.push(y);
                }
            }
        } else if s.starts_with("P~") {
            let p0: Vec<&str> = s.split("^^").next().unwrap_or("").split('~').collect();
            if p0.len() >= 6 {
                if let (Ok(x), Ok(y)) = (p0[4].parse::<f32>(), p0[5].parse::<f32>()) {
                    xs.push(x); ys.push(y);
                }
            }
        }
    }
    if xs.is_empty() { return (0.0, 0.0); }
    let cx = xs.iter().sum::<f32>() / xs.len() as f32;
    let cy = ys.iter().sum::<f32>() / ys.len() as f32;
    (cx, cy)
}

/// SVG arc endpoint parameterisation → midpoint on the arc (EasyEDA coordinates).
fn svg_arc_mid(x1: f32, y1: f32, mut rx: f32, mut ry: f32, f_a: bool, f_s: bool, x2: f32, y2: f32) -> [f32; 2] {
    use std::f32::consts::PI;
    let dx2 = (x1 - x2) / 2.0;
    let dy2 = (y1 - y2) / 2.0;
    // phi=0 so x1'=dx2, y1'=dy2
    let x1p = dx2; let y1p = dy2;
    let x1p2 = x1p * x1p; let y1p2 = y1p * y1p;
    let lambda = (x1p2 / (rx * rx) + y1p2 / (ry * ry)).sqrt();
    if lambda > 1.0 { rx *= lambda; ry *= lambda; }
    let rx2 = rx * rx; let ry2 = ry * ry;
    let sign = if f_a == f_s { -1.0_f32 } else { 1.0_f32 };
    let sq = ((rx2 * ry2 - rx2 * y1p2 - ry2 * x1p2) / (rx2 * y1p2 + ry2 * x1p2).max(1e-9)).max(0.0).sqrt();
    let cxp = sign * sq *  rx * y1p / ry;
    let cyp = sign * sq * -ry * x1p / rx;
    let cx = cxp + (x1 + x2) / 2.0;
    let cy = cyp + (y1 + y2) / 2.0;
    // angles
    let va = |ux: f32, uy: f32, vx: f32, vy: f32| -> f32 {
        let n = ((ux*ux+uy*uy)*(vx*vx+vy*vy)).sqrt().max(1e-9);
        let mut a = ((ux*vx+uy*vy)/n).clamp(-1.0,1.0).acos();
        if ux*vy - uy*vx < 0.0 { a = -a; }
        a
    };
    let ux = (x1p - cxp) / rx; let uy = (y1p - cyp) / ry;
    let vx = (-x1p - cxp) / rx; let vy = (-y1p - cyp) / ry;
    let theta1 = va(1.0, 0.0, ux, uy);
    let mut dtheta = va(ux, uy, vx, vy);
    if  f_s && dtheta < 0.0 { dtheta += 2.0 * PI; }
    if !f_s && dtheta > 0.0 { dtheta -= 2.0 * PI; }
    let mt = theta1 + dtheta / 2.0;
    [cx + rx * mt.cos(), cy + ry * mt.sin()]
}

/// Parse "M x y A rx ry xrot fa fs x2 y2" from an EasyEDA A~ path string.
fn parse_ee_arc(path: &str) -> Option<([f32;2], f32, f32, bool, bool, [f32;2])> {
    let t: Vec<&str> = path.split_whitespace().collect();
    let mi = t.iter().position(|s| *s == "M")?;
    let ai = t.iter().position(|s| *s == "A")?;
    if ai < mi + 3 || t.len() < ai + 8 { return None; }
    Some(([t[mi+1].parse().ok()?, t[mi+2].parse().ok()?],
          t[ai+1].parse().ok()?, t[ai+2].parse().ok()?,
          t[ai+4].parse::<u8>().ok()? != 0, t[ai+5].parse::<u8>().ok()? != 0,
          [t[ai+6].parse().ok()?, t[ai+7].parse().ok()?]))
}

fn extract_sym_graphics(easyeda: &serde_json::Value) -> Vec<SymGraphic> {
    let shapes = ee_shape_array(easyeda);
    if shapes.is_empty() { return vec![]; }
    let (cx, cy) = ee_pin_centroid(&shapes);
    const S: f32 = 0.254; // EasyEDA unit → mm

    // Transform: centre + Y-flip + scale
    let t = |ex: f32, ey: f32| -> [f32; 2] { [(ex - cx) * S, -(ey - cy) * S] };

    let mut out: Vec<SymGraphic> = Vec::new();

    for shape in &shapes {
        let s = match shape.as_str() { Some(s) => s, None => continue };

        if s.starts_with("A~") || s.starts_with("ARC~") {
            // A~<svgpath>~~color~width~...
            let p: Vec<&str> = s.splitn(10, '~').collect();
            let path  = p.get(1).unwrap_or(&"");
            let width = p.get(4).and_then(|s| s.parse::<f32>().ok()).unwrap_or(1.0) * S;
            if let Some(([x1,y1], rx, ry, fa, fs, [x2,y2])) = parse_ee_arc(path) {
                let mid = svg_arc_mid(x1, y1, rx, ry, fa, fs, x2, y2);
                // Snap arc endpoint Y to nearest grid (0.254 mm = 1 EasyEDA unit)
                // so adjacent arcs share exactly the same baseline Y.
                let snap = |v: f32| (v / S).round() * S;
                let mut s = t(x1, y1); s[1] = snap(s[1]);
                let mut e = t(x2, y2); e[1] = snap(e[1]);
                out.push(SymGraphic::Arc {
                    start: s, mid: t(mid[0], mid[1]), end: e, width,
                });
            }

        } else if s.starts_with("POLYLINE~") || s.starts_with("LINE~") {
            // POLYLINE~x1 y1 x2 y2 ...~width~...
            let p: Vec<&str> = s.splitn(5, '~').collect();
            let pts_str = p.get(1).unwrap_or(&"");
            let width = p.get(2).and_then(|s| s.parse::<f32>().ok()).unwrap_or(1.0) * S;
            let nums: Vec<f32> = pts_str.split(|c: char| c.is_whitespace() || c == ',')
                .filter_map(|s| s.parse().ok()).collect();
            let pts: Vec<[f32;2]> = nums.chunks_exact(2).map(|c| t(c[0], c[1])).collect();
            if pts.len() >= 2 { out.push(SymGraphic::Poly { pts, width, fill: false }); }

        } else if s.starts_with("CIRCLE~") {
            // CIRCLE~cx~cy~r~...~width~fillColor~...
            let p: Vec<&str> = s.split('~').collect();
            if p.len() < 4 { continue; }
            let ecx: f32 = p[1].parse().unwrap_or(0.0);
            let ecy: f32 = p[2].parse().unwrap_or(0.0);
            let er:  f32 = p[3].parse().unwrap_or(0.0);
            let width = p.get(5).and_then(|s| s.parse::<f32>().ok()).unwrap_or(1.0) * S;
            let fill  = p.get(6).map(|s| *s != "none" && !s.is_empty()).unwrap_or(false);
            let c = t(ecx, ecy);
            out.push(SymGraphic::Circle { cx: c[0], cy: c[1], r: er * S, width, fill });

        } else if s.starts_with("RECT~") {
            // RECT~x~y~width~height~...~strokeWidth~...
            let p: Vec<&str> = s.split('~').collect();
            if p.len() < 5 { continue; }
            let ex: f32 = p[1].parse().unwrap_or(0.0);
            let ey: f32 = p[2].parse().unwrap_or(0.0);
            let ew: f32 = p[3].parse().unwrap_or(0.0);
            let eh: f32 = p[4].parse().unwrap_or(0.0);
            let width = p.get(7).and_then(|s| s.parse::<f32>().ok()).unwrap_or(1.0) * S;
            let p0 = t(ex, ey); let p1 = t(ex + ew, ey + eh);
            out.push(SymGraphic::Rect { x0: p0[0], y0: p0[1], x1: p1[0], y1: p1[1], width, fill: true });
        }
    }
    out
}

fn extract_pads(easyeda: &serde_json::Value) -> Vec<Pad> {
    let pd = match easyeda.get("packageDetail") {
        Some(v) => v,
        None => return vec![],
    };
    let shapes = ee_shape_array(pd);
    if shapes.is_empty() { return vec![]; }

    // EasyEDA footprint units: 1 unit = 10 mil = 0.254 mm
    const SCALE: f32 = 0.254;

    // (cx, cy, w, h, number, shape, rotation, is_tht)
    let mut raw: Vec<(f32, f32, f32, f32, String, String, f32, bool)> = Vec::new();

    for shape in &shapes {
        let s = match shape.as_str() {
            Some(s) if s.starts_with("PAD~") => s,
            _ => continue,
        };
        // PAD~shape~cx~cy~w~h~layer~net~number~???~oval_pts~rotation~id~...
        let p: Vec<&str> = s.split('~').collect();
        if p.len() < 6 { continue; }

        let ee_shape = p[1];
        let cx: f32 = p[2].parse().unwrap_or(0.0);
        let cy: f32 = p[3].parse().unwrap_or(0.0);
        let w:  f32 = p[4].parse().unwrap_or(0.0);
        let h:  f32 = p[5].parse().unwrap_or(0.0);
        if w <= 0.0 || h <= 0.0 { continue; }

        let layer  = p.get(6).copied().unwrap_or("1");
        let number = p.get(8).copied().unwrap_or("1").to_string();
        // p[9] is an EasyEDA-specific parameter; actual pad rotation is at p[11]
        let rotation: f32 = p.get(11)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);

        let is_tht = layer == "11";

        let kicad_shape = match ee_shape.to_uppercase().as_str() {
            "OVAL" | "ELLIPSE" => "oval",
            "RECT"             => "rect",
            _                  => "circle",
        };

        raw.push((cx, cy, w, h, number, kicad_shape.to_string(), rotation, is_tht));
    }

    if raw.is_empty() { return vec![]; }

    let cx0 = raw.iter().map(|(x, ..)| *x).sum::<f32>() / raw.len() as f32;
    let cy0 = raw.iter().map(|(_, y, ..)| *y).sum::<f32>() / raw.len() as f32;

    raw.into_iter().map(|(cx, cy, w, h, number, shape, rotation, is_tht)| {
        // Through-hole drill: approximate as half the smaller pad dimension
        let drill = if is_tht { w.min(h) * SCALE * 0.5 } else { 0.0 };
        Pad {
            cx: (cx - cx0) * SCALE,
            cy: (cy - cy0) * SCALE,
            w:  w * SCALE,
            h:  h * SCALE,
            number,
            shape,
            rotation,
            drill,
        }
    }).collect()
}

fn ee_layer_to_kicad(layer: i32) -> Option<&'static str> {
    match layer {
        3  => Some("F.SilkS"),
        4  => Some("B.SilkS"),
        10 => Some("F.Fab"),
        11 => Some("B.Fab"),
        13 => Some("F.CrtYd"),
        14 => Some("B.CrtYd"),
        _  => None,
    }
}

fn pad_centroid(shapes: &[serde_json::Value]) -> (f32, f32) {
    let mut xs: Vec<f32> = Vec::new();
    let mut ys: Vec<f32> = Vec::new();
    for shape in shapes {
        let s = match shape.as_str() {
            Some(s) if s.starts_with("PAD~") => s,
            _ => continue,
        };
        let p: Vec<&str> = s.split('~').collect();
        if p.len() < 4 { continue; }
        xs.push(p[2].parse().unwrap_or(0.0));
        ys.push(p[3].parse().unwrap_or(0.0));
    }
    if xs.is_empty() { return (0.0, 0.0); }
    (xs.iter().sum::<f32>() / xs.len() as f32,
     ys.iter().sum::<f32>() / ys.len() as f32)
}

fn extract_fp_graphics(easyeda: &serde_json::Value) -> Vec<FpGraphic> {
    let pd = match easyeda.get("packageDetail") {
        Some(v) => v,
        None => return vec![],
    };
    let shapes = ee_shape_array(pd);
    if shapes.is_empty() { return vec![]; }

    const SCALE: f32 = 0.254;
    let (cx0, cy0) = pad_centroid(&shapes);
    let to_mm = |ex: f32, ey: f32| -> (f32, f32) {
        ((ex - cx0) * SCALE, (ey - cy0) * SCALE)
    };

    let mut out: Vec<FpGraphic> = Vec::new();

    for shape in &shapes {
        let s = match shape.as_str() { Some(s) => s, None => continue };

        if s.starts_with("TRACK~") {
            // TRACK~stroke_width~layer~net~x1 y1 x2 y2...~id
            let p: Vec<&str> = s.split('~').collect();
            if p.len() < 5 { continue; }
            let sw: f32 = p[1].parse().unwrap_or(0.0);
            let layer_n: i32 = p[2].parse().unwrap_or(-1);
            let layer = match ee_layer_to_kicad(layer_n) { Some(l) => l.to_string(), None => continue };
            let width = (sw * SCALE).max(0.01);
            let pts_raw: Vec<f32> = p[4].split_whitespace().filter_map(|t| t.parse().ok()).collect();
            if pts_raw.len() < 4 { continue; }
            let pts: Vec<(f32, f32)> = pts_raw.chunks_exact(2).map(|c| to_mm(c[0], c[1])).collect();
            for i in 0..pts.len() - 1 {
                out.push(FpGraphic::Line {
                    x1: pts[i].0, y1: pts[i].1,
                    x2: pts[i+1].0, y2: pts[i+1].1,
                    width, layer: layer.clone(),
                });
            }

        } else if s.starts_with("CIRCLE~") {
            // CIRCLE~cx~cy~radius~stroke_width~layer~id
            let p: Vec<&str> = s.split('~').collect();
            if p.len() < 6 { continue; }
            let ecx: f32 = p[1].parse().unwrap_or(0.0);
            let ecy: f32 = p[2].parse().unwrap_or(0.0);
            let r:   f32 = p[3].parse().unwrap_or(0.0);
            let sw:  f32 = p[4].parse().unwrap_or(0.0);
            let layer_n: i32 = p[5].parse().unwrap_or(-1);
            let layer = match ee_layer_to_kicad(layer_n) { Some(l) => l.to_string(), None => continue };
            let (cx, cy) = to_mm(ecx, ecy);
            let r_mm = r * SCALE;
            if r_mm > 0.0 {
                out.push(FpGraphic::Circle {
                    cx, cy, r: r_mm, width: (sw * SCALE).max(0.01), layer,
                });
            }

        } else if s.starts_with("SOLIDREGION~") {
            // SOLIDREGION~layer~net~polygon_points~type~id
            let p: Vec<&str> = s.split('~').collect();
            if p.len() < 4 { continue; }
            let layer_n: i32 = p[1].parse().unwrap_or(-1);
            let layer = match ee_layer_to_kicad(layer_n) { Some(l) => l.to_string(), None => continue };
            let pts_raw: Vec<f32> = p[3]
                .split(|c: char| c == ' ' || c == ',')
                .filter_map(|t| { let t = t.trim(); if t.is_empty() { None } else { t.parse().ok() } })
                .collect();
            if pts_raw.len() < 6 { continue; }
            let pts: Vec<[f32; 2]> = pts_raw.chunks_exact(2)
                .map(|c| { let (x, y) = to_mm(c[0], c[1]); [x, y] })
                .collect();

            // Skip polygons with unreasonably large coordinates (corrupted EasyEDA data)
            // Reasonable footprints should be within ±100mm
            let max_coord = pts.iter()
                .flat_map(|p| [p[0].abs(), p[1].abs()])
                .fold(0.0f32, f32::max);
            if max_coord > 100.0 {
                continue; // Skip this polygon - the outline from TRACK/LINE is still exported
            }

            let fill = layer != "F.CrtYd" && layer != "B.CrtYd";
            let width = if layer == "F.CrtYd" || layer == "B.CrtYd" { 0.05 } else { 0.12 };
            out.push(FpGraphic::Poly { pts, width, layer, fill });
        }
    }
    out
}

fn extract_fp_drawings(easyeda: &serde_json::Value) -> Vec<FpDrawing> {
    let pd = match easyeda.get("packageDetail") {
        Some(v) => v,
        None => return vec![],
    };
    let shapes = ee_shape_array(pd);
    if shapes.is_empty() { return vec![]; }

    // EasyEDA footprint units: 1 unit = 10 mil = 0.254 mm
    const SCALE: f32 = 0.254;

    let (cx0, cy0) = pad_centroid(&shapes);

    let mut drawings: Vec<FpDrawing> = Vec::new();

    for shape in &shapes {
        let s = match shape.as_str() { Some(s) => s, None => continue };

        if s.starts_with("TRACK~") {
            // TRACK~stroke_width~layer~net~x1 y1 x2 y2...~id
            let p: Vec<&str> = s.split('~').collect();
            if p.len() < 5 { continue; }
            let sw: f32 = p[1].parse().unwrap_or(0.0);
            let layer: i32 = p[2].parse().unwrap_or(-1);
            let color = match layer {
                3  => [0.95_f32, 0.95, 0.95],  // F.SilkS only
                _  => continue,
            };
            let pts_raw: Vec<f32> = p[4].split_whitespace()
                .filter_map(|t| t.parse().ok())
                .collect();
            if pts_raw.len() < 4 { continue; }
            let pts: Vec<(f32, f32)> = pts_raw.chunks_exact(2)
                .map(|c| {
                    let x = (c[0] - cx0) * SCALE;
                    let z = (c[1] - cy0) * SCALE;
                    (x, z)
                })
                .collect();
            if pts.len() < 2 { continue; }
            let hw = sw * SCALE / 2.0;
            if hw <= 0.0 { continue; }
            let mut tris: Vec<[f32; 2]> = Vec::new();
            for i in 0..pts.len() - 1 {
                let (x1, z1) = pts[i];
                let (x2, z2) = pts[i + 1];
                let dx = x2 - x1;
                let dz = z2 - z1;
                let len = (dx * dx + dz * dz).sqrt();
                if len < 1e-6 { continue; }
                let px = -dz / len * hw;
                let pz =  dx / len * hw;
                // 4 corners
                let a = [x1 + px, z1 + pz];
                let b = [x1 - px, z1 - pz];
                let c = [x2 + px, z2 + pz];
                let d = [x2 - px, z2 - pz];
                // 2 triangles
                tris.extend_from_slice(&[a, b, c]);
                tris.extend_from_slice(&[b, d, c]);
            }
            if !tris.is_empty() {
                drawings.push(FpDrawing { tris, color });
            }

        } else if s.starts_with("CIRCLE~") {
            // CIRCLE~cx~cy~radius~stroke_width~layer~id
            let p: Vec<&str> = s.split('~').collect();
            if p.len() < 6 { continue; }
            let cx: f32 = p[1].parse().unwrap_or(0.0);
            let cy: f32 = p[2].parse().unwrap_or(0.0);
            let r:  f32 = p[3].parse().unwrap_or(0.0);
            let sw: f32 = p[4].parse().unwrap_or(0.0);
            let layer: i32 = p[5].parse().unwrap_or(-1);
            let color = match layer {
                3           => [0.95_f32, 0.95, 0.95],
                100 | 12    => [0.6_f32, 0.45, 0.1],
                99          => [0.20_f32, 0.60, 0.22],
                _           => continue,
            };
            let outer_r = (r + sw / 2.0) * SCALE;
            let inner_r = ((r - sw / 2.0) * SCALE).max(0.0);
            if outer_r <= 0.0 { continue; }
            let ox = (cx - cx0) * SCALE;
            let oz = (cy - cy0) * SCALE;
            const N: usize = 24;
            let mut tris: Vec<[f32; 2]> = Vec::new();
            for i in 0..N {
                let a0 = (i as f32) * std::f32::consts::TAU / N as f32;
                let a1 = (i as f32 + 1.0) * std::f32::consts::TAU / N as f32;
                let (sa0, ca0) = (a0.sin(), a0.cos());
                let (sa1, ca1) = (a1.sin(), a1.cos());
                // outer arc: (ox + outer_r*cos, oz + outer_r*sin)
                let o0 = [ox + outer_r * ca0, oz + outer_r * sa0];
                let o1 = [ox + outer_r * ca1, oz + outer_r * sa1];
                let i0 = [ox + inner_r * ca0, oz + inner_r * sa0];
                let i1 = [ox + inner_r * ca1, oz + inner_r * sa1];
                tris.extend_from_slice(&[o0, i0, o1]);
                tris.extend_from_slice(&[i0, i1, o1]);
            }
            if !tris.is_empty() {
                drawings.push(FpDrawing { tris, color });
            }

        } else if s.starts_with("SOLIDREGION~") {
            continue; // filled regions excluded — only show silkscreen tracks and circles
        }
    }

    drawings
}

// ── Internal helpers ──────────────────────────────────────────────────────────

struct JlcpcbDetail {
    value: String,
    manufacturer: String,
    package: String,
    category: String,
    description: String,
    datasheet: String,
    stock: u64,
    price: String,
    min_qty: u32,
    class: String,
    attributes: Vec<Attribute>,
}

fn fetch_jlcpcb_by_lcsc(lcsc_id: &str) -> Result<JlcpcbDetail> {
    let body = serde_json::json!({
        "keyword": lcsc_id,
        "currentPage": 1,
        "pageSize": 5
    });
    let data = post_json(JLCPCB_SEARCH_API, &body)?;
    let list = data
        .pointer("/data/componentPageInfo/list")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("No JLCPCB results for {}", lcsc_id))?;

    let item = list
        .iter()
        .find(|it| it["componentCode"].as_str() == Some(lcsc_id))
        .or_else(|| list.first())
        .ok_or_else(|| anyhow!("{} not found", lcsc_id))?;

    let prices = item["componentPrices"].as_array();
    let price = prices
        .and_then(|arr| arr.first())
        .and_then(|p| p["productPrice"].as_f64())
        .unwrap_or(0.0);

    let class = if item["componentLibraryType"].as_str() == Some("base") {
        "Basic"
    } else {
        "Extended"
    };

    let attributes: Vec<Attribute> = item["attributes"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|a| {
            let name = a["attribute_name_en"].as_str()?.to_string();
            let value = a["attribute_value_name"].as_str()?.to_string();
            if value == "-" || value.is_empty() { return None; }
            Some(Attribute { name, value })
        })
        .collect();

    Ok(JlcpcbDetail {
        value: item["componentModelEn"].as_str().unwrap_or("").to_string(),
        manufacturer: item["componentBrandEn"].as_str().unwrap_or("").to_string(),
        package: item["componentSpecificationEn"].as_str().unwrap_or("").to_string(),
        category: item["componentTypeEn"].as_str().unwrap_or("").to_string(),
        description: sanitize_description(item["describe"].as_str().unwrap_or("")),
        datasheet: item["dataManualUrl"].as_str().unwrap_or("").to_string(),
        stock: item["stockCount"].as_u64().unwrap_or(0),
        price: format!("{:.4}", price),
        min_qty: item["minPurchaseNum"].as_u64().unwrap_or(1) as u32,
        class: class.to_string(),
        attributes,
    })
}

// ── 3D model info from SVGNODE ────────────────────────────────────────────────

struct ModelInfo {
    uuid:     String,
    offset:   [f32; 3],   // mm, relative to footprint origin
    rotation: [f32; 3],   // degrees
}

fn extract_model_info(easyeda: &serde_json::Value) -> Option<ModelInfo> {
    let pd = easyeda.get("packageDetail")?;

    // footprint origin in EasyEDA raw units (same scale as c_origin)
    let fp_origin: (f32, f32) = {
        let ds = pd.get("dataStr")?;
        let parsed: serde_json::Value = if ds.is_string() {
            serde_json::from_str(ds.as_str()?).ok()?
        } else {
            ds.clone()
        };
        (
            parsed.pointer("/head/x").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
            parsed.pointer("/head/y").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
        )
    };

    for shape in &ee_shape_array(pd) {
        let s = match shape.as_str() {
            Some(s) if s.starts_with("SVGNODE~") => s,
            _ => continue,
        };
        let data: serde_json::Value = match serde_json::from_str(&s["SVGNODE~".len()..]) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let uuid = match data.pointer("/attrs/uuid").and_then(|v| v.as_str()) {
            Some(u) if !u.is_empty() => u.to_string(),
            _ => continue,
        };

        let c_origin = data.pointer("/attrs/c_origin")
            .and_then(|v| v.as_str()).unwrap_or("0,0");
        let ov: Vec<f32> = c_origin.split(',').filter_map(|s| s.parse().ok()).collect();
        let ox = ov.first().copied().unwrap_or(0.0);
        let oy = ov.get(1).copied().unwrap_or(0.0);

        // z can be a string or number
        let oz: f32 = data.pointer("/attrs/z")
            .map(|v| if v.is_string() {
                v.as_str().unwrap_or("0").parse().unwrap_or(0.0)
            } else {
                v.as_f64().unwrap_or(0.0) as f32
            }).unwrap_or(0.0);

        // /100 converts EasyEDA raw units to mm (same as JLC2KiCad_lib)
        let tx = (ox - fp_origin.0) / 100.0;
        let ty = -(oy - fp_origin.1) / 100.0;
        let tz = oz / 100.0;

        let rot_str = data.pointer("/attrs/c_rotation")
            .and_then(|v| v.as_str()).unwrap_or("0,0,0");
        let rv: Vec<f32> = rot_str.split(',').filter_map(|s| s.parse().ok()).collect();
        let rotation = [
            rv.first().copied().unwrap_or(0.0),
            rv.get(1).copied().unwrap_or(0.0),
            rv.get(2).copied().unwrap_or(0.0),
        ];

        return Some(ModelInfo { uuid, offset: [tx, ty, tz], rotation });
    }
    None
}

// ── Download WRL (EasyEDA OBJ-like → VRML 2.0) ───────────────────────────────

pub fn download_wrl(url: &str) -> Option<Vec<u8>> {
    let text = client()
        .get(url)
        .header("Accept", "*/*")
        .header("Referer", "https://easyeda.com/")
        .send().ok()?
        .text().ok()?;
    if text.trim().is_empty() { return None; }
    Some(easyeda_obj_to_wrl(&text))
}

pub fn easyeda_obj_to_wrl(text: &str) -> Vec<u8> {
    use std::collections::HashMap;

    struct Mat { diffuse: [f32;3], specular: [f32;3], transparency: f32 }
    impl Default for Mat {
        fn default() -> Self { Self { diffuse: [0.75;3], specular: [0.0;3], transparency: 0.0 } }
    }

    // Parse material blocks  (newmtl … endmtl)
    let mut mats: HashMap<String, Mat> = HashMap::new();
    let mut cur_name = String::new();
    let mut cur_mat  = Mat::default();
    let mut in_mat   = false;

    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("newmtl ") {
            cur_name = name.trim().to_string();
            cur_mat  = Mat::default();
            in_mat   = true;
        } else if line == "endmtl" {
            if in_mat { mats.insert(cur_name.clone(), cur_mat); cur_mat = Mat::default(); }
            in_mat = false;
        } else if in_mat {
            let p: Vec<&str> = line.split_whitespace().collect();
            let f3 = |p: &[&str]| -> [f32;3] {
                let mut a = [0.0f32; 3];
                for (i, s) in p.iter().take(3).enumerate() {
                    a[i] = s.parse().unwrap_or(0.0);
                }
                a
            };
            match p.first().copied().unwrap_or("") {
                "Kd" => cur_mat.diffuse      = f3(&p[1..]),
                "Ks" => cur_mat.specular     = f3(&p[1..]),
                "d"  => cur_mat.transparency = 1.0 - p.get(1).and_then(|s| s.parse().ok()).unwrap_or(1.0_f32),
                _    => {}
            }
        }
    }

    // EasyEDA OBJ uses 2.54 mm per unit (0.1-inch convention).
    // Dividing by 2.54 converts to mm, which KiCad 6+ VRML expects (scale xyz 1 1 1).
    let mut global_verts: Vec<[f32;3]> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("v ") {
            let n: Vec<f32> = rest.split_whitespace().filter_map(|s| s.parse().ok()).collect();
            if n.len() >= 3 {
                global_verts.push([n[0]/2.54, n[1]/2.54, n[2]/2.54]);
            }
        }
    }

    // Parse shapes grouped by usemtl
    let mut output = String::from("#VRML V2.0 utf8\n#created by jlcpcb-kicad\n");

    let mut current_mat  = String::new();
    let mut face_lines: Vec<String> = Vec::new();

    let flush = |mat_name: &str, faces: &[String], out: &mut String| {
        if faces.is_empty() { return; }
        let mat = mats.get(mat_name).unwrap_or(&Mat { diffuse: [0.75;3], specular: [0.0;3], transparency: 0.0 });

        // Build per-shape dedup vertex list and remapped indices
        let mut local_verts: Vec<[f32;3]> = Vec::new();
        let mut idx_map: HashMap<usize, usize> = HashMap::new();
        let mut coord_idx: Vec<String> = Vec::new();

        for face in faces {
            let indices: Vec<usize> = face.split_whitespace()
                .skip(1)
                .filter_map(|tok| {
                    // handle v, v/t, v/t/n, v//n
                    tok.split('/').next().and_then(|s| s.parse::<usize>().ok())
                })
                .filter(|&i| i > 0 && i <= global_verts.len())
                .collect();
            if indices.len() < 3 { continue; }

            let mut local_face: Vec<String> = Vec::new();
            for &vi in &indices {
                let vi0 = vi - 1;
                let li = *idx_map.entry(vi0).or_insert_with(|| {
                    let li = local_verts.len();
                    local_verts.push(global_verts[vi0]);
                    li
                });
                local_face.push(li.to_string());
            }
            local_face.push("-1".to_string());
            coord_idx.push(local_face.join(","));
        }
        if local_verts.is_empty() { return; }

        let pts = local_verts.iter()
            .map(|v| format!("{:.4} {:.4} {:.4}", v[0], v[1], v[2]))
            .collect::<Vec<_>>().join(", ");
        let idx = coord_idx.join(",");

        out.push_str(&format!(
            "Shape{{\n  appearance Appearance{{\n    material Material{{\n\
             diffuseColor {:.4} {:.4} {:.4}\nspecularColor {:.4} {:.4} {:.4}\n\
             ambientIntensity 0.2\ntransparency 0.0000\nshininess 0.5\n    }}\n  }}\n\
             geometry IndexedFaceSet{{\nccw TRUE\nsolid FALSE\n\
             coord Coordinate{{point [{pts}]}}\n\
             coordIndex [{idx}]\n  }}\n}}\n",
            mat.diffuse[0], mat.diffuse[1], mat.diffuse[2],
            mat.specular[0], mat.specular[1], mat.specular[2],
        ));
    };

    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("usemtl ") {
            flush(&current_mat, &face_lines, &mut output);
            face_lines.clear();
            current_mat = name.trim().to_string();
        } else if line.starts_with("f ") {
            face_lines.push(line.to_string());
        }
    }
    flush(&current_mat, &face_lines, &mut output);

    output.into_bytes()
}

fn fetch_easyeda_info(lcsc_id: &str) -> Result<serde_json::Value> {
    let url = EASYEDA_COMPONENT_API.replace("{id}", lcsc_id);
    let data = get_json(&url)?;
    if data["result"].is_object() {
        Ok(data["result"].clone())
    } else {
        Err(anyhow!("No EasyEDA result for {}", lcsc_id))
    }
}

#[derive(Default)]
pub struct Svgs {
    pub symbol: Option<String>,
    pub footprint: Option<String>,
}

fn fetch_svgs(lcsc_id: &str) -> Result<Svgs> {
    let url = EASYEDA_SVG_API.replace("{id}", lcsc_id);
    let data = get_json(&url)?;
    let entries = match data["result"].as_array() {
        Some(a) => a.clone(),
        None => return Ok(Svgs::default()),
    };
    if entries.is_empty() {
        return Ok(Svgs::default());
    }
    let symbol = if entries.len() >= 2 {
        entries[0]["svg"].as_str().map(|s| s.to_string())
    } else {
        None
    };
    let footprint = entries.last()
        .and_then(|e| e["svg"].as_str())
        .map(|s| s.to_string());
    Ok(Svgs { symbol, footprint })
}

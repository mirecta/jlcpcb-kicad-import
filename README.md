# JLCPCB → KiCad

A desktop app for browsing JLCPCB's parts catalog and importing components directly into a KiCad symbol/footprint library.

## What it does

- Search JLCPCB's full parts database by keyword, value, LCSC number, or category (e.g. `ULN2003`, `100nF 0402`, `C7512`)
- Preview the KiCad schematic symbol and PCB footprint before importing
- View an interactive 3D model of the component on a PCB
- Export a ready-to-use `.kicad_sym` symbol, `.kicad_mod` footprint, and 3D model (STEP + WRL) into your local KiCad library with one click

## Building

Requires Rust (stable). Install via [rustup](https://rustup.rs).

```bash
git clone https://github.com/yourname/jlcpcb-kicad-import
cd jlcpcb-kicad-import
cargo build --release
./target/release/jlcpcb-kicad
```

## Usage

1. **Search** — type any query in the search bar and press Enter or click Search.  Tick **Basic only** to limit results to JLCPCB's Basic Component library (no extra fee).
2. **Pick a part** — click any row to load the full component detail, symbol preview, footprint preview, and 3D model.
3. **Adjust placement** (optional):
   - Drag the symbol or footprint previews to pan; **Ctrl+scroll** to zoom.
   - Drag the 3D viewer to orbit; scroll to zoom the camera.
   - Adjust **3D Model Offset / Rotation / Scale** to fine-tune how the model sits on the footprint.  Enable **Unified scale** to resize the model uniformly.
4. **Configure library path** — click **Settings**, enter the path to your KiCad library folder and the library name.
5. **Import** — click **Import** to write the symbol, footprint, and 3D model files into your library.

After importing, register the library in KiCad:

- **Symbols:** Preferences → Manage Symbol Libraries → add `<lib_path>/<lib_name>/<lib_name>.kicad_sym`
- **Footprints:** Preferences → Manage Footprint Libraries → add `<lib_path>/<lib_name>.pretty`

## Output files

| File | Location |
|------|----------|
| Symbol | `<lib_path>/<lib_name>/<lib_name>.kicad_sym` |
| Footprint | `<lib_path>/<lib_name>.pretty/<value>_<lcsc>.kicad_mod` |
| 3D model (STEP) | `<lib_path>/<lib_name>.3dshapes/<value>_<lcsc>.step` |
| 3D model (WRL) | `<lib_path>/<lib_name>.3dshapes/<value>_<lcsc>.wrl` |

## Data sources

- Part metadata, pricing, and stock: [JLCPCB](https://jlcpcb.com/parts)
- Schematic symbols, footprints, and 3D models: [EasyEDA](https://easyeda.com) (LCSC component data)

## License

MIT

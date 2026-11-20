# JLCPCB → KiCad

A desktop app for browsing JLCPCB's parts catalog and importing components directly into a KiCad symbol/footprint library.

## Screenshots

![Search results with symbol, footprint, and 3D preview](media/Screenshot_20260528_163650.png)
*Search results with live symbol preview, footprint preview, and colored 3D model*

![Symbol and footprint previews](media/Screenshot_20260528_163740.png)
*Zoomable schematic symbol and PCB footprint side by side*

![Component attributes and symbol detail](media/Screenshot_20260528_163758.png)
*Full component attributes panel — ESP32-S3-WROOM with symbol text position controls*

![3D viewer with colored model and import controls](media/Screenshot_20260528_163840.png)
*Interactive 3D viewer (orbit, pan, zoom) with STEP color extraction and import controls*

## What it does

- Search JLCPCB's full parts database by keyword, value, LCSC number, or category (e.g. `ULN2003`, `100nF 0402`, `C7512`)
- Preview the KiCad schematic symbol and PCB footprint before importing
- View an interactive 3D model of the component on a PCB — supports colored WRL (from EasyEDA) and colored STEP (via [OpenCASCADE](https://dev.opencascade.org/))
- Load a custom STEP or STL file as the 3D model when none is available from JLCPCB
- Export a ready-to-use `.kicad_sym` symbol, `.kicad_mod` footprint, and 3D model (STEP + WRL) into your local KiCad library with one click

## Download

**AppImage** (recommended) — portable, no installation required:
- Download the latest `.AppImage` from [Releases](https://github.com/mirecta/jlcpcb-kicad-import/releases)
- Make executable: `chmod +x jlcpcb-kicad-*.AppImage`
- Run: `./jlcpcb-kicad-*.AppImage`

The AppImage includes all dependencies (OpenCASCADE, Rust runtime, etc.) — no setup needed!

## Building from Source

Requires:
- Rust (stable) — install via [rustup](https://rustup.rs)
- OpenCASCADE 7.8+ — for STEP file parsing with per-face colors

```bash
# Install OpenCASCADE
sudo apt-get install libocct-data-exchange-dev libocct-foundation-dev \
  libocct-modeling-data-dev libocct-modeling-algorithms-dev

# Build
git clone https://github.com/mirecta/jlcpcb-kicad-import
cd jlcpcb-kicad-import
cargo build --release
./target/release/jlcpcb-kicad
```

Or run directly:

```bash
cargo run --release
```

### Building AppImage

```bash
./build-appimage.sh
```

This creates a portable AppImage with all dependencies bundled.

## Usage

1. **Search** — type any query in the search bar and press Enter or click Search. Tick **Basic only** to limit results to JLCPCB's Basic Component library (no extra fee).
2. **Pick a part** — click any row to load the full component detail, symbol preview, footprint preview, and 3D model. In the attributes table, the **Datasheet** row has **🌐 Open** (browser) and **💾 Save as…** (downloads the actual PDF — LCSC's viewer page is resolved to the real file).
3. **Adjust placement** (optional):
   - Drag the symbol or footprint previews to pan; **Ctrl+scroll** to zoom.
   - Drag the 3D viewer to orbit; **middle-drag** to pan; scroll to zoom.
   - Adjust **3D Model Offset / Rotation / Scale** to fine-tune how the model sits on the footprint. Enable **Unified scale** to resize uniformly.
   - Load a **custom STEP** or **custom STL** if no 3D model is available.
4. **Configure library path** — click **Settings** and either **Open existing library…** (pick its `.kicad_sym`) or **Browse folder…**. The folder and library name are detected automatically — picking a `.pretty`/`.3dshapes` folder by mistake is corrected, existing libraries in the folder are listed for one-click selection, and a warning is shown if the name would create a new library.
5. **Browse your library** — the **📚 Library** tab in the left panel lists every component already in the configured library, with footprint/3D status, a filter box, broken footprint links and unused footprints highlighted. It reloads automatically when the files change; click a row to load the part from JLCPCB for preview or re-import. When opened from the Library tab, the 3D viewer shows the library's own model with the transform stored in the footprint — adjust offset / rotation / scale and click **💾 Save 3D transform to library** to write it back (only the `(model …)` block is changed; legacy `(at …)` in inches is converted to `(offset …)` in mm).
6. **Import** — tick Symbol, Footprint, and/or 3D Model, then click **Import**.

After importing, register the library in KiCad:

- **Symbols:** Preferences → Manage Symbol Libraries → add `<lib_path>/<lib_name>.kicad_sym`
- **Footprints:** Preferences → Manage Footprint Libraries → add `<lib_path>/<lib_name>.pretty`
- **3D models:** add a path variable `<LIBNAME>_3D` pointing to `<lib_path>/<lib_name>.3dshapes`

## MCP server (AI agents)

The same binary can run as an [MCP](https://modelcontextprotocol.io) server on stdio, so an AI agent (Claude Code, Claude Desktop, …) can search parts, import them, browse the library and adjust 3D model placement:

```bash
jlcpcb-kicad --mcp                 # or: ./jlcpcb-kicad-*.AppImage --mcp
claude mcp add jlcpcb-kicad -- /path/to/jlcpcb-kicad --mcp   # register in Claude Code
```

It uses the same settings file as the GUI (library folder / name), and every tool accepts `lib_path` + `lib_name` to override them.

| Tool | What it does |
|------|--------------|
| `get_library_config` / `set_library_config` | Show / change the target library; `pick` auto-detects folder + name from any library path |
| `search_parts` | Search JLCPCB (keyword, value, MPN, LCSC id; `basic_only`) |
| `get_part` | Attributes, pins, 3D availability, datasheet, whether it is already in the library |
| `render_part` | Symbol and footprint images |
| `list_library` | Library contents with footprint / 3D status (resolved via KiCad `fp-lib-table`) |
| `import_part` | Import symbol / footprint / 3D model, optional initial 3D transform |
| `refresh_parts` | Regenerate imported parts from fresh JLCPCB data (keeps 3D transform) |
| `get_3d_transform` / `set_3d_transform` | Read / write 3D offset, rotation, scale in a library footprint (absolute or relative) |
| `render_3d` | Render the 3D model on its footprint (iso / top / front / … views), optionally previewing an unsaved transform |
| `save_datasheet` | Download the datasheet PDF |

## Output files

| File | Location |
|------|----------|
| Symbol | `<lib_path>/<lib_name>.kicad_sym` |
| Footprint | `<lib_path>/<lib_name>.pretty/<package>.kicad_mod` |
| 3D model (STEP) | `<lib_path>/<lib_name>.3dshapes/<package>.step` |
| 3D model (WRL) | `<lib_path>/<lib_name>.3dshapes/<package>.wrl` |

## Data sources

- Part metadata, pricing, and stock: [JLCPCB](https://jlcpcb.com/parts)
- Schematic symbols, footprints, and 3D models: [EasyEDA](https://easyeda.com) (LCSC component data)

## License

MIT

# OpenCASCADE STEP Color Support

This project includes optional OpenCASCADE (OCCT) support for **perfect STEP file color rendering** with per-face colors, matching KiCad exactly.

## Why OpenCASCADE?

**Without OpenCASCADE (default):**
- STEP files show "dominant color per shell"
- Multi-shell files (like connectors): ✓ Perfect colors
- Single-shell multi-color files (some JLCPCB parts): ✗ Only one color shown

**With OpenCASCADE:**
- ✓ Perfect per-face colors for ALL STEP files
- ✓ Same quality as KiCad (KiCad uses OpenCASCADE too!)
- ✓ Dark plastic body + gold pins + all details

## Installation

### Ubuntu/Debian

```bash
# Install OpenCASCADE development packages
sudo apt-get install -y \
  libocct-data-exchange-dev \
  libocct-foundation-dev \
  libocct-modeling-data-dev \
  libocct-modeling-algorithms-dev \
  libocct-visualization-dev

# Build with OpenCASCADE support
cargo build --release --features opencascade
```

### Arch Linux

```bash
# Install OpenCASCADE
sudo pacman -S opencascade

# Build with OpenCASCADE support
cargo build --release --features opencascade
```

### Fedora/RHEL

```bash
# Install OpenCASCADE
sudo dnf install OpenCASCADE-devel

# Build with OpenCASCADE support
cargo build --release --features opencascade
```

## Building Without OpenCASCADE

If you don't install OCCT, the project still builds and works fine:

```bash
# Build without OpenCASCADE (fallback STEP parser)
cargo build --release
```

You'll see a warning during build, but the app works normally. STEP files will use the fallback parser (dominant color per shell).

## Technical Details

**OpenCASCADE wrapper:**
- `src/step_occ.cpp` - C++ wrapper using OCCT
- `src/step_occ_ffi.rs` - Rust FFI bindings
- `build.rs` - Compiles C++ code if OCCT available

**Fallback parser:**
- `src/model3d.rs` - Uses `truck` crate
- Works for geometry
- Colors: dominant per shell only

## Troubleshooting

**Build fails with "OCCT not found":**
- Install packages above
- Run `pkg-config --libs occt` to verify installation

**C++ compilation errors:**
- Make sure g++ is installed: `sudo apt-get install build-essential`
- Check OCCT version: `pkg-config --modversion occt` (should be 7.x)

**Link errors:**
- Install all dev packages listed above
- Try: `sudo ldconfig`

## Performance

OpenCASCADE parsing is slightly slower than the fallback (truck), but the difference is negligible for typical component sizes:

- Small components (<1000 faces): ~10-50ms
- Large components (>5000 faces): ~100-300ms

The perfect colors are worth it!

#!/bin/bash
# Build AppImage with bundled OpenCASCADE
set -e

# Load Rust environment
export PATH="$HOME/.cargo/bin:$PATH"

APP_NAME="jlcpcb-kicad"
VERSION=$(git describe --tags --always --dirty 2>/dev/null || echo "dev")
ARCH=$(uname -m)

echo "🔨 Building ${APP_NAME} v${VERSION} for ${ARCH}..."

# Clean previous builds
rm -rf AppDir *.AppImage

# Build release binary with OpenCASCADE
echo "📦 Compiling..."
cargo build --release

# Create AppDir structure
echo "📁 Creating AppDir..."
mkdir -p AppDir/usr/bin
mkdir -p AppDir/usr/lib
mkdir -p AppDir/usr/share/applications
mkdir -p AppDir/usr/share/icons/hicolor/256x256/apps

# Copy binary
cp target/release/${APP_NAME} AppDir/usr/bin/

# Create desktop file
cat > AppDir/usr/share/applications/${APP_NAME}.desktop <<EOF
[Desktop Entry]
Type=Application
Name=JLCPCB KiCad Import
Comment=Browse JLCPCB parts and import to KiCad
Exec=${APP_NAME}
Icon=${APP_NAME}
Categories=Development;Electronics;
Terminal=false
EOF

# Create icon (simple placeholder - can be replaced with actual icon)
cat > AppDir/usr/share/icons/hicolor/256x256/apps/${APP_NAME}.svg <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<svg width="256" height="256" xmlns="http://www.w3.org/2000/svg">
  <rect width="256" height="256" fill="#2196F3"/>
  <text x="128" y="140" font-size="120" text-anchor="middle" fill="white" font-family="sans-serif" font-weight="bold">JK</text>
</svg>
EOF

# Create AppRun
cat > AppDir/AppRun <<'EOF'
#!/bin/bash
# AppImage entry point
SELF=$(readlink -f "$0")
HERE=${SELF%/*}
export PATH="${HERE}/usr/bin:${PATH}"
export LD_LIBRARY_PATH="${HERE}/usr/lib:${LD_LIBRARY_PATH}"
exec "${HERE}/usr/bin/jlcpcb-kicad" "$@"
EOF
chmod +x AppDir/AppRun

# Download linuxdeploy if not present
if [ ! -f linuxdeploy-${ARCH}.AppImage ]; then
    echo "📥 Downloading linuxdeploy..."
    wget -q https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-${ARCH}.AppImage
    chmod +x linuxdeploy-${ARCH}.AppImage
fi

# Bundle dependencies (including OpenCASCADE libs)
echo "📦 Bundling dependencies..."
./linuxdeploy-${ARCH}.AppImage \
    --appdir AppDir \
    --executable target/release/${APP_NAME} \
    --desktop-file AppDir/usr/share/applications/${APP_NAME}.desktop \
    --icon-file AppDir/usr/share/icons/hicolor/256x256/apps/${APP_NAME}.svg \
    --output appimage

# linuxdeploy automatically bundles all shared library dependencies!

# Rename to include version
APPIMAGE_NAME="${APP_NAME}-${VERSION}-${ARCH}.AppImage"
if [ -f "JLCPCB_KiCad_Import-${ARCH}.AppImage" ]; then
    mv "JLCPCB_KiCad_Import-${ARCH}.AppImage" "${APPIMAGE_NAME}"
fi

echo ""
echo "✅ AppImage built successfully!"
echo "📦 ${APPIMAGE_NAME}"
echo ""
echo "Test it:"
echo "  ./${APPIMAGE_NAME}"
echo ""

#!/bin/bash
# One-liner to install OpenCASCADE and rebuild with perfect STEP colors

set -e

echo "🔧 Installing OpenCASCADE..."
sudo apt-get update
sudo apt-get install -y \
  libocct-data-exchange-dev \
  libocct-foundation-dev \
  libocct-modeling-data-dev \
  libocct-modeling-algorithms-dev \
  libocct-visualization-dev

echo ""
echo "✅ OpenCASCADE installed!"
echo ""
echo "🔨 Building with OpenCASCADE support..."
cargo clean
cargo build --release --features opencascade

echo ""
echo "🎉 Done! Run the app with perfect STEP colors!"
echo ""
echo "Test with a JLCPCB component - you should see:"
echo '  [STEP] ✓ OpenCASCADE: XXXX vertices with per-face colors'
echo ""

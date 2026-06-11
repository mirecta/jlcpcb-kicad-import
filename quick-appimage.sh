#!/bin/bash
# Quick AppImage build without questions
set -e

echo "🚀 Quick AppImage build..."
./build-appimage.sh

APPIMAGE=$(ls -t *.AppImage | head -1)
echo ""
echo "✅ Done! Test it:"
echo "   ./${APPIMAGE}"
echo ""

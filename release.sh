#!/bin/bash
# Complete release workflow: build, package, tag, push
set -e

# Check if on main branch
BRANCH=$(git branch --show-current)
if [ "$BRANCH" != "main" ]; then
    echo "⚠️  Not on main branch (currently on: $BRANCH)"
    echo "Switch to main? [y/N]"
    read -r response
    if [[ ! "$response" =~ ^[Yy]$ ]]; then
        exit 1
    fi
    git checkout main
fi

# Check for uncommitted changes
if ! git diff-index --quiet HEAD --; then
    echo "⚠️  Uncommitted changes detected"
    echo "Commit them? [y/N]"
    read -r response
    if [[ "$response" =~ ^[Yy]$ ]]; then
        git add -A
        echo "Commit message:"
        read -r msg
        git commit -m "$msg"
    else
        exit 1
    fi
fi

# Get version from user or use git describe
echo "Version tag (or press Enter for auto-version):"
read -r VERSION
if [ -z "$VERSION" ]; then
    VERSION=$(git describe --tags --always --dirty 2>/dev/null || echo "v0.1.0")
fi

echo ""
echo "🚀 Release workflow for ${VERSION}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Build AppImage
echo "1️⃣  Building AppImage..."
./build-appimage.sh

# Tag version
APPIMAGE=$(ls -t *.AppImage | head -1)
echo ""
echo "2️⃣  Tagging version..."
if git tag -a "${VERSION}" -m "Release ${VERSION}"; then
    echo "✓ Tagged ${VERSION}"
else
    echo "⚠️  Tag already exists, skipping"
fi

# Push to origin
echo ""
echo "3️⃣  Pushing to origin..."
git push origin main
git push origin "${VERSION}" 2>/dev/null || echo "⚠️  Tag already pushed"

# Upload AppImage as release artifact (if gh CLI available)
if command -v gh &> /dev/null; then
    echo ""
    echo "4️⃣  Creating GitHub release..."

    # Check if release exists
    if gh release view "${VERSION}" &>/dev/null; then
        echo "Release ${VERSION} already exists, uploading artifact..."
        gh release upload "${VERSION}" "${APPIMAGE}" --clobber
    else
        echo "Creating new release ${VERSION}..."
        gh release create "${VERSION}" "${APPIMAGE}" \
            --title "Release ${VERSION}" \
            --notes "AppImage with bundled OpenCASCADE for perfect STEP colors"
    fi
else
    echo ""
    echo "4️⃣  GitHub CLI not found, skipping release upload"
    echo "📦 AppImage ready: ${APPIMAGE}"
    echo "Upload manually to GitHub releases"
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "✅ Release complete!"
echo ""
echo "📦 AppImage: ${APPIMAGE}"
echo "🏷️  Tag: ${VERSION}"
echo "🌐 Repository: $(git remote get-url origin)"
echo ""

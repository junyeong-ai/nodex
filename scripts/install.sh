#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
#  nodex installer
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

echo "📦 nodex installer"
echo ""

# 1. Build binary
echo "🔨 Building nodex..."
(cd "$REPO_DIR" && cargo build --release --quiet)
BINARY="$REPO_DIR/target/release/nodex"

if [ ! -f "$BINARY" ]; then
    echo "❌ Build failed: binary not found"
    exit 1
fi

VERSION=$("$BINARY" --version | awk '{print $2}')
echo "✅ Built nodex $VERSION"

# 2. Install binary
echo ""
echo "📂 Installing binary to $INSTALL_DIR..."
mkdir -p "$INSTALL_DIR"
cp "$BINARY" "$INSTALL_DIR/nodex"
chmod +x "$INSTALL_DIR/nodex"

if [[ "$OSTYPE" == darwin* ]]; then
    codesign -s - "$INSTALL_DIR/nodex" 2>/dev/null || true
fi

echo "✅ Binary installed: $INSTALL_DIR/nodex"

if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
    echo ""
    echo "⚠️  $INSTALL_DIR is not in your PATH."
    echo "   Add to your shell profile:"
    echo "   export PATH=\"$INSTALL_DIR:\$PATH\""
fi

# 3. Install skill
echo ""
echo "🧠 Skill installation"
echo "   1) ~/.claude/skills/nodex   (user-level — all projects, recommended)"
echo "   2) ./.claude/skills/nodex   (project-level — shared via git)"
echo "   3) Skip"
read -rp "   Choice [1]: " choice
choice="${choice:-1}"

SKILL_SRC="$REPO_DIR/.claude/skills/nodex"

install_skill() {
    local target="$1"
    if [ -d "$target" ]; then
        BACKUP="${target}.backup_$(date +%Y%m%d_%H%M%S)"
        cp -r "$target" "$BACKUP"
        echo "   📋 Backup: $BACKUP"
        rm -rf "$target"
    fi
    mkdir -p "$target"
    cp -r "$SKILL_SRC/." "$target/"
    echo "   ✅ Installed: $target"
}

case "$choice" in
    1) install_skill "$HOME/.claude/skills/nodex" ;;
    2) install_skill "$(pwd)/.claude/skills/nodex"
       echo "   💡 Commit .claude/skills/nodex/ to share with your team." ;;
    *) echo "   Skipped." ;;
esac

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "✅ Installation complete!"
echo ""
echo "Next steps:"
echo "  nodex init          # Initialize nodex.toml in your project"
echo "  nodex build         # Build the document graph"
echo "  nodex query search  # Search documents"
echo "  /nodex              # Use as Claude Code skill"

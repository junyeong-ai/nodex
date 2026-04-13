#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
USER_SKILL_DIR="$HOME/.claude/skills/nodex"

echo "🗑️  nodex uninstaller"
echo ""

# 1. Remove binary
BINARY="$INSTALL_DIR/nodex"
if [ -f "$BINARY" ]; then
    rm "$BINARY"
    echo "✅ Removed binary: $BINARY"
else
    echo "⚠️  Binary not found: $BINARY"
fi

# 2. Remove user-level skill
if [ -d "$USER_SKILL_DIR" ]; then
    echo ""
    read -rp "Remove skill at $USER_SKILL_DIR? [y/N] " answer
    if [[ "$answer" =~ ^[Yy]$ ]]; then
        read -rp "Create backup first? [Y/n] " backup
        backup="${backup:-Y}"
        if [[ "$backup" =~ ^[Yy]$ ]]; then
            BACKUP="${USER_SKILL_DIR}.backup_$(date +%Y%m%d_%H%M%S)"
            cp -r "$USER_SKILL_DIR" "$BACKUP"
            echo "📋 Backup: $BACKUP"
        fi
        rm -rf "$USER_SKILL_DIR"
        echo "✅ Removed skill: $USER_SKILL_DIR"

        # Clean empty parent
        SKILLS_DIR="$HOME/.claude/skills"
        if [ -d "$SKILLS_DIR" ] && [ -z "$(ls -A "$SKILLS_DIR")" ]; then
            rmdir "$SKILLS_DIR"
        fi
    fi
fi

echo ""
echo "✅ Uninstallation complete."
echo "   Project-level skills (.claude/skills/nodex/) are managed by git."

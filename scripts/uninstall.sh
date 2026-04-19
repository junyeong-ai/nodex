#!/usr/bin/env bash
# nodex uninstaller — see `./uninstall.sh --help` for full usage.
set -euo pipefail

BINARY_NAME="nodex"
SKILL_NAME="nodex"

INSTALL_DIR="${NODEX_INSTALL_DIR:-$HOME/.local/bin}"
NODEX_KEEP_SKILL="${NODEX_KEEP_SKILL:-0}"
NODEX_KEEP_BACKUP="${NODEX_KEEP_BACKUP:-0}"
NODEX_YES="${NODEX_YES:-0}"

INPUT_FD=""

die()      { printf '%s✗ %s%s\n' "$C_RED" "$*" "$C_RESET" >&2; exit 1; }
log_info() { printf '%s  %s%s\n' "$C_DIM" "$*" "$C_RESET"; }
log_warn() { printf '%s!  %s%s\n' "$C_YELLOW" "$*" "$C_RESET"; }
log_ok()   { printf '%s✓  %s%s\n' "$C_GREEN" "$*" "$C_RESET"; }
render_step() { printf '%s▸  %s%s\n' "$C_BLUE" "$*" "$C_RESET"; }

init_colors() {
    if [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-}" != "dumb" ]; then
        C_RESET=$'\033[0m'; C_DIM=$'\033[2m'
        C_RED=$'\033[31m'; C_GREEN=$'\033[32m'
        C_YELLOW=$'\033[33m'; C_BLUE=$'\033[34m'; C_BOLD=$'\033[1m'
    else
        C_RESET="" C_DIM="" C_RED="" C_GREEN="" C_YELLOW="" C_BLUE="" C_BOLD=""
    fi
}

detect_tty() {
    if [ "$NODEX_YES" = "1" ]; then INPUT_FD=""; return 1; fi
    if [ -t 0 ]; then INPUT_FD="0"; return 0; fi
    if [ -e /dev/tty ] && [ -r /dev/tty ]; then INPUT_FD="/dev/tty"; return 0; fi
    INPUT_FD=""; return 1
}

read_line() {
    local answer
    if [ "$INPUT_FD" = "0" ]; then IFS= read -r answer || answer=""
    else IFS= read -r answer < /dev/tty || answer=""
    fi
    printf '%s' "$answer"
}

prompt_yesno() {
    local question="$1" default="$2" answer
    if [ -z "$INPUT_FD" ]; then
        [ "$default" = "Y" ] && return 0 || return 1
    fi
    local hint; [ "$default" = "Y" ] && hint="[Y/n]" || hint="[y/N]"
    printf '%s%s%s %s ' "$C_BOLD" "$question" "$C_RESET" "$hint" >&2
    answer="$(read_line)"
    answer="${answer:-$default}"
    case "$answer" in [Yy]*) return 0 ;; *) return 1 ;; esac
}

backup_path() {
    local target="$1"
    [ -e "$target" ] || return 0
    local backup="${target}.backup_$(date +%Y%m%d_%H%M%S)"
    cp -r "$target" "$backup"
    log_info "Backup: $backup"
}

uninstall_binary() {
    local dest="${INSTALL_DIR}/${BINARY_NAME}"
    render_step "Removing binary"
    if [ -f "$dest" ]; then
        rm -f "$dest"
        log_ok "Removed $dest"
    else
        log_info "Binary not found at $dest"
    fi
}

uninstall_skill() {
    local target="$HOME/.claude/skills/${SKILL_NAME}"
    if [ ! -d "$target" ]; then
        log_info "No user-level skill at $target"
        return
    fi
    if [ "$NODEX_KEEP_SKILL" = "1" ]; then
        log_info "Keeping skill (--keep-skill)"
        return
    fi
    # --yes means non-interactive full cleanup. Only interactive runs are
    # asked (with a conservative default of N) since skills can outlive the
    # binary when shared across projects.
    if [ "$NODEX_YES" != "1" ] && ! prompt_yesno "Remove skill at $target?" "N"; then
        log_info "Skill kept"; return
    fi
    render_step "Removing skill"
    [ "$NODEX_KEEP_BACKUP" = "1" ] || backup_path "$target"
    rm -rf "$target"
    log_ok "Removed $target"

    local parent="$HOME/.claude/skills"
    if [ -d "$parent" ] && [ -z "$(ls -A "$parent")" ]; then
        rmdir "$parent"
        log_info "Cleaned empty $parent"
    fi
}

parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --install-dir) INSTALL_DIR="$2"; shift 2 ;;
            --keep-skill)  NODEX_KEEP_SKILL=1; shift ;;
            --keep-backup) NODEX_KEEP_BACKUP=1; shift ;;
            --yes|-y)      NODEX_YES=1; shift ;;
            --help|-h)     print_usage; exit 0 ;;
            *)             die "Unknown flag: $1" ;;
        esac
    done
}

print_usage() {
    cat <<'USAGE'
nodex uninstaller

Usage:
  curl -fsSL https://raw.githubusercontent.com/junyeong-ai/nodex/main/scripts/uninstall.sh | bash
  ./scripts/uninstall.sh [flags]

Flags:
  --install-dir PATH  Binary directory (default: $HOME/.local/bin)
  --keep-skill        Do not remove user-level skill
  --keep-backup       Do not back up skill before removing
  --yes, -y           Non-interactive full cleanup (binary + skill)
  --help, -h          Show this message

Environment variables (flags win over env, env wins over defaults):
  NODEX_INSTALL_DIR, NODEX_KEEP_SKILL, NODEX_KEEP_BACKUP, NODEX_YES, NO_COLOR
USAGE
}

main() {
    init_colors
    parse_args "$@"
    detect_tty || true

    printf '\n%s%snodex uninstaller%s\n\n' "$C_BOLD" "" "$C_RESET"
    uninstall_binary
    uninstall_skill
    printf '\n%s✅ Uninstall complete%s\n' "$C_GREEN$C_BOLD" "$C_RESET"
    log_info "Project-level skills (.claude/skills/${SKILL_NAME}/) are managed by git"
}

main "$@"

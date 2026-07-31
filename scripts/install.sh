#!/usr/bin/env bash
# nodex installer — see `./install.sh --help` for full usage.
set -euo pipefail

REPO="junyeong-ai/nodex"
BINARY_NAME="nodex"
SKILL_NAME="nodex"
LATEST_URL="https://github.com/${REPO}/releases/latest"
RELEASE_BASE="https://github.com/${REPO}/releases/download"

# ── settings (env wins over built-in default; flags win over env) ─────────
#
# Each setting has a matching EXPLICIT_* flag that is set to 1 when the user
# overrode the default via either environment variable or CLI flag. Prompts
# are skipped for any setting whose EXPLICIT_* flag is 1.
#
# EXPLICIT_* must be evaluated BEFORE the default substitution below, while
# the original env var is still distinguishable from "unset".
EXPLICIT_INSTALL_DIR=0;  [ -n "${NODEX_INSTALL_DIR:-}" ]  && EXPLICIT_INSTALL_DIR=1
EXPLICIT_VERSION=0;      [ -n "${NODEX_VERSION:-}" ]      && EXPLICIT_VERSION=1
EXPLICIT_SKILL_LEVEL=0;  [ -n "${NODEX_SKILL_LEVEL:-}" ]  && EXPLICIT_SKILL_LEVEL=1
EXPLICIT_FROM_SOURCE=0;  [ "${NODEX_FROM_SOURCE:-0}" = "1" ] && EXPLICIT_FROM_SOURCE=1

INSTALL_DIR="${NODEX_INSTALL_DIR:-$HOME/.local/bin}"
NODEX_VERSION="${NODEX_VERSION:-}"
NODEX_SKILL_LEVEL="${NODEX_SKILL_LEVEL:-}"
NODEX_FROM_SOURCE="${NODEX_FROM_SOURCE:-0}"
NODEX_FORCE="${NODEX_FORCE:-0}"
NODEX_YES="${NODEX_YES:-0}"
DRY_RUN="${NODEX_DRY_RUN:-0}"

# ── runtime state ──────────────────────────────────────────────────────────
INPUT_FD=""
TMP_DIR=""
USE_UTF8=0
# Colors are set by init_colors(); declared empty here so any code path that
# triggers `die` before init_colors runs (there shouldn't be any, but set -u
# is strict) still finds the variables bound.
C_RESET=""; C_DIM=""; C_RED=""; C_GREEN=""; C_YELLOW=""; C_BLUE=""; C_BOLD=""

# ═════════════════════════════ UTIL ════════════════════════════════════════

# All human-visible output goes to stderr so stdout is reserved for values
# captured via command substitution (e.g. `$(build_from_source ...)`).
die()      { printf '%s✗ %s%s\n' "$C_RED" "$*" "$C_RESET" >&2; exit 1; }
log_info() { printf '%s  %s%s\n' "$C_DIM" "$*" "$C_RESET" >&2; }
log_warn() { printf '%s!  %s%s\n' "$C_YELLOW" "$*" "$C_RESET" >&2; }
log_ok()   { printf '%s✓  %s%s\n' "$C_GREEN" "$*" "$C_RESET" >&2; }
render_step() { printf '%s▸  %s%s\n' "$C_BLUE" "$*" "$C_RESET" >&2; }

init_colors() {
    if [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-}" != "dumb" ]; then
        C_RESET=$'\033[0m'
        C_DIM=$'\033[2m'
        C_RED=$'\033[31m'
        C_GREEN=$'\033[32m'
        C_YELLOW=$'\033[33m'
        C_BLUE=$'\033[34m'
        C_BOLD=$'\033[1m'
    else
        C_RESET="" C_DIM="" C_RED="" C_GREEN="" C_YELLOW="" C_BLUE="" C_BOLD=""
    fi
    case "${LANG:-}${LC_ALL:-}" in *UTF-8*|*utf8*) USE_UTF8=1 ;; esac
}

detect_tty() {
    if [ "$NODEX_YES" = "1" ]; then INPUT_FD=""; return 1; fi
    if [ -t 0 ]; then INPUT_FD="0"; return 0; fi
    if [ -e /dev/tty ] && [ -r /dev/tty ]; then INPUT_FD="/dev/tty"; return 0; fi
    INPUT_FD=""; return 1
}

read_line() {
    local answer
    if [ "$INPUT_FD" = "0" ]; then
        IFS= read -r answer || answer=""
    else
        IFS= read -r answer < /dev/tty || answer=""
    fi
    printf '%s' "$answer"
}

# ═════════════════════════════ PROMPTS ═════════════════════════════════════

prompt_choice() {
    # prompt_choice "Title" default_idx "opt1" "opt2" …
    local title="$1"; shift
    local default_idx="$1"; shift
    local options=("$@")
    local i answer

    if [ -z "$INPUT_FD" ]; then
        printf '%s\n' "${options[$((default_idx - 1))]}"
        return 0
    fi

    printf '\n%s%s%s\n' "$C_BOLD" "$title" "$C_RESET" >&2
    for i in "${!options[@]}"; do
        printf '  %s%d)%s %s\n' "$C_DIM" "$((i + 1))" "$C_RESET" "${options[$i]}" >&2
    done
    for _ in 1 2 3; do
        printf '%s❯ [%d]%s ' "$C_BLUE" "$default_idx" "$C_RESET" >&2
        answer="$(read_line)"
        answer="${answer:-$default_idx}"
        if [[ "$answer" =~ ^[0-9]+$ ]] && [ "$answer" -ge 1 ] && [ "$answer" -le "${#options[@]}" ]; then
            printf '%s\n' "${options[$((answer - 1))]}"
            return 0
        fi
        log_warn "Invalid choice: $answer" >&2
    done
    die "Too many invalid responses"
}

prompt_yesno() {
    # prompt_yesno "Question?" default(Y|N)
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

prompt_path() {
    local question="$1" default="$2" answer
    if [ -z "$INPUT_FD" ]; then printf '%s\n' "$default"; return; fi
    printf '%s%s%s [%s] ' "$C_BOLD" "$question" "$C_RESET" "$default" >&2
    answer="$(read_line)"
    answer="${answer:-$default}"
    case "$answer" in "~"*) answer="$HOME${answer:1}" ;; esac
    printf '%s\n' "$answer"
}

# ═════════════════════════════ DETECT ══════════════════════════════════════

detect_platform() {
    local os arch
    os="$(uname -s | tr '[:upper:]' '[:lower:]')"
    arch="$(uname -m)"
    case "$os-$arch" in
        linux-x86_64)          echo "x86_64-unknown-linux-musl" ;;
        linux-aarch64|linux-arm64) echo "aarch64-unknown-linux-musl" ;;
        darwin-*)              echo "universal-apple-darwin" ;;
        *) die "Unsupported platform: $os/$arch" ;;
    esac
}

fetch_latest_version() {
    # Resolve through GitHub's stable HTML redirect
    # (https://github.com/OWNER/REPO/releases/latest → 302 /releases/tag/vX.Y.Z)
    # rather than api.github.com. The HTML path is auth-free and not subject
    # to the 60-req/hr unauthenticated API rate limit, which routinely
    # exhausts on shared egress IPs (CI runners, corporate NAT, VPN exits).
    local final
    final="$(curl -fsSLI --retry 3 --retry-delay 2 -o /dev/null \
        -w '%{url_effective}' "$LATEST_URL")" || return 1
    case "$final" in
        */tag/v*) printf '%s\n' "${final##*/tag/v}" ;;
        *) return 1 ;;
    esac
}

resolve_version() {
    if [ -n "$NODEX_VERSION" ]; then echo "$NODEX_VERSION"; return; fi
    local v; v="$(fetch_latest_version || true)"
    [ -n "$v" ] || die "Cannot fetch latest version (network issue or no release exists yet)"
    echo "$v"
}

# ═════════════════════════════ DOWNLOAD/INSTALL ════════════════════════════

download_archive() {
    local version="$1" archive_name="$2"
    local url="${RELEASE_BASE}/v${version}/${archive_name}"
    render_step "Downloading ${archive_name}"
    curl -fL --retry 3 --retry-delay 2 --progress-bar \
        -o "${TMP_DIR}/${archive_name}" "$url" \
        || die "Download failed: $url"
    curl -fsSL --retry 3 --retry-delay 2 \
        -o "${TMP_DIR}/${archive_name}.sha256" "${url}.sha256" \
        || die "Checksum download failed: ${url}.sha256"
    log_ok "Downloaded"
}

verify_checksum() {
    local archive="$1"
    render_step "Verifying SHA256"
    ( cd "$TMP_DIR" && {
        if command -v sha256sum >/dev/null 2>&1; then
            sha256sum -c "${archive}.sha256" >/dev/null
        elif command -v shasum >/dev/null 2>&1; then
            shasum -a 256 -c "${archive}.sha256" >/dev/null
        else
            die "No sha256 tool found (need sha256sum or shasum)"
        fi
    } ) || die "Checksum mismatch for ${archive}"
    log_ok "Checksum match"
}

extract_archive() {
    local archive="$1"
    render_step "Extracting"
    case "$archive" in
        *.tar.gz) tar -xzf "${TMP_DIR}/${archive}" -C "${TMP_DIR}" ;;
        *.zip)    unzip -q "${TMP_DIR}/${archive}" -d "${TMP_DIR}" ;;
        *)        die "Unknown archive format: $archive" ;;
    esac
    log_ok "Extracted"
}

strip_quarantine() {
    [ "$(uname -s)" = "Darwin" ] || return 0
    command -v xattr >/dev/null 2>&1 || return 0
    xattr -d com.apple.quarantine "$1" 2>/dev/null || true
}

codesign_adhoc() {
    [ "$(uname -s)" = "Darwin" ] || return 0
    command -v codesign >/dev/null 2>&1 || return 0
    codesign --force --sign - "$1" 2>/dev/null || true
}

install_binary() {
    local src="$1"
    local dest_dir="$2"
    local dest="${dest_dir}/${BINARY_NAME}"
    render_step "Installing binary to ${dest}"
    mkdir -p "$dest_dir"
    cp "$src" "$dest.tmp"
    chmod +x "$dest.tmp"
    strip_quarantine "$dest.tmp"
    codesign_adhoc "$dest.tmp"
    mv "$dest.tmp" "$dest"
    log_ok "$dest"
}

build_from_source() {
    local repo_dir="$1"
    render_step "Building from source (cargo build --release)"
    command -v cargo >/dev/null 2>&1 || die "cargo not found — install Rust from https://rustup.rs"
    ( cd "$repo_dir" && cargo build --release --quiet --package nodex-cli ) || die "cargo build failed"
    echo "${repo_dir}/target/release/${BINARY_NAME}"
}

# ═════════════════════════════ SKILL ═══════════════════════════════════════

# SemVer §11 precedence. `sort -V` cannot answer this: it orders version
# strings the way a filename listing wants them and has no notion of a
# prerelease ranking below its own release, so implementations disagree about
# `1.0.0-rc.1` against `1.0.0` and the installer's answer would depend on
# which `sort` is on PATH. Here the rule is the specification: numeric
# major/minor/patch, then a version carrying a prerelease ranks below the same
# version without one, then dot-separated identifiers compared field by field —
# numeric ones numerically and below alphanumeric ones, the rest byte-wise, and
# a shorter run of equal fields ranks lower. Build metadata is ignored.
# --- version precedence: lifted whole by scripts/test-compare-versions.sh ---
# Whether a version's core and prerelease are shapes the specification allows:
# one to three numeric core fields with no leading zero, and a prerelease of
# non-empty identifiers whose numeric ones carry no leading zero either.
semver_shaped() {
    local core="$1" pre="$2" field seen=0
    while [ -n "$core" ]; do
        field="${core%%.*}"
        case "$core" in
            *.*) core="${core#*.}"; [ -n "$core" ] || return 1 ;;
            *) core="" ;;
        esac
        case "$field" in ""|*[!0-9]*|0?*) return 1 ;; esac
        seen=$((seen + 1))
        [ "$seen" -le 3 ] || return 1
    done
    [ "$seen" -ge 1 ] || return 1
    [ -n "${2-}" ] || return 0
    while [ -n "$pre" ]; do
        field="${pre%%.*}"
        case "$pre" in
            *.*) pre="${pre#*.}"; [ -n "$pre" ] || return 1 ;;
            *) pre="" ;;
        esac
        case "$field" in "") return 1 ;; *[!0-9]*) ;; 0?*) return 1 ;; esac
    done
    return 0
}

# Order two runs of digits without the shell's integer range: the longer run is
# the larger number and equal lengths compare byte-wise. `[ -lt ]` would fail
# on anything past intmax_t, leaving both comparisons false and the answer
# silently "equal". A leading zero is not a number this can order — SemVer
# forbids one in a numeric identifier — so it is said to be unusable rather
# than normalised into a decision.
compare_numeric() {
    local a="$1" b="$2"
    case "$a" in 0?*) echo "unknown"; return ;; esac
    case "$b" in 0?*) echo "unknown"; return ;; esac
    if [ "${#a}" -ne "${#b}" ]; then
        [ "${#a}" -lt "${#b}" ] && echo "older" || echo "newer"
    elif [ "$a" \< "$b" ]; then echo "older"
    elif [ "$a" \> "$b" ]; then echo "newer"
    else echo "equal"
    fi
}

compare_versions() {
    # echoes: equal | older | newer | unknown
    # The only normalization is stripping a leading "v" so "v1.2.3" matches
    # "1.2.3"; "+build" is dropped because it never affects precedence.
    local a="${1#v}" b="${2#v}"
    a="${a%%+*}"; b="${b%%+*}"
    { [ -n "$a" ] && [ -n "$b" ]; } || { echo "unknown"; return; }
    # One string is itself whatever shape it is, and saying so is both true and
    # the more useful answer: it reaches the "already installed" prompt rather
    # than silently proceeding.
    [ "$a" = "$b" ] && { echo "equal"; return; }

    local a_rest="${a%%-*}" b_rest="${b%%-*}"
    local a_pre="" b_pre=""
    # A trailing `-` is only visible here: an empty prerelease is not a shape
    # the specification allows, and `${a#*-}` cannot tell it from none at all.
    case "$a" in *-*) a_pre="${a#*-}"; [ -n "$a_pre" ] || { echo "unknown"; return; } ;; esac
    case "$b" in *-*) b_pre="${b#*-}"; [ -n "$b_pre" ] || { echo "unknown"; return; } ;; esac
    # Shape before order: a version the specification does not allow is
    # unusable whatever it is being compared against, and deciding that only
    # once earlier fields tie would let a malformed one suppress an install.
    semver_shaped "$a_rest" "$a_pre" || { echo "unknown"; return; }
    semver_shaped "$b_rest" "$b_pre" || { echo "unknown"; return; }

    local i a_f b_f rel
    for i in 1 2 3; do
        a_f="${a_rest%%.*}"; b_f="${b_rest%%.*}"
        case "$a_rest" in *.*) a_rest="${a_rest#*.}" ;; *) a_rest="" ;; esac
        case "$b_rest" in *.*) b_rest="${b_rest#*.}" ;; *) b_rest="" ;; esac
        a_f="${a_f:-0}"; b_f="${b_f:-0}"
        case "$a_f$b_f" in *[!0-9]*) echo "unknown"; return ;; esac
        rel="$(compare_numeric "$a_f" "$b_f")"
        [ "$rel" = "equal" ] || { echo "$rel"; return; }
    done

    # Same core: a prerelease ranks below the release it precedes.
    [ -z "$a_pre" ] && [ -n "$b_pre" ] && { echo "newer"; return; }
    [ -n "$a_pre" ] && [ -z "$b_pre" ] && { echo "older"; return; }
    [ "$a_pre" = "$b_pre" ] && { echo "equal"; return; }

    a_rest="$a_pre"; b_rest="$b_pre"
    local a_num b_num
    while :; do
        # A shorter run of otherwise-equal identifiers ranks lower.
        [ -z "$a_rest" ] && [ -z "$b_rest" ] && { echo "equal"; return; }
        [ -z "$a_rest" ] && { echo "older"; return; }
        [ -z "$b_rest" ] && { echo "newer"; return; }
        a_f="${a_rest%%.*}"; b_f="${b_rest%%.*}"
        case "$a_rest" in *.*) a_rest="${a_rest#*.}" ;; *) a_rest="" ;; esac
        case "$b_rest" in *.*) b_rest="${b_rest#*.}" ;; *) b_rest="" ;; esac
        case "$a_f" in *[!0-9]*) a_num=0 ;; *) a_num=1 ;; esac
        case "$b_f" in *[!0-9]*) b_num=0 ;; *) b_num=1 ;; esac
        if [ "$a_num" = 1 ] && [ "$b_num" = 1 ]; then
            rel="$(compare_numeric "$a_f" "$b_f")"
            [ "$rel" = "equal" ] || { echo "$rel"; return; }
        elif [ "$a_num" != "$b_num" ]; then
            # Numeric identifiers always rank below alphanumeric ones.
            if [ "$a_num" = 1 ]; then echo "older"; else echo "newer"; fi
            return
        else
            [ "$a_f" \< "$b_f" ] && { echo "older"; return; }
            [ "$a_f" \> "$b_f" ] && { echo "newer"; return; }
        fi
    done
}
# --- end version precedence ---

# Skills ship inside release tarballs named `${BINARY_NAME}-skill-v${X}.tar.gz`.
# SKILL.md carries a `version:` frontmatter field the release gate keeps in
# lockstep with the binary, but identical content is the real "nothing
# changed" signal — the hash also catches local edits a version compare
# would miss.
skill_sha256() {
    local skill_md="$1"
    [ -f "$skill_md" ] || { echo ""; return; }
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$skill_md" | awk '{print $1}'
    else
        shasum -a 256 "$skill_md" | awk '{print $1}'
    fi
}

backup_path() {
    local target="$1"
    [ -e "$target" ] || return 0
    # Backups live outside the live skills tree: Claude Code registers any
    # <dir>/SKILL.md under a skills root as an installed skill, so a sibling
    # copy would surface as a duplicate command.
    local backup_root; backup_root="$(dirname "$target").backup"
    # Include PID so two runs in the same second never collide.
    local backup="${backup_root}/$(basename "$target")_$(date +%Y%m%d_%H%M%S)_$$"
    mkdir -p "$backup_root"
    cp -r "$target" "$backup"
    log_info "Backup: $backup"
}

# Download and extract the skill release asset to a temp directory,
# verify its checksum, and echo the extracted skill path on stdout.
# Echoes empty string on failure (caller handles "skip").
download_skill_tarball() {
    local version="$1"
    local archive="${BINARY_NAME}-skill-v${version}.tar.gz"
    local url="${RELEASE_BASE}/v${version}/${archive}"
    local dest="${TMP_DIR}/skill-src"

    render_step "Downloading skill ${archive}"
    if ! curl -fsSL --retry 3 --retry-delay 2 -o "${TMP_DIR}/${archive}" "$url" 2>/dev/null; then
        log_warn "Skill archive unavailable at $url; skipping skill install"
        echo ""; return 0
    fi
    if ! curl -fsSL --retry 3 --retry-delay 2 -o "${TMP_DIR}/${archive}.sha256" "${url}.sha256" 2>/dev/null; then
        log_warn "Skill checksum unavailable; skipping skill install"
        echo ""; return 0
    fi
    ( cd "$TMP_DIR" && {
        if command -v sha256sum >/dev/null 2>&1; then sha256sum -c "${archive}.sha256" >/dev/null
        else shasum -a 256 -c "${archive}.sha256" >/dev/null
        fi
    } ) || { log_warn "Skill checksum mismatch; skipping skill install"; echo ""; return 0; }
    mkdir -p "$dest"
    tar -xzf "${TMP_DIR}/${archive}" -C "${TMP_DIR}"
    echo "${TMP_DIR}/${SKILL_NAME}"
}

install_skill() {
    local level="$1" src="$2"
    [ "$level" = "none" ] && { log_info "Skill install skipped"; return; }
    [ -d "$src" ] || die "Skill source not found: $src — the archive did not hold the \
expected layout, so the binary would stand without a matching skill."

    local target
    case "$level" in
        user)    target="$HOME/.claude/skills/${SKILL_NAME}" ;;
        project) target="$(pwd)/.claude/skills/${SKILL_NAME}" ;;
        *)       die "Invalid skill level: $level" ;;
    esac

    render_step "Installing skill → $target"
    if [ -d "$target" ]; then
        # Decide reinstall by content hash rather than the frontmatter
        # `version:` field — identical content is the real "nothing
        # changed" signal, and the hash also catches local edits a
        # version compare would miss.
        local existing new
        existing="$(skill_sha256 "$target/SKILL.md")"
        new="$(skill_sha256 "$src/SKILL.md")"
        if [ -n "$existing" ] && [ "$existing" = "$new" ]; then
            if [ "$NODEX_FORCE" != "1" ] && ! prompt_yesno "Skill is already current. Reinstall?" "N"; then
                log_info "Skill kept"
                return
            fi
        fi
        backup_path "$target"
        rm -rf "$target"
    fi
    mkdir -p "$(dirname "$target")"
    cp -r "$src" "$target"
    log_ok "Skill installed"
}

# ═════════════════════════════ ORCHESTRATION ═══════════════════════════════

print_usage() {
    cat <<'USAGE'
nodex installer

Usage:
  curl -fsSL https://raw.githubusercontent.com/junyeong-ai/nodex/main/scripts/install.sh | bash
  ./scripts/install.sh [flags]

Flags:
  --version VERSION          Install specific version (default: latest)
  --install-dir PATH         Install binary here (default: $HOME/.local/bin)
  --skill user|project|none  Skill install level (default: user)
  --from-source              Build from source instead of downloading prebuilt
  --force                    Overwrite existing install without prompting
  --yes, -y                  Accept all defaults non-interactively
  --dry-run                  Print plan, do not execute
  --help, -h                 Show this message

Environment variables (flags win over env, env wins over defaults):
  NODEX_INSTALL_DIR, NODEX_VERSION, NODEX_SKILL_LEVEL,
  NODEX_FROM_SOURCE, NODEX_FORCE, NODEX_YES, NODEX_DRY_RUN, NO_COLOR
USAGE
}

parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --version)       NODEX_VERSION="$2"; EXPLICIT_VERSION=1; shift 2 ;;
            --install-dir)   INSTALL_DIR="$2"; EXPLICIT_INSTALL_DIR=1; shift 2 ;;
            --skill)         NODEX_SKILL_LEVEL="$2"; EXPLICIT_SKILL_LEVEL=1; shift 2 ;;
            --from-source)   NODEX_FROM_SOURCE=1; EXPLICIT_FROM_SOURCE=1; shift ;;
            --force)         NODEX_FORCE=1; shift ;;
            --yes|-y)        NODEX_YES=1; shift ;;
            --dry-run)       DRY_RUN=1; shift ;;
            --help|-h)       print_usage; exit 0 ;;
            *)               die "Unknown flag: $1 (use --help)" ;;
        esac
    done
}

render_banner() {
    local platform="$1" version="$2"
    local top bot
    if [ "$USE_UTF8" = "1" ]; then
        top="╭──────────────────────────────────────────╮"
        bot="╰──────────────────────────────────────────╯"
    else
        top="+------------------------------------------+"
        bot="+------------------------------------------+"
    fi
    printf '\n%s%s%s\n' "$C_BOLD" "$top" "$C_RESET"
    printf '%s  nodex installer%s\n' "$C_BOLD" "$C_RESET"
    printf '%s  v%s • %s%s\n' "$C_DIM" "$version" "$platform" "$C_RESET"
    printf '%s%s%s\n' "$C_BOLD" "$bot" "$C_RESET"
}

render_review() {
    local method="$1" dest="$2" skill_level="$3" version="$4"
    printf '\n%sReview%s\n' "$C_BOLD" "$C_RESET"
    printf '  %sbinary%s  %s (v%s, %s)\n' "$C_DIM" "$C_RESET" "$dest" "$version" "$method"
    case "$skill_level" in
        user)    printf '  %sskill%s   ~/.claude/skills/%s\n' "$C_DIM" "$C_RESET" "$SKILL_NAME" ;;
        project) printf '  %sskill%s   ./.claude/skills/%s\n' "$C_DIM" "$C_RESET" "$SKILL_NAME" ;;
        none)    printf '  %sskill%s   (skipped)\n' "$C_DIM" "$C_RESET" ;;
    esac
}

check_path() {
    local dir="$1"
    case ":$PATH:" in
        *":$dir:"*) log_ok "$dir is in PATH" ;;
        *)
            log_warn "$dir is not in PATH"
            echo "   Add to your shell profile:"
            echo "     export PATH=\"$dir:\$PATH\""
            ;;
    esac
}

cleanup() { [ -n "$TMP_DIR" ] && [ -d "$TMP_DIR" ] && rm -rf "$TMP_DIR"; }

# Returns 0 if the given path is writable either directly (exists) or
# creatable (parent is writable). Does NOT create the directory.
check_writable() {
    local dir="$1"
    if [ -e "$dir" ]; then
        [ -w "$dir" ]
    else
        local parent; parent="$(dirname "$dir")"
        [ -d "$parent" ] && [ -w "$parent" ]
    fi
}

main() {
    init_colors
    parse_args "$@"
    trap cleanup EXIT INT TERM
    TMP_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t nodex-install)"

    detect_tty || true
    local platform version method dest skill_level binary_src
    platform="$(detect_platform)"

    local repo_dir=""
    if [ -f "$(dirname "$0")/../Cargo.toml" ]; then
        repo_dir="$(cd "$(dirname "$0")/.." && pwd)"
    fi

    # Resolve method
    if [ "$EXPLICIT_FROM_SOURCE" = "1" ] || [ -z "$INPUT_FD" ]; then
        method=$([ "$NODEX_FROM_SOURCE" = "1" ] && echo "source" || echo "prebuilt")
    else
        local pick
        pick="$(prompt_choice "Install method" 1 \
            "Prebuilt binary        (recommended)" \
            "Build from source      (requires Rust)")"
        case "$pick" in Prebuilt*) method="prebuilt" ;; Build*) method="source" ;; esac
    fi

    # Resolve version (only needed for prebuilt download)
    local checkout_version=""
    if [ -n "$repo_dir" ]; then
        checkout_version="$(grep -m1 '^version' "$repo_dir/Cargo.toml" 2>/dev/null | cut -d'"' -f2 || echo "")"
    fi
    if [ "$method" = "prebuilt" ]; then
        version="$(resolve_version)"
    else
        version="${checkout_version:-dev}"
    fi

    render_banner "$platform" "$version"

    # Resolve install dir (skip prompt when user explicitly overrode)
    if [ -n "$INPUT_FD" ] && [ "$EXPLICIT_INSTALL_DIR" != "1" ]; then
        local loc
        loc="$(prompt_choice "Install location" 1 \
            "~/.local/bin          (recommended)" \
            "/usr/local/bin        (may need sudo)" \
            "Custom path…")"
        case "$loc" in
            "~/.local/bin"*)   INSTALL_DIR="$HOME/.local/bin" ;;
            "/usr/local/bin"*) INSTALL_DIR="/usr/local/bin" ;;
            "Custom"*)         INSTALL_DIR="$(prompt_path "Install path" "$HOME/.local/bin")" ;;
        esac
    fi
    dest="${INSTALL_DIR}/${BINARY_NAME}"

    # Resolve skill level (skip prompt when user explicitly overrode)
    if [ "$EXPLICIT_SKILL_LEVEL" = "1" ]; then
        skill_level="$NODEX_SKILL_LEVEL"
    elif [ -z "$INPUT_FD" ]; then
        skill_level="user"
    else
        local pick
        pick="$(prompt_choice "Claude Code skill" 1 \
            "User-level            ~/.claude/skills/${SKILL_NAME}" \
            "Project-level         ./.claude/skills/${SKILL_NAME}" \
            "Skip")"
        case "$pick" in
            User-level*)    skill_level="user" ;;
            Project-level*) skill_level="project" ;;
            Skip)           skill_level="none" ;;
        esac
    fi

    case "$skill_level" in user|project|none) ;;
        *) die "Invalid skill level: $skill_level (expected user|project|none)" ;;
    esac

    render_review "$method" "$dest" "$skill_level" "$version"

    if [ "$DRY_RUN" = "1" ]; then
        printf '\n%s(dry-run) Not executing%s\n' "$C_YELLOW" "$C_RESET"
        exit 0
    fi

    if [ -n "$INPUT_FD" ] && ! prompt_yesno "Proceed?" "Y"; then
        log_info "Aborted by user"; exit 0
    fi

    # Existing install check
    if [ -f "$dest" ] && [ "$NODEX_FORCE" != "1" ]; then
        local existing; existing="$("$dest" --version 2>/dev/null | awk '{print $2}' || echo "")"
        local cmp; cmp="$(compare_versions "$existing" "$version")"
        case "$cmp" in
            equal)
                prompt_yesno "nodex v$existing already installed. Reinstall?" "N" || { log_info "Kept existing install"; skip_binary=1; } ;;
            newer)
                prompt_yesno "Installed v$existing is newer than v$version. Downgrade?" "N" || { log_info "Kept existing install"; skip_binary=1; } ;;
            older|unknown) : ;;
        esac
    fi
    skip_binary="${skip_binary:-0}"

    printf '\n'

    if [ "$skip_binary" != "1" ]; then
        if ! check_writable "$INSTALL_DIR"; then
            die "Install dir not writable: $INSTALL_DIR
  Try:   ./scripts/install.sh --install-dir \"\$HOME/.local/bin\"
  Or:    sudo ./scripts/install.sh --install-dir \"$INSTALL_DIR\""
        fi
        case "$method" in
            prebuilt)
                local archive="${BINARY_NAME}-v${version}-${platform}.tar.gz"
                download_archive "$version" "$archive"
                verify_checksum "$archive"
                extract_archive "$archive"
                binary_src="${TMP_DIR}/${BINARY_NAME}"
                ;;
            source)
                [ -n "$repo_dir" ] || die "--from-source requires running from a cloned repo"
                binary_src="$(build_from_source "$repo_dir")"
                ;;
        esac
        install_binary "$binary_src" "$INSTALL_DIR"
    fi

    if [ "$skill_level" != "none" ]; then
        local skill_src=""
        # Declining a reinstall or a downgrade leaves the binary that was
        # already there, and the skill describes the binary on disk — not the
        # one that was asked for.
        # Declining a reinstall or a downgrade leaves the binary that was
        # already there, and the skill describes the binary on disk — not the
        # one that was asked for. A checkout build is its own case: when the
        # binary on disk is this checkout's, the checkout's skill is the
        # matching half, and a re-run that changes nothing has nothing to do.
        local on_disk="$version"
        if [ "$skip_binary" = "1" ]; then
            on_disk="$("$dest" --version 2>/dev/null | awk '{print $2}' || echo "")"
            if [ -n "$on_disk" ]; then
                version="$on_disk"
            else
                log_warn "Cannot read the installed version; leaving the skill untouched"
                skill_level="none"
            fi
        fi
        # The skill and the binary are one contract, so they come from the
        # same place: a binary built from this checkout takes the checkout's
        # skill, and a released binary takes that release's — otherwise
        # `--version` pins one half of the pair and a clone supplies the
        # other, which installs cleanly and describes a binary that is not
        # there. The tarball is a single release asset, so a multi-file skill
        # installs atomically and verified.
        if [ "$method" = "source" ] && [ "$on_disk" = "$checkout_version" ] &&
            [ -d "$repo_dir/.claude/skills/$SKILL_NAME" ]; then
            skill_src="$repo_dir/.claude/skills/$SKILL_NAME"
        else
            skill_src="$(download_skill_tarball "$version")"
        fi
        # A skill that was asked for and could not be installed leaves the
        # binary paired with whatever was there before — the split contract
        # this seam exists to prevent. Say so and fail, rather than report a
        # complete installation of half of it; `--skill none` is how a caller
        # asks for the binary alone.
        if [ -n "$skill_src" ]; then
            install_skill "$skill_level" "$skill_src"
        else
            die "Skill v${version} could not be installed, so it would not match \
the binary on disk. Re-run when the release asset is reachable, or pass --skill none."
        fi
    fi

    printf '\n'
    check_path "$INSTALL_DIR"
    printf '\n%s✅ Installation complete%s\n' "$C_GREEN$C_BOLD" "$C_RESET"
    printf '\nNext steps:\n'
    printf '  %s%s init%s       Initialize nodex.toml\n' "$C_BOLD" "$BINARY_NAME" "$C_RESET"
    printf '  %s%s build%s      Build the document graph\n' "$C_BOLD" "$BINARY_NAME" "$C_RESET"
    printf '  %s/nodex%s            Use as Claude Code skill\n' "$C_BOLD" "$C_RESET"
}

main "$@"

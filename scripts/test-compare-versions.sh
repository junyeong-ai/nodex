#!/usr/bin/env bash
# Precedence cases for install.sh's `compare_versions`, run on every platform
# CI installs on. `sort -V` cannot answer this — it orders version strings the
# way a filename listing wants them and implementations disagree about where a
# prerelease ranks — so the rule is the SemVer §11 specification and this is
# where that claim is checked rather than asserted.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
# The installer runs `main` when executed, so the one function under test is
# lifted out rather than the file sourced. If it ever moves or is renamed the
# lift yields nothing, and saying so here beats every case failing for a
# reason that reads like a precedence bug.
comparator="$(sed -n '/^compare_versions() {/,/^}/p' "$here/install.sh")"
[ -n "$comparator" ] || { echo "compare_versions not found in install.sh" >&2; exit 1; }
# shellcheck disable=SC1090,SC1091
. /dev/stdin <<< "$comparator"

pass=0
fail=0
chk() {
    local got
    got="$(compare_versions "$1" "$2")"
    if [ "$got" = "$3" ]; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
        printf 'FAIL  %-22s vs %-22s expected=%-8s got=%s\n' "$1" "$2" "$3" "$got"
    fi
}

# The chain SemVer §11 spells out, forwards and back.
chk 1.0.0-alpha       1.0.0-alpha.1     older
chk 1.0.0-alpha.1     1.0.0-alpha.beta  older
chk 1.0.0-alpha.beta  1.0.0-beta        older
chk 1.0.0-beta        1.0.0-beta.2      older
chk 1.0.0-beta.2      1.0.0-beta.11     older
chk 1.0.0-beta.11     1.0.0-rc.1        older
chk 1.0.0-rc.1        1.0.0             older
chk 1.0.0             1.0.0-rc.1        newer
chk 1.0.0-beta.11     1.0.0-beta.2      newer

# Core precedence is numeric, not lexical.
chk 1.9.0 1.10.0 older
chk 1.0.9 1.0.10 older
chk 0.9.0 0.10.0 older
chk 2.0.0 1.0.0  newer

# A missing core field reads as zero; build metadata never affects precedence.
chk 1.2 1.2.0 equal
chk 1   1.0.0 equal
chk 1.2 1.2.1 older
chk 1.0.0+build.1 1.0.0+build.2 equal
chk 1.0.0-rc.1+a  1.0.0-rc.1+b  equal
chk v1.2.3 1.2.3 equal

# A numeric identifier ranks below an alphanumeric one; a shorter run of
# otherwise-equal identifiers ranks lower.
chk 1.0.0-1     1.0.0-alpha older
chk 1.0.0-alpha 1.0.0-1     newer
chk 1.0.0-a.b.c   1.0.0-a.b.c.d older
chk 1.0.0-a.b.c.d 1.0.0-a.b.c   newer

# What the installer actually asks: is the installed build behind the one
# requested? An installed release candidate is, so the final release installs.
chk 0.25.1     0.25.2 older
chk 0.26.0-rc.1 0.26.0 older
chk 0.26.0     0.26.0-rc.1 newer

# Unusable input is said to be unusable rather than guessed at.
chk ""    1.0.0 unknown
chk 1.0.0 ""    unknown
chk abc   1.0.0 unknown

printf '\ncompare_versions: pass=%d fail=%d\n' "$pass" "$fail"
[ "$fail" -eq 0 ]

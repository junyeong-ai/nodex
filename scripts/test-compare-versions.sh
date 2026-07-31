#!/usr/bin/env bash
# Precedence cases for install.sh's `compare_versions`, run on every platform
# CI installs on. `sort -V` cannot answer this — it orders version strings the
# way a filename listing wants them and implementations disagree about where a
# prerelease ranks — so the rule is the SemVer §11 specification and this is
# where that claim is checked rather than asserted.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
# The installer runs `main` when executed, so the precedence code is lifted
# out rather than the file sourced. The region is marked in install.sh so a
# helper added beside the comparator comes with it; if the markers ever move,
# the lift yields nothing and says so, which beats every case failing for a
# reason that reads like a precedence bug.
comparator="$(sed -n '/^# --- version precedence/,/^# --- end version precedence/p' "$here/install.sh")"
# Both markers, not just the opening one: `sed` runs a start marker with no end
# to the end of the file, which would source the installer instead of lifting
# a function.
case "$comparator" in
    *"# --- end version precedence"*) ;;
    *) echo "version-precedence region not delimited in install.sh" >&2; exit 1 ;;
esac
case "$comparator" in
    *"compare_versions() {"*) ;;
    *) echo "compare_versions not in the lifted region" >&2; exit 1 ;;
esac
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

# Numeric identifiers have no width limit in the specification, and the shell's
# integer range is not the answer: past it `[ -lt ]` fails both ways and the
# comparison would silently read equal.
chk 99999999999999999999.0.0 1.0.0                   newer
chk 1.0.0 99999999999999999999.0.0                   older
chk 1.0.0-99999999999999999999 1.0.0-2               newer
chk 1.0.0-99999999999999999999 1.0.0-99999999999999999998 newer

# Unusable input is said to be unusable rather than guessed at.
chk ""    1.0.0 unknown
chk 1.0.0 ""    unknown
chk abc   1.0.0 unknown
chk 1.2.3.4 1.2.3   unknown
chk 1.2.3   1.2.3.4 unknown
chk 01.0.0  1.0.0   unknown
chk 1.0.0-01 1.0.0-1 unknown
chk 1.0.0-  1.0.0   unknown
chk 1.0.0-a..b 1.0.0-a.b unknown
chk 1.0.0-0 1.0.0-0 equal
# One string is itself whatever shape it is — the answer is true, and it is the
# one that reaches the "already installed" prompt.
chk 01.0.0 01.0.0 equal
chk abc    abc    equal
chk 1.0.0- 1.0.0- equal
# A shape the specification disallows is unusable wherever it appears, not only
# once the fields before it tie.
chk 1.2.4.5 1.2.3 unknown
chk 1.0.0-a..b 1.0.0-c unknown

printf '\ncompare_versions: pass=%d fail=%d\n' "$pass" "$fail"
[ "$fail" -eq 0 ]

#!/usr/bin/env bash
# Decompile DayZ functions with headless Ghidra (inside the build-box container, which
# has the JDK). The first run imports and auto-analyses DayZ_x64.exe into the Ghidra
# project under build/ghidra (slow, tens of minutes); later runs reuse the project.
#
#   scripts/ghidra-decompile.sh <rva> [<rva> ...]      e.g. scripts/ghidra-decompile.sh 1dde4b 1b8f00
#   scripts/ghidra-decompile.sh string:<text> import:<name> addr:<rva> table:<rva>:<n> vtables:<class> ...
#       finds the string / import, lists its references and decompiles the referencing
#       functions (FindRefs.java); mixes freely with RVAs.
#
# Output: build/ghidra/decomp/DayZ+0x<entry>.c, one per containing function, plus
# build/ghidra/decomp/refs-<query>.txt per string/import query.
# Environment overrides: DAYZ_DIR, GHIDRA_HOME, BUILD_CONTAINER, GHIDRA_MAXMEM (default 8G),
# GHIDRA_CPUS (default 4).
set -euo pipefail
IFS=$'\n\t'

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
project_dir="$(dirname -- "$script_dir")"
ghidra_home="${GHIDRA_HOME:-/run/media/system/Data/Applications/ghidra/ghidra_12.1.4_PUBLIC}"
dayz_dir="${DAYZ_DIR:-/run/media/system/Data/Games/Steam/steamapps/common/DayZ}"
container="${BUILD_CONTAINER:-build-box}"
work_dir="$project_dir/build/ghidra"
out_dir="$work_dir/decomp"
project_name="DayZ"

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

(( $# >= 1 )) || die "pass at least one RVA (hex) or string:/import: query"
rvas=()
queries=()
for arg in "$@"; do
  case "$arg" in
    string:* | import:* | addr:* | table:* | vtables:*) queries+=("$arg") ;;
    *) rvas+=("$arg") ;;
  esac
done
[[ -x "$ghidra_home/support/analyzeHeadless" ]] || die "Ghidra not found at $ghidra_home"
[[ -f "$dayz_dir/DayZ_x64.exe" ]] || die "DayZ_x64.exe not found in $dayz_dir"
mkdir -p "$work_dir" "$out_dir"

import_args=()
if [[ ! -f "$work_dir/$project_name.gpr" ]]; then
  say "first run: importing and analysing DayZ_x64.exe (this takes a while)"
  import_args=(-import "$dayz_dir/DayZ_x64.exe")
else
  import_args=(-process DayZ_x64.exe -noanalysis)
fi

cmd=("$ghidra_home/support/analyzeHeadless" "$work_dir" "$project_name"
  "${import_args[@]}" -max-cpu "${GHIDRA_CPUS:-4}" -scriptPath "$script_dir/ghidra")
(( ${#rvas[@]} == 0 )) || cmd+=(-postScript DecompileRvas.java "$out_dir" "${rvas[@]}")
(( ${#queries[@]} == 0 )) || cmd+=(-postScript FindRefs.java "$out_dir" "${queries[@]}")
say "running: ${cmd[*]}"
# Ghidra's launcher takes the first java on PATH before JAVA_HOME, and on this host
# ~/.local/bin/java is a distrobox bridge wrapper that fails inside the container,
# so the container JDK's bin directory is put in front of PATH.
java_home="${JAVA_HOME_IN_CONTAINER:-/usr/lib/jvm/java-21-openjdk-amd64}"
if [[ -f /run/.containerenv || -n "${CONTAINER_ID:-}" ]]; then
  PATH="$java_home/bin:$PATH" JAVA_HOME="$java_home" MAXMEM="${GHIDRA_MAXMEM:-8G}" \
    nice -n 10 "${cmd[@]}"
else
  command -v distrobox >/dev/null || die "distrobox is required"
  distrobox enter "$container" -- env PATH="$java_home/bin:$PATH" JAVA_HOME="$java_home" \
    MAXMEM="${GHIDRA_MAXMEM:-8G}" nice -n 10 "${cmd[@]}"
fi
say "decompiled output in $out_dir"

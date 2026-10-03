#!/usr/bin/env bash
# Build gate for the DayZ plugin loader.
#
# Steps, in order: format check, clippy on the host, tests on the host, docs, clippy for the
# Windows target, release build of the DLLs, then an optional deploy into a game directory.
# Nothing here filters compiler output; a step that fails stops the script.
#
#   scripts/build.sh                 # full gate, no deploy
#   scripts/build.sh --deploy        # gate, then install into $DAYZ_DIR
#   scripts/build.sh --fast          # skip the host gate, build the DLLs only
#   scripts/build.sh --check         # host gate only, no Windows build
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
container="${BUILD_BOX:-build-box}"
target="x86_64-pc-windows-msvc"
dayz_dir="${DAYZ_DIR:-/run/media/system/Data/Games/Steam/steamapps/common/DayZ}"

deploy=0
fast=0
check_only=0
for arg in "$@"; do
    case "$arg" in
        --deploy) deploy=1 ;;
        --fast) fast=1 ;;
        --check) check_only=1 ;;
        -h | --help)
            sed -n '2,11p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown option: $arg" >&2
            exit 2
            ;;
    esac
done

# Everything runs inside the container: the host is an immutable OS without a toolchain.
run() {
    echo "+ $*"
    distrobox enter "$container" -- bash -c "cd '$repo_root' && $*"
}

if [[ $fast -eq 0 ]]; then
    run cargo fmt --all --check
    run cargo clippy --workspace --all-targets --all-features
    run cargo test --workspace
    run cargo doc --workspace --no-deps
fi

if [[ $check_only -eq 1 ]]; then
    echo "host gate passed"
    exit 0
fi

# The loader and the plugins only compile for Windows, and only the MSVC target has the
# structured exception handling that isolates plugin faults.
run cargo xwin clippy -p dayz-plugin-loader -p hello-plugin --target "$target" --all-targets
run cargo xwin build --release -p dayz-plugin-loader -p hello-plugin --target "$target"

out="$repo_root/target/$target/release"
for artifact in dxgi.dll hello.dll; do
    if [[ ! -f "$out/$artifact" ]]; then
        echo "missing build output: $out/$artifact" >&2
        exit 1
    fi
    printf '%-12s %8s bytes  %s\n' "$artifact" "$(stat -c %s "$out/$artifact")" \
        "$(date -r "$out/$artifact" '+%F %T')"
done

if [[ $deploy -eq 0 ]]; then
    exit 0
fi

if [[ ! -x "$dayz_dir/DayZ_x64.exe" && ! -f "$dayz_dir/DayZ_x64.exe" ]]; then
    echo "no DayZ_x64.exe in $dayz_dir; set DAYZ_DIR" >&2
    exit 1
fi

plugins_dir="$dayz_dir/dayz-plugins/plugins"
config_dir="$dayz_dir/dayz-plugins/config"
mkdir -p "$plugins_dir" "$config_dir" "$dayz_dir/dayz-plugins/logs"
cp -f "$out/dxgi.dll" "$dayz_dir/dxgi.dll"
cp -f "$out/hello.dll" "$plugins_dir/hello.dll"
# Config files carry the user's own values; never overwrite one that already exists.
for sample in "$repo_root"/config/*.toml; do
    name="$(basename "$sample")"
    if [[ ! -f "$config_dir/$name" ]]; then
        cp -f "$sample" "$config_dir/$name"
        echo "installed default $name"
    fi
done
echo "deployed to $dayz_dir"
echo
echo "run the game with WINEDLLOVERRIDES=\"dxgi=n,b\" and --console for a log window"

#!/usr/bin/env bash
# Driver script for silabs-data.
#
# Subcommands:
#   download-all     fetch vendor packs into silabs-data-source/packs/
#   seed             one-shot bootstrap of data/registers/*.yaml from SVDs
#   extract-pins     one-shot bootstrap of data/pins/*.yaml from the pin tool
#   gen-all          regenerate build/data/ and build/silabs-metapac/

set -euo pipefail
cd "$(dirname "$0")"

SOURCE_DIR="../silabs-data-source"
PACKS_DIR="$SOURCE_DIR/packs"

usage() {
    cat <<'EOF'
Usage: ./d <subcommand>

Subcommands:
  download-all     Fetch vendor packs into silabs-data-source/packs/.
  seed [--chips <regex>] [--candidates-dir <dir>]
                   One-shot bootstrap of data/registers/*.yaml from SVDs.
                   Merges additive differences, stops on conflicts.
  extract-pins [--force] <pin-tool-dir> <sdk-label> [<pin-tool-dir> <sdk-label>]...
                   One-shot bootstrap of data/pins/<family>.yaml from the
                   Silicon Labs pin tool (`.../hwconf_data/pin_tool`). Runs
                   once for each family directory whose name is the exact
                   family of chips in build/data/chips. Writes only facts,
                   never pin-tool text. The label goes into the file header,
                   for example "Simplicity SDK 2025.12.0". The files are
                   maintained by hand: when a file differs from the output,
                   the command prints the difference and stops. --force
                   overwrites it. gen-all never runs it.
  gen-all          Regenerate build/data/ (per-chip JSON) and
                   build/silabs-metapac/ (PAC crate) from committed
                   data/registers/*.yaml. Never writes to data/registers/.
EOF
}

# List the .pack files in silabs-data-source/packs/.
# Prints absolute paths, one per line.
discover_packs() {
    for p in "$PACKS_DIR"/*.pack; do
        [ -f "$p" ] || continue
        printf '%s\n' "$p"
    done
}

# Build a `--pack <path> --pack <path>` argument list for the metapac-gen CLI.
# Avoids `mapfile`/`readarray` which are bash 4+ features (macOS bash is 3.2).
pack_args() {
    while IFS= read -r p; do
        printf -- '--pack\n%s\n' "$p"
    done < <(discover_packs)
}

cmd_download_all() {
    if [ ! -x "$SOURCE_DIR/scripts/download.sh" ]; then
        echo "missing $SOURCE_DIR/scripts/download.sh" >&2
        exit 1
    fi
    ( cd "$SOURCE_DIR" && ./scripts/download.sh )
}

cmd_seed() {
    mkdir -p build/data
    # silabs-data-gen gen requires --pack one at a time.
    for pack in $(discover_packs); do
        echo "[seed] silabs-data-gen gen --pack $pack"
        cargo run -q -p silabs-data-gen --release -- gen \
            --pack "$pack" \
            --out-dir build/data
    done
    # silabs-metapac-gen seed takes multiple --pack.
    # macOS bash 3.2 has no mapfile, so read the list one line at a time.
    pa=()
    while IFS= read -r line; do
        pa+=("$line")
    done < <(pack_args)
    echo "[seed] silabs-metapac-gen seed"
    cargo run -q -p silabs-metapac-gen --release -- seed \
        --data-dir build/data \
        --transforms-dir transforms \
        --registers-yaml-dir data/registers \
        "${pa[@]}" "$@"
}

cmd_extract_pins() {
    force=()
    if [ "${1:-}" = "--force" ]; then
        force=(--force)
        shift
    fi
    if [ $# -eq 0 ] || [ $(( $# % 2 )) -ne 0 ]; then
        echo "extract-pins needs <pin-tool-dir> <sdk-label> pairs" >&2
        usage
        exit 1
    fi
    if [ ! -d build/data/chips ]; then
        echo "missing build/data/chips: run ./d gen-all first" >&2
        exit 1
    fi
    mkdir -p data/pins
    while [ $# -gt 0 ]; do
        root="$1"
        label="$2"
        shift 2
        for dir in "$root"/*/; do
            family="$(basename "$dir")"
            upper="$(printf '%s' "$family" | tr '[:lower:]' '[:upper:]')"
            # Only families that are the exact `family` of some chip JSON.
            if ! grep -lq "\"family\": \"$upper\"" build/data/chips/*.json; then
                continue
            fi
            echo "[extract-pins] $family ($label)"
            cargo run -q -p silabs-data-gen --release -- extract-pins \
                --pin-tool "${dir%/}" \
                --sdk "$label" \
                --data-dir build/data \
                --out-dir data/pins \
                --registers-dir data/registers \
                --pack-dir "$PACKS_DIR" \
                ${force[@]+"${force[@]}"}
        done
    done
}

cmd_gen_all() {
    mkdir -p build/data build/silabs-metapac
    for pack in $(discover_packs); do
        echo "[gen] silabs-data-gen gen --pack $pack"
        cargo run -q -p silabs-data-gen --release -- gen \
            --pack "$pack" \
            --out-dir build/data \
            --pins-dir data/pins \
            --registers-dir data/registers
    done
    pa=()
    while IFS= read -r line; do
        pa+=("$line")
    done < <(pack_args)
    echo "[gen] silabs-metapac-gen gen"
    cargo run -q -p silabs-metapac-gen --release -- gen \
        --data-dir build/data \
        --registers-yaml-dir data/registers \
        --out-dir build/silabs-metapac \
        "${pa[@]}"
}

case "${1:-}" in
    download-all) cmd_download_all ;;
    seed)         shift; cmd_seed "$@" ;;
    extract-pins) shift; cmd_extract_pins "$@" ;;
    gen-all)      cmd_gen_all ;;
    -h|--help|help|"") usage ;;
    *) echo "unknown subcommand: $1" >&2; usage; exit 1 ;;
esac

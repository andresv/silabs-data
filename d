#!/usr/bin/env bash
# Driver script for silabs-data.
#
# Subcommands:
#   download-all     fetch vendor packs into silabs-data-source/packs/
#   seed             one-shot bootstrap of data/registers/*.yaml from SVDs
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
  seed             One-shot bootstrap of data/registers/*.yaml from SVDs.
                   Hash-bails on cross-chip (kind, version) divergence.
  gen-all          Regenerate build/data/ (per-chip JSON) and
                   build/silabs-metapac/ (PAC crate) from committed
                   data/registers/*.yaml. Never writes to data/registers/.
EOF
}

# Discover all .pack files referenced in silabs-data-source/families.toml.
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
    # macOS bash 3.2 lacks mapfile; capture into a positional array via a subshell + set.
    pa=()
    while IFS= read -r line; do
        pa+=("$line")
    done < <(pack_args)
    echo "[seed] silabs-metapac-gen seed"
    cargo run -q -p silabs-metapac-gen --release -- seed \
        --data-dir build/data \
        --transforms-dir transforms \
        --registers-yaml-dir data/registers \
        "${pa[@]}"
}

cmd_gen_all() {
    mkdir -p build/data build/silabs-metapac
    for pack in $(discover_packs); do
        echo "[gen] silabs-data-gen gen --pack $pack"
        cargo run -q -p silabs-data-gen --release -- gen \
            --pack "$pack" \
            --out-dir build/data
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
    seed)         cmd_seed ;;
    gen-all)      cmd_gen_all ;;
    -h|--help|help|"") usage ;;
    *) echo "unknown subcommand: $1" >&2; usage; exit 1 ;;
esac

#!/usr/bin/env bash
# Deploy the generated metapac + a freshly-rendered README into the
# silabs-data-generated release repo.
#
# Steps:
#   1. Run ./d gen-all (regenerate build/silabs-metapac/ + build/data/).
#   2. Run the `summary` binary to render the per-peripheral support table.
#   3. Concatenate it under a fixed header and write the result as
#      <dest>/README.md.
#   4. rsync build/silabs-metapac/ into <dest>/silabs-metapac/, preserving
#      the dest's Cargo.lock and excluding /target.
#
# Usage:
#   ./deploy.sh                       # dest = ../silabs-data-generated
#   ./deploy.sh /path/to/other-repo   # explicit dest

set -euo pipefail
cd "$(dirname "$0")"

DEST="${1:-../silabs-data-generated}"

if [ ! -d "$DEST" ]; then
    echo "deploy: dest '$DEST' is not a directory" >&2
    exit 1
fi

# --- 1. Regenerate -----------------------------------------------------------
./d gen-all

# --- 2. Render the support-matrix summary -----------------------------------
SUMMARY_TMP="$(mktemp -t silabs-summary.XXXXXX.md)"
trap 'rm -f "$SUMMARY_TMP"' EXIT

cargo run -q -p silabs-data-gen --release --bin summary -- > "$SUMMARY_TMP"

# --- 3. Assemble README ------------------------------------------------------
# The header is the canonical preamble that prefixes every release README.
# It points back at silabs-data so consumers can find the source.
README_TMP="$(mktemp -t silabs-readme.XXXXXX.md)"
trap 'rm -f "$SUMMARY_TMP" "$README_TMP"' EXIT

cat > "$README_TMP" <<'EOF'
# silabs-data generated output

This repo contains generated output for [`silabs-data`](https://github.com/andresv/silabs-data). See the `silabs-data` README for the full pipeline description.

## Silabs Peripheral Support Matrix

The following table shows which peripheral versions are supported across EFR32 / EFM32 families.

EOF
cat "$SUMMARY_TMP" >> "$README_TMP"

# --- 4. Copy artefacts into dest --------------------------------------------
mv "$README_TMP" "$DEST/README.md"
trap 'rm -f "$SUMMARY_TMP"' EXIT

# Preserve dest's Cargo.lock (developer-local state) and never touch
# build artefacts. --delete removes files that no longer exist in the
# regenerated tree (e.g. dropped chips).
rsync -a --delete \
    --exclude='Cargo.lock' \
    --exclude='/target' \
    build/silabs-metapac/ \
    "$DEST/silabs-metapac/"

echo "[deploy] wrote $DEST/README.md"
echo "[deploy] synced build/silabs-metapac/ -> $DEST/silabs-metapac/"

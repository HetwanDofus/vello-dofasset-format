#!/bin/bash
# Compile accessories: SVG frames → spritesheet → .dofasset
# Uses the existing svg-spritesheet tool + dofasset compiler
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
COMPILER_DIR="$SCRIPT_DIR/.."
WEBCLIENT="/Users/grandnainconnu/Work/personal/dofus/dofus1.29/dofus-client-recode/dofuswebclient2"
ACC_INPUT="$WEBCLIENT/apps/electrobun/public/assets/spritesheets/accessories"
SPRITES_OUTPUT="$WEBCLIENT/apps/electrobun/public/assets/spritesheets/sprites"
SPRITESHEET_TOOL="$WEBCLIENT/tools/svg-spritesheet"

TMPDIR=$(mktemp -d)
trap "rm -rf $TMPDIR" EXIT

ok=0; fail=0; skip=0; total=0

echo "=== Compiling accessories to .dofasset ==="
echo "Input:  $ACC_INPUT"
echo "Output: $SPRITES_OUTPUT"
echo ""

for acc_dir in "$ACC_INPUT"/*/; do
    acc_id=$(basename "$acc_dir")  # e.g., "16_10"
    total=$((total + 1))

    # Output as acc_{type}_{gfxId}.dofasset in the sprites directory
    dofasset_path="$SPRITES_OUTPUT/acc_${acc_id}.dofasset"
    if [ -f "$dofasset_path" ]; then
        skip=$((skip + 1))
        continue
    fi

    # Step 1: Flatten frame SVGs: {direction}/frame_N.svg → {direction}_N.svg
    flat_dir="$TMPDIR/input/$acc_id"
    mkdir -p "$flat_dir"

    has_svgs=false
    for dir_path in "$acc_dir"*/; do
        [ -d "$dir_path" ] || continue
        direction=$(basename "$dir_path")
        for svg in "$dir_path"frame_*.svg; do
            [ -f "$svg" ] || continue
            has_svgs=true
            frame_num=$(basename "$svg" .svg | sed 's/frame_//')
            cp "$svg" "$flat_dir/${direction}_${frame_num}.svg"
        done
    done

    if [ "$has_svgs" = false ]; then
        skip=$((skip + 1))
        continue
    fi

    # Step 2: Run spritesheet tool
    sheet_dir="$TMPDIR/sheet"
    rm -rf "$sheet_dir"
    mkdir -p "$sheet_dir"

    if ! (cd "$SPRITESHEET_TOOL" && bun run src/cli.ts "$TMPDIR/input" "$sheet_dir" 2>/dev/null); then
        fail=$((fail + 1))
        echo "FAIL spritesheet: $acc_id"
        rm -rf "$flat_dir"
        continue
    fi

    # Step 3: Compile to .dofasset
    if ! bun run "$COMPILER_DIR/packages/compiler/src/index.ts" --input "$sheet_dir/$acc_id" --output "$dofasset_path" > /dev/null 2>&1; then
        fail=$((fail + 1))
        echo "FAIL compile: $acc_id"
        rm -rf "$flat_dir"
        continue
    fi

    ok=$((ok + 1))
    rm -rf "$flat_dir"

    # Progress every 50
    if [ $((ok % 50)) -eq 0 ]; then
        echo "  ... $ok compiled"
    fi
done

echo ""
echo "=== SUMMARY ==="
echo "  OK:      $ok"
echo "  Failed:  $fail"
echo "  Skipped: $skip"
echo "  Total:   $total"

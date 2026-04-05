#!/bin/bash
# Batch compile all sprites and tiles from dofuswebclient2 assets to .dofasset format
set -e

COMPILER_DIR="$(cd "$(dirname "$0")/.." && pwd)"
ASSETS="/Users/grandnainconnu/Work/personal/dofus/dofus1.29/dofus-client-recode/dofuswebclient2/apps/electrobun/public/assets/spritesheets"
OUTPUT="/Users/grandnainconnu/Work/personal/dofus/dofus1.29/dofus-client-recode/dofuswebclient3/assets"

mkdir -p "$OUTPUT/sprites" "$OUTPUT/tiles/ground" "$OUTPUT/tiles/objects"

COMPILER="bun run $COMPILER_DIR/packages/compiler/src/index.ts"

ok=0; fail=0; skip=0; total=0

compile_asset() {
    local input="$1"
    local output="$2"
    local label="$3"
    total=$((total + 1))

    # Skip if output already exists and is newer than input
    if [ -f "$output" ] && [ "$output" -nt "$input/atlas.svg" ] 2>/dev/null; then
        skip=$((skip + 1))
        return
    fi

    result=$($COMPILER --input "$input" --output "$output" 2>&1)
    if echo "$result" | grep -q "Binary written"; then
        ok=$((ok + 1))
    else
        fail=$((fail + 1))
        echo "FAIL: $label"
    fi
}

echo "=== Compiling sprites ==="
for dir in "$ASSETS/sprites"/*/; do
    id=$(basename "$dir")
    compile_asset "$dir" "$OUTPUT/sprites/${id}.dofasset" "sprite/$id"
done
echo "  Sprites: $ok ok, $fail fail, $skip skip / $total total"

sprite_ok=$ok; sprite_fail=$fail
ok=0; fail=0; skip=0; total=0

echo "=== Compiling ground tiles ==="
for dir in "$ASSETS/tiles/ground"/*/; do
    id=$(basename "$dir")
    compile_asset "$dir" "$OUTPUT/tiles/ground/${id}.dofasset" "ground/$id"
done
echo "  Ground: $ok ok, $fail fail, $skip skip / $total total"

ground_ok=$ok; ground_fail=$fail
ok=0; fail=0; skip=0; total=0

echo "=== Compiling object tiles ==="
for dir in "$ASSETS/tiles/objects"/*/; do
    id=$(basename "$dir")
    compile_asset "$dir" "$OUTPUT/tiles/objects/${id}.dofasset" "objects/$id"
done
echo "  Objects: $ok ok, $fail fail, $skip skip / $total total"

echo ""
echo "=== SUMMARY ==="
echo "  Sprites: $sprite_ok ok, $sprite_fail fail"
echo "  Ground:  $ground_ok ok, $ground_fail fail"
echo "  Objects: $ok ok, $fail fail"
echo "  Total:   $((sprite_ok + ground_ok + ok)) ok, $((sprite_fail + ground_fail + fail)) fail"

#!/bin/bash
# Compile ALL assets (tiles + sprites) to .dofasset for the web client (dofuswebclient2)
set -e

COMPILER_DIR="$(cd "$(dirname "$0")/.." && pwd)"
ASSETS="/Users/grandnainconnu/Work/personal/dofus/dofus1.29/dofus-client-recode/dofuswebclient2/apps/electrobun/public/assets/spritesheets"

COMPILER="bun run $COMPILER_DIR/packages/compiler/src/index.ts"

ok=0; fail=0; skip=0; total=0

compile_asset() {
    local input="$1"
    local output="$2"
    local label="$3"
    total=$((total + 1))

    # Skip if output already exists and is newer than any svg in input
    if [ -f "$output" ]; then
        local newest_svg
        newest_svg=$(find "$input" -name "*.svg" -newer "$output" 2>/dev/null | head -1)
        if [ -z "$newest_svg" ]; then
            skip=$((skip + 1))
            return
        fi
    fi

    result=$($COMPILER --input "$input" --output "$output" 2>&1)
    if echo "$result" | grep -q "Binary written"; then
        ok=$((ok + 1))
    else
        fail=$((fail + 1))
        echo "FAIL: $label — $result" | head -1
    fi
}

# --- Ground tiles ---
echo "=== Compiling ground tiles ==="
for dir in "$ASSETS/tiles/ground"/*/; do
    [ -d "$dir" ] || continue
    id=$(basename "$dir")
    compile_asset "$dir" "$ASSETS/tiles/ground/${id}.dofasset" "ground/$id"
done
echo "  Ground: $ok ok, $fail fail, $skip skip / $total total"
ground_ok=$ok; ground_fail=$fail; ground_skip=$skip

# --- Object tiles ---
ok=0; fail=0; skip=0; total=0
echo "=== Compiling object tiles ==="
for dir in "$ASSETS/tiles/objects"/*/; do
    [ -d "$dir" ] || continue
    id=$(basename "$dir")
    compile_asset "$dir" "$ASSETS/tiles/objects/${id}.dofasset" "objects/$id"
done
echo "  Objects: $ok ok, $fail fail, $skip skip / $total total"
obj_ok=$ok; obj_fail=$fail; obj_skip=$skip

# --- Sprites ---
ok=0; fail=0; skip=0; total=0
echo "=== Compiling sprites ==="
for dir in "$ASSETS/sprites"/*/; do
    [ -d "$dir" ] || continue
    id=$(basename "$dir")
    # Sprites have subdirectories per animation, not manifest.json at root
    # Only compile if there are animation subdirs with atlas.svg
    if [ ! -f "$dir/manifest.json" ] && ! ls "$dir"/*/atlas.svg &>/dev/null; then
        continue
    fi
    compile_asset "$dir" "$ASSETS/sprites/${id}.dofasset" "sprite/$id"
done
echo "  Sprites: $ok ok, $fail fail, $skip skip / $total total"
spr_ok=$ok; spr_fail=$fail; spr_skip=$skip

echo ""
echo "=== SUMMARY ==="
echo "  Ground:  $ground_ok ok, $ground_fail fail, $ground_skip skip"
echo "  Objects: $obj_ok ok, $obj_fail fail, $obj_skip skip"
echo "  Sprites: $spr_ok ok, $spr_fail fail, $spr_skip skip"
echo "  Total:   $((ground_ok + obj_ok + spr_ok)) ok, $((ground_fail + obj_fail + spr_fail)) fail, $((ground_skip + obj_skip + spr_skip)) skip"

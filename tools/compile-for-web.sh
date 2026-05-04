#!/bin/bash
# Compile ALL assets (tiles + sprites) to .dofasset for the web client (dofuswebclient2).
#
# Input atlases are produced by the just recipes in dofuswebclient2:
#   `just tiles-spritesheet` and `just sprites-spritesheet`
# which write to `dofuswebclient2/assets/spritesheets/`.
#
# Output .dofasset binaries are written to `dofuswebclient2/apps/electrobun/public/assets/spritesheets/`
# where the Electrobun web client loads them at runtime.
set -e

COMPILER_DIR="$(cd "$(dirname "$0")/.." && pwd)"
WEBCLIENT_ROOT="/Users/grandnainconnu/Work/personal/dofus/dofus1.29/dofus-client-recode/dofuswebclient2"
INPUT_ASSETS="$WEBCLIENT_ROOT/assets/spritesheets"
OUTPUT_ASSETS="$WEBCLIENT_ROOT/apps/electrobun/public/assets/spritesheets"

COMPILER="bun run $COMPILER_DIR/packages/compiler/src/index.ts"

ok=0; fail=0; skip=0; total=0

compile_asset() {
    local input="$1"
    local output="$2"
    local label="$3"
    total=$((total + 1))

    # Ensure the output directory exists (new IDs may not have a parent yet)
    mkdir -p "$(dirname "$output")"

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
for dir in "$INPUT_ASSETS/tiles/ground"/*/; do
    [ -d "$dir" ] || continue
    id=$(basename "$dir")
    compile_asset "$dir" "$OUTPUT_ASSETS/tiles/ground/${id}.dofasset" "ground/$id"
done
echo "  Ground: $ok ok, $fail fail, $skip skip / $total total"
ground_ok=$ok; ground_fail=$fail; ground_skip=$skip

# --- Object tiles ---
ok=0; fail=0; skip=0; total=0
echo "=== Compiling object tiles ==="
for dir in "$INPUT_ASSETS/tiles/objects"/*/; do
    [ -d "$dir" ] || continue
    id=$(basename "$dir")
    compile_asset "$dir" "$OUTPUT_ASSETS/tiles/objects/${id}.dofasset" "objects/$id"
done
echo "  Objects: $ok ok, $fail fail, $skip skip / $total total"
obj_ok=$ok; obj_fail=$fail; obj_skip=$skip

# --- Sprites ---
ok=0; fail=0; skip=0; total=0
echo "=== Compiling sprites ==="
# Use nullglob locally so empty dirs don't expand to a literal "*/atlas.svg" pattern,
# which would otherwise trip `set -e` inside the inner test.
shopt -s nullglob
for dir in "$INPUT_ASSETS/sprites"/*/; do
    [ -d "$dir" ] || continue
    id=$(basename "$dir")
    # Sprites have subdirectories per animation, not manifest.json at root.
    # Only compile if there's either a root manifest.json or at least one
    # animation subdir with an atlas.svg.
    has_anim_atlas=0
    for anim_svg in "$dir"*/atlas.svg; do
        has_anim_atlas=1
        break
    done
    if [ ! -f "$dir/manifest.json" ] && [ "$has_anim_atlas" -eq 0 ]; then
        continue
    fi
    compile_asset "$dir" "$OUTPUT_ASSETS/sprites/${id}.dofasset" "sprite/$id"
done
shopt -u nullglob
echo "  Sprites: $ok ok, $fail fail, $skip skip / $total total"
spr_ok=$ok; spr_fail=$fail; spr_skip=$skip

# --- Spells ---
# Spells use the exact same directory layout as sprites (root manifest.json
# plus per-animation subdirs with atlas.svg + atlas.json), so the compiler
# accepts them unchanged. Each spell's bespoke TypeScript class in
# src/game/spells/spell-<id>.ts expects `anim1` plus the extra `sprite_<n>`
# sub-animations named in the manifest; all of them bake into one .dofasset.
ok=0; fail=0; skip=0; total=0
echo "=== Compiling spells ==="
shopt -s nullglob
for dir in "$INPUT_ASSETS/spells"/*/; do
    [ -d "$dir" ] || continue
    id=$(basename "$dir")
    has_anim_atlas=0
    for anim_svg in "$dir"*/atlas.svg; do
        has_anim_atlas=1
        break
    done
    if [ ! -f "$dir/manifest.json" ] && [ "$has_anim_atlas" -eq 0 ]; then
        continue
    fi
    compile_asset "$dir" "$OUTPUT_ASSETS/spells/${id}.dofasset" "spell/$id"
    # The runtime also needs the manifest JSON alongside the binary so the
    # spell runtime can read sound triggers, stop/fading frames, and the
    # requiresTypeScript flag before it calls into Vello.
    mkdir -p "$OUTPUT_ASSETS/spells/${id}"
    if [ -f "$dir/manifest.json" ]; then
        cp "$dir/manifest.json" "$OUTPUT_ASSETS/spells/${id}/manifest.json"
    fi
done
shopt -u nullglob
echo "  Spells: $ok ok, $fail fail, $skip skip / $total total"
spell_ok=$ok; spell_fail=$fail; spell_skip=$skip

echo ""
echo "=== SUMMARY ==="
echo "  Ground:  $ground_ok ok, $ground_fail fail, $ground_skip skip"
echo "  Objects: $obj_ok ok, $obj_fail fail, $obj_skip skip"
echo "  Sprites: $spr_ok ok, $spr_fail fail, $spr_skip skip"
echo "  Spells:  $spell_ok ok, $spell_fail fail, $spell_skip skip"
echo "  Total:   $((ground_ok + obj_ok + spr_ok + spell_ok)) ok, $((ground_fail + obj_fail + spr_fail + spell_fail)) fail, $((ground_skip + obj_skip + spr_skip + spell_skip)) skip"

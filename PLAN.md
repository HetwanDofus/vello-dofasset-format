# Dofus Asset Format (.dofasset) — Compiler + Renderer

## Problem

Dofus character sprites are SVG atlases. The current pipeline composes SVGs server-side (injecting accessories, applying colors via fill replacement), then rasterizes in the browser. This is too slow for 200+ unique characters — SVG parsing alone takes ~26ms/svg.

A test with Vello (`test-vello/`) proved GPU vector rendering can handle 200 composed sprites in ~500ms, but the bottleneck is SVG parsing. Solution: compile SVGs into a compact binary `.dofasset` format at build time, deserialize+render with Vello at runtime (~0.1ms deserialize vs ~26ms parse).

## Format Name

`.dofasset` — generic, will handle sprites now, maps/items/effects later.

## Key Paradigm Shift

**Old**: SVG spritesheets = 40 separate rasterized images per animation (frame-by-frame).
**New**: Body parts defined once as vector data + per-frame transform lists. "Animation" = swapping transforms on cached body parts. No re-rasterization, no re-parsing. The renderer understands movement — same body parts, different transforms each frame.

## Source SVG Structure

Each animation folder (e.g., `input/sprites/10/staticF/`) has:
- `atlas.json` — frame metadata (positions, offsets, fps, frameOrder, duplicates map)
- `atlas.svg` — the spritesheet SVG

The SVG contains:

### `<defs>` section
- **Reusable body part groups** (`<g id="...">`) — 15-25 groups of `<path>` elements with fills, strokes, opacity
- **Clip paths** (`<clipPath>`) — rectangular regions defining each frame's viewport
- **A single `<pattern>` element** with embedded base64 PNG (leather texture fill)
- **Alias `<use>` elements** — groups composed via `<use xlink:href="#id">`, creating chains that must be recursively resolved

### Main content
Sequence of `<g clip-path="url(#...)">` groups, one per unique frame:
- Positioning `<g transform="translate(x, y)">` for frame offset
- ~15-20 `<use xlink:href="#bodyPart" transform="matrix(a,b,c,d,tx,ty)">` referencing body parts
- Optional `<rect data-acc-slot="N" data-tx="X" data-ty="Y" data-matrix="..." data-depth="D"/>` accessory placeholders

### Color System
- `metadata.json` per sprite defines `colorZones: Record<string, string[]>` and `colorMapping: Record<string, number>` (zone→player color index 1-3)
- Player colors replace zone colors: preserve original lightness, apply player's hue+saturation
- Color replacement logic: `vite.config.ts:257-286` (`buildColorReplacements`)

### Accessory System
- 5 slots: weapon(0), hat(1), cape(2), pet(3), shield(4)
- Accessories are separate SVGs at `accessories/{type}_{gfxId}/{direction}/frame_0.svg`
- Composed into main SVG at correct depth with namespaced IDs
- Composition logic: `vite.config.ts:304-333` (`composeAccessory`)

### Pattern (Texture) System
- `<pattern>` elements with embedded base64 PNG + `patternTransform` matrix
- Used as `fill="url(#patternId)"` on paths
- Vello doesn't support SVG patterns natively — `test-vello/src/main.rs` shows how to render as tiled `Brush::Image` with `Extend::Repeat`

## Animation Analysis

Transforms between consecutive frames are NOT smoothly interpolatable (hand-keyed art with 100+ degree jumps). Each frame stores a complete set of body-part transforms. All frames of an animation use the exact same set of body parts — only transforms change.

Runtime animation = advance frame index at FPS rate, look up transform list, render same body parts with new transforms.

## Maximum Deduplication Strategy (6 levels)

1. **Path Data**: Many paths reused across body parts → store unique `d` strings once in path table
2. **Draw Commands**: (path_id + fill/stroke attrs) → dedup identical draw commands
3. **Body Parts** (cross-animation): `#ac` (hand) identical across staticF, walkF, runF → store once
4. **Transforms**: Many frames share transforms → global transform table, reference by index
5. **Frames**: atlas.json duplicates + identical frames across animations
6. **Images**: Same base64 PNG across all animations → store once

## Monorepo Structure

```
dofus-vello-custom-format/
├── package.json                    # Bun workspace root
├── packages/
│   ├── compiler/                   # Bun/TypeScript — SVG → .dofasset
│   │   ├── package.json            # deps: cheerio
│   │   ├── tsconfig.json
│   │   └── src/
│   │       ├── index.ts            # CLI entry
│   │       ├── types.ts            # All type definitions (no `any`)
│   │       ├── svg-parser.ts       # Parse SVG with cheerio — defs, frames, use-chains
│   │       ├── path-parser.ts      # SVG path `d` attr → PathSegment[]
│   │       ├── deduplicator.ts     # 6-level dedup across all animations
│   │       ├── image-extractor.ts  # Extract/dedup base64 PNGs from patterns
│   │       ├── color-mapper.ts     # Map fills to color zones via metadata.json
│   │       ├── accessory-mapper.ts # Extract <rect data-acc-slot> placeholders
│   │       └── binary-writer.ts    # Serialize to .dofasset
│   └── renderer/                   # Rust + Vello — .dofasset → GPU render
│       ├── Cargo.toml              # deps: vello 0.5, wgpu 25, pollster, image, bytemuck
│       └── src/
│           ├── main.rs             # CLI: load .dofasset, render frames to PNG
│           ├── format.rs           # Binary deserialization → DofAsset structs
│           ├── scene_builder.rs    # Build Vello Scene from frame data
│           ├── color.rs            # HSL color zone replacement
│           └── pattern.rs          # Pattern fill → Vello image brush
├── input/sprites/10/               # Copied test sprite data (already done)
└── output/                         # Generated .dofasset + test PNGs
```

## Binary Format (.dofasset v1)

Little-endian. f32 coords. u16 indices (u32 for transform/path/draw tables).

```
HEADER (20 bytes)
  magic: [u8; 4] = "DASF"
  version: u16 = 1
  asset_type: u16 = 0 (sprite)
  asset_id: u32
  section_count: u16
  flags: u16
  reserved: u32

SECTION DIRECTORY (accessed by offset, order doesn't matter)
  For each section: type: u16, offset: u32, length: u32

SECTIONS:

  PATH_TABLE
    count: u32
    Per path: segment_count: u16, segments: [type: u8, coords: f32[0-6]]

  DRAW_CMD_TABLE
    count: u32
    Per command:
      type: u8 (0=fill, 1=stroke, 2=pattern_fill)
      path_id: u32
      fill_rule: u8
      transform: [f32; 6]
      type 0: color_rgba: [u8;4], zone_id: u8
      type 1: color_rgba: [u8;4], zone_id: u8, width_mode: u8, width: f32, opacity: f32, cap: u8, join: u8
      type 2: image_id: u16, pattern_transform: [f32; 6]

  BODY_PART_TABLE
    count: u16
    Per part: cmd_count: u16, cmd_ids: u32[]

  TRANSFORM_TABLE
    count: u32
    transforms: [f32; 6][] (packed 24 bytes each)

  IMAGE_TABLE
    count: u16
    Per image: width: u32, height: u32, data_len: u32, png_bytes: u8[]

  COLOR_ZONE_TABLE
    zone_count: u8
    Per zone: zone_id: u8, player_color_idx: u8, color_count: u16, colors: [u8;3][]

  STRING_TABLE
    count: u16, entries: (offset: u32, len: u16)[], blob: u8[]

  ANIMATION_TABLE
    count: u16
    Per anim: name_id: u16, fps: u16, offset_x: f32, offset_y: f32, frame_count: u16, frame_ids: u32[]

  FRAME_TABLE
    count: u32
    Per frame: clip: [f32;4], offset: [f32;2], part_count: u16, acc_count: u8,
              parts: (body_part_id: u16, transform_id: u32)[],
              accs: (slot: u8, depth: u8, transform_id: u32)[]
```

## Implementation Steps

### Step 1: Scaffold monorepo + copy test data
- `package.json` workspace, `packages/compiler/package.json` (dep: `cheerio`), `tsconfig.json`
- `packages/renderer/Cargo.toml` (deps: `vello 0.5`, `wgpu 25`, `pollster`, `image`, `bytemuck`)
- Copy `assets/spritesheets/sprites/10/` → `input/sprites/10/` (already done)

### Step 2: Compiler — path parser + SVG parser (cheerio)
- `path-parser.ts`: SVG `d` → `PathSegment[]` (M/L/Q/C/Z/H/V/S/T/A + lowercase relative variants)
- `svg-parser.ts`: Parse atlas.svg with **cheerio** (`cheerio.load(svg, { xml: true })`):
  - Extract `<defs>`: `<path id>`, `<g id>`, `<clipPath>`, `<pattern>`, `<use>` alias chains
  - Recursively resolve `<use xlink:href="#...">` chains to leaf paths
  - Extract frames: `<g clip-path>` → list of (resolved_body_part, transform)
  - Extract `<pattern>` with base64 images + patternTransform
  - Extract `<rect data-acc-slot>` accessory placeholders

### Step 3: Compiler — 6-level deduplicator
- Parse ALL 132 animations of sprite 10
- Level 1: Path table (hash normalized `d` strings)
- Level 2: Draw command table (path_id + fill/stroke attrs)
- Level 3: Body part table (ordered cmd_id lists, cross-animation dedup)
- Level 4: Transform table (dedup identical `[a,b,c,d,tx,ty]` 6-tuples)
- Level 5: Frame dedup (atlas.json duplicates + cross-animation identical frames)
- Level 6: Image dedup (content hash of base64 PNG bytes)
- Print dedup stats

### Step 4: Compiler — Color mapper + accessory mapper + binary writer + CLI
- `color-mapper.ts`: metadata.json → tag draw commands with zone_id
- `accessory-mapper.ts`: `<rect data-acc-slot>` → per-frame slot data
- `binary-writer.ts`: Serialize all deduped tables to `.dofasset`
- `index.ts`: CLI — `bun run src/index.ts --input <sprite_dir> --output <file.dofasset>`

### Step 5: Renderer — Format deserialization
- `format.rs`: Read .dofasset binary → `DofAsset` struct
- Path segments → `kurbo::BezPath`
- Colors → `peniko::Color`, transforms → `kurbo::Affine`
- Decode PNGs → `peniko::Image` via `image` crate

### Step 6: Renderer — Scene builder + basic rendering
- `scene_builder.rs`: `(animation_name, frame_index, player_colors?, resolution)` → Vello Scene
  - Look up frame via animation's frame_order
  - Push clip layer for frame clip_rect
  - Iterate part instances: compose `frame_offset * part_transform * draw_cmd_transform`
  - `scene.fill()` / `scene.stroke()` with resolved brushes
- `main.rs`: GPU setup (wgpu), render to texture, save PNG, print timing

### Step 7: Renderer — Pattern fills + strokes
- `pattern.rs`: `Brush::Image` + `Extend::Repeat` + pattern transform (from test-vello code)
- Resolution-dependent stroke: `width = 1.0 / resolution`
- Stroke opacity handling

### Step 8: Renderer — Color zones
- `color.rs`: Port HSL replacement from `vite.config.ts:257-286`
  - `rgb_to_hsl()`, `hsl_to_rgb()` — exact same logic
  - Pre-compute color lookup table per character

### Step 9: Validation
- Render staticF (1 frame, static) → compare vs SVG
- Render walkF (42 frames, animation) → compare vs SVG
- Render with player colors → compare vs vite-composed SVG
- Target: near pixel-perfect match with `test-vello/svgs/sprite_000.svg`

## Transform Composition Order

```
final = frame_offset_translate * use_element_transform * group_internal_transforms * path_translate
```

SVG tree: `<g clip-path> → <g translate(offset)> → <use transform="matrix(...)"> → <g id="part"> → [paths with own transforms]`

## Key Reference Files

| File | What |
|------|------|
| `input/sprites/10/staticF/atlas.svg` | Primary test SVG — full structure |
| `input/sprites/10/staticF/atlas.json` | Frame metadata — 1 frame |
| `input/sprites/10/walkF/atlas.svg` | Animation test — 42 frames |
| `input/sprites/10/walkF/atlas.json` | Animation metadata |
| `input/sprites/10/anim0L/atlas.svg` | Complex attack animation |
| `../dofuswebclient3-vello-shared-test/apps/electrobun/vite.config.ts:97-450` | Color replacement + accessory composition logic |
| `../dofuswebclient3-vello-shared-test/test-vello/src/main.rs` | Vello pattern fills, clips, GPU setup |
| `../dofuswebclient3-vello-shared-test/apps/electrobun/src/lib/ank/battlefield/stress-test.ts` | Look string format + generation |

## Exit Condition

The renderer produces output that is near pixel-perfect compared to the composed SVGs in `test-vello/svgs/sprite_000.svg` (which has colors + shield + accessories applied). Must work on:
- **Static poses** (staticF — 1 frame)
- **Animations** (walkF — 42 frames, anim0L — 42 frames)

## Rules

- Clean code, no `any` types in TypeScript
- Use **cheerio** for SVG parsing
- Compiler in Bun/TypeScript, renderer in Rust with Vello
- Deduplicate as aggressively as possible at every level

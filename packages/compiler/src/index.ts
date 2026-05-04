import { readFileSync, writeFileSync, readdirSync, statSync, existsSync, mkdirSync } from "node:fs";
import { join, resolve, dirname, basename } from "node:path";
import { parseSvg } from "./svg-parser.js";
import { extractImages } from "./image-extractor.js";
import { deduplicate } from "./deduplicator.js";
import { applyColorZones } from "./color-mapper.js";
import { writeBinary } from "./binary-writer.js";
import type { AtlasJson, ParsedSvg, ParsedPattern, SpriteMetadata } from "./types.js";

function parseArgs(): { input: string; output: string } {
  const args = process.argv.slice(2);
  let input = "";
  let output = "";

  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--input" && args[i + 1]) {
      input = args[i + 1]!;
      i++;
    } else if (args[i] === "--output" && args[i + 1]) {
      output = args[i + 1]!;
      i++;
    }
  }

  if (!input || !output) {
    console.error("Usage: bun run src/index.ts --input <sprite_dir> --output <file.dofasset>");
    process.exit(1);
  }

  return { input: resolve(input), output: resolve(output) };
}

function discoverAnimations(spriteDir: string): string[] {
  return readdirSync(spriteDir)
    .filter((name) => {
      if (name === "_liaison_" || name === "manifest.json" || name === "metadata.json") return false;
      const stat = statSync(join(spriteDir, name));
      return stat.isDirectory();
    })
    .sort();
}

function main(): void {
  const { input, output } = parseArgs();
  const startTime = performance.now();

  console.log(`Compiling sprite: ${input}`);

  // Extract sprite ID from directory name
  const spriteId = parseInt(basename(input), 10) || 0;

  // Parse all animations — supports two input formats:
  // 1. Sprite format: subdirectories with atlas.svg + atlas.json per animation
  // 2. Tile format: manifest.json + atlas.svg at root level
  const allPatterns: ParsedPattern[] = [];
  const animationInputs: { name: string; svg: ParsedSvg; atlas: AtlasJson }[] = [];
  let totalSvgBytes = 0;

  const manifestPath = join(input, "manifest.json");
  const isTile = existsSync(manifestPath) && existsSync(join(input, "atlas.svg"));

  if (isTile) {
    // Tile format: single atlas.svg + manifest.json at root level.
    //
    // Two layouts are supported:
    //  - NEW (slim manifest): manifest.json enumerates animation names only;
    //    the real frame data lives in a sibling atlas.json at the same level.
    //    svg-spritesheet writes this layout with `frames: []` in manifest.json
    //    because tiles always have a single animation.
    //  - LEGACY: manifest.json contains everything including the frames array.
    //    Kept for backward-compat with older atlases.
    const manifest = JSON.parse(readFileSync(manifestPath, "utf-8"));
    const svgContent = readFileSync(join(input, "atlas.svg"), "utf-8");
    totalSvgBytes += svgContent.length;
    const svg = parseSvg(svgContent);
    allPatterns.push(...svg.patterns);

    const atlasJsonPath = join(input, "atlas.json");
    const atlasJson = existsSync(atlasJsonPath)
      ? (JSON.parse(readFileSync(atlasJsonPath, "utf-8")) as Partial<AtlasJson> & { animation?: string })
      : null;

    for (const [animName, animData] of Object.entries(manifest.animations) as [string, any][]) {
      // Prefer atlas.json as the source of truth; fall back to manifest entry fields.
      // For tiles (always single-animation) atlas.json fully describes the animation.
      const frames = atlasJson?.frames ?? animData.frames ?? [];
      const frameOrder = atlasJson?.frameOrder ?? animData.frameOrder ?? [];
      if (frames.length === 0) {
        console.warn(`  ${animName}: no frames found (atlas.json=${atlasJson ? "present" : "missing"})`);
      }
      const atlas: AtlasJson = {
        version: manifest.version ?? atlasJson?.version ?? 1,
        animation: animName,
        width: atlasJson?.width ?? animData.width,
        height: atlasJson?.height ?? animData.height,
        offsetX: atlasJson?.offsetX ?? animData.offsetX ?? 0,
        offsetY: atlasJson?.offsetY ?? animData.offsetY ?? 0,
        frames,
        frameOrder,
        duplicates: atlasJson?.duplicates ?? animData.duplicates ?? {},
        fps: atlasJson?.fps ?? animData.fps ?? 60,
        baseFrame: atlasJson?.baseFrame ?? animData.baseFrame,
        baseZOrder: atlasJson?.baseZOrder ?? animData.baseZOrder,
      };
      animationInputs.push({ name: animName, svg, atlas });
    }
    console.log(`Tile format: ${animationInputs.length} animation(s)`);
  } else {
    // Sprite format: discover animation subdirectories
    const animNames = discoverAnimations(input);
    console.log(`Found ${animNames.length} animations`);

    for (const animName of animNames) {
      const animDir = join(input, animName);
      const svgPath = join(animDir, "atlas.svg");
      const jsonPath = join(animDir, "atlas.json");

      if (!existsSync(svgPath) || !existsSync(jsonPath)) {
        console.warn(`  Skipping ${animName}: missing atlas.svg or atlas.json`);
        continue;
      }

      const svgContent = readFileSync(svgPath, "utf-8");
      const atlasContent = readFileSync(jsonPath, "utf-8");
      totalSvgBytes += svgContent.length;

      const svg = parseSvg(svgContent);
      const atlas = JSON.parse(atlasContent) as AtlasJson;

      allPatterns.push(...svg.patterns);
      animationInputs.push({ name: animName, svg, atlas });
    }
  }

  const parseTime = performance.now();
  console.log(`Parsed ${animationInputs.length} animations in ${(parseTime - startTime).toFixed(0)}ms`);
  console.log(`Total SVG size: ${(totalSvgBytes / 1024).toFixed(1)} KB`);

  // Extract and deduplicate images
  const images = extractImages(allPatterns);
  console.log(`Extracted ${images.length} unique image(s)`);

  // Run deduplication
  const asset = deduplicate(spriteId, animationInputs, images);

  // Apply color zones if metadata exists
  const metadataPath = join(input, "metadata.json");
  let metadata: SpriteMetadata | null = null;
  if (existsSync(metadataPath)) {
    metadata = JSON.parse(readFileSync(metadataPath, "utf-8")) as SpriteMetadata;
    console.log(`Loaded color zone metadata`);
  }
  applyColorZones(asset, metadata);

  const dedupTime = performance.now();
  console.log(`Deduplication complete in ${(dedupTime - parseTime).toFixed(0)}ms`);

  // Print dedup stats
  console.log(`\n=== Deduplication Stats ===`);
  console.log(`  Unique paths:      ${asset.paths.length}`);
  console.log(`  Draw commands:     ${asset.drawCommands.length}`);
  console.log(`  Body parts:        ${asset.bodyParts.length}`);
  console.log(`  Transforms:        ${asset.transforms.length}`);
  console.log(`  Frames:            ${asset.frames.length}`);
  console.log(`  Animations:        ${asset.animations.length}`);
  console.log(`  Images:            ${asset.images.length}`);
  console.log(`  Color zones:       ${asset.colorZones.length}`);

  // Write binary
  const binary = writeBinary(asset);

  // Ensure output directory exists
  const outDir = dirname(output);
  if (!existsSync(outDir)) {
    mkdirSync(outDir, { recursive: true });
  }

  writeFileSync(output, binary);

  const writeTime = performance.now();
  console.log(`\nBinary written: ${output}`);
  console.log(`  Size: ${(binary.length / 1024).toFixed(1)} KB (vs ${(totalSvgBytes / 1024).toFixed(1)} KB SVG)`);
  console.log(`  Compression: ${((1 - binary.length / totalSvgBytes) * 100).toFixed(1)}% smaller`);
  console.log(`\nTotal time: ${(writeTime - startTime).toFixed(0)}ms`);
}

main();

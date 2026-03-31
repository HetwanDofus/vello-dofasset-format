import { readdir, unlink } from "node:fs/promises";
import { join } from "node:path";
import type { AtlasJson } from "./types.js";

async function spawn(cmd: string[], opts?: { cwd?: string }): Promise<void> {
  const proc = Bun.spawn(cmd, { stdout: "pipe", stderr: "pipe", ...opts });
  const exit = await proc.exited;
  if (exit !== 0) {
    const err = await new Response(proc.stderr).text();
    throw new Error(`${cmd[0]} failed (${exit}): ${err}`);
  }
}

async function cropFromAtlas(
  atlasPng: string,
  x: number,
  y: number,
  w: number,
  h: number,
  resolution: number,
  outputPath: string,
): Promise<void> {
  const cropW = Math.ceil(w) * resolution;
  const cropH = Math.ceil(h) * resolution;
  const cropX = Math.round(x * resolution);
  const cropY = Math.round(y * resolution);
  await spawn([
    "magick", atlasPng,
    "-crop", `${cropW}x${cropH}+${cropX}+${cropY}`,
    "+repage", outputPath,
  ]);
}

/**
 * Render the reference SVG atlas via rsvg-convert and crop individual frames.
 * For baseFrame animations, composites the base frame below each delta frame.
 */
export async function renderReferenceFrames(
  spriteDir: string,
  animName: string,
  resolution: number,
  outputDir: string,
  svgRendererBin: string,
): Promise<{ framePaths: Map<number, string>; atlas: AtlasJson }> {
  const animDir = join(spriteDir, animName);
  const svgPath = join(animDir, "atlas.svg");
  const jsonPath = join(animDir, "atlas.json");

  const atlas: AtlasJson = await Bun.file(jsonPath).json();
  const svgContent = await Bun.file(svgPath).text();

  // Replace __RESOLUTION__ with 1/scale
  const resValue = (1 / resolution).toFixed(10);
  const processedSvg = svgContent.replaceAll("__RESOLUTION__", resValue);

  // Write processed SVG to temp file
  const tmpSvg = join(outputDir, `_tmp_${animName}.svg`);
  await Bun.write(tmpSvg, processedSvg);

  // Render full atlas via rsvg-convert
  const atlasFullPng = join(outputDir, `${animName}_atlas.png`);
  await spawn([svgRendererBin, tmpSvg, "--zoom", String(resolution), "--output", atlasFullPng]);

  // Clean up temp SVG
  await unlink(tmpSvg).catch(() => {});

  // Build frame index
  const frameIdToIndex = new Map<string, number>();
  for (let i = 0; i < atlas.frames.length; i++) {
    frameIdToIndex.set(atlas.frames[i]!.id, i);
  }

  // If this animation has a baseFrame, crop it once
  const hasBase = atlas.baseFrame != null;
  let basePng: string | undefined;
  if (hasBase) {
    const bf = atlas.baseFrame!;
    basePng = join(outputDir, `${animName}_base.png`);
    await cropFromAtlas(atlasFullPng, bf.x, bf.y, bf.width, bf.height, resolution, basePng);
  }

  // Two-pass: crop unique frames first, then copy duplicates
  const framePaths = new Map<number, string>();
  const uniqueFrameMap = new Map<number, number>();
  const cropPromises: Promise<void>[] = [];
  const duplicateCopies: { displayIdx: number; sourceDisplayIdx: number }[] = [];

  for (let displayIdx = 0; displayIdx < atlas.frameOrder.length; displayIdx++) {
    const frameId = atlas.frameOrder[displayIdx]!;
    const resolvedId = atlas.duplicates[frameId] ?? frameId;
    const uniqueIdx = frameIdToIndex.get(resolvedId);
    if (uniqueIdx === undefined) continue;

    const framePng = join(outputDir, `${animName}_frame_${displayIdx}.png`);
    framePaths.set(displayIdx, framePng);

    const existingDisplayIdx = uniqueFrameMap.get(uniqueIdx);
    if (existingDisplayIdx !== undefined) {
      duplicateCopies.push({ displayIdx, sourceDisplayIdx: existingDisplayIdx });
      continue;
    }
    uniqueFrameMap.set(uniqueIdx, displayIdx);

    const frame = atlas.frames[uniqueIdx]!;

    if (hasBase) {
      // For baseFrame animations: crop the delta, then composite base + delta
      const bf = atlas.baseFrame!;
      const deltaPng = join(outputDir, `${animName}_delta_${displayIdx}.png`);

      cropPromises.push(
        (async () => {
          await cropFromAtlas(atlasFullPng, frame.x, frame.y, frame.width, frame.height, resolution, deltaPng);

          // Create a canvas the size of the base frame, composite delta on top
          const canvasW = Math.ceil(bf.width) * resolution;
          const canvasH = Math.ceil(bf.height) * resolution;

          // Delta position relative to base: both use the same inner transform in the SVG,
          // so delta appears at its clip position minus base clip position
          const deltaX = Math.round((frame.x - bf.x) * resolution);
          const deltaY = Math.round((frame.y - bf.y) * resolution);

          const baseBelow = atlas.baseZOrder !== "above";
          if (baseBelow) {
            // Base below: start with base, overlay delta
            await spawn([
              "magick", basePng!,
              "(", deltaPng, ")",
              "-geometry", `+${deltaX}+${deltaY}`,
              "-composite", framePng,
            ]);
          } else {
            // Base above: start with delta on canvas, overlay base
            await spawn([
              "magick", "-size", `${canvasW}x${canvasH}`, "xc:transparent",
              "(", deltaPng, ")", "-geometry", `+${deltaX}+${deltaY}`, "-composite",
              "(", basePng!, ")", "-composite",
              framePng,
            ]);
          }

          await unlink(deltaPng).catch(() => {});
        })(),
      );
    } else {
      // Normal frame: just crop
      cropPromises.push(
        cropFromAtlas(atlasFullPng, frame.x, frame.y, frame.width, frame.height, resolution, framePng),
      );
    }
  }

  await Promise.all(cropPromises);

  // Copy duplicates
  for (const { displayIdx, sourceDisplayIdx } of duplicateCopies) {
    const sourcePng = join(outputDir, `${animName}_frame_${sourceDisplayIdx}.png`);
    const destPng = join(outputDir, `${animName}_frame_${displayIdx}.png`);
    const buf = await Bun.file(sourcePng).arrayBuffer();
    await Bun.write(destPng, buf);
  }

  return { framePaths, atlas };
}

/**
 * List animation subdirectories for a sprite.
 */
export async function listAnimations(spriteDir: string): Promise<string[]> {
  const entries = await readdir(spriteDir, { withFileTypes: true });
  const anims: string[] = [];
  for (const entry of entries) {
    if (!entry.isDirectory()) continue;
    const svgFile = Bun.file(join(spriteDir, entry.name, "atlas.svg"));
    if (await svgFile.exists()) {
      anims.push(entry.name);
    }
  }
  return anims.sort();
}

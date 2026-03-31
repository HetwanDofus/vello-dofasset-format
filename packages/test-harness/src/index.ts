import { mkdir, rm } from "node:fs/promises";
import { join, resolve } from "node:path";
import type { TestConfig, FrameResult } from "./types.js";
import { renderReferenceFrames, listAnimations } from "./reference-renderer.js";
import { renderOurFrames, ensureRendererBuilt } from "./our-renderer.js";
import { compareFrames, buildFrameResult } from "./comparator.js";
import { printFrameResult, printSummary, buildReport } from "./report.js";

const ROOT = resolve(import.meta.dir, "../../..");

const PRESETS: Record<string, Partial<TestConfig>> = {
  smoke: {
    spriteIds: [10],
    animations: ["staticF", "walkF"],
    frames: "first",
  },
  standard: {
    spriteIds: [],
    animations: "all",
    frames: "first",
  },
  full: {
    spriteIds: [], // filled dynamically from input dir
    animations: "all",
    frames: "all",
  },
};

function parseArgs(): TestConfig {
  const args = process.argv.slice(2);
  let preset: string | undefined;
  let spriteIds: number[] | undefined;
  let animations: string[] | "all" | undefined;
  let frames: "all" | "first" | number[] | undefined;
  let resolution = 3;
  let threshold = 0.001;

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--preset":
        preset = args[++i];
        break;
      case "--sprite":
        spriteIds = args[++i]!.split(",").map(Number);
        break;
      case "--animation":
        animations = args[++i]!.split(",");
        break;
      case "--frames":
        const v = args[++i]!;
        frames = v === "all" ? "all" : v === "first" ? "first" : v.split(",").map(Number);
        break;
      case "--resolution":
        resolution = parseFloat(args[++i]!);
        break;
      case "--threshold":
        threshold = parseFloat(args[++i]!);
        break;
    }
  }

  const p = preset ? PRESETS[preset] : {};

  return {
    spriteIds: spriteIds ?? (p?.spriteIds?.length ? p.spriteIds : []),
    animations: animations ?? p?.animations ?? "all",
    frames: frames ?? p?.frames ?? "first",
    resolution,
    rmseThreshold: threshold,
    inputDir: join(ROOT, "input", "sprites"),
    dofassetDir: join(ROOT, "output"),
    outputDir: join(ROOT, "test-output"),
    rendererBin: join(ROOT, "packages", "renderer", "target", "release", "dofasset-renderer"),
    svgRendererBin: join(ROOT, "packages", "renderer", "target", "release", "svg-renderer"),
  };
}

async function discoverSprites(inputDir: string): Promise<number[]> {
  const { readdir } = await import("node:fs/promises");
  const entries = await readdir(inputDir);
  return entries.filter((e) => /^\d+$/.test(e)).map(Number).sort((a, b) => a - b);
}

async function run(): Promise<void> {
  const config = parseArgs();

  // Discover sprites if full preset
  if (config.spriteIds.length === 0) {
    config.spriteIds = await discoverSprites(config.inputDir);
  }

  console.log(`\nTest config:`);
  console.log(`  Sprites: ${config.spriteIds.join(", ")}`);
  console.log(`  Animations: ${Array.isArray(config.animations) ? config.animations.join(", ") : config.animations}`);
  console.log(`  Frames: ${Array.isArray(config.frames) ? config.frames.join(", ") : config.frames}`);
  console.log(`  Resolution: ${config.resolution}x`);
  console.log(`  RMSE threshold: ${config.rmseThreshold}`);

  // Build renderer
  console.log("\nBuilding renderer...");
  const rendererDir = join(ROOT, "packages", "renderer");
  const { dofassetBin, svgBin } = await ensureRendererBuilt(rendererDir);
  config.rendererBin = dofassetBin;
  config.svgRendererBin = svgBin;
  console.log(`  Renderer: ${config.rendererBin}`);
  console.log(`  SVG Renderer: ${config.svgRendererBin}`);

  const allResults: FrameResult[] = [];

  for (const spriteId of config.spriteIds) {
    const spriteDir = join(config.inputDir, String(spriteId));
    const dofassetPath = join(config.dofassetDir, `sprite_${spriteId}.dofasset`);

    // Check .dofasset exists
    if (!(await Bun.file(dofassetPath).exists())) {
      console.log(`\n  Skipping sprite ${spriteId}: no .dofasset file`);
      continue;
    }

    // Determine animations to test
    let animNames: string[];
    if (config.animations === "all") {
      animNames = await listAnimations(spriteDir);
    } else {
      animNames = config.animations;
    }

    console.log(`\nSprite ${spriteId}: testing ${animNames.length} animations`);

    for (const animName of animNames) {
      // Create output directories
      const refDir = join(config.outputDir, "reference", String(spriteId));
      const ourDir = join(config.outputDir, "ours", String(spriteId));
      const diffDir = join(config.outputDir, "diffs", String(spriteId));
      await mkdir(refDir, { recursive: true });
      await mkdir(ourDir, { recursive: true });
      await mkdir(diffDir, { recursive: true });

      try {
        // Render reference frames
        const { framePaths: refFrames, atlas } = await renderReferenceFrames(
          spriteDir, animName, config.resolution, refDir, config.svgRendererBin,
        );

        // Determine which frame indices to test
        let frameIndices: number[];
        if (config.frames === "all") {
          frameIndices = [...refFrames.keys()].sort((a, b) => a - b);
        } else if (config.frames === "first") {
          frameIndices = refFrames.size > 0 ? [0] : [];
        } else {
          frameIndices = config.frames.filter((i) => refFrames.has(i));
        }

        if (frameIndices.length === 0) continue;

        // Render our frames
        const ourFrames = await renderOurFrames(
          config.rendererBin, dofassetPath, animName,
          frameIndices, config.resolution, ourDir,
        );

        // Compare each frame
        for (const idx of frameIndices) {
          const refPath = refFrames.get(idx);
          const ourPath = ourFrames.get(idx);
          if (!refPath || !ourPath) continue;

          const diffPath = join(diffDir, `${animName}_frame_${idx}_diff.png`);

          try {
            const compare = await compareFrames(refPath, ourPath, diffPath);
            const result = buildFrameResult(
              spriteId, animName, idx, compare, config.rmseThreshold, diffPath,
            );
            allResults.push(result);
            printFrameResult(result);

            // Clean up diff image for passing tests
            if (result.status === "pass") {
              try {
                const { unlink } = await import("node:fs/promises");
                await unlink(diffPath);
              } catch {}
            }
          } catch (err) {
            const result: FrameResult = {
              spriteId,
              animation: animName,
              frameIndex: idx,
              status: "error",
              rmse: 0,
              normalizedRmse: 0,
              refDimensions: [0, 0],
              ourDimensions: [0, 0],
              errorMessage: String(err),
            };
            allResults.push(result);
            printFrameResult(result);
          }
        }
      } catch (err) {
        console.log(`  [ERROR] ${animName}: ${err}`);
        allResults.push({
          spriteId,
          animation: animName,
          frameIndex: 0,
          status: "error",
          rmse: 0,
          normalizedRmse: 0,
          refDimensions: [0, 0],
          ourDimensions: [0, 0],
          errorMessage: String(err),
        });
      }
    }

    // Cleanup rendered files for this sprite to save disk space
    await rm(join(config.outputDir, "reference", String(spriteId)), { recursive: true, force: true });
    await rm(join(config.outputDir, "ours", String(spriteId)), { recursive: true, force: true });
  }

  // Print summary
  if (allResults.length > 0) {
    printSummary(allResults);

    // Write JSON report
    const report = buildReport(config, allResults);
    const reportPath = join(config.outputDir, "report.json");
    await Bun.write(reportPath, JSON.stringify(report, null, 2));
    console.log(`Report saved to ${reportPath}`);
  } else {
    console.log("\nNo frames were tested.");
  }

  // Exit with non-zero if any failures
  const failures = allResults.filter((r) => r.status === "fail" || r.status === "error");
  if (failures.length > 0) {
    process.exit(1);
  }
}

run().catch((err) => {
  console.error("Fatal error:", err);
  process.exit(2);
});

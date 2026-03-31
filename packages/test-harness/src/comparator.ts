import type { FrameResult } from "./types.js";

interface CompareResult {
  rmse: number;
  normalizedRmse: number;
  refDimensions: [number, number];
  ourDimensions: [number, number];
}

/**
 * Get image dimensions via magick identify.
 */
async function getImageDimensions(path: string): Promise<[number, number]> {
  const proc = Bun.spawn(
    ["magick", "identify", "-format", "%wx%h", path],
    { stdout: "pipe", stderr: "pipe" },
  );
  const exit = await proc.exited;
  if (exit !== 0) return [0, 0];
  const out = await new Response(proc.stdout).text();
  const [w, h] = out.trim().split("x").map(Number);
  return [w ?? 0, h ?? 0];
}

/**
 * Compare two PNGs using ImageMagick RMSE metric.
 */
export async function compareFrames(
  refPath: string,
  ourPath: string,
  diffPath: string,
): Promise<CompareResult> {
  const [refDims, ourDims] = await Promise.all([
    getImageDimensions(refPath),
    getImageDimensions(ourPath),
  ]);

  // If dimensions differ, resize ours to match ref for comparison
  let comparePath = ourPath;
  if (refDims[0] !== ourDims[0] || refDims[1] !== ourDims[1]) {
    // Still compare, but note the mismatch — resize to ref dims
    comparePath = ourPath.replace(".png", "_resized.png");
    const resizeProc = Bun.spawn(
      ["magick", ourPath, "-resize", `${refDims[0]}x${refDims[1]}!`, comparePath],
      { stdout: "pipe", stderr: "pipe" },
    );
    await resizeProc.exited;
  }

  const proc = Bun.spawn(
    ["magick", "compare", "-metric", "RMSE", refPath, comparePath, diffPath],
    { stdout: "pipe", stderr: "pipe" },
  );
  await proc.exited;

  // magick compare writes RMSE to stderr: "1234.56 (0.0188)" or "2.78 (4.24692e-05)"
  const errOutput = await new Response(proc.stderr).text();
  const match = errOutput.match(/[\d.]+\s+\(([\d.eE+-]+)\)/);
  const normalizedRmse = match ? parseFloat(match[1]!) : 1.0;
  const rmseMatch = errOutput.match(/([\d.eE+-]+)\s+\(/);
  const rmse = rmseMatch ? parseFloat(rmseMatch[1]!) : 65535;

  // Clean up resized file
  if (comparePath !== ourPath) {
    try {
      const { unlink } = await import("node:fs/promises");
      await unlink(comparePath);
    } catch {}
  }

  return { rmse, normalizedRmse, refDimensions: refDims, ourDimensions: ourDims };
}

/**
 * Build a FrameResult from comparison.
 */
export function buildFrameResult(
  spriteId: number,
  animation: string,
  frameIndex: number,
  compare: CompareResult,
  threshold: number,
  diffPath: string,
): FrameResult {
  const dimsMismatch =
    compare.refDimensions[0] !== compare.ourDimensions[0] ||
    compare.refDimensions[1] !== compare.ourDimensions[1];

  const passed = compare.normalizedRmse <= threshold && !dimsMismatch;

  return {
    spriteId,
    animation,
    frameIndex,
    status: passed ? "pass" : "fail",
    rmse: compare.rmse,
    normalizedRmse: compare.normalizedRmse,
    refDimensions: compare.refDimensions,
    ourDimensions: compare.ourDimensions,
    diffImagePath: passed ? undefined : diffPath,
    errorMessage: dimsMismatch
      ? `Dimension mismatch: ref ${compare.refDimensions.join("x")} vs ours ${compare.ourDimensions.join("x")}`
      : undefined,
  };
}

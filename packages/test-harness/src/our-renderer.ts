import { join } from "node:path";

/**
 * Render a single frame from a .dofasset file using our Rust renderer.
 */
export async function renderOurFrame(
  rendererBin: string,
  dofassetPath: string,
  animName: string,
  frameIndex: number,
  resolution: number,
  outputPath: string,
): Promise<void> {
  const proc = Bun.spawn(
    [
      rendererBin,
      dofassetPath,
      "--animation", animName,
      "--frame", String(frameIndex),
      "--resolution", String(resolution),
      "--output", outputPath,
    ],
    { stdout: "pipe", stderr: "pipe" },
  );
  const exit = await proc.exited;
  if (exit !== 0) {
    const err = await new Response(proc.stderr).text();
    throw new Error(`dofasset-renderer failed (${exit}): ${err}`);
  }
}

/**
 * Render multiple frames, returning a map of frameIndex → outputPath.
 */
export async function renderOurFrames(
  rendererBin: string,
  dofassetPath: string,
  animName: string,
  frameIndices: number[],
  resolution: number,
  outputDir: string,
): Promise<Map<number, string>> {
  const framePaths = new Map<number, string>();

  // Render sequentially to avoid GPU contention
  for (const idx of frameIndices) {
    const outputPath = join(outputDir, `${animName}_frame_${idx}.png`);
    await renderOurFrame(rendererBin, dofassetPath, animName, idx, resolution, outputPath);
    framePaths.set(idx, outputPath);
  }

  return framePaths;
}

/**
 * Build the renderer binary if needed, return the path to the executable.
 */
export async function ensureRendererBuilt(rendererDir: string): Promise<{ dofassetBin: string; svgBin: string }> {
  const proc = Bun.spawn(
    ["cargo", "build", "--release", "--bin", "dofasset-renderer", "--bin", "svg-renderer"],
    {
      cwd: rendererDir,
      stdout: "pipe",
      stderr: "pipe",
    },
  );
  const exit = await proc.exited;
  if (exit !== 0) {
    const err = await new Response(proc.stderr).text();
    throw new Error(`cargo build failed: ${err}`);
  }
  return {
    dofassetBin: join(rendererDir, "target", "release", "dofasset-renderer"),
    svgBin: join(rendererDir, "target", "release", "svg-renderer"),
  };
}

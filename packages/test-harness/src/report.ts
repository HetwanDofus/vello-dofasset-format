import type { FrameResult, TestConfig, TestReport } from "./types.js";

export function printFrameResult(r: FrameResult): void {
  const tag = r.status === "pass" ? "\x1b[32mPASS\x1b[0m" : "\x1b[31mFAIL\x1b[0m";
  const rmseStr = r.normalizedRmse.toFixed(4);
  const dims =
    r.refDimensions[0] !== r.ourDimensions[0] || r.refDimensions[1] !== r.ourDimensions[1]
      ? ` dims: ${r.refDimensions.join("x")} vs ${r.ourDimensions.join("x")}`
      : "";
  console.log(
    `  [${tag}] sprite ${r.spriteId} / ${r.animation} frame ${r.frameIndex}: RMSE ${rmseStr}${dims}`,
  );
  if (r.errorMessage) {
    console.log(`         ${r.errorMessage}`);
  }
}

export function printSummary(results: FrameResult[]): void {
  const total = results.length;
  const passed = results.filter((r) => r.status === "pass").length;
  const failed = results.filter((r) => r.status === "fail").length;
  const errored = results.filter((r) => r.status === "error").length;
  const skipped = results.filter((r) => r.status === "skip").length;

  const worst = results.reduce(
    (max, r) => (r.normalizedRmse > max.normalizedRmse ? r : max),
    results[0]!,
  );

  console.log("\n========================================");
  console.log(`  Total:   ${total}`);
  console.log(
    `  Passed:  \x1b[32m${passed}\x1b[0m  Failed: \x1b[31m${failed}\x1b[0m  Errors: ${errored}  Skipped: ${skipped}`,
  );
  console.log(`  Pass rate: ${((passed / total) * 100).toFixed(1)}%`);
  if (worst) {
    console.log(
      `  Worst RMSE: ${worst.normalizedRmse.toFixed(4)} (sprite ${worst.spriteId} / ${worst.animation} frame ${worst.frameIndex})`,
    );
  }
  console.log("========================================\n");
}

export function buildReport(
  config: TestConfig,
  results: FrameResult[],
): TestReport {
  const total = results.length;
  const passed = results.filter((r) => r.status === "pass").length;
  const failed = results.filter((r) => r.status === "fail").length;
  const errored = results.filter((r) => r.status === "error").length;
  const skipped = results.filter((r) => r.status === "skip").length;

  const worst = results.reduce(
    (max, r) => (r.normalizedRmse > max.normalizedRmse ? r : max),
    results[0]!,
  );

  return {
    timestamp: new Date().toISOString(),
    config,
    summary: {
      total,
      passed,
      failed,
      errored,
      skipped,
      passRate: total > 0 ? passed / total : 0,
      worstRmse: worst?.normalizedRmse ?? 0,
      worstFrame: worst
        ? `sprite_${worst.spriteId}/${worst.animation}/frame_${worst.frameIndex}`
        : "",
    },
    results,
  };
}

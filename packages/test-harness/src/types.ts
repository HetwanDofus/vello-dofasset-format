export interface TestConfig {
  spriteIds: number[];
  animations: string[] | "all";
  frames: "all" | "first" | number[];
  resolution: number;
  rmseThreshold: number;
  inputDir: string;
  dofassetDir: string;
  outputDir: string;
  rendererBin: string;
  svgRendererBin: string;
}

export interface BaseFrame {
  id: string;
  x: number;
  y: number;
  width: number;
  height: number;
  offsetX: number;
  offsetY: number;
}

export interface AtlasJson {
  version: number;
  animation: string;
  width: number;
  height: number;
  offsetX: number;
  offsetY: number;
  frames: AtlasFrame[];
  frameOrder: string[];
  duplicates: Record<string, string>;
  fps: number;
  baseFrame?: BaseFrame;
  baseZOrder?: string;
}

export interface AtlasFrame {
  id: string;
  x: number;
  y: number;
  width: number;
  height: number;
  offsetX: number;
  offsetY: number;
}

export interface FrameResult {
  spriteId: number;
  animation: string;
  frameIndex: number;
  status: "pass" | "fail" | "error" | "skip";
  rmse: number;
  normalizedRmse: number;
  refDimensions: [number, number];
  ourDimensions: [number, number];
  diffImagePath?: string;
  errorMessage?: string;
}

export interface TestReport {
  timestamp: string;
  config: TestConfig;
  summary: {
    total: number;
    passed: number;
    failed: number;
    errored: number;
    skipped: number;
    passRate: number;
    worstRmse: number;
    worstFrame: string;
  };
  results: FrameResult[];
}

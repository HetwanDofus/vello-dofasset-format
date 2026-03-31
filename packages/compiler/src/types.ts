// ===== SVG Path Segments =====

export type PathCommandType = "M" | "L" | "Q" | "C" | "Z";

export interface PathSegment {
  type: PathCommandType;
  coords: number[]; // M=2, L=2, Q=4, C=6, Z=0
}

// ===== Draw Commands =====

export const enum DrawCommandType {
  Fill = 0,
  Stroke = 1,
  PatternFill = 2,
  GradientFill = 3,
}

export const enum StrokeWidthMode {
  Fixed = 0,
  Resolution = 1,
}

export const enum FillRule {
  NonZero = 0,
  EvenOdd = 1,
}

export interface Color {
  r: number;
  g: number;
  b: number;
  a: number; // 0-255
}

export interface FillDrawCommand {
  type: DrawCommandType.Fill;
  pathId: number;
  fillRule: FillRule;
  color: Color;
  colorZoneId: number; // 0 = none, 1-3 = zone
  transform: AffineTransform;
}

export interface StrokeDrawCommand {
  type: DrawCommandType.Stroke;
  pathId: number;
  fillRule: FillRule;
  color: Color;
  colorZoneId: number;
  widthMode: StrokeWidthMode;
  width: number;
  opacity: number;
  lineCap: number; // 0=butt, 1=round, 2=square
  lineJoin: number; // 0=miter, 1=round, 2=bevel
  transform: AffineTransform;
}

export interface PatternFillDrawCommand {
  type: DrawCommandType.PatternFill;
  pathId: number;
  fillRule: FillRule;
  imageId: number;
  patternTransform: AffineTransform;
  transform: AffineTransform;
}

export interface GradientStop {
  offset: number; // 0-1
  color: Color;
}

export interface GradientFillDrawCommand {
  type: DrawCommandType.GradientFill;
  pathId: number;
  fillRule: FillRule;
  gradientType: number; // 0 = radial, 1 = linear
  cx: number;
  cy: number;
  fx: number;
  fy: number;
  r: number;
  gradientTransform: AffineTransform;
  stops: GradientStop[];
  transform: AffineTransform;
}

export type DrawCommand = FillDrawCommand | StrokeDrawCommand | PatternFillDrawCommand | GradientFillDrawCommand;

// ===== Transforms =====

/** [a, b, c, d, tx, ty] - 2D affine transform matrix */
export type AffineTransform = [number, number, number, number, number, number];

export const IDENTITY_TRANSFORM: AffineTransform = [1, 0, 0, 1, 0, 0];

// ===== Body Parts =====

export interface BodyPart {
  id: number;
  drawCommandIds: number[];
}

// ===== Images =====

export interface ExtractedImage {
  id: number;
  width: number;
  height: number;
  pngBytes: Uint8Array;
  contentHash: string;
}

// ===== Color Zones =====

export interface ColorZone {
  zoneId: number; // 1-3
  playerColorIndex: number; // 1-3
  originalColors: Color[];
}

// ===== Frames =====

export interface PartInstance {
  bodyPartId: number;
  transformId: number;
}

export interface AccessorySlot {
  slotId: number; // 0-4
  depthIndex: number;
  transformId: number;
}

export interface Frame {
  clipRect: [number, number, number, number]; // x, y, w, h
  offsetX: number;
  offsetY: number;
  parts: PartInstance[];
  accessorySlots: AccessorySlot[];
  /** Transform ID for the frame's SVG offset (used for accessory positioning) */
  frameTransformId: number;
}

// ===== Animations =====

export interface Animation {
  name: string;
  fps: number;
  offsetX: number;
  offsetY: number;
  frameIds: number[]; // indices into global frame table (handles duplicates)
  /** Global frame ID for the base frame, or -1 if none */
  baseFrameId: number;
  /** 0 = below (base renders first), 1 = above */
  baseZOrder: number;
}

// ===== Atlas JSON (input format) =====

export interface AtlasFrame {
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
  baseFrame?: AtlasFrame;
  baseZOrder?: string;
}

// ===== Metadata JSON (color zones) =====

export interface SpriteMetadata {
  colorZones: Record<string, string[]>;
  colorMapping: Record<string, number>;
}

// ===== SVG Parser Output =====

export interface ParsedPath {
  segments: PathSegment[];
  fill: string | null; // hex color or "url(#patternId)" or "none"
  fillOpacity: number;
  fillRule: FillRule;
  stroke: string | null;
  strokeOpacity: number;
  strokeWidth: string | null; // "__RESOLUTION__" or numeric
  strokeLinecap: string;
  strokeLinejoin: string;
  transform: AffineTransform;
}

export interface ParsedGroup {
  id: string;
  children: ParsedNode[];
  transform: AffineTransform;
}

export interface ParsedUse {
  href: string;
  transform: AffineTransform;
}

export type ParsedNode =
  | { type: "path"; data: ParsedPath }
  | { type: "group"; data: ParsedGroup }
  | { type: "use"; data: ParsedUse };

export interface ParsedPattern {
  id: string;
  imageDataUri: string;
  patternTransform: AffineTransform;
  width: number;
  height: number;
}

export interface ParsedGradientStop {
  offset: number;
  color: string; // hex
  opacity: number;
}

export interface ParsedGradient {
  id: string;
  type: "radial" | "linear";
  cx: number;
  cy: number;
  fx: number;
  fy: number;
  r: number;
  gradientTransform: AffineTransform;
  stops: ParsedGradientStop[];
}

export interface ParsedClipRect {
  id: string;
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface ParsedFrameUse {
  href: string;
  transform: AffineTransform;
}

export interface ParsedAccessorySlot {
  slotId: number;
  tx: number;
  ty: number;
  matrix: AffineTransform | null;
  depth: number;
  /** Index into the uses array: this slot renders after uses[insertAfterPart - 1] */
  insertAfterPart: number;
}

export interface ParsedFrame {
  clipPathId: string;
  clipRect: ParsedClipRect;
  offsetTransform: AffineTransform;
  uses: ParsedFrameUse[];
  accessorySlots: ParsedAccessorySlot[];
}

export interface ParsedSvg {
  width: number;
  height: number;
  definitions: Map<string, ParsedNode>;
  clipPaths: Map<string, ParsedClipRect>;
  patterns: ParsedPattern[];
  gradients: ParsedGradient[];
  frames: ParsedFrame[];
}

// ===== Compiled Asset =====

export interface CompiledAsset {
  assetId: number;
  paths: PathSegment[][];
  drawCommands: DrawCommand[];
  bodyParts: BodyPart[];
  transforms: AffineTransform[];
  images: ExtractedImage[];
  colorZones: ColorZone[];
  animations: Animation[];
  frames: Frame[];
}

// ===== Binary Format Constants =====

export const MAGIC = new Uint8Array([0x44, 0x41, 0x53, 0x46]); // "DASF"
export const FORMAT_VERSION = 1;

export const enum AssetType {
  Sprite = 0,
}

export const enum SectionType {
  PathTable = 0,
  DrawCmdTable = 1,
  BodyPartTable = 2,
  TransformTable = 3,
  ImageTable = 4,
  ColorZoneTable = 5,
  StringTable = 6,
  AnimationTable = 7,
  FrameTable = 8,
}

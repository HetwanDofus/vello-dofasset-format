import { match } from "ts-pattern";
import type {
  ParsedSvg,
  ParsedNode,
  PathSegment,
  DrawCommand,
  DrawCommandType,
  FillDrawCommand,
  StrokeDrawCommand,
  PatternFillDrawCommand,
  GradientFillDrawCommand,
  GradientStop,
  BodyPart,
  AffineTransform,
  FillRule,
  StrokeWidthMode,
  Frame,
  PartInstance,
  AccessorySlot,
  Animation,
  AtlasJson,
  ExtractedImage,
  CompiledAsset,
} from "./types.js";
import { IDENTITY_TRANSFORM } from "./types.js";
import { serializePath } from "./path-parser.js";

function hash(data: string): string {
  return createHash("sha256").update(data).digest("hex").slice(0, 16);
}

function serializeTransform(t: AffineTransform): string {
  return t.map((v) => Math.round(v * 10000) / 10000).join(",");
}

function composeTransforms(a: AffineTransform, b: AffineTransform): AffineTransform {
  // Use Math.fround to match usvg's f32 transform composition precision.
  // Without this, JavaScript f64 intermediate results produce different f32 values
  // than usvg's native f32 composition, causing sub-pixel rendering differences.
  const f = Math.fround;
  return [
    f(f(a[0] * b[0]) + f(a[2] * b[1])),
    f(f(a[1] * b[0]) + f(a[3] * b[1])),
    f(f(a[0] * b[2]) + f(a[2] * b[3])),
    f(f(a[1] * b[2]) + f(a[3] * b[3])),
    f(f(f(a[0] * b[4]) + f(a[2] * b[5])) + a[4]),
    f(f(f(a[1] * b[4]) + f(a[3] * b[5])) + a[5]),
  ];
}

/** CSS named colors → hex */
const CSS_COLORS: Record<string, string> = {
  black: "#000000", white: "#ffffff", red: "#ff0000", green: "#008000", blue: "#0000ff",
  yellow: "#ffff00", cyan: "#00ffff", magenta: "#ff00ff", orange: "#ffa500", purple: "#800080",
  pink: "#ffc0cb", brown: "#a52a2a", gray: "#808080", grey: "#808080",
  lime: "#00ff00", navy: "#000080", teal: "#008080", maroon: "#800000", olive: "#808000",
  aqua: "#00ffff", fuchsia: "#ff00ff", silver: "#c0c0c0",
};

/** Parse fill color string to RGBA */
function parseColor(fill: string, opacity: number): { r: number; g: number; b: number; a: number } {
  let hex: string | undefined;
  if (fill.startsWith("#")) {
    hex = fill.slice(1);
  } else if (CSS_COLORS[fill.toLowerCase()]) {
    hex = CSS_COLORS[fill.toLowerCase()]!.slice(1);
  }
  if (hex) {
    if (hex.length === 3) hex = hex[0]! + hex[0]! + hex[1]! + hex[1]! + hex[2]! + hex[2]!;
    return {
      r: parseInt(hex.slice(0, 2), 16),
      g: parseInt(hex.slice(2, 4), 16),
      b: parseInt(hex.slice(4, 6), 16),
      a: Math.round(opacity * 255),
    };
  }
  return { r: 0, g: 0, b: 0, a: Math.round(opacity * 255) };
}

function lineCapToNum(cap: string): number {
  switch (cap) {
    case "round": return 1;
    case "square": return 2;
    default: return 0; // butt
  }
}

function lineJoinToNum(join: string): number {
  switch (join) {
    case "round": return 1;
    case "bevel": return 2;
    default: return 0; // miter
  }
}

interface PatternLookup {
  patternId: string;
  imageId: number;
  patternTransform: AffineTransform;
}

interface GradientLookup {
  gradientType: number; // 0 = radial, 1 = linear
  cx: number;
  cy: number;
  fx: number;
  fy: number;
  r: number;
  gradientTransform: AffineTransform;
  stops: GradientStop[];
}

function parseGradientStopColor(color: string, opacity: number): { r: number; g: number; b: number; a: number } {
  if (color.startsWith("#")) {
    let hex = color.slice(1);
    if (hex.length === 3) hex = hex[0]! + hex[0]! + hex[1]! + hex[1]! + hex[2]! + hex[2]!;
    return {
      r: parseInt(hex.slice(0, 2), 16),
      g: parseInt(hex.slice(2, 4), 16),
      b: parseInt(hex.slice(4, 6), 16),
      a: Math.round(opacity * 255),
    };
  }
  return { r: 0, g: 0, b: 0, a: Math.round(opacity * 255) };
}

/** Try to create a fill command for url(#...) references — patterns or gradients */
function tryCreateUrlFill(
  fillRef: string,
  pathId: number,
  fillRule: FillRule,
  transform: AffineTransform,
  patternLookup: Map<string, PatternLookup>,
  gradientLookup: Map<string, GradientLookup>,
): DrawCommand | null {
  const refId = fillRef.match(/url\(#([^)]+)\)/)?.[1] ?? "";
  const pat = patternLookup.get(refId);
  if (pat) {
    return {
      type: 2 as DrawCommandType.PatternFill,
      pathId,
      fillRule,
      imageId: pat.imageId,
      patternTransform: pat.patternTransform,
      transform,
    } satisfies PatternFillDrawCommand;
  }
  const grad = gradientLookup.get(refId);
  if (grad) {
    return {
      type: 3 as DrawCommandType.GradientFill,
      pathId,
      fillRule,
      gradientType: grad.gradientType,
      cx: grad.cx,
      cy: grad.cy,
      fx: grad.fx,
      fy: grad.fy,
      r: grad.r,
      gradientTransform: grad.gradientTransform,
      stops: grad.stops,
      transform,
    } satisfies GradientFillDrawCommand;
  }
  return null;
}

/**
 * Recursively resolve a definition node into a flat list of draw commands.
 * Follows <use> references and composes transforms along the way.
 */
function resolveToDrawCommands(
  nodeId: string,
  definitions: Map<string, ParsedNode>,
  parentTransform: AffineTransform,
  patternLookup: Map<string, PatternLookup>,
  gradientLookup: Map<string, GradientLookup>,
  pathDedup: Map<string, number>,
  allPaths: PathSegment[][],
  visited: Set<string>,
): DrawCommand[] {
  if (visited.has(nodeId)) return []; // prevent cycles
  visited.add(nodeId);

  const node = definitions.get(nodeId);
  if (!node) {
    visited.delete(nodeId);
    return [];
  }

  const commands: DrawCommand[] = [];

  if (node.type === "path") {
    const p = node.data;
    if (p.segments.length === 0) {
      visited.delete(nodeId);
      return [];
    }

    const pathKey = serializePath(p.segments);
    let pathId = pathDedup.get(pathKey);
    if (pathId === undefined) {
      pathId = allPaths.length;
      allPaths.push(p.segments);
      pathDedup.set(pathKey, pathId);
    }

    const transform = composeTransforms(parentTransform, p.transform);

    // Fill command
    if (p.fill && p.fill !== "none") {
      if (p.fill.startsWith("url(#")) {
        const cmd = tryCreateUrlFill(p.fill, pathId, p.fillRule, transform, patternLookup, gradientLookup);
        if (cmd) commands.push(cmd);
      } else {
        const color = parseColor(p.fill, p.fillOpacity);
        commands.push({
          type: 0 as DrawCommandType.Fill,
          pathId,
          fillRule: p.fillRule,
          color,
          colorZoneId: 0,
          transform,
        } satisfies FillDrawCommand);
      }
    }

    // Stroke command
    if (p.stroke && p.stroke !== "none" && p.strokeWidth) {
      const color = parseColor(p.stroke, p.strokeOpacity);
      const widthMode: StrokeWidthMode = p.strokeWidth === "__RESOLUTION__" ? 1 : 0;
      const width = widthMode === 1 ? 1.0 : parseFloat(p.strokeWidth);
      commands.push({
        type: 1 as DrawCommandType.Stroke,
        pathId,
        fillRule: p.fillRule,
        color,
        colorZoneId: 0,
        widthMode,
        width,
        opacity: p.strokeOpacity,
        lineCap: lineCapToNum(p.strokeLinecap),
        lineJoin: lineJoinToNum(p.strokeLinejoin),
        transform,
      } satisfies StrokeDrawCommand);
    }
  } else if (node.type === "group") {
    const g = node.data;
    const groupTransform = composeTransforms(parentTransform, g.transform);
    for (const child of g.children) {
      if (child.type === "path") {
        // Inline path in group
        const p = child.data;
        if (p.segments.length === 0) continue;

        const pathKey = serializePath(p.segments);
        let pathId = pathDedup.get(pathKey);
        if (pathId === undefined) {
          pathId = allPaths.length;
          allPaths.push(p.segments);
          pathDedup.set(pathKey, pathId);
        }

        const transform = composeTransforms(groupTransform, p.transform);

        if (p.fill && p.fill !== "none") {
          if (p.fill.startsWith("url(#")) {
            const cmd = tryCreateUrlFill(p.fill, pathId, p.fillRule, transform, patternLookup, gradientLookup);
            if (cmd) commands.push(cmd);
          } else {
            const color = parseColor(p.fill, p.fillOpacity);
            commands.push({
              type: 0 as DrawCommandType.Fill,
              pathId,
              fillRule: p.fillRule,
              color,
              colorZoneId: 0,
              transform,
            } satisfies FillDrawCommand);
          }
        }

        if (p.stroke && p.stroke !== "none" && p.strokeWidth) {
          const color = parseColor(p.stroke, p.strokeOpacity);
          const widthMode: StrokeWidthMode = p.strokeWidth === "__RESOLUTION__" ? 1 : 0;
          const width = widthMode === 1 ? 1.0 : parseFloat(p.strokeWidth);
          commands.push({
            type: 1 as DrawCommandType.Stroke,
            pathId,
            fillRule: p.fillRule,
            color,
            colorZoneId: 0,
            widthMode,
            width,
            opacity: p.strokeOpacity,
            lineCap: lineCapToNum(p.strokeLinecap),
            lineJoin: lineJoinToNum(p.strokeLinejoin),
            transform,
          } satisfies StrokeDrawCommand);
        }
      } else if (child.type === "use") {
        const useTransform = composeTransforms(groupTransform, child.data.transform);
        const resolved = resolveToDrawCommands(
          child.data.href, definitions, useTransform, patternLookup, gradientLookup, pathDedup, allPaths, visited,
        );
        commands.push(...resolved);
      } else if (child.type === "group") {
        const nestedTransform = composeTransforms(groupTransform, child.data.transform);
        // Process nested group children
        for (const nested of child.data.children) {
          if (nested.type === "path") {
            const p = nested.data;
            if (p.segments.length === 0) continue;
            const pathKey = serializePath(p.segments);
            let pathId = pathDedup.get(pathKey);
            if (pathId === undefined) {
              pathId = allPaths.length;
              allPaths.push(p.segments);
              pathDedup.set(pathKey, pathId);
            }
            const transform = composeTransforms(nestedTransform, p.transform);
            if (p.fill && p.fill !== "none") {
              if (p.fill.startsWith("url(#")) {
                const cmd = tryCreateUrlFill(p.fill, pathId, p.fillRule, transform, patternLookup, gradientLookup);
                if (cmd) commands.push(cmd);
              } else {
                commands.push({
                  type: 0 as DrawCommandType.Fill, pathId, fillRule: p.fillRule,
                  color: parseColor(p.fill, p.fillOpacity), colorZoneId: 0, transform,
                } satisfies FillDrawCommand);
              }
            }
            if (p.stroke && p.stroke !== "none" && p.strokeWidth) {
              commands.push({
                type: 1 as DrawCommandType.Stroke, pathId, fillRule: p.fillRule,
                color: parseColor(p.stroke, p.strokeOpacity), colorZoneId: 0,
                widthMode: p.strokeWidth === "__RESOLUTION__" ? 1 : 0,
                width: p.strokeWidth === "__RESOLUTION__" ? 1.0 : parseFloat(p.strokeWidth),
                opacity: p.strokeOpacity,
                lineCap: lineCapToNum(p.strokeLinecap), lineJoin: lineJoinToNum(p.strokeLinejoin),
                transform,
              } satisfies StrokeDrawCommand);
            }
          } else if (nested.type === "use") {
            const resolved = resolveToDrawCommands(
              nested.data.href, definitions,
              composeTransforms(nestedTransform, nested.data.transform),
              patternLookup, gradientLookup, pathDedup, allPaths, visited,
            );
            commands.push(...resolved);
          }
        }
      }
    }
  } else if (node.type === "use") {
    // Alias: resolve the target
    const useTransform = composeTransforms(parentTransform, node.data.transform);
    const resolved = resolveToDrawCommands(
      node.data.href, definitions, useTransform, patternLookup, gradientLookup, pathDedup, allPaths, visited,
    );
    commands.push(...resolved);
  }

  visited.delete(nodeId);
  return commands;
}

function serializeDrawCommand(cmd: DrawCommand): string {
  const base = [cmd.type, cmd.pathId, cmd.fillRule, serializeTransform(cmd.transform)];
  const extra = match(cmd)
    .with({ type: 0 }, (c) => [c.color.r, c.color.g, c.color.b, c.color.a, c.colorZoneId])
    .with({ type: 1 }, (c) => [c.color.r, c.color.g, c.color.b, c.color.a, c.colorZoneId,
      c.widthMode, c.width, c.opacity, c.lineCap, c.lineJoin])
    .with({ type: 2 }, (c) => [c.imageId, serializeTransform(c.patternTransform)])
    .with({ type: 3 }, (c) => [c.gradientType, c.cx, c.cy, c.fx, c.fy, c.r,
      serializeTransform(c.gradientTransform),
      c.stops.map((s) => `${s.offset}:${s.color.r},${s.color.g},${s.color.b},${s.color.a}`).join(";")])
    .exhaustive();
  return [...base, ...extra].join("|");
}

interface AnimationInput {
  name: string;
  svg: ParsedSvg;
  atlas: AtlasJson;
}

/**
 * Main deduplication pipeline.
 * Takes parsed data from all animations and produces a CompiledAsset.
 */
export function deduplicate(
  assetId: number,
  animations: AnimationInput[],
  images: ExtractedImage[],
): CompiledAsset {
  // Shared dedup tables
  const pathDedup = new Map<string, number>(); // serialized path → pathId
  const allPaths: PathSegment[][] = [];

  const drawCmdDedup = new Map<string, number>(); // serialized cmd → cmdId
  const allDrawCommands: DrawCommand[] = [];

  const bodyPartDedup = new Map<string, number>(); // serialized cmd list → bodyPartId
  const allBodyParts: BodyPart[] = [];

  const transformDedup = new Map<string, number>(); // serialized transform → transformId
  const allTransforms: AffineTransform[] = [];

  const frameDedup = new Map<string, number>(); // serialized frame → frameId
  const allFrames: Frame[] = [];

  const compiledAnimations: Animation[] = [];

  function getOrAddTransform(t: AffineTransform): number {
    const key = serializeTransform(t);
    let id = transformDedup.get(key);
    if (id === undefined) {
      id = allTransforms.length;
      allTransforms.push(t);
      transformDedup.set(key, id);
    }
    return id;
  }

  function getOrAddDrawCommand(cmd: DrawCommand): number {
    const key = serializeDrawCommand(cmd);
    let id = drawCmdDedup.get(key);
    if (id === undefined) {
      id = allDrawCommands.length;
      allDrawCommands.push(cmd);
      drawCmdDedup.set(key, id);
    }
    return id;
  }

  function getOrAddBodyPart(cmdIds: number[]): number {
    const key = cmdIds.join(",");
    let id = bodyPartDedup.get(key);
    if (id === undefined) {
      id = allBodyParts.length;
      allBodyParts.push({ id, drawCommandIds: cmdIds });
      bodyPartDedup.set(key, id);
    }
    return id;
  }

  for (const anim of animations) {
    const { svg, atlas } = anim;

    // Build per-animation pattern and gradient lookups to avoid ID collisions
    // (different animations reuse the same element IDs like d0, d1, d2...)
    const patternLookup = new Map<string, PatternLookup>();
    for (const pattern of svg.patterns) {
      const imageIdx = images.findIndex((img) => {
        const b64 = pattern.imageDataUri.replace(/^data:image\/\w+;base64,/, "");
        const buf = Buffer.from(b64, "base64");
        return buf.equals(Buffer.from(img.pngBytes));
      });
      if (imageIdx >= 0) {
        patternLookup.set(pattern.id, {
          patternId: pattern.id,
          imageId: imageIdx,
          patternTransform: pattern.patternTransform,
        });
      }
    }

    const gradientLookup = new Map<string, GradientLookup>();
    for (const grad of svg.gradients) {
      if (!gradientLookup.has(grad.id)) {
        gradientLookup.set(grad.id, {
          gradientType: grad.type === "radial" ? 0 : 1,
          cx: grad.cx,
          cy: grad.cy,
          fx: grad.fx,
          fy: grad.fy,
          r: grad.r,
          gradientTransform: grad.gradientTransform,
          stops: grad.stops.map((s) => ({
            offset: s.offset,
            color: parseGradientStopColor(s.color, s.opacity),
          })),
        });
      }
    }

    // For each frame in the SVG, resolve body parts
    const svgFrameData: { bodyPartIds: PartInstance[]; accSlots: AccessorySlot[]; frameTransformId: number }[] = [];

    for (const frame of svg.frames) {
      const parts: PartInstance[] = [];
      const accSlots: AccessorySlot[] = [];

      for (const use of frame.uses) {
        // Resolve this body part reference to draw commands
        const drawCmds = resolveToDrawCommands(
          use.href, svg.definitions, [...IDENTITY_TRANSFORM] as AffineTransform,
          patternLookup, gradientLookup, pathDedup, allPaths, new Set(),
        );

        // Dedup draw commands
        const cmdIds = drawCmds.map((cmd) => getOrAddDrawCommand(cmd));

        // Dedup body part
        const bodyPartId = getOrAddBodyPart(cmdIds);

        // Compose frame offset transform with the use element's transform
        // The SVG frame has: <g transform="translate(x,y)"> → <use transform="matrix(...)">
        // We need: frameOffset * useTransform
        const composedTransform = composeTransforms(frame.offsetTransform, use.transform);
        const transformId = getOrAddTransform(composedTransform);
        parts.push({ bodyPartId, transformId });
      }

      for (const slot of frame.accessorySlots) {
        // Store the raw acc-slot matrix WITHOUT frame offset.
        // The renderer will compose the full transform at render time
        // using the accessory's offsetX/offsetY for pivot-point rotation.
        const rawMatrix: AffineTransform = slot.matrix
          ? [...slot.matrix] as AffineTransform
          : [1, 0, 0, 1, slot.tx, slot.ty];
        const transformId = getOrAddTransform(rawMatrix);
        // depthIndex = insertion position: render this slot after parts[0..depthIndex-1]
        accSlots.push({ slotId: slot.slotId, depthIndex: slot.insertAfterPart, transformId });
      }

      const frameTransformId = getOrAddTransform(frame.offsetTransform);
      svgFrameData.push({ bodyPartIds: parts, accSlots, frameTransformId });
    }

    // Map atlas frames to SVG frames, handling duplicates
    const uniqueFrameMap = new Map<string, number>(); // atlas frame id → svgFrameIndex
    for (let i = 0; i < atlas.frames.length; i++) {
      const frame = atlas.frames[i]!;
      uniqueFrameMap.set(frame.id, i);
    }

    // Build compiled frames for this animation's unique frames
    const animFrameIds: number[] = [];
    const localFrameCache = new Map<number, number>(); // svgFrameIdx → global frame id

    for (const frameId of atlas.frameOrder) {
      // Resolve duplicates
      const resolvedId = atlas.duplicates[frameId] ?? frameId;
      const svgFrameIdx = uniqueFrameMap.get(resolvedId);
      if (svgFrameIdx === undefined || svgFrameIdx >= svgFrameData.length) {
        // Fallback: use first frame
        animFrameIds.push(0);
        continue;
      }

      // Check if we already compiled this SVG frame
      let globalFrameId = localFrameCache.get(svgFrameIdx);
      if (globalFrameId === undefined) {
        const frameData = svgFrameData[svgFrameIdx]!;
        const atlasFrame = atlas.frames[svgFrameIdx]!;

        const frame: Frame = {
          clipRect: [atlasFrame.x, atlasFrame.y, atlasFrame.width, atlasFrame.height],
          offsetX: atlasFrame.offsetX,
          offsetY: atlasFrame.offsetY,
          parts: frameData.bodyPartIds,
          accessorySlots: frameData.accSlots,
          frameTransformId: frameData.frameTransformId,
        };

        // Dedup frame globally
        const frameKey = JSON.stringify(frame);
        globalFrameId = frameDedup.get(frameKey);
        if (globalFrameId === undefined) {
          globalFrameId = allFrames.length;
          allFrames.push(frame);
          frameDedup.set(frameKey, globalFrameId);
        }
        localFrameCache.set(svgFrameIdx, globalFrameId);
      }

      animFrameIds.push(globalFrameId);
    }

    // Handle baseFrame if present
    let baseFrameId = -1;
    let baseZOrder = 0;
    const baseFrameMeta = (atlas as Record<string, unknown>).baseFrame as { x: number; y: number; width: number; height: number; offsetX: number; offsetY: number } | undefined;
    const baseZOrderStr = (atlas as Record<string, unknown>).baseZOrder as string | undefined;
    if (baseFrameMeta && svg.frames.length > 0) {
      // The base frame is the first <g clip-path> in the SVG that matches baseFrame coordinates
      // It's at index 0 before the animation-specific delta frames
      const baseIdx = svg.frames.findIndex((f) =>
        Math.abs(f.clipRect.x - baseFrameMeta.x) < 1 &&
        Math.abs(f.clipRect.y - baseFrameMeta.y) < 1 &&
        Math.abs(f.clipRect.width - baseFrameMeta.width) < 1 &&
        Math.abs(f.clipRect.height - baseFrameMeta.height) < 1
      );
      if (baseIdx >= 0 && baseIdx < svgFrameData.length) {
        const baseData = svgFrameData[baseIdx]!;
        const baseFrame: Frame = {
          clipRect: [baseFrameMeta.x, baseFrameMeta.y, baseFrameMeta.width, baseFrameMeta.height],
          offsetX: baseFrameMeta.offsetX,
          offsetY: baseFrameMeta.offsetY,
          parts: baseData.bodyPartIds,
          accessorySlots: baseData.accSlots,
          frameTransformId: baseData.frameTransformId,
        };
        const baseKey = JSON.stringify(baseFrame);
        let gid = frameDedup.get(baseKey);
        if (gid === undefined) {
          gid = allFrames.length;
          allFrames.push(baseFrame);
          frameDedup.set(baseKey, gid);
        }
        baseFrameId = gid;
      }
      baseZOrder = baseZOrderStr === "above" ? 1 : 0;
    }

    compiledAnimations.push({
      name: anim.name,
      fps: atlas.fps,
      offsetX: atlas.offsetX,
      offsetY: atlas.offsetY,
      frameIds: animFrameIds,
      baseFrameId,
      baseZOrder,
    });
  }

  return {
    assetId,
    paths: allPaths,
    drawCommands: allDrawCommands,
    bodyParts: allBodyParts,
    transforms: allTransforms,
    images,
    colorZones: [],
    animations: compiledAnimations,
    frames: allFrames,
  };
}

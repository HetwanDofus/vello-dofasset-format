import { createHash } from "node:crypto";
import type { ParsedPattern, ExtractedImage } from "./types.js";

/**
 * Extract and deduplicate images from pattern definitions across all animations.
 */
export function extractImages(allPatterns: ParsedPattern[]): ExtractedImage[] {
  const seen = new Map<string, number>(); // content hash → image id
  const images: ExtractedImage[] = [];

  for (const pattern of allPatterns) {
    const b64 = pattern.imageDataUri.replace(/^data:image\/\w+;base64,/, "");
    const pngBytes = new Uint8Array(Buffer.from(b64, "base64"));
    const contentHash = createHash("sha256").update(pngBytes).digest("hex").slice(0, 16);

    if (seen.has(contentHash)) continue;

    // Decode PNG header to get dimensions
    const { width, height } = parsePngDimensions(pngBytes);

    const id = images.length;
    images.push({ id, width, height, pngBytes, contentHash });
    seen.set(contentHash, id);
  }

  return images;
}

/**
 * Read width/height from PNG IHDR chunk.
 * PNG format: 8-byte signature, then IHDR chunk with width (4 bytes BE) and height (4 bytes BE).
 */
function parsePngDimensions(data: Uint8Array): { width: number; height: number } {
  // PNG signature is 8 bytes, then IHDR chunk: 4 bytes length, 4 bytes "IHDR", then 4 bytes width, 4 bytes height
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  const width = view.getUint32(16, false); // big-endian at offset 16
  const height = view.getUint32(20, false); // big-endian at offset 20
  return { width, height };
}

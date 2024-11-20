export function createChunk(size: number): Uint8Array {
  const chunk = new Uint8Array(size);
  chunk.fill(0);
  return chunk;
}

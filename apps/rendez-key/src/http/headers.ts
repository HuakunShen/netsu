export function applyNoStore(headers: Headers): void {
  headers.set("Cache-Control", "no-store");
}

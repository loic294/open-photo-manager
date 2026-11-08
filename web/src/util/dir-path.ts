/** Exact on-disk path comparison, including the volume root and segment boundaries. */
export function isSameOrBelow(dir: string, base: string): boolean {
  return base === '' || dir === base || dir.startsWith(`${base}/`);
}

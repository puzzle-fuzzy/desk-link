export function retainSessionSurface(
  previous: string | null,
  next: string | null,
  sameView: boolean,
  hasViewport: boolean,
): boolean {
  return previous !== null && previous === next && sameView && hasViewport;
}

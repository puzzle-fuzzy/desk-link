export type RemotePanOrigin = {
  clientX: number;
  clientY: number;
  scrollLeft: number;
  scrollTop: number;
};

export type RemotePanExtent = {
  clientWidth: number;
  clientHeight: number;
  scrollWidth: number;
  scrollHeight: number;
};

export function remotePanPosition(
  origin: RemotePanOrigin,
  clientX: number,
  clientY: number,
  extent: RemotePanExtent,
): { left: number; top: number } {
  const maxLeft = Math.max(0, extent.scrollWidth - extent.clientWidth);
  const maxTop = Math.max(0, extent.scrollHeight - extent.clientHeight);
  return {
    left: clamp(origin.scrollLeft + origin.clientX - clientX, 0, maxLeft),
    top: clamp(origin.scrollTop + origin.clientY - clientY, 0, maxTop),
  };
}

function clamp(value: number, minimum: number, maximum: number): number {
  if (!Number.isFinite(value)) {
    return minimum;
  }
  return Math.max(minimum, Math.min(maximum, value));
}

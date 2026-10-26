export interface NativeDropPosition {
  x: number;
  y: number;
}

export interface FileDropTarget {
  streamId: number;
  deviceId: string;
  generation: number;
}

const DROP_AREAS = '.remote-session[data-runtime-state="connected"] [data-remote-viewport], .remote-session[data-runtime-state="connected"] [data-controller-transfer-panel]';
const DROP_BLOCKERS = 'button, a, input, textarea, select, summary, form, dialog, [role="dialog"], [role="menu"], [role="menuitem"]';

/** Native drag coordinates are physical client pixels; DOM hit testing uses CSS pixels. */
export function nativeFileDropHitsSurface(
  position: NativeDropPosition,
  pixelRatio: number,
  document: Pick<Document, "hidden" | "elementFromPoint" | "querySelectorAll">,
): boolean {
  if (document.hidden || !Number.isFinite(pixelRatio) || pixelRatio <= 0) return false;
  const x = position.x / pixelRatio;
  const y = position.y / pixelRatio;
  if (!Number.isFinite(x) || !Number.isFinite(y) || x < 0 || y < 0) return false;
  const top = document.elementFromPoint(x, y);
  if (!top || top.closest(DROP_BLOCKERS)) return false;
  return [...document.querySelectorAll<HTMLElement>(DROP_AREAS)].some((area) => {
    if (!area.isConnected || area.getClientRects().length === 0 || !area.contains(top)) return false;
    const bounds = area.getBoundingClientRect();
    return x >= bounds.left && y >= bounds.top && x < bounds.right && y < bounds.bottom;
  });
}

export class FileDropTargetGate {
  #target: FileDropTarget | null = null;

  enter(target: FileDropTarget | null): void {
    this.#target = target ? { ...target } : null;
  }

  accept(target: FileDropTarget | null, hit: boolean): FileDropTarget | null {
    const original = this.#target;
    if (!original || !target || original.streamId !== target.streamId
      || original.deviceId !== target.deviceId || original.generation !== target.generation) {
      this.clear();
      return null;
    }
    return hit ? original : null;
  }

  clear(): void {
    this.#target = null;
  }
}

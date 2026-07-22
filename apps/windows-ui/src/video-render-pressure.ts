export const PRESENTATION_DROP_PRESSURE_THRESHOLD = 8;
export const PRESENTATION_DROP_SEVERE_THRESHOLD = 24;
export const PRESENTATION_DROP_DECODE_QUEUE_HINT = 5;
export const PRESENTATION_DROP_SEVERE_DECODE_QUEUE_HINT = 8;

/**
 * Converts a one-second display-drop sample into the existing playback
 * pressure scale used by automatic remote quality. Small samples are ignored
 * so an occasional compositor hiccup cannot change quality.
 */
export function presentationDropDecodeQueueHint(coalescedFrameDrops: number): number {
  const drops = Number.isFinite(coalescedFrameDrops)
    ? Math.max(0, Math.trunc(coalescedFrameDrops))
    : 0;
  if (drops >= PRESENTATION_DROP_SEVERE_THRESHOLD) {
    return PRESENTATION_DROP_SEVERE_DECODE_QUEUE_HINT;
  }
  if (drops >= PRESENTATION_DROP_PRESSURE_THRESHOLD) {
    return PRESENTATION_DROP_DECODE_QUEUE_HINT;
  }
  return 0;
}

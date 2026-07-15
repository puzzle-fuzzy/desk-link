import type { ControllerVideoConfigSignal } from "./types";

export function videoConfigKey(
  config: Pick<ControllerVideoConfigSignal, "streamId" | "configVersion">,
): string {
  return `${config.streamId}:${config.configVersion}`;
}

export function h264CodecFromSequenceHeader(header: Uint8Array): string {
  for (let index = 0; index + 3 < header.length; index += 1) {
    const fourByteStart = header[index] === 0
      && header[index + 1] === 0
      && header[index + 2] === 0
      && header[index + 3] === 1;
    const threeByteStart = header[index] === 0
      && header[index + 1] === 0
      && header[index + 2] === 1;
    const nalIndex = index + (fourByteStart ? 4 : threeByteStart ? 3 : 0);
    if (
      nalIndex !== index
      && nalIndex + 3 < header.length
      && (header[nalIndex]! & 0x1f) === 7
    ) {
      return `avc1.${hexByte(header[nalIndex + 1]!)}${hexByte(header[nalIndex + 2]!)}${hexByte(header[nalIndex + 3]!)}`;
    }
  }
  return "avc1.42E01E";
}

function hexByte(value: number): string {
  return value.toString(16).padStart(2, "0").toUpperCase();
}

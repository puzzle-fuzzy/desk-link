const NAL_TYPE_IDR = 5;
const NAL_TYPE_SPS = 7;
const NAL_TYPE_PPS = 8;

export function annexBNalTypes(bytes: Uint8Array): number[] {
  const types: number[] = [];
  let searchFrom = 0;
  while (searchFrom < bytes.byteLength) {
    const start = findStartCode(bytes, searchFrom);
    if (!start) {
      break;
    }
    const nalIndex = start.index + start.length;
    if (nalIndex < bytes.byteLength) {
      types.push(bytes[nalIndex]! & 0x1f);
    }
    searchFrom = nalIndex + 1;
  }
  return types;
}

export function isH264Keyframe(accessUnit: Uint8Array, signalledKeyframe: boolean): boolean {
  return signalledKeyframe || annexBNalTypes(accessUnit).includes(NAL_TYPE_IDR);
}

export function prepareH264AccessUnit(
  sequenceHeader: Uint8Array,
  accessUnit: Uint8Array,
  keyframe: boolean,
): Uint8Array {
  if (!keyframe || sequenceHeader.byteLength === 0) {
    return accessUnit;
  }
  const types = annexBNalTypes(accessUnit);
  if (types.includes(NAL_TYPE_SPS) && types.includes(NAL_TYPE_PPS)) {
    return accessUnit;
  }
  const output = new Uint8Array(sequenceHeader.byteLength + accessUnit.byteLength);
  output.set(sequenceHeader, 0);
  output.set(accessUnit, sequenceHeader.byteLength);
  return output;
}

function findStartCode(
  bytes: Uint8Array,
  from: number,
): { index: number; length: 3 | 4 } | null {
  for (let index = Math.max(0, from); index + 2 < bytes.byteLength; index += 1) {
    if (bytes[index] !== 0 || bytes[index + 1] !== 0) {
      continue;
    }
    if (bytes[index + 2] === 1) {
      return { index, length: 3 };
    }
    if (index + 3 < bytes.byteLength && bytes[index + 2] === 0 && bytes[index + 3] === 1) {
      return { index, length: 4 };
    }
  }
  return null;
}

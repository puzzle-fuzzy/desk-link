const MAX_U64_DECIMAL = "18446744073709551615";
const CANONICAL_DECIMAL = /^(?:0|[1-9][0-9]{0,19})$/;

// Use only for reviewed u64 fields. Preserve their original wire values in JSON;
// the canonical form is for identity keys and exact local comparisons.
export function canonicalU64(value: unknown): string | null {
  if (typeof value === "number") {
    return Number.isSafeInteger(value) && value >= 0 && !Object.is(value, -0) ? String(value) : null;
  }
  if (typeof value !== "string" || value.length > 20 || !CANONICAL_DECIMAL.test(value)) return null;
  if (value.length === 20 && value > MAX_U64_DECIMAL) return null;
  return value;
}

// BigInt stays local: neither payloads nor serialized reports may contain it.
export function parseU64(value: unknown): bigint | null {
  const decimal = canonicalU64(value);
  return decimal === null ? null : BigInt(decimal);
}

export type JsonExactUnsigned = number | string;

// Aggregates can exceed one u64. Serialize their exact decimal value while
// retaining the historical number shape when it is lossless.
export function jsonExactUnsigned(value: bigint): JsonExactUnsigned {
  if (value < 0n) throw new RangeError("diagnostic aggregate must be nonnegative");
  return value <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(value) : value.toString();
}

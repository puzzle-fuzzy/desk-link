const U64_MAX = 18_446_744_073_709_551_615n;

/** Lossless identity accepted at the Windows JSON boundary. */
export function isU64Identity(value: unknown): value is string {
  return typeof value === "string"
    && value.length > 0 && value.length <= 20
    && value[0] !== "0" && !/[^0-9]/.test(value)
    && BigInt(value) <= U64_MAX;
}

const DEVICE_ID_DIGITS = 12;
const TEMPORARY_PASSWORD_LENGTH = 8;
const TEMPORARY_PASSWORD_PATTERN = /^[23456789ABCDEFGHJKLMNPQRSTUVWXYZ]{8}$/;
/** Fixed passwords are 8–64 UTF-8 bytes of any characters; the bound is in
 * bytes so multi-byte characters cannot exceed the shared crypto rule. */
export const ACCESS_PASSWORD_MAX_BYTES = 64;
export const ACCESS_PASSWORD_MIN_BYTES = 8;
const passwordEncoder = new TextEncoder();

export function formatDeviceId(value: string): string {
  return value
    .replace(/\D/g, "")
    .slice(0, DEVICE_ID_DIGITS)
    .replace(/(\d{3})(?=\d)/g, "$1 ");
}

export function normalizeTemporaryPassword(value: string): string {
  return value
    .toUpperCase()
    .replace(/[^A-Z0-9]/g, "")
    .slice(0, TEMPORARY_PASSWORD_LENGTH);
}

export function accessPasswordIsValid(password: string): boolean {
  // The temporary interpretation must be exact: normalizeTemporaryPassword
  // truncates to eight characters, so a longer alphabet-only string would
  // otherwise masquerade as a temporary code.
  const temporary = password.toUpperCase().replace(/[^A-Z0-9]/g, "");
  if (temporary.length === TEMPORARY_PASSWORD_LENGTH
    && TEMPORARY_PASSWORD_PATTERN.test(temporary)) {
    return true;
  }
  const bytes = passwordEncoder.encode(password.trim());
  return bytes.length >= ACCESS_PASSWORD_MIN_BYTES && bytes.length <= ACCESS_PASSWORD_MAX_BYTES;
}

export function deviceCredentialsAreValid(deviceId: string, temporaryPassword: string): boolean {
  return deviceId.replace(/\D/g, "").length === DEVICE_ID_DIGITS
    && accessPasswordIsValid(temporaryPassword);
}

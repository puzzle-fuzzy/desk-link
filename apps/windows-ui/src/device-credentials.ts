const DEVICE_ID_DIGITS = 12;
const TEMPORARY_PASSWORD_LENGTH = 8;
const TEMPORARY_PASSWORD_PATTERN = /^[23456789ABCDEFGHJKLMNPQRSTUVWXYZ]{8}$/;

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

export function deviceCredentialsAreValid(deviceId: string, temporaryPassword: string): boolean {
  return deviceId.replace(/\D/g, "").length === DEVICE_ID_DIGITS
    && TEMPORARY_PASSWORD_PATTERN.test(normalizeTemporaryPassword(temporaryPassword));
}

export interface ParsedPairingCode {
  relayAddress: string;
  serverName: string;
  invitation: string;
}

export function parsePairingCode(
  value: string,
  fallbackRelayAddress: string,
  fallbackServerName: string,
): ParsedPairingCode | null {
  const normalized = value.trim();
  const lines = normalized.split(/\r?\n/).map((line) => line.trim());
  if (lines[0] === "DESKLINK-PAIR-1") {
    const relayAddress = lines[1];
    const serverName = lines[2];
    const invitation = lines[3];
    if (lines.length !== 4 || !relayAddress || !serverName || !invitation) {
      return null;
    }
    return isHexInvitation(invitation)
      ? { relayAddress, serverName, invitation }
      : null;
  }
  return isHexInvitation(normalized)
    ? {
        relayAddress: fallbackRelayAddress,
        serverName: fallbackServerName,
        invitation: normalized,
      }
    : null;
}

export function pairingCodeWithRelayAddress(
  value: string,
  relayAddress: string,
): string | null {
  const normalizedAddress = relayAddress.trim();
  if (
    !normalizedAddress ||
    normalizedAddress.length > 255 ||
    normalizedAddress.includes("\n") ||
    normalizedAddress.includes("\r")
  ) {
    return null;
  }
  const parsed = parsePairingCode(value, "", "");
  if (!parsed || value.trim().split(/\r?\n/)[0]?.trim() !== "DESKLINK-PAIR-1") {
    return null;
  }
  return `DESKLINK-PAIR-1\n${normalizedAddress}\n${parsed.serverName}\n${parsed.invitation}`;
}

function isHexInvitation(value: string): boolean {
  return value.length === 362 && /^[0-9a-fA-F]+$/.test(value);
}

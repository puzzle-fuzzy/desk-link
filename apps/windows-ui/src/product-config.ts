export const MANAGED_RELAY_ADDRESS = "101.35.246.159:4433";
export const MANAGED_RELAY_SERVER_NAME = "turn.p2p.yxswy.com";

export function isManagedRelay(relayAddress: string, serverName: string): boolean {
  return relayAddress.trim() === MANAGED_RELAY_ADDRESS
    && serverName.trim().toLowerCase() === MANAGED_RELAY_SERVER_NAME;
}

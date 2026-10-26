import { isIP } from "node:net";

export function normalizeAddress(value: string | undefined): string | undefined {
  if (!value || !isIP(value)) return undefined;
  return value.startsWith("::ffff:") && isIP(value.slice(7)) === 4 ? value.slice(7) : value.toLowerCase();
}

export function clientAddress(request: Request, peer: string | undefined, trustedProxies: readonly string[]): string {
  const address = normalizeAddress(peer);
  if (!address) return "unknown";
  // X-Real-IP must be overwritten by the explicitly trusted immediate proxy.
  // XFF and CF headers are never an identity source in this deployment contract.
  if (trustedProxies.includes(address)) {
    return normalizeAddress(request.headers.get("x-real-ip") ?? undefined) ?? address;
  }
  return address;
}

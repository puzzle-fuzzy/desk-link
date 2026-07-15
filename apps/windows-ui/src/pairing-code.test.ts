import { describe, expect, test } from "bun:test";

import { pairingCodeWithRelayAddress, parsePairingCode } from "./pairing-code";

const invitation = "ab".repeat(181);

describe("parsePairingCode", () => {
  test("uses the relay details carried by a LAN pairing package", () => {
    const code = `DESKLINK-PAIR-1\n192.168.1.20:4433\ndesklink-lan\n${invitation}`;

    expect(parsePairingCode(code, "127.0.0.1:4433", "localhost")).toEqual({
      relayAddress: "192.168.1.20:4433",
      serverName: "desklink-lan",
      invitation,
    });
  });

  test("keeps legacy 362-character invitations compatible", () => {
    expect(parsePairingCode(invitation, "203.0.113.10:4433", "relay.example.com")).toEqual({
      relayAddress: "203.0.113.10:4433",
      serverName: "relay.example.com",
      invitation,
    });
  });

  test("rejects incomplete or malformed packages", () => {
    expect(parsePairingCode("DESKLINK-PAIR-1\n192.168.1.20:4433", "", "")).toBeNull();
    expect(parsePairingCode(`${invitation.slice(2)}zz`, "", "")).toBeNull();
  });
});

describe("pairingCodeWithRelayAddress", () => {
  test("switches a LAN package to another detected adapter address", () => {
    const code = `DESKLINK-PAIR-1\n10.0.0.8:4433\ndesklink-lan\n${invitation}`;

    expect(pairingCodeWithRelayAddress(code, "192.168.50.12:4433")).toBe(
      `DESKLINK-PAIR-1\n192.168.50.12:4433\ndesklink-lan\n${invitation}`,
    );
  });

  test("rejects legacy codes and address line injection", () => {
    expect(pairingCodeWithRelayAddress(invitation, "192.168.1.20:4433")).toBeNull();
    expect(
      pairingCodeWithRelayAddress(
        `DESKLINK-PAIR-1\n10.0.0.8:4433\ndesklink-lan\n${invitation}`,
        "192.168.1.20:4433\nrelay.example.com",
      ),
    ).toBeNull();
  });
});

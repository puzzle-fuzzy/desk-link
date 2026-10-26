import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./main.ts", import.meta.url), "utf8");
const action = source.slice(source.indexOf("async function changeHostSharing("), source.indexOf("\nasync function enableManagedConnection("));

test("actual sharing pause is single-flight, refreshes authoritative Host state and preserves Controller ownership", async () => {
  const calls: string[] = [];
  let resolve!: () => void;
  const pending = new Promise<void>((next) => {resolve = next;});
  const create = new Function("pauseHostSharing", "resumeHostSharing", "refreshSnapshot", "clearPairingSecrets", `
    let hostSharingBusy = false, feedback = null, pairingSession = {};
    const render = () => {}, normalizeError = error => error.message;
    ${new Bun.Transpiler({loader: "ts"}).transformSync(action)}
    return {run:changeHostSharing, state:()=>({hostSharingBusy,feedback,pairingSession})};
  `);
  const flow = create(async () => {calls.push("pause-host"); await pending;}, async () => {calls.push("resume-host");}, async () => {calls.push("refresh-host");}, () => {calls.push("clear-one-time-pairing");});
  const first = flow.run(false);
  await flow.run(true);
  expect(calls).toEqual(["pause-host"]);
  expect(flow.state().hostSharingBusy).toBe(true);
  resolve();
  await first;
  expect(calls).toEqual(["pause-host", "clear-one-time-pairing", "refresh-host"]);
  expect(flow.state().pairingSession).toBeNull();
  await flow.run(true);
  expect(calls.at(-3)).toBe("resume-host");
});

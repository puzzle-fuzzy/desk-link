import { readFileSync, writeFileSync } from "node:fs";

const protocol = readFileSync(new URL("../../../crates/desklink-protocol/src/lib.rs", import.meta.url), "utf8");
const constants = ["MAX_POINTER_COORDINATE", "MAX_WHEEL_DELTA"];
const output = "// Generated from desklink-protocol. Run bun scripts/generate-input-contract.ts.\n"
  + constants.map((name) => {
    const value = protocol.match(new RegExp(`pub const ${name}: \\w+ = ([\\d_]+);`))?.[1];
    if (!value) throw new Error(`Missing protocol constant: ${name}`);
    return `export const ${name} = ${Number(value.replaceAll("_", ""))};\n`;
  }).join("");
const target = new URL("../src/input-contract.generated.ts", import.meta.url);
if (process.argv.includes("--check")) {
  if (readFileSync(target, "utf8") !== output) throw new Error("Input contract is stale; regenerate it");
} else {
  writeFileSync(target, output);
}

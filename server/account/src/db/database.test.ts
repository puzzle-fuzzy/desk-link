import { expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { AccountDatabase } from "./database";

test("fixed SQL working set stays reused and shutdown releases evicted-cache-sized file workloads without GC", () => {
  const directory = mkdtempSync(join(tmpdir(), "desklink-database-"));
  const database = new AccountDatabase(join(directory, "local.sqlite"), { create: true });
  try {
    const first = database.query("SELECT 1 AS n");
    for (let pass = 0; pass < 3; pass++) {
      for (let index = 0; index < 200; index++) database.query(`SELECT ${index} AS n`).get();
    }
    expect(database.query("SELECT 1 AS n")).toBe(first);
    database.close(true);
    // No GC or deletion retries: close must release every native statement now.
    rmSync(directory, { recursive: true });
  } finally { database.close(); }
});

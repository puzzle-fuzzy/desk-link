import { Database, type SQLQueryBindings, type Statement } from "bun:sqlite";

// Retain the service's fixed statements until deterministic shutdown rather than
// leaving cache-evicted native handles to GC after close_v2.
export class DiagnosticDatabase extends Database {
  private readonly statements = new Map<string, Statement>();
  override query<Result, Params extends SQLQueryBindings | SQLQueryBindings[]>(sql: string): Statement<Result, Params extends any[] ? Params : [Params]> {
    let statement = this.statements.get(sql);
    if (!statement) { statement = super.prepare(sql); this.statements.set(sql, statement); }
    return statement as Statement<Result, Params extends any[] ? Params : [Params]>;
  }
  override close(throwOnError = true): void {
    for (const statement of this.statements.values()) statement.finalize();
    this.statements.clear();
    super.close(throwOnError);
  }
}

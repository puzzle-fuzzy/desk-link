import { Database, type SQLQueryBindings, type Statement } from "bun:sqlite";

// Bun's query cache can evict statements before their JS wrappers are collected.
// Cache the service's fixed SQL ourselves and finalize it at shutdown, avoiding
// both GC-dependent file release and accumulation of evicted statement wrappers.
export class AccountDatabase extends Database {
  private readonly statements = new Map<string, Statement>();

  override query<Result, Params extends SQLQueryBindings | SQLQueryBindings[]>(sql: string): Statement<Result, Params extends any[] ? Params : [Params]> {
    let statement = this.statements.get(sql);
    if (!statement) {
      statement = super.prepare(sql);
      this.statements.set(sql, statement);
    }
    return statement as Statement<Result, Params extends any[] ? Params : [Params]>;
  }

  override close(throwOnError = true): void {
    for (const statement of this.statements.values()) statement.finalize();
    this.statements.clear();
    super.close(throwOnError);
  }
}

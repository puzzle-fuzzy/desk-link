export interface AccountOperation { generation: number; action: string }

/** A generation survives WebView reload ordering by starting from wall-clock microseconds. */
export class AccountOperations {
  private generation: number;
  constructor(now = Date.now()) { this.generation = now * 1000; }
  begin(action: string): AccountOperation { return { generation: ++this.generation, action }; }
  invalidate(): number { return ++this.generation; }
  current(operation: AccountOperation): boolean { return operation.generation === this.generation; }
}

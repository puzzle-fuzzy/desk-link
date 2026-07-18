export type RemoteDisplaySwitchOutcome = "idle" | "applied" | "rejected";

export class RemoteDisplaySwitchState {
  #pendingId: number | null = null;

  get pendingId(): number | null {
    return this.#pendingId;
  }

  begin(targetId: number, activeId: number | null, availableIds: readonly number[]): boolean {
    if (
      this.#pendingId !== null
      || targetId === activeId
      || !availableIds.includes(targetId)
    ) {
      return false;
    }
    this.#pendingId = targetId;
    return true;
  }

  acknowledge(activeId: number): RemoteDisplaySwitchOutcome {
    const requested = this.#pendingId;
    this.#pendingId = null;
    if (requested === null) {
      return "idle";
    }
    return requested === activeId ? "applied" : "rejected";
  }

  fail(targetId: number): boolean {
    if (this.#pendingId !== targetId) {
      return false;
    }
    this.#pendingId = null;
    return true;
  }

  reset(): void {
    this.#pendingId = null;
  }
}

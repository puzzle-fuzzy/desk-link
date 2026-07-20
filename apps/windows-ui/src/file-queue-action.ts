export type FileQueueActionKind = "remove" | "clear" | "resume" | "protect";

export type FileQueueActionToken = Readonly<{
  revision: number;
  kind: FileQueueActionKind;
  transferId: string | null;
}>;

export class FileQueueActionGate {
  #revision = 0;
  #active: FileQueueActionToken | null = null;

  get active(): FileQueueActionToken | null {
    return this.#active;
  }

  get busy(): boolean {
    return this.#active !== null;
  }

  begin(kind: FileQueueActionKind, transferId: string | null = null): FileQueueActionToken | null {
    if (this.#active) return null;
    this.#revision += 1;
    this.#active = { revision: this.#revision, kind, transferId };
    return this.#active;
  }

  finish(token: FileQueueActionToken): boolean {
    if (this.#active?.revision !== token.revision) return false;
    this.#active = null;
    return true;
  }

  matches(kind: FileQueueActionKind, transferId: string | null = null): boolean {
    return this.#active?.kind === kind && this.#active.transferId === transferId;
  }
}

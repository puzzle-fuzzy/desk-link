type TextKeyboardEvent = Pick<KeyboardEvent,
  "key" | "code" | "keyCode" | "isComposing" | "ctrlKey" | "altKey" | "metaKey"
> & { getModifierState?: (key: string) => boolean };

export function isCompositionKey(event: Pick<TextKeyboardEvent, "key" | "keyCode" | "isComposing">): boolean {
  // WebView can dispatch the opening key before compositionstart and the
  // confirming key after compositionend. 229 also covers those boundaries.
  return event.isComposing || event.keyCode === 229 || event.key === "Process" || event.key === "Dead";
}

/** Local editable text has one commit path; remote shortcuts stay physical. */
export class RemoteTextInput {
  private enabled = true;
  private acceptsInput = true;
  private composing = false;
  private compositionEcho: string | null = null;
  private readonly localKeys = new Set<string>();

  constructor(
    private readonly commit: (text: string) => void,
    private readonly releaseKeys: () => void,
  ) {}

  get isComposing(): boolean { return this.composing; }

  focus(): void { this.enabled = true; }

  reset(): void {
    this.enabled = false;
    this.acceptsInput = false;
    this.composing = false;
    this.compositionEcho = null;
    this.localKeys.clear();
  }

  keydown(event: TextKeyboardEvent): boolean {
    this.enabled = true;
    const code = event.code || event.key;
    if (this.composing || isCompositionKey(event)) {
      this.localKeys.add(code);
      this.releaseKeys();
      return true;
    }
    this.compositionEcho = null;
    this.acceptsInput = true;
    const altGraph = event.getModifierState?.("AltGraph") ?? false;
    if (Array.from(event.key).length === 1 && (altGraph || !(event.ctrlKey || event.altKey || event.metaKey))) {
      this.localKeys.add(code);
      return true;
    }
    return false;
  }

  keyup(event: TextKeyboardEvent): boolean {
    return this.localKeys.delete(event.code || event.key)
      || this.composing || isCompositionKey(event) || !this.enabled;
  }

  compositionstart(): void {
    if (!this.enabled) return;
    this.composing = true;
    this.acceptsInput = true;
    this.compositionEcho = null;
    this.releaseKeys();
  }

  compositionend(data: string): void {
    if (!this.enabled || !this.composing) return;
    this.composing = false;
    this.compositionEcho = data;
    // Empty data means cancellation. The textarea can still contain preedit.
    if (data) this.send(data);
  }

  input(value: string, data: string | null, inputType: string, isComposing: boolean): boolean {
    if (this.composing || isComposing) return false;
    if (!this.enabled || !this.acceptsInput || inputType === "insertCompositionText"
      || inputType === "insertFromComposition" || inputType === "deleteCompositionText") return true;
    if (this.compositionEcho !== null) {
      this.compositionEcho = null;
      return true;
    }
    this.compositionEcho = null;
    if (inputType.startsWith("insert")) this.send(value || data || "");
    return true;
  }

  private send(text: string): void {
    if (!text) return;
    this.releaseKeys();
    this.commit(text);
  }
}

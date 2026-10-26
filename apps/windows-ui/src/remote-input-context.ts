import type { ControllerVideoConfigSignal, RemoteInputContext } from "./types";
import { isU64Identity } from "./u64-identity";

type Configuration = Pick<ControllerVideoConfigSignal, "streamId" | "inputEpoch" | "configVersion">;

export interface PaintedInputFrame {
  readonly configuration: Configuration;
  readonly channelGeneration: number;
  readonly frameToken: object;
}

export function sameInputContext(left: RemoteInputContext | null, right: RemoteInputContext | null): boolean {
  return left !== null && right !== null && left.streamId === right.streamId && left.inputEpoch === right.inputEpoch;
}

export function validInputContext(context: unknown): context is RemoteInputContext {
  if (!context || typeof context !== "object") return false;
  const value = context as Partial<RemoteInputContext>;
  return isU64Identity(value.streamId) && isU64Identity(value.inputEpoch);
}

function sameConfiguration(left: Configuration | null, right: Configuration): boolean {
  return sameInputContext(left, right) && left?.configVersion === right.configVersion;
}

/** Announced video becomes interactive only after that exact frame is painted
 * and a subsequent visible animation frame confirms the same session/config. */
export class RemoteInputPresentation {
  #configuration: Configuration | null = null;
  #channelGeneration = -1;
  #context: RemoteInputContext | null = null;
  #active = false;
  #painted: PaintedInputFrame | null = null;
  #latestFrameToken: object | null = null;

  get context(): RemoteInputContext | null { return this.#active ? this.#context : null; }
  get painted(): PaintedInputFrame | null { return this.#painted; }

  announce(configuration: Configuration, channelGeneration: number): { changed: boolean; contextChanged: boolean } {
    const changed = this.#channelGeneration !== channelGeneration || !sameConfiguration(this.#configuration, configuration);
    const contextChanged = this.#configuration !== null
      && (this.#channelGeneration !== channelGeneration || !sameInputContext(this.#configuration, configuration));
    if (!changed) return { changed: false, contextChanged: false };
    this.#configuration = { ...configuration };
    this.#channelGeneration = channelGeneration;
    this.#context = validInputContext(configuration)
      ? Object.freeze({ streamId: configuration.streamId, inputEpoch: configuration.inputEpoch }) : null;
    this.#active = false;
    this.#painted = null;
    this.#latestFrameToken = null;
    return { changed, contextChanged };
  }

  drew(configuration: Configuration, channelGeneration: number, frameToken: object): PaintedInputFrame | null {
    if (!this.#context || channelGeneration !== this.#channelGeneration || !sameConfiguration(this.#configuration, configuration)) return null;
    this.#latestFrameToken = frameToken;
    if (this.#active) return null;
    this.#painted = { configuration: { ...configuration }, channelGeneration, frameToken };
    return this.#painted;
  }

  activate(frame: PaintedInputFrame, channelGeneration: number, runtimeStreamId: string | null, visible: boolean): boolean {
    if (!visible || this.#painted !== frame || frame.frameToken !== this.#latestFrameToken || frame.channelGeneration !== channelGeneration
      || channelGeneration !== this.#channelGeneration || runtimeStreamId !== frame.configuration.streamId
      || !sameConfiguration(this.#configuration, frame.configuration)) return false;
    this.#active = this.#context !== null;
    return this.#active;
  }

  suspend(): void {
    this.#active = false;
    if (this.#configuration && this.#latestFrameToken && this.#painted?.frameToken !== this.#latestFrameToken) {
      this.#painted = { configuration: { ...this.#configuration }, channelGeneration: this.#channelGeneration, frameToken: this.#latestFrameToken };
    }
  }

  clear(): void {
    this.#configuration = null;
    this.#context = null;
    this.#painted = null;
    this.#latestFrameToken = null;
    this.#active = false;
    this.#channelGeneration = -1;
  }
}

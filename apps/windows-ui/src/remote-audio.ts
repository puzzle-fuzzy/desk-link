import {
  REMOTE_AUDIO_MAX_PACKET_SAMPLES,
  REMOTE_AUDIO_SAMPLE_RATE,
  REMOTE_AUDIO_TARGET_BUFFER_SAMPLES,
} from "./remote-audio-buffer";

export type BinaryPayload = ArrayBuffer | ArrayBufferView | number[];

export interface RemoteAudioPacket {
  streamId: bigint;
  sequence: bigint;
  captureTimestampUs: bigint;
  sampleRate: number;
  samples: Float32Array;
}

const AUDIO_PREFIX_BYTES = 28;
const AUDIO_SAMPLE_RATE = REMOTE_AUDIO_SAMPLE_RATE;
const MAX_AUDIO_PAYLOAD_BYTES = 960;
const MAX_CAPTURE_GAP_US = 100_000n;
const TRANSFER_POOL_PACKETS = 25;

export interface RemoteAudioPlaybackSnapshot {
  state: "starting" | "running" | "suspended" | "muted" | "failed" | "closed";
  message: string;
  bufferedSamples: number;
  underrunSamples: number;
  overflowSamples: number;
  staleSamples: number;
  renderedSamples: number;
  droppedPackets: number;
  jitterMs: number;
}

interface AudioPlayerOptions {
  createContext?: () => AudioContext;
  createNode?: (context: AudioContext, targetSamples: number) => AudioWorkletNode;
  loadModule?: (context: AudioContext) => Promise<void>;
  now?: () => number;
  targetBufferMs?: number;
}

function readRemoteAudioPayload(payload: BinaryPayload): {
  view: DataView;
  streamId: bigint;
  sequence: bigint;
  captureTimestampUs: bigint;
  sampleRate: number;
  sampleCount: number;
} {
  const bytes = toUint8Array(payload);
  const pcmBytes = bytes.byteLength - AUDIO_PREFIX_BYTES;
  if (
    pcmBytes <= 0
    || pcmBytes > MAX_AUDIO_PAYLOAD_BYTES
    || pcmBytes % 2 !== 0
  ) {
    throw new TypeError("远程声音数据长度无效");
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const streamId = view.getBigUint64(0, true);
  const sequence = view.getBigUint64(8, true);
  const captureTimestampUs = view.getBigUint64(16, true);
  const sampleRate = view.getUint32(24, true);
  if (
    streamId === 0n
    || sequence === 0n
    || captureTimestampUs === 0n
    || sampleRate !== AUDIO_SAMPLE_RATE
  ) {
    throw new TypeError("远程声音数据格式无效");
  }

  return {
    view,
    streamId,
    sequence,
    captureTimestampUs,
    sampleRate,
    sampleCount: pcmBytes / 2,
  };
}

export function decodeRemoteAudioPayload(payload: BinaryPayload): RemoteAudioPacket {
  const packet = readRemoteAudioPayload(payload);
  const samples = new Float32Array(packet.sampleCount);
  decodePcm(packet.view, samples, packet.sampleCount);
  return {
    streamId: packet.streamId, sequence: packet.sequence,
    captureTimestampUs: packet.captureTimestampUs, sampleRate: packet.sampleRate, samples,
  };
}

function decodePcm(view: DataView, samples: Float32Array, count: number): void {
  for (let index = 0; index < count; index += 1) {
    samples[index] = view.getInt16(AUDIO_PREFIX_BYTES + index * 2, true) / 32_768;
  }
}

export class RemoteAudioPlayer {
  private context: AudioContext | null = null;
  private gain: GainNode | null = null;
  private node: AudioWorkletNode | null = null;
  private loading: Promise<void> | null = null;
  private generation = 0;
  private pool: Float32Array[] = [];
  private pending: { samples: Float32Array; count: number; queuedAt: number }[] = [];
  private enabled = true;
  private streamId: bigint | null = null;
  private nextSequence: bigint | null = null;
  private lastCaptureTimestampUs: bigint | null = null;
  private lastArrivalMs: number | null = null;
  private resetAfterDrop = false;
  private readonly options: AudioPlayerOptions;
  private readonly listeners = new Set<(snapshot: RemoteAudioPlaybackSnapshot) => void>();
  private snapshot: RemoteAudioPlaybackSnapshot = {
    state: "starting", message: "正在准备本机声音播放。", bufferedSamples: 0,
    underrunSamples: 0, overflowSamples: 0, staleSamples: 0, renderedSamples: 0,
    droppedPackets: 0, jitterMs: 0,
  };

  constructor(options: AudioPlayerOptions = {}) {
    this.options = options;
  }

  getSnapshot(): RemoteAudioPlaybackSnapshot { return { ...this.snapshot }; }

  subscribe(listener: (snapshot: RemoteAudioPlaybackSnapshot) => void): () => void {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  }

  prepare(): void {
    if (!this.enabled) return;
    if (this.context?.state === "closed") this.release();
    if (this.snapshot.state === "failed") this.update({ state: "starting", message: "正在重试本机声音播放。" });
    this.ensurePrepared(true);
  }

  private ensurePrepared(resume = false): void {
    if (!this.enabled || this.snapshot.state === "failed") return;
    try {
      if (!this.context) {
        resume = true;
        const context = this.options.createContext?.()
          ?? new window.AudioContext({ latencyHint: "interactive", sampleRate: AUDIO_SAMPLE_RATE });
        this.context = context;
        this.pool = Array.from({ length: TRANSFER_POOL_PACKETS }, () => new Float32Array(REMOTE_AUDIO_MAX_PACKET_SAMPLES));
        this.gain = context.createGain();
        this.gain.connect(context.destination);
        context.onstatechange = () => {
          if (this.context !== context) return;
          // Never replay audio captured before a suspension / device interruption.
          if (context.state !== "running") this.resetConnection();
          this.updateContextState();
        };
      }
      const context = this.context;
      if (!this.node && !this.loading) {
        const loadModule = this.options.loadModule ?? (async (audioContext: AudioContext) => {
          const { default: workletUrl } = await import("./remote-audio-worklet.ts?worker&url");
          await audioContext.audioWorklet.addModule(workletUrl);
        });
        this.loading = loadModule(context).then(() => {
          if (this.context !== context || context.state === "closed") return;
          const targetSamples = Math.round(Math.max(10, Math.min(100, this.options.targetBufferMs ?? 40)) * AUDIO_SAMPLE_RATE / 1_000);
          const node = this.options.createNode?.(context, targetSamples) ?? new AudioWorkletNode(context, "desklink-remote-audio", {
            numberOfInputs: 0, numberOfOutputs: 1, outputChannelCount: [1],
            processorOptions: { targetSamples: targetSamples || REMOTE_AUDIO_TARGET_BUFFER_SAMPLES },
          });
          this.node = node;
          node.port.onmessage = (event: MessageEvent) => {
            if (this.node !== node) return;
            const message = event.data;
            if (message?.type === "recycle" && message.samples instanceof Float32Array) {
              this.pool.push(message.samples);
            } else if (message?.type === "metrics" && message.generation === this.generation) {
              this.update({
                bufferedSamples: message.bufferedSamples, underrunSamples: message.underrunSamples,
                overflowSamples: message.overflowSamples, staleSamples: message.staleSamples,
                renderedSamples: message.renderedSamples,
              });
            }
          };
          node.onprocessorerror = () => {
            if (this.node === node) this.fail("本机声音播放发生错误，请重新打开声音。");
          };
          node.connect(this.gain!);
          node.port.postMessage({ type: "reset", generation: this.generation });
          for (const packet of this.pending.splice(0)) this.send(packet);
          this.updateContextState();
        }).catch(() => {
          if (this.context === context) this.fail("本机声音播放无法启动，请重新打开声音。");
        }).finally(() => {
          if (this.context === context) this.loading = null;
        });
      }
      if (resume && context.state === "suspended") {
        void context.resume().then(() => {
          if (this.context === context) this.updateContextState();
        }).catch(() => {
          if (this.context === context) this.update({ state: "suspended", message: "本机声音已暂停，请点击打开声音重试。" });
        });
      }
      this.updateContextState();
    } catch {
      this.fail("本机声音播放不可用，请重新打开声音。");
    }
  }

  setEnabled(enabled: boolean): void {
    this.enabled = enabled;
    if (this.gain) this.gain.gain.value = enabled ? 1 : 0;
    if (!enabled) {
      this.resetConnection();
      this.updateContextState();
      return;
    }
    this.prepare();
  }

  push(payload: BinaryPayload): void {
    if (!this.enabled) return;
    const packet = readRemoteAudioPayload(payload);
    this.ensurePrepared();
    const context = this.context;
    if (!context || context.state !== "running" || this.snapshot.state === "failed") return;

    const sequenceDiscontinuity =
      this.streamId !== null
      && (packet.streamId !== this.streamId || packet.sequence !== this.nextSequence);
    const captureDiscontinuity =
      this.lastCaptureTimestampUs !== null
      && (
        packet.captureTimestampUs < this.lastCaptureTimestampUs
        || packet.captureTimestampUs - this.lastCaptureTimestampUs > MAX_CAPTURE_GAP_US
      );
    if (
      sequenceDiscontinuity
      || captureDiscontinuity
    ) {
      this.resetConnection();
    }
    const now = this.options.now?.() ?? performance.now();
    if (this.lastArrivalMs !== null) {
      const deviation = Math.abs(now - this.lastArrivalMs - packet.sampleCount / AUDIO_SAMPLE_RATE * 1_000);
      this.snapshot.jitterMs += (deviation - this.snapshot.jitterMs) / 16;
    }
    this.lastArrivalMs = now;
    let samples = this.pool.pop();
    if (!samples && this.pending.length > 0) {
      samples = this.pending.shift()!.samples;
      this.snapshot.droppedPackets += 1;
    }
    if (!samples) {
      this.snapshot.droppedPackets += 1;
      this.resetAfterDrop = true;
      this.streamId = packet.streamId;
      this.nextSequence = packet.sequence + 1n;
      this.lastCaptureTimestampUs = packet.captureTimestampUs;
      return;
    }
    if (this.resetAfterDrop) this.resetConnection();
    decodePcm(packet.view, samples, packet.sampleCount);
    const queued = { samples, count: packet.sampleCount, queuedAt: context.currentTime };
    if (this.node) this.send(queued);
    else this.pending.push(queued);
    this.streamId = packet.streamId;
    this.nextSequence = packet.sequence + 1n;
    this.lastCaptureTimestampUs = packet.captureTimestampUs;
  }

  resetConnection(): void {
    this.generation += 1;
    for (const packet of this.pending.splice(0)) this.pool.push(packet.samples);
    this.node?.port.postMessage({ type: "reset", generation: this.generation });
    this.streamId = null;
    this.nextSequence = null;
    this.lastCaptureTimestampUs = null;
    this.lastArrivalMs = null;
    this.resetAfterDrop = false;
    this.snapshot.bufferedSamples = 0;
  }

  release(): void {
    this.resetConnection();
    const context = this.context;
    this.node?.disconnect();
    this.node?.port.close();
    this.gain?.disconnect();
    this.node = null;
    this.context = null;
    this.gain = null;
    this.loading = null;
    this.pool = [];
    this.update({ state: "closed", message: "声音播放已结束。" });
    if (context && context.state !== "closed") {
      context.onstatechange = null;
      void context.close().catch(() => {});
    }
  }

  private send(packet: { samples: Float32Array; count: number; queuedAt: number }): void {
    this.node!.port.postMessage({ type: "samples", generation: this.generation, ...packet }, [packet.samples.buffer]);
  }

  private updateContextState(): void {
    if (this.snapshot.state === "failed") return;
    if (!this.enabled) this.update({ state: "muted", message: "远端系统声音已静音。" });
    else if (this.context?.state === "suspended") this.update({ state: "suspended", message: "本机声音已暂停，请点击打开声音重试。" });
    else if (this.context?.state === "closed") this.update({ state: "closed", message: "声音播放已结束。" });
    else if (this.node) this.update({ state: "running", message: "正在播放远端系统声音。" });
    else this.update({ state: "starting", message: "正在准备本机声音播放。" });
  }

  private fail(message: string): void {
    this.resetConnection();
    this.node?.disconnect();
    this.node?.port.close();
    this.node = null;
    this.pool = Array.from({ length: TRANSFER_POOL_PACKETS }, () => new Float32Array(REMOTE_AUDIO_MAX_PACKET_SAMPLES));
    this.update({ state: "failed", message });
  }

  private update(patch: Partial<RemoteAudioPlaybackSnapshot>): void {
    if (Object.entries(patch).every(([key, value]) => this.snapshot[key as keyof RemoteAudioPlaybackSnapshot] === value)) return;
    this.snapshot = { ...this.snapshot, ...patch };
    for (const listener of this.listeners) listener(this.getSnapshot());
  }
}

function toUint8Array(payload: BinaryPayload): Uint8Array {
  if (payload instanceof Uint8Array) return payload;
  if (ArrayBuffer.isView(payload)) {
    return new Uint8Array(payload.buffer, payload.byteOffset, payload.byteLength);
  }
  if (payload instanceof ArrayBuffer) return new Uint8Array(payload);
  if (Array.isArray(payload)) return Uint8Array.from(payload);
  throw new TypeError("Tauri 返回了无法识别的声音二进制格式");
}

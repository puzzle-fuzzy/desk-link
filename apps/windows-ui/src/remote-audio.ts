export type BinaryPayload = ArrayBuffer | ArrayBufferView | number[];

export interface RemoteAudioPacket {
  streamId: bigint;
  sequence: bigint;
  captureTimestampUs: bigint;
  sampleRate: number;
  samples: Float32Array;
}

const AUDIO_PREFIX_BYTES = 28;
const AUDIO_SAMPLE_RATE = 48_000;
const MAX_AUDIO_PAYLOAD_BYTES = 960;
const STARTUP_LEAD_SECONDS = 0.04;
const MAX_SCHEDULE_AHEAD_SECONDS = 0.25;
const MAX_CAPTURE_GAP_US = 100_000n;

export function decodeRemoteAudioPayload(payload: BinaryPayload): RemoteAudioPacket {
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

  const samples = new Float32Array(pcmBytes / 2);
  for (let index = 0; index < samples.length; index += 1) {
    samples[index] = view.getInt16(AUDIO_PREFIX_BYTES + index * 2, true) / 32_768;
  }
  return {
    streamId,
    sequence,
    captureTimestampUs,
    sampleRate,
    samples,
  };
}

export class RemoteAudioPlayer {
  private context: AudioContext | null = null;
  private gain: GainNode | null = null;
  private sources = new Set<AudioBufferSourceNode>();
  private enabled = true;
  private nextStart = 0;
  private streamId: bigint | null = null;
  private nextSequence: bigint | null = null;
  private lastCaptureTimestampUs: bigint | null = null;

  prepare(): void {
    if (!this.enabled) return;
    if (!this.context) {
      const AudioContextConstructor = window.AudioContext;
      this.context = new AudioContextConstructor({ latencyHint: "interactive" });
      this.gain = this.context.createGain();
      this.gain.gain.value = 1;
      this.gain.connect(this.context.destination);
    }
    if (this.context.state === "suspended") {
      void this.context.resume().catch(() => {
        // A later click on the sound button provides another activation chance.
      });
    }
  }

  setEnabled(enabled: boolean): void {
    this.enabled = enabled;
    if (!enabled) {
      this.stopScheduled();
      return;
    }
    this.prepare();
  }

  push(payload: BinaryPayload): void {
    if (!this.enabled) return;
    const packet = decodeRemoteAudioPayload(payload);
    this.prepare();
    const context = this.context;
    const gain = this.gain;
    if (!context || !gain || context.state === "closed") return;

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
      || this.nextStart - context.currentTime > MAX_SCHEDULE_AHEAD_SECONDS
    ) {
      this.stopScheduled();
    }

    const buffer = context.createBuffer(1, packet.samples.length, packet.sampleRate);
    buffer.getChannelData(0).set(packet.samples);
    const source = context.createBufferSource();
    source.buffer = buffer;
    source.connect(gain);
    source.addEventListener("ended", () => {
      source.disconnect();
      this.sources.delete(source);
    }, { once: true });

    const startAt = Math.max(
      context.currentTime + STARTUP_LEAD_SECONDS,
      this.nextStart,
    );
    source.start(startAt);
    this.sources.add(source);
    this.nextStart = startAt + buffer.duration;
    this.streamId = packet.streamId;
    this.nextSequence = packet.sequence + 1n;
    this.lastCaptureTimestampUs = packet.captureTimestampUs;
  }

  resetConnection(): void {
    this.stopScheduled();
  }

  release(): void {
    this.stopScheduled();
    const context = this.context;
    this.context = null;
    this.gain = null;
    if (context && context.state !== "closed") {
      void context.close().catch(() => {});
    }
  }

  private stopScheduled(): void {
    for (const source of this.sources) {
      try {
        source.stop();
      } catch {
        // A source that has already ended needs no further cleanup.
      }
      source.disconnect();
    }
    this.sources.clear();
    this.nextStart = 0;
    this.streamId = null;
    this.nextSequence = null;
    this.lastCaptureTimestampUs = null;
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

export const REMOTE_AUDIO_SAMPLE_RATE = 48_000;
export const REMOTE_AUDIO_MAX_PACKET_SAMPLES = 480;
export const REMOTE_AUDIO_MAX_BUFFER_SAMPLES = 12_000;
export const REMOTE_AUDIO_TARGET_BUFFER_SAMPLES = 1_920;

/** A single allocation owned by the audio thread; overflow discards the oldest PCM. */
export class RemoteAudioBuffer {
  private readonly samples: Float32Array;
  private readonly queuedAt: Float64Array;
  private readIndex = 0;
  private length = 0;
  private playing = false;
  private started = false;
  readonly targetSamples: number;
  underrunSamples = 0;
  overflowSamples = 0;
  renderedSamples = 0;
  staleSamples = 0;

  constructor(
    readonly capacity = REMOTE_AUDIO_MAX_BUFFER_SAMPLES,
    targetSamples = REMOTE_AUDIO_TARGET_BUFFER_SAMPLES,
  ) {
    if (!Number.isInteger(capacity) || capacity < 1) throw new RangeError("Invalid audio capacity");
    this.samples = new Float32Array(capacity);
    this.queuedAt = new Float64Array(capacity);
    this.targetSamples = Math.max(1, Math.min(capacity, Math.round(targetSamples)));
  }

  get bufferedSamples(): number { return this.length; }

  reset(): void {
    this.readIndex = 0;
    this.length = 0;
    this.playing = false;
    this.started = false;
  }

  push(samples: Float32Array, count = samples.length, queuedAt = 0): void {
    count = Math.min(samples.length, Math.max(0, Math.floor(count)));
    const skipped = Math.max(0, count - this.capacity);
    const discard = Math.max(0, this.length + count - skipped - this.capacity);
    this.readIndex = (this.readIndex + discard) % this.capacity;
    this.length -= discard;
    this.overflowSamples += skipped + discard;
    for (let index = skipped; index < count; index += 1) {
      const writeIndex = (this.readIndex + this.length) % this.capacity;
      this.samples[writeIndex] = samples[index]!;
      this.queuedAt[writeIndex] = queuedAt;
      this.length += 1;
    }
  }

  render(output: Float32Array, now = 0): void {
    output.fill(0);
    while (this.length > 0 && now - this.queuedAt[this.readIndex]! > REMOTE_AUDIO_MAX_BUFFER_SAMPLES / REMOTE_AUDIO_SAMPLE_RATE) {
      this.readIndex = (this.readIndex + 1) % this.capacity;
      this.length -= 1;
      this.staleSamples += 1;
    }
    if (!this.playing) {
      if (this.length < this.targetSamples) {
        if (this.started) this.underrunSamples += output.length;
        return;
      }
      this.playing = true;
      this.started = true;
    }
    const count = Math.min(output.length, this.length);
    for (let index = 0; index < count; index += 1) {
      output[index] = this.samples[this.readIndex]!;
      this.readIndex = (this.readIndex + 1) % this.capacity;
    }
    this.length -= count;
    this.renderedSamples += count;
    if (count < output.length) {
      this.underrunSamples += output.length - count;
      this.playing = false;
    }
  }
}

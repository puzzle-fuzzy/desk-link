import {
  REMOTE_AUDIO_MAX_BUFFER_SAMPLES,
  REMOTE_AUDIO_SAMPLE_RATE,
  REMOTE_AUDIO_TARGET_BUFFER_SAMPLES,
  RemoteAudioBuffer,
} from "./remote-audio-buffer";

declare const currentTime: number;
declare class AudioWorkletProcessor {
  readonly port: MessagePort;
}
declare function registerProcessor(name: string, processor: typeof AudioWorkletProcessor): void;

interface AudioMessage {
  type: "samples";
  generation: number;
  samples: Float32Array;
  count: number;
  queuedAt: number;
}

class DeskLinkAudioProcessor extends AudioWorkletProcessor {
  private readonly buffer: RemoteAudioBuffer;
  private generation = 0;
  private staleSamples = 0;
  private reportAfter = 0;

  constructor(options?: { processorOptions?: { targetSamples?: number } }) {
    super();
    this.buffer = new RemoteAudioBuffer(
      REMOTE_AUDIO_MAX_BUFFER_SAMPLES,
      options?.processorOptions?.targetSamples ?? REMOTE_AUDIO_TARGET_BUFFER_SAMPLES,
    );
    this.port.onmessage = (event: MessageEvent<AudioMessage | { type: "reset"; generation: number }>) => {
      const message = event.data;
      if (message.type === "reset") {
        this.generation = message.generation;
        this.buffer.reset();
        return;
      }
      if (message.type !== "samples" || !(message.samples instanceof Float32Array)) return;
      if (
        message.generation === this.generation
        && currentTime - message.queuedAt <= REMOTE_AUDIO_MAX_BUFFER_SAMPLES / REMOTE_AUDIO_SAMPLE_RATE
      ) {
        this.buffer.push(message.samples, message.count, message.queuedAt);
      } else {
        this.staleSamples += message.count;
      }
      this.port.postMessage({ type: "recycle", samples: message.samples }, [message.samples.buffer]);
    };
  }

  process(_inputs: Float32Array[][], outputs: Float32Array[][]): boolean {
    const output = outputs[0]?.[0];
    if (!output) return true;
    this.buffer.render(output, currentTime);
    this.reportAfter += output.length;
    if (this.reportAfter >= REMOTE_AUDIO_SAMPLE_RATE / 4) {
      this.reportAfter = 0;
      this.port.postMessage({
        type: "metrics",
        generation: this.generation,
        bufferedSamples: this.buffer.bufferedSamples,
        underrunSamples: this.buffer.underrunSamples,
        overflowSamples: this.buffer.overflowSamples,
        renderedSamples: this.buffer.renderedSamples,
        staleSamples: this.staleSamples + this.buffer.staleSamples,
      });
    }
    return true;
  }
}

registerProcessor("desklink-remote-audio", DeskLinkAudioProcessor);

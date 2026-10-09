import type { JobOutput } from "./api";
import { isRunning } from "./format";

export type Stream = "stdout" | "stderr";
export interface OutputChunk {
  id: number;
  stream: Stream;
  text: string;
}

/** Each stream keeps its own UTF-8 decoder because characters can span events. */
export class OutputBuffer {
  after: number | null = null;
  chunks: OutputChunk[] = [];
  truncated = false;
  private decoders = new Map<Stream, TextDecoder>();
  private length = 0;
  private nextId = 0;
  private flushed = false;

  consume(result: JobOutput) {
    if (this.after === null && result.events[0]?.seq > 1) this.truncated = true;
    for (const event of result.events) {
      if (this.after !== null && event.seq <= this.after) continue;
      const stream = event.stream === "stderr" ? "stderr" : "stdout";
      let decoder = this.decoders.get(stream);
      if (!decoder) {
        decoder = new TextDecoder();
        this.decoders.set(stream, decoder);
      }
      const bytes = Uint8Array.from(atob(event.data_base64), (char) =>
        char.charCodeAt(0),
      );
      this.append(stream, decoder.decode(bytes, { stream: true }));
      this.after = event.seq;
    }
    if (this.after === null) this.after = result.job.last_log_seq;
    if (!isRunning(result.job) && !result.has_more && !this.flushed) {
      for (const [stream, decoder] of this.decoders)
        this.append(stream, decoder.decode());
      this.flushed = true;
    }
  }

  private append(stream: Stream, value: string) {
    // React renders this as text; strip terminal escape sequences and controls too.
    const text = value
      .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "")
      .replace(/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g, "");
    if (!text) return;
    this.chunks.push({ id: this.nextId++, stream, text });
    this.length += text.length;
    while (this.length > 1024 * 1024 || this.chunks.length > 2000) {
      this.length -= this.chunks.shift()!.text.length;
      this.truncated = true;
    }
  }
}

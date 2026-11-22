// Live updates over /api/events (SSE).
// EventSource reconnects by itself after a dropped connection, but gives up when the
// server answers with an error status (readyState CLOSED). We then reconnect with
// exponential backoff. A `hello` whose generation differs from the last one seen
// means we missed an `index-finished` while disconnected, so we report it as stale.
import { SERVER_EVENT_TYPES, type ServerEvent } from "./types";

export interface EventSourceLike {
  readonly readyState: number;
  onopen: ((ev: Event) => unknown) | null;
  onerror: ((ev: Event) => unknown) | null;
  addEventListener(type: string, listener: (ev: MessageEvent) => void): void;
  close(): void;
}

export type EventSourceFactory = (url: string) => EventSourceLike;

export type ConnectionState = "connecting" | "open" | "reconnecting" | "closed";

export interface LiveEventsOptions {
  url: string;
  factory?: EventSourceFactory;
  onEvent: (event: ServerEvent) => void;
  /** Called when a `hello` reveals that we missed at least one re-index. */
  onStale?: (generation: number) => void;
  onState?: (state: ConnectionState) => void;
  minDelayMs?: number;
  maxDelayMs?: number;
  /** Timer functions, injectable for tests. */
  setTimer?: (fn: () => void, ms: number) => unknown;
  clearTimer?: (handle: unknown) => void;
}

const CLOSED = 2;

export class LiveEvents {
  private source: EventSourceLike | null = null;
  private attempt = 0;
  private timer: unknown = null;
  private stopped = false;
  private lastGeneration: number | null = null;
  private readonly factory: EventSourceFactory;
  private readonly setTimer: (fn: () => void, ms: number) => unknown;
  private readonly clearTimer: (handle: unknown) => void;

  constructor(private readonly opts: LiveEventsOptions) {
    this.factory = opts.factory ?? ((url) => new EventSource(url) as unknown as EventSourceLike);
    this.setTimer = opts.setTimer ?? ((fn, ms) => setTimeout(fn, ms));
    this.clearTimer = opts.clearTimer ?? ((h) => clearTimeout(h as ReturnType<typeof setTimeout>));
  }

  get generation(): number | null {
    return this.lastGeneration;
  }

  /** Records a generation obtained elsewhere (for example from /api/meta). */
  noteGeneration(generation: number) {
    if (this.lastGeneration === null || generation > this.lastGeneration) this.lastGeneration = generation;
  }

  start() {
    this.stopped = false;
    this.connect();
  }

  stop() {
    this.stopped = true;
    if (this.timer !== null) this.clearTimer(this.timer);
    this.timer = null;
    this.source?.close();
    this.source = null;
    this.opts.onState?.("closed");
  }

  /** Delay before the next reconnect attempt: 1 s, 2 s, 4 s... capped. */
  nextDelay(): number {
    const min = this.opts.minDelayMs ?? 1000;
    const max = this.opts.maxDelayMs ?? 30000;
    return Math.min(max, min * 2 ** Math.max(0, this.attempt - 1));
  }

  private connect() {
    if (this.stopped) return;
    this.opts.onState?.(this.attempt === 0 ? "connecting" : "reconnecting");
    const source = this.factory(this.opts.url);
    this.source = source;
    source.onopen = () => {
      this.attempt = 0;
      this.opts.onState?.("open");
    };
    source.onerror = () => {
      if (this.stopped || this.source !== source) return;
      if (source.readyState === CLOSED) {
        source.close();
        this.scheduleReconnect();
      } else {
        // The browser retries on its own; just report it.
        this.opts.onState?.("reconnecting");
      }
    };
    for (const type of SERVER_EVENT_TYPES) {
      source.addEventListener(type, (ev: MessageEvent) => {
        let data: unknown;
        try {
          data = JSON.parse(String(ev.data));
        } catch {
          return;
        }
        this.dispatch({ type, data } as ServerEvent);
      });
    }
  }

  private dispatch(event: ServerEvent) {
    if (event.type === "hello") {
      this.attempt = 0;
      this.opts.onState?.("open");
      const previous = this.lastGeneration;
      if (previous !== null && event.data.generation !== previous) this.opts.onStale?.(event.data.generation);
      // Unconditional: after a server restart the generation starts over, and holding on
      // to the old, higher value would make every later comparison wrong.
      this.lastGeneration = event.data.generation;
    } else if (event.type === "index-finished") {
      this.noteGeneration(event.data.generation);
    }
    this.opts.onEvent(event);
  }

  private scheduleReconnect() {
    if (this.stopped) return;
    this.attempt += 1;
    this.opts.onState?.("reconnecting");
    const delay = this.nextDelay();
    this.timer = this.setTimer(() => {
      this.timer = null;
      this.connect();
    }, delay);
  }
}

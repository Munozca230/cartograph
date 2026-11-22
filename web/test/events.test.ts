import { describe, expect, it } from "vitest";
import { LiveEvents, type EventSourceLike } from "../src/api/events";
import type { ServerEvent } from "../src/api/types";

class FakeSource implements EventSourceLike {
  readyState = 0;
  onopen: ((ev: Event) => unknown) | null = null;
  onerror: ((ev: Event) => unknown) | null = null;
  closed = false;
  private listeners = new Map<string, ((ev: MessageEvent) => void)[]>();
  addEventListener(type: string, listener: (ev: MessageEvent) => void) {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
  }
  close() {
    this.closed = true;
    this.readyState = 2;
  }
  open() {
    this.readyState = 1;
    this.onopen?.(new Event("open"));
  }
  emit(type: string, data: unknown) {
    for (const l of this.listeners.get(type) ?? []) l({ data: JSON.stringify(data) } as MessageEvent);
  }
  fail(closed: boolean) {
    this.readyState = closed ? 2 : 0;
    this.onerror?.(new Event("error"));
  }
}

function harness() {
  const sources: FakeSource[] = [];
  const timers: { fn: () => void; ms: number }[] = [];
  const events: ServerEvent[] = [];
  const stale: number[] = [];
  const states: string[] = [];
  const live = new LiveEvents({
    url: "/api/events",
    factory: () => {
      const s = new FakeSource();
      sources.push(s);
      return s;
    },
    onEvent: (e) => events.push(e),
    onStale: (g) => stale.push(g),
    onState: (s) => states.push(s),
    setTimer: (fn, ms) => {
      timers.push({ fn, ms });
      return timers.length;
    },
    clearTimer: () => {},
  });
  return { live, sources, timers, events, stale, states };
}

describe("LiveEvents", () => {
  it("dispatches typed events", () => {
    const t = harness();
    t.live.start();
    t.sources[0]!.open();
    t.sources[0]!.emit("hello", { generation: 3, status: "ready" });
    t.sources[0]!.emit("index-progress", { done: 10, total: 20 });
    expect(t.events.map((e) => e.type)).toEqual(["hello", "index-progress"]);
    expect(t.live.generation).toBe(3);
    expect(t.states.at(-1)).toBe("open");
  });

  it("lets the browser retry transient errors without opening a second source", () => {
    const t = harness();
    t.live.start();
    t.sources[0]!.fail(false);
    expect(t.sources).toHaveLength(1);
    expect(t.timers).toHaveLength(0);
    expect(t.states.at(-1)).toBe("reconnecting");
  });

  it("reconnects with exponential backoff after a closed connection", () => {
    const t = harness();
    t.live.start();
    t.sources[0]!.fail(true);
    expect(t.timers.map((x) => x.ms)).toEqual([1000]);
    t.timers[0]!.fn();
    expect(t.sources).toHaveLength(2);
    t.sources[1]!.fail(true);
    expect(t.timers.map((x) => x.ms)).toEqual([1000, 2000]);
    t.timers[1]!.fn();
    t.sources[2]!.open();
    t.sources[2]!.fail(true);
    // A successful open resets the backoff.
    expect(t.timers.at(-1)!.ms).toBe(1000);
  });

  it("reports a stale view when the hello after a reconnect has a new generation", () => {
    const t = harness();
    t.live.start();
    t.sources[0]!.emit("hello", { generation: 4, status: "ready" });
    t.sources[0]!.fail(true);
    t.timers[0]!.fn();
    t.sources[1]!.emit("hello", { generation: 6, status: "ready" });
    expect(t.stale).toEqual([6]);
    // Same generation again: nothing was missed.
    t.sources[1]!.fail(true);
    t.timers[1]!.fn();
    t.sources[2]!.emit("hello", { generation: 6, status: "ready" });
    expect(t.stale).toEqual([6]);
  });

  it("follows a generation reset after a server restart", () => {
    const t = harness();
    t.live.start();
    t.sources[0]!.emit("hello", { generation: 9, status: "ready" });
    t.sources[0]!.fail(true);
    t.timers[0]!.fn();
    t.sources[1]!.emit("hello", { generation: 1, status: "ready" });
    expect(t.stale).toEqual([1]);
    expect(t.live.generation).toBe(1);
    // Later re-indexes of the new process count again, and a reconnect at the same generation is quiet.
    t.sources[1]!.emit("index-finished", { generation: 2, ms: 1, changed: [], counts: {} });
    expect(t.live.generation).toBe(2);
    t.sources[1]!.fail(true);
    t.timers[1]!.fn();
    t.sources[2]!.emit("hello", { generation: 2, status: "ready" });
    expect(t.stale).toEqual([1]);
  });

  it("tracks the generation from index-finished and stops cleanly", () => {
    const t = harness();
    t.live.start();
    t.sources[0]!.emit("index-finished", { generation: 9, ms: 10, changed: [], counts: {} });
    expect(t.live.generation).toBe(9);
    t.live.stop();
    expect(t.sources[0]!.closed).toBe(true);
    t.sources[0]!.fail(true);
    expect(t.timers).toHaveLength(0);
  });

  it("caps the backoff", () => {
    const t = harness();
    t.live.start();
    for (let i = 0; i < 8; i++) {
      t.sources.at(-1)!.fail(true);
      t.timers.at(-1)!.fn();
    }
    expect(Math.max(...t.timers.map((x) => x.ms))).toBe(30000);
  });
});

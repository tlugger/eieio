// Behavioural coverage for `streamLogs` (DAEMON §9.6, §11) — driven and read the way a real
// consumer (`InspectorPanel.svelte`) would: what a subscriber's `onEvent` actually receives,
// not just the field-name/required-field check `mock-parity.test.ts` already does.
//
// This is the exact surface eieio-m9s.15 found broken: `mock.ts` emitted `timestamp` where
// `decodeLogFrame` requires `at`, so every mock log line failed to decode and never reached a
// subscriber — and it went unnoticed for one reason: nothing in this repository had ever driven
// `streamLogs` and looked at what came out the other end. `mock-parity.test.ts` drives it too,
// but only to inspect the raw SSE text before it decodes; this file is the missing half, reading
// the decoded `LogLineEvent`s a real caller gets.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { streamLogs } from './mock';
import type { LogFilter, LogLineEvent, StreamStatus } from './types';

afterEach(() => {
  vi.useRealTimers();
});

function collect(nodeId: string, filter: LogFilter = {}) {
  vi.useFakeTimers(); // installed before `streamLogs` schedules its own opening timer —
  // the reverse order starves it (mock-parity.test.ts's beforeAll doc explains why).
  const events: LogLineEvent[] = [];
  const statuses: Array<{ status: StreamStatus; error?: string }> = [];
  const handle = streamLogs(nodeId, filter, {
    onEvent: (e) => events.push(e),
    onStatus: (status, detail) => statuses.push({ status, error: detail?.error }),
  });
  return { events, statuses, handle };
}

describe('streamLogs', () => {
  it('reports connecting then open, and delivers a 5-line backlog synchronously on open', async () => {
    const { events, statuses, handle } = collect('node-porch');
    expect(statuses).toEqual([{ status: 'connecting', error: undefined }]);

    await vi.advanceTimersByTimeAsync(100); // the mock's own opening timer
    expect(statuses.map((s) => s.status)).toEqual(['connecting', 'open']);
    expect(events).toHaveLength(5);
    handle.close();
  });

  it('a subscriber receives every field a log line carries, correctly decoded — not just parsed JSON', async () => {
    const { events, handle } = collect('node-porch');
    await vi.advanceTimersByTimeAsync(100);
    expect(events.length).toBeGreaterThan(0);
    for (const line of events) {
      expect(line.type).toBe('log');
      // The exact bug shape this file exists to catch: a wrong wire field name decodes to
      // `undefined` here instead of failing to decode at all, so this checks the actual value.
      expect(typeof line.timestamp).toBe('string');
      expect(() => new Date(line.timestamp).toISOString()).not.toThrow();
      expect(['INFO', 'WARN', 'ERROR']).toContain(line.level);
      expect(typeof line.message).toBe('string');
      expect(line.message.length).toBeGreaterThan(0);
      expect(typeof line.service).toBe('string');
      expect(typeof line.instance).toBe('string');
    }
    handle.close();
  });

  it('the backlog arrives oldest first', async () => {
    const { events, handle } = collect('node-porch');
    await vi.advanceTimersByTimeAsync(100);
    const times = events.map((e) => Date.parse(e.timestamp));
    expect(times).toEqual([...times].sort((a, b) => a - b));
    handle.close();
  });

  it('cycles the backlog through more than one (service, instance) pair, like a real multi-block service', async () => {
    const { events, handle } = collect('node-porch');
    await vi.advanceTimersByTimeAsync(100);
    const pairs = new Set(events.map((e) => `${e.service}/${e.instance}`));
    expect(pairs.size).toBeGreaterThan(1);
    handle.close();
  });

  it('keeps delivering live lines after the backlog, roughly once a second', async () => {
    const { events, handle } = collect('node-porch');
    await vi.advanceTimersByTimeAsync(100);
    const afterBacklog = events.length;
    await vi.advanceTimersByTimeAsync(1100 * 3);
    expect(events.length).toBeGreaterThan(afterBacklog);
    handle.close();
  });

  it('filters by level: a subscriber asking for ERROR only never sees INFO or WARN', async () => {
    const { events, handle } = collect('node-porch', { level: 'ERROR' });
    await vi.advanceTimersByTimeAsync(100 + 1100 * 6);
    expect(events.length).toBeGreaterThan(0);
    expect(events.every((e) => e.level === 'ERROR')).toBe(true);
    handle.close();
  });

  it('filters by service: only lines from the named service arrive', async () => {
    const { events, handle } = collect('node-porch', { service: 'kitchen' });
    await vi.advanceTimersByTimeAsync(100 + 1100 * 6);
    expect(events.length).toBeGreaterThan(0);
    expect(events.every((e) => e.service === 'kitchen')).toBe(true);
    handle.close();
  });

  it('filters by instance: only lines attributed to that block arrive', async () => {
    const { events, handle } = collect('node-porch', { instance: 'b7k2' });
    await vi.advanceTimersByTimeAsync(100 + 1100 * 8);
    expect(events.length).toBeGreaterThan(0);
    expect(events.every((e) => e.instance === 'b7k2')).toBe(true);
    handle.close();
  });

  it('a node with no services opens and stays open, but streams nothing — not a fabricated, unattributed line', async () => {
    // eieio-m9s.17: this used to fail. `instancesFor` returns `[]` for an unknown node id, and
    // the old code still ran `lineAt`/`dispatch` regardless of `instances.length`, so this case
    // "opened" and then emitted synthetic lines whose `service`/`instance` were both absent —
    // not the empty stream the (unwired) comment beside it claimed. See `mock.ts`'s own comment
    // on the fix, in `streamLogs`'s opening timer.
    const { events, statuses, handle } = collect('no-such-node');
    await vi.advanceTimersByTimeAsync(100 + 1100 * 3);
    expect(statuses.map((s) => s.status)).toEqual(['connecting', 'open']);
    expect(events).toEqual([]);
    handle.close();
  });

  it('close() stops delivery and reports closed; no further events after release', async () => {
    const { events, statuses, handle } = collect('node-porch');
    await vi.advanceTimersByTimeAsync(100 + 1100 * 2);
    const countBeforeClose = events.length;
    handle.close();
    expect(statuses.at(-1)?.status).toBe('closed');

    await vi.advanceTimersByTimeAsync(60_000); // long enough to catch a leaked interval
    expect(events.length).toBe(countBeforeClose);
  });

  // eieio-m9s.28: `closet-relay` (`node-closet`) is this fixture set's `class: 'leaf'` node.
  // Unlike `'no-such-node'` above — which is a real "empty stream" case, DESIGNER §3.1 says
  // nothing about an unknown id — a leaf is a node the operator deliberately registered, and its
  // logs can never be fetched by design. `streamLogs` is not `async`, so this reports the choke
  // point's refusal through `onStatus('closed', ...)` rather than opening and staying silent the
  // way `'no-such-node'` does; those two must not read the same, and this pins the difference.
  it('refuses closet-relay (leaf) by naming the class, rather than opening an empty stream', async () => {
    const { events, statuses, handle } = collect('node-closet');
    await vi.advanceTimersByTimeAsync(0);
    expect(statuses).toEqual([{ status: 'closed', error: expect.stringMatching(/leaf-class/) }]);
    expect(events).toEqual([]);
    handle.close();
  });
});

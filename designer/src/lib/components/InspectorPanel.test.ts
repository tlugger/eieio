// eieio-m9s.41: pins what the panel says when a stream stops for good.
//
// eieio-m9s.39 made a refused stream stop instead of spinning 'reconnecting'
// forever, and `sse.ts` now reports the HTTP status that stopped it
// (`StreamStatusDetail.status`). This suite pins the four renderings that
// status buys, because a distinction held by nothing regresses back into one
// message:
//
//   - `401` — the login gate is already coming up off the same detail
//     (`client.ts`'s `watchStreamSession`), so the panel says the session
//     expired rather than printing the number that explains nothing.
//   - `404` on a tap — DAEMON §9.6: a released tap is gone, its id can only
//     ever `404` again, and only `POST /taps` makes another. Permanent *and*
//     actionable, so the panel offers the action.
//   - a permanent status nobody anticipated — the rule `sse.ts` applies is a
//     *class* (4xx except `408`/`429`), deliberately wider than those two,
//     because a reverse proxy's `403` is the same situation with a different
//     number. The fallback still has to say the two useful things: waiting
//     will not fix it, and here is the status.
//   - a transient disconnect — must keep saying 'reconnecting', because that
//     one does come back on its own. Blurring it back into the stopped case
//     is the regression this test exists to catch.
//
// `../api/client` is mocked wholesale: this is a test of what the panel does
// with a status detail, and the real module would drag `mock.ts`, the proxy
// and `useRealBackend()` in behind it. The stream handlers the panel hands
// `streamTap`/`streamLogs` are captured so a test can deliver exactly the
// transition `sse.ts` would have delivered.
import { flushSync, mount, unmount } from 'svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { StreamStatus, StreamStatusDetail, TapSummary } from '../api/types';

interface CapturedStatusHandler {
  onStatus: (status: StreamStatus, detail?: StreamStatusDetail) => void;
}

const tapStreams: CapturedStatusHandler[] = [];
const logStreams: CapturedStatusHandler[] = [];
const createTap = vi.fn<(nodeId: string, service: string, connection: string) => Promise<TapSummary>>();
const deleteTap = vi.fn<(nodeId: string, tapId: string) => Promise<void>>();

vi.mock('../api/client', () => ({
  createTap: (...args: [string, string, string]) => createTap(...args),
  deleteTap: (...args: [string, string]) => deleteTap(...args),
  streamTap: (_nodeId: string, _tapId: string, handlers: CapturedStatusHandler) => {
    tapStreams.push(handlers);
    return { close: () => {} };
  },
  streamLogs: (_nodeId: string, _filter: unknown, handlers: CapturedStatusHandler) => {
    logStreams.push(handlers);
    return { close: () => {} };
  },
}));

const InspectorPanel = (await import('./InspectorPanel.svelte')).default;

let tapSeq = 0;

function renderPanel() {
  const target = document.createElement('div');
  document.body.appendChild(target);
  const exports = mount(InspectorPanel, {
    target,
    props: {
      open: true,
      nodeId: 1,
      serviceName: 'svc',
      tappedConnection: { fromId: 'a', fromPort: 'out', toId: 'b', toPort: 'in' },
      selectedBlockId: null,
      onClose: () => {},
      onReleaseTap: () => {},
      resolvePropName: () => undefined,
    },
  });
  return { target, exports };
}

function statusLine(target: HTMLElement): string {
  const el = target.querySelector('.inspector__status');
  if (!el) throw new Error('status line not rendered');
  return el.textContent!.replace(/\s+/g, ' ').trim();
}

function recreateButton(target: HTMLElement): HTMLButtonElement | null {
  return target.querySelector('.inspector__recreate');
}

/** Lets the panel's `createTap` promise (and Svelte's own effect flush) settle,
 *  so `streamTap`'s handlers have been captured. */
async function settle() {
  flushSync();
  await Promise.resolve();
  await Promise.resolve();
  flushSync();
}

/** Delivers a transition the way `sse.ts` would, then flushes the render. */
function deliver(stream: CapturedStatusHandler, status: StreamStatus, detail?: StreamStatusDetail) {
  stream.onStatus(status, detail);
  flushSync();
}

beforeEach(() => {
  tapStreams.length = 0;
  logStreams.length = 0;
  createTap.mockReset();
  deleteTap.mockReset();
  deleteTap.mockResolvedValue(undefined);
  createTap.mockImplementation(async () => {
    tapSeq += 1;
    return { tap_id: `tap-${tapSeq}`, service: 'svc', connection: 'a.out -> b.in', instance: 'a', port: 'out' };
  });
});

afterEach(() => {
  document.body.innerHTML = '';
});

describe('InspectorPanel — a stopped stream says what happened, not what the transport said (eieio-m9s.41)', () => {
  it('renders a 401 as an expired session, not as the status line', async () => {
    const { target, exports } = renderPanel();
    await settle();
    deliver(tapStreams[0]!, 'closed', { error: 'stream request refused: 401', status: 401 });

    expect(statusLine(target)).toContain('Session expired');
    // The gate `client.ts` raises off the same detail is what the operator is
    // about to see; the transport's own words would only compete with it.
    expect(statusLine(target)).not.toContain('401');
    expect(statusLine(target)).not.toContain('stream request refused');
    expect(recreateButton(target)).toBeNull();

    unmount(exports);
  });

  it('renders a 404 on a tap as a released tap and offers to re-create it', async () => {
    const { target, exports } = renderPanel();
    await settle();
    deliver(tapStreams[0]!, 'closed', { error: 'stream request refused: 404', status: 404 });

    expect(statusLine(target)).toContain('released');
    expect(statusLine(target)).not.toContain('stream request refused');
    expect(recreateButton(target)).not.toBeNull();

    unmount(exports);
  });

  it('re-creates the tap for real — POST /taps again, a new id, a live stream', async () => {
    const { target, exports } = renderPanel();
    await settle();
    expect(createTap).toHaveBeenCalledTimes(1);
    deliver(tapStreams[0]!, 'closed', { error: 'stream request refused: 404', status: 404 });

    recreateButton(target)!.click();
    await settle();

    // DAEMON §9.6: the released id is gone for good, so recovery is a second
    // `POST /taps` and a second stream — not a reconnect to the old id.
    expect(createTap).toHaveBeenCalledTimes(2);
    expect(tapStreams).toHaveLength(2);
    deliver(tapStreams[1]!, 'open');
    expect(statusLine(target)).toContain('Live');
    expect(recreateButton(target)).toBeNull();

    // The dead tap's own stream must be inert once a new one is running: a
    // late transition from the stale generation cannot overwrite the live
    // status line.
    deliver(tapStreams[0]!, 'closed', { error: 'stream request refused: 404', status: 404 });
    expect(statusLine(target)).toContain('Live');

    unmount(exports);
  });

  it('renders an unanticipated permanent status with the status and the fact that waiting cannot help', async () => {
    // A `403` from a reverse proxy: never enumerated anywhere in the client,
    // caught only by `isPermanentStreamStatus`'s class.
    const { target, exports } = renderPanel();
    await settle();
    deliver(tapStreams[0]!, 'closed', { error: 'stream request refused: 403', status: 403 });

    expect(statusLine(target)).toContain('HTTP 403');
    expect(statusLine(target)).toContain('repeating cannot change');
    expect(recreateButton(target)).toBeNull();

    unmount(exports);
  });

  it('still renders a transient disconnect as reconnecting', async () => {
    const { target, exports } = renderPanel();
    await settle();
    deliver(tapStreams[0]!, 'reconnecting', { error: 'stream ended' });

    expect(statusLine(target)).toContain('reconnecting');
    expect(statusLine(target)).toContain('stream ended');
    expect(statusLine(target)).not.toContain('Stopped');
    expect(recreateButton(target)).toBeNull();

    unmount(exports);
  });

  it('leaves a 500 alone — 5xx is transient and the loop is still retrying it', async () => {
    const { target, exports } = renderPanel();
    await settle();
    deliver(tapStreams[0]!, 'reconnecting', { error: 'stream request failed: 500' });

    expect(statusLine(target)).toContain('reconnecting');
    expect(recreateButton(target)).toBeNull();

    unmount(exports);
  });

  it('does not offer tap re-creation for a 404 on the log stream', async () => {
    const { target, exports } = renderPanel();
    await settle();
    target.querySelectorAll<HTMLButtonElement>('.inspector__tab')[1]!.click();
    flushSync();
    deliver(logStreams[0]!, 'closed', { error: 'stream request refused: 404', status: 404 });

    // A log stream has no released-tap story: `POST /taps` recovers nothing
    // here, so this falls to the unanticipated-permanent rendering.
    expect(statusLine(target)).toContain('HTTP 404');
    expect(statusLine(target)).not.toContain('released');
    expect(recreateButton(target)).toBeNull();

    unmount(exports);
  });

  it('renders a 401 on the log stream as an expired session too', async () => {
    const { target, exports } = renderPanel();
    await settle();
    target.querySelectorAll<HTMLButtonElement>('.inspector__tab')[1]!.click();
    flushSync();
    deliver(logStreams[0]!, 'closed', { error: 'stream request refused: 401', status: 401 });

    expect(statusLine(target)).toContain('Session expired');

    unmount(exports);
  });

  it('keeps the plain text for a stop that carries no status', async () => {
    const { target, exports } = renderPanel();
    await settle();
    deliver(tapStreams[0]!, 'closed', { error: 'boom' });

    expect(statusLine(target)).toContain('Stopped: boom');
    expect(recreateButton(target)).toBeNull();

    unmount(exports);
  });
});

// eieio-m9s.43: the login gate is raised where a `401` is recognised, not where a call is made.
//
// The behaviour these pin is DESIGNER §6's normative rule — "a `401` reopens the login gate
// wherever it appears, streams included" — and the *reason* they exist as their own suite is
// that "wherever" used to be enforced by hand. `client.ts` wrapped 23 calls in
// `watchSession(...)` and the two stream calls in a second adapter, `watchStreamSession(...)`;
// the gate was correct for exactly the calls someone had remembered to wrap, and every suite
// that covered it drove a *particular wrapped function*. A test like that cannot fail for the
// bug that actually threatened this seam — a new function, or a new transport, added without
// the wrapper.
//
// So these tests deliberately do **not** go through `client.ts`. They exercise the three
// modules that recognise a `401` directly, because that is where the rule now lives, and the
// end-to-end suites in `backend.test.ts` and `proxy.test.ts` (which do go through `client.ts`,
// unchanged) prove the two halves still meet.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { notifySessionRequired, onSessionRequired } from './session';
import { SessionRequiredError, listSystems } from './backend';
import { ProxyUnauthorizedError } from './proxy';
import { connectSse } from './sse';

/** Subscribes for the duration of one test and reports whether the gate was raised. */
function watchGate(): { raised: () => number; stop: () => void } {
  let count = 0;
  const unsubscribe = onSessionRequired(() => {
    count += 1;
  });
  return { raised: () => count, stop: unsubscribe };
}

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });
}

/** A response that never yields a body — every `connectSse` test here refuses before the body
 *  would matter, and a stream that opened would hang the test rather than assert anything. */
function refusal(status: number): Response {
  return jsonResponse(status, { error: 'unauthorized', message: 'no live session' });
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('session.ts — the signal itself', () => {
  it('notifies every subscriber, and stops after unsubscribe', () => {
    const seen: string[] = [];
    const stopA = onSessionRequired(() => seen.push('a'));
    const stopB = onSessionRequired(() => seen.push('b'));

    notifySessionRequired();
    expect(seen).toEqual(['a', 'b']);

    stopA();
    notifySessionRequired();
    expect(seen).toEqual(['a', 'b', 'b']);
    stopB();

    notifySessionRequired();
    expect(seen).toEqual(['a', 'b', 'b']);
  });

  it('a listener that throws neither propagates nor starves the listeners after it', () => {
    // This matters more than a defensive-coding nicety, because the notifier is now called
    // from inside `SessionRequiredError`'s constructor and from inside `sse.ts`'s fetch loop.
    // A subscriber that threw would otherwise replace the `401` with its own error at the one
    // moment the app most needs to say "log in again" — `App.svelte`'s subscriber is a Svelte
    // `$state` assignment, which is not a thing this seam can promise never throws.
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
    const seen: string[] = [];
    const stopA = onSessionRequired(() => {
      throw new Error('a subscriber fell over');
    });
    const stopB = onSessionRequired(() => seen.push('b'));

    try {
      expect(() => notifySessionRequired()).not.toThrow();
      expect(seen).toEqual(['b']);
      expect(spy).toHaveBeenCalled();
    } finally {
      stopA();
      stopB();
    }
  });
});

describe('the gate is raised by recognising a 401, not by wrapping a call', () => {
  // --- Prove it can fail: drop `notifySessionRequired()` from `SessionRequiredError`'s
  // constructor in `backend.ts` and every assertion in this describe block that goes through a
  // Designer route fails at `expect(gate.raised()).toBe(1)`. Under the old shape — the wrapper
  // in `client.ts` — the first test here failed *while the app was correct*, and the third
  // passed while the app was broken, which is the whole reason the seam moved.

  it('a 401 on a Designer route raises it from backend.ts, with no client.ts in the picture', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(refusal(401)));
    const gate = watchGate();
    try {
      await expect(listSystems()).rejects.toBeInstanceOf(SessionRequiredError);
      expect(gate.raised()).toBe(1);
    } finally {
      gate.stop();
    }
  });

  it('a function this seam has never heard of raises it too, just by throwing the error', () => {
    // The acceptance criterion, stated as a test: "adding a new API function cannot forget the
    // gate." This stands in for that new function. It does not exist in `backend.ts`, nothing
    // wraps it, `client.ts` does not dispatch to it — and the gate goes up anyway, because the
    // only sane thing a new `/api` caller can do with a `401` is throw the type every caller
    // already switches on, and throwing it is raising the gate.
    const gate = watchGate();
    try {
      const tomorrowsApiCall = () => {
        throw new SessionRequiredError('/api/whatever-lands-next');
      };
      expect(tomorrowsApiCall).toThrow(SessionRequiredError);
      expect(gate.raised()).toBe(1);
    } finally {
      gate.stop();
    }
  });

  it('a proxied 401 raises it from proxy.ts, for the same reason', () => {
    // `proxy.ts`'s module doc: a proxied 401 is ambiguous between "logged out of the Designer"
    // and "this node's stored bearer token went stale", and nothing in either wire contract
    // distinguishes them (DAEMON §9.2 forbids parsing `message`). Both want a login prompt.
    const gate = watchGate();
    try {
      expect(new ProxyUnauthorizedError('5', '/api/nodes/5/daemon/services', 'no live session').name).toBe(
        'ProxyUnauthorizedError',
      );
      expect(gate.raised()).toBe(1);
    } finally {
      gate.stop();
    }
  });
});

describe('the gate on a stream does not depend on the reconnect policy (eieio-m9s.43)', () => {
  // The fragility this bead names, pinned. eieio-m9s.39's fix made the gate fire only if all
  // three of these held: `isPermanentStreamStatus(401)` was true, `sse.ts` attached
  // `detail.status` to the terminal transition, and `client.ts` wrapped the handlers. The first
  // two are decisions about *reconnection* — whether it is worth asking again, and what to
  // render — and the third is a decision about *this call site*. None of them is about
  // sessions, and any of them could be changed for a good reason by someone who never thought
  // about the login gate at all.
  //
  // `connectSse` now calls `notifySessionRequired()` the moment it sees a `401`, above and
  // independent of the permanent/transient branch. These tests drive the transport directly,
  // with no `client.ts` and no handler wrapper anywhere, which is the only way to show that.

  it('a 401 raises the gate from connectSse itself, with the handlers passed through untouched', async () => {
    const gate = watchGate();
    const statuses: Array<{ status: string; detail?: { status?: number } }> = [];
    try {
      const fetchImpl = vi.fn().mockResolvedValue(refusal(401));
      const handle = connectSse(
        'http://node/taps/1/stream',
        { onFrame: () => {}, onStatus: (status, detail) => statuses.push({ status, detail }) },
        { fetchImpl, wait: async () => {} },
      );
      await vi.waitFor(() => expect(gate.raised()).toBe(1));
      // Still the eieio-m9s.39 behaviour DESIGNER §6 makes normative, unchanged: closed once,
      // status attached, no retry. The gate is additional to that, never a substitute for it.
      expect(statuses.map((s) => s.status)).toEqual(['connecting', 'closed']);
      expect(statuses.find((s) => s.status === 'closed')?.detail?.status).toBe(401);
      expect(fetchImpl).toHaveBeenCalledTimes(1);
      handle.close();
    } finally {
      gate.stop();
    }
  });

  it('no other status raises it: 403 and 404 end the stream, 429 and 503 retry, the gate stays down', async () => {
    // One test over both sides of `isPermanentStreamStatus` on purpose. Whichever side of that
    // line a status falls, the gate's answer is the same and comes from the status alone — the
    // predicate is consulted for whether to reconnect and for nothing else.
    for (const status of [403, 404]) {
      const gate = watchGate();
      try {
        const fetchImpl = vi.fn().mockResolvedValue(refusal(status));
        connectSse(
          'http://node/taps/1/stream',
          { onFrame: () => {}, onStatus: () => {} },
          { fetchImpl, wait: async () => {} },
        );
        await vi.waitFor(() => expect(fetchImpl).toHaveBeenCalled());
        await new Promise((r) => setTimeout(r, 20));
        expect(gate.raised()).toBe(0);
      } finally {
        gate.stop();
      }
    }

    for (const status of [429, 503]) {
      const gate = watchGate();
      try {
        const fetchImpl = vi
          .fn()
          .mockResolvedValueOnce(refusal(status))
          .mockImplementation(() => new Promise<Response>(() => {}));
        const handle = connectSse(
          'http://node/logs/stream',
          { onFrame: () => {}, onStatus: () => {} },
          { fetchImpl, wait: async () => {} },
        );
        await vi.waitFor(() => expect(fetchImpl.mock.calls.length).toBeGreaterThanOrEqual(2));
        expect(gate.raised()).toBe(0);
        handle.close();
      } finally {
        gate.stop();
      }
    }
  });
});

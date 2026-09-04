// The login gate's one signal (DESIGNER §3.1, §6), and nothing else.
//
// DESIGNER §3.1's gate is one password field and `POST /api/session`. §6 makes the rule
// normative for streams as well: **a `401` reopens the login gate wherever it appears.**
// "Wherever" is the whole point of this module existing on its own — it is the fact that
// decides where the notifier belongs.
//
// --- Why this is not in `client.ts` (eieio-m9s.43) ------------------------------------------
//
// It was, and the mechanism was right — one listener set, one signal — but the *seam* sat one
// layer too high. `client.ts` wrapped each call: `watchSession(...)` around 23 promise-shaped
// calls, plus a second adapter, `watchStreamSession(...)`, around the two stream-shaped ones,
// because a `StreamHandle` is returned synchronously and has no rejection for a wrapper to sit
// in front of — 25 wrappings and one adapter per call shape. Two consequences, both structural
// rather than cosmetic:
//
//   1. **Every future API function had to remember to wrap.** The gate was correct only for
//      the calls someone had wrapped, and a call added without the wrapper failed silently —
//      it rejected, nothing reopened the gate, and the app showed "Failed to load" for a dead
//      session. Nothing could catch that but review.
//   2. **A third transport shape needed a third adapter.** A WebSocket, a download, an
//      `EventSource` — each returns something else again, so each would need its own wrapper
//      before its `401` could raise the gate.
//
// And the stream adapter was fragile in a way worth spelling out, because it is the shape of
// bug this module exists to make unconstructible: the gate fired only if
// `isPermanentStreamStatus(401)` was true **and** `sse.ts` attached `detail.status` **and**
// `client.ts` wrapped the handlers. Three independent conditions for one rule; widen or narrow
// the permanent/transient carve-outs in `sse.ts` — a decision about *reconnection*, made for
// reasons that have nothing to do with sessions — and the login gate stops working, with
// nothing to notice.
//
// A `401` is recognised in exactly three places, and each of them already knows:
//
//   - `backend.ts` constructs `SessionRequiredError` — this Designer's own `/api` routes.
//   - `proxy.ts` constructs `ProxyUnauthorizedError` — the catch-all node proxy.
//   - `sse.ts` sees `response.status === 401` on a stream.
//
// Those three call {@link notifySessionRequired} where they already do the recognising, so the
// gate is a property of *recognising a 401* rather than of *remembering to wrap a call*. A
// fourth transport shape raises the gate by calling the notifier at the one point it already
// has to notice the status, and needs no adapter at all.
//
// This is a plain listener set and not a Svelte store on purpose: it is imported by
// `backend.test.ts`, `proxy.test.ts` and the `mock-*.test.ts` suites, none of which run inside
// a Svelte component, and a store would make the whole API seam depend on `svelte` for no
// reason those tests need. `App.svelte` — the one place in this SPA that owns "is the gate up"
// — subscribes once at the top of its own script and turns this into `$state` itself.
//
// This module deliberately has **no imports**. It sits below `backend.ts`, `proxy.ts` and
// `sse.ts`, all three of which import it, so anything it depended on would be a cycle waiting
// to happen through one of them.

type SessionRequiredListener = () => void;

const listeners = new Set<SessionRequiredListener>();

/**
 * Calls `listener` the next time (and every time) something in this seam discovers there is no
 * live session. Returns an unsubscribe function. `App.svelte`'s gate is the only intended
 * subscriber — see this module's own note above on why it is a plain set rather than a store.
 */
export function onSessionRequired(listener: SessionRequiredListener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/**
 * Reports that a `401` was just recognised: the Designer's session is gone, or a node's stored
 * bearer token is (DAEMON §9.1) — nothing in either wire contract distinguishes those, and both
 * want the same login prompt on screen (DESIGNER §6, `proxy.ts`'s module doc).
 *
 * Call this **where the `401` is recognised**, never at a call site — that is the whole of this
 * module's argument, above. It is deliberately fire-and-forget: it returns nothing, it cannot
 * fail, and it never changes what the caller then does with the `401` (every one of the three
 * callers goes on to throw or close exactly as it did before). That is what makes it safe to
 * put on the recognising line rather than around it.
 *
 * A listener that throws is caught and reported, not propagated. The notifier is called from
 * inside error construction and from inside `sse.ts`'s fetch loop, and in both a listener's own
 * failure replacing the `401` — an `App.svelte` `$state` assignment throwing, say — would
 * substitute a confusing error for a clear one at the exact moment the app most needs to say
 * "log in again". Every listener is offered the signal regardless of what an earlier one did.
 */
export function notifySessionRequired(): void {
  for (const listener of listeners) {
    try {
      listener();
    } catch (error) {
      // Nothing here can do anything about it, and the 401 must still surface intact.
      console.error('onSessionRequired listener threw', error);
    }
  }
}

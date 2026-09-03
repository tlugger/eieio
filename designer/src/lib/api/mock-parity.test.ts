// eieio-m9s.15: `mock.ts` is a THIRD, unchecked source of wire shapes.
//
// `schema-parity.test.ts` (eieio-m9s.11, eieio-m9s.13) holds `types.ts` against the daemon's
// live `utoipa`/`Observation`/`What` schemas. It does not, and structurally cannot, see
// `mock.ts` — which manufactures the SSE frames and API responses the Designer is developed and
// demoed against. So the mock is a third statement of every wire shape, and until this file
// nothing compared it to the other two. It was wrong about two SSE fields `schema-parity.test.ts`
// had just finished fixing on the *decoder* side (`span` as `{start, end}` instead of the wire's
// `"start..end"` string; `property`, a fabricated name, instead of the wire's numeric `prop`),
// found only by a person reading both files side by side. This file is that comparison, made
// automatic, so the next drift fails a test instead of waiting for another manual read.
//
// This is a **sibling** of `schema-parity.test.ts`, not an extension of it — that file is not
// owned by this bead and is not touched here. Both read the same generated
// `daemon-response-shapes.json` (`crates/cli/tests/response_shapes.rs`), and both regenerate it
// themselves in `beforeAll` for the same reason that file's own doc gives: `just ci` runs its
// stages in parallel background jobs with no ordering guarantee, so a check trusting a
// possibly-stale or possibly-absent generated file would be worse than the drift it exists to
// catch.
//
// # The mock legitimately emits subsets — so this is not a set-equality check
//
// DAEMON §9.6 and ABI §11 both make a *sometimes*-present field **absent, not null**: a log line
// from a daemon subsystem carries no real instance, a `signals` frame's `port` is only ever
// populated when the observation has one to report, and an `expr_failure` that is not per-signal
// carries no `signal`. A mock that faithfully reproduces that has to be allowed to omit fields
// too, so this file asserts two separate, narrower rules instead of "the mock's fields equal the
// daemon's fields":
//
// 1. **No invented fields.** Every key a mock response or SSE frame carries must be a field name
//    the daemon's own schema declares for that shape. This is the rule the two historical bugs
//    above both violate under a *name* reading (see the type-vs-name caveat below for why `span`
//    needs a second look) and the rule proof 2 and proof 3 (this file's own negative-proof
//    transcripts, in the final report) exercise directly.
// 2. **Every field the daemon *always* sends is present.** `crates/cli/tests/response_shapes.rs`
//    was extended by this bead (`required_of`/`sse_required`, see that file's own module doc) to
//    also emit each schema's and each SSE event's **required** field names — `Schema::Object
//    .required`, i.e. not `Option` on the Rust side — precisely so this rule has something
//    non-guessed to check against, rather than a hand-typed "these fields are always there" list
//    sitting beside this file, which would be exactly the third source of truth this whole
//    mechanism exists to prevent.
//
// # A deliberate scope decision: field *names*, not field *types*
//
// This file, like `schema-parity.test.ts` before it, compares field-name sets. It does **not**
// compare the JSON *type* of a field (string vs. object, say). This means a mock that emits
// `span` as `{ start, end }` instead of the wire's `"start..end"` string — the exact shape of the
// historical `a36f7a7` bug — passes rule 1 and rule 2 here: `span` is a real field name the
// daemon really does send, and it is present, so a name-and-presence check has nothing to object
// to. This is a genuine, acknowledged gap in this check's reach, not an oversight — see this
// file's own final report for proof 1's transcript, which demonstrates it directly rather than
// asserting it. Catching a wrong *type* would need this file (or `response_shapes.rs`) to carry
// each field's JSON type too, which is real design work `crates/cli/tests/response_shapes.rs`'s
// own `flatten` does not do today for any of its three consumers; adding it as a guess, rather
// than something the daemon's schema is asked for deliberately, is exactly the kind of
// unmeasured widening CLAUDE.md's prime directive warns against. Reported as follow-up rather
// than silently left uncovered.
//
// # How this file gets at what the mock actually emits
//
// `mock.ts` builds every SSE frame through its own local `sseFrame(event, data)` closures inside
// `streamTap`/`streamLogs`, with no exported seam. Rather than refactor `mock.ts` to add one — a
// bigger diff in a file another bead's fixtures (`mock.test.ts`, `mock-taps.test.ts`) already
// depend on, for a check that does not need it — this file drives the mock exactly the way
// `mock-taps.test.ts` already does (create a tap, open its stream, advance fake timers) and
// intercepts `./stream-events`'s `decodeTapFrame`/`decodeLogFrame` with `vi.mock`, capturing the
// raw `SseFrame` (`{event, data}`) each is called with *before* it decodes anything. That is the
// actual wire text the mock produced — `JSON.parse(frame.data)`'s own keys, not the decoded
// `TapStreamEvent`/`LogLineEvent` object's fields, which would already have papered over a
// `span`-shape bug by the time this file could look at it. Non-SSE responses (`listServices`,
// `getNodeInfo`, `getService`'s `.error`) are read directly — `mock.ts` returns plain JS objects
// for those rather than pre-serialized text, so this file round-trips each one through
// `JSON.stringify`/`JSON.parse` itself before flattening, which is what makes an `undefined`-
// valued key (`ServiceSummary.error` on a service with none) drop out the same way a real
// `fetch().json()` would see it drop, rather than reading as an invented field.
//
// # Coverage: which emitters this file reaches, and which it does not
//
// **Reached** (driven at least once, every captured frame/response checked against both rules):
// `listServices` (`ServiceSummary`, including a service whose `error` is populated — DAEMON §9's
// eieio-m9s.12 amendment), `getNodeInfo` (`NodeInfo`), `getService`'s `.error` and
// `getServiceErrors` (`ApiError`, eieio-m9s.18: both now read the *same* fixture value —
// `MockService.error` — that a real daemon would answer identically for `GET /services`'s
// listing, `GET /services/{s}` and `GET /services/{s}/errors`, so checking all three against one
// generated target is checking that they stay identical, not just that each is shaped right), and
// `streamTap`'s `signals`, `expr_failure`, `discarded` and `lagged` frames, and `streamLogs`'s
// `log` frame. `discarded` (eieio-m9s.17) was the one of the five SSE event names `mock.ts` never
// dispatched anywhere until that bead — see `mock.ts`'s own comment on the `tick % 7 === 0`
// branch in `streamTap`'s `tickOnce` for which `DiscardReason` it manufactures and why.
//
// **Not reached, and why:**
// - **`getService`'s own top-level shape (`ServiceDefinition`).** Not one of
//   `response_shapes.rs`'s targets, and deliberately so — see that file's module doc: the daemon
//   answers `{name, state, definition, autostart, error?}` and this shell's `ServiceDefinition`
//   is the *parsed* model built from that text, so a field-set diff between them would compare
//   two different kinds of thing. Its `.error` sub-object *is* reached, above, because that one
//   really is an `ApiError`.
// - **`createTap`/`listTaps` (`Tap`, eieio-m9s.17).** `response_shapes.rs` gained a `Tap` target
//   this bead, and `schema-parity.test.ts`'s `PAIRS` checks `TapSummary` against it — but *this*
//   file's mechanism cannot: [`wireFields`] reads a value's own runtime JSON keys, with no
//   equivalent of `schema-parity.test.ts`'s `wireNameOf`/`@wire` tag to say "this key stands for a
//   differently-named wire field." `createTap`/`listTaps` return `{tap_id, ...}` directly — there
//   is no earlier, pre-rename wire representation to intercept the way `vi.mock('./stream-events')`
//   captures an SSE frame's raw JSON *before* `decodeTapFrame` renames anything, because nothing
//   here plays that decoder's role: `client.ts`'s own doc says a real backend swap means
//   *rewriting* `createTap`/`listTaps`' bodies, so the `id`→`tap_id` translation `TapSummary`'s
//   doc comment describes is work a real implementation would still have to do, not something the
//   current mock skips by mistake. Tried directly and confirmed to fail exactly this way (`tap_id`
//   reported as an invented field) before being reverted — see the final report for the
//   transcript. Teaching this file the same rename mechanism `schema-parity.test.ts` has is future
//   work, not a gap silently left uncovered here.
// - **`POST /taps`'s own `TapRequest` schema.** A *request* body (`{service, connection}`) — the
//   mock's `createTap` takes those as function parameters, not something it emits back, so there
//   is nothing of this shape to check.
// - **`serviceEdit`/`putService`'s own result shapes** (`ServiceEditResult`, `PutServiceResult`)
//   and **`listBlockManifests`/`listSystems`/`listNodes`** (`BlockManifest`, `SystemSummary`,
//   `NodeSummary`) — none of these has a daemon schema in `response_shapes.rs`'s target list, and
//   eieio-m9s.17 found real, distinct reasons each one still can't (see that file's module doc for
//   the full detail, including a correction: `crates/designer` is not unbuilt the way `CLAUDE.md`
//   says — it has real handlers for all five of these, this bead just isn't the one that owns
//   that crate). `serviceEdit`/`putService` are parsed client-side models regardless of whether
//   the backend exists; `listBlockManifests`' `BlockManifest` is a parsed model too, *and* even a
//   wire mirror of `CachedBlock` alone could not check its `manifest` field, which has no
//   `ToSchema` derive to generate one from; `listSystems`/`listNodes` name real
//   `crates/designer/src/api/systems.rs`/`nodes.rs` handlers with no `utoipa` anywhere in that
//   crate to generate a schema from — and reading them by hand turned up real drift
//   (`response_shapes.rs`'s doc has the specifics) this file cannot check for the same reason.
//   None of these is a gap this file could have closed by trying harder against what it already
//   reads — each is reported in `response_shapes.rs`'s module doc, in place, rather than repeated
//   here as a second copy of the same reasoning.

import { execSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { beforeAll, describe, expect, it, vi } from 'vitest';
import type { SseFrame } from './sse';

const REPO_ROOT = path.resolve(process.cwd(), '..');
const GENERATED_PATH = path.resolve(process.cwd(), 'src/lib/api/__generated__/daemon-response-shapes.json');

let daemonShapes: Record<string, string[]>;
let daemonRequired: Record<string, string[]>;
let daemonSse: Record<string, string[]>;
let daemonSseRequired: Record<string, string[]>;

// Self-sufficient the same way `schema-parity.test.ts` is, and for the same reason (see that
// file's own `beforeAll` doc): `just ci`'s stages run in parallel, so nothing guarantees the
// generated file exists or is fresh by the time this suite runs.
beforeAll(() => {
  // Skipped when the harness already generated it (`just ci`'s `shapes` recipe sets this).
  // Shelling out to cargo here while the `test` stage holds the target-directory lock is
  // what timed this hook out on CI; regenerating remains the default so a bare `npm test`
  // is still self-sufficient and never compares against a stale file.
  if (!process.env.EIO_SHAPES_PREGENERATED) {
    execSync('cargo test -p eio-cli --test response_shapes', {
      cwd: REPO_ROOT,
      stdio: 'pipe',
    });
  }
  const parsed = JSON.parse(readFileSync(GENERATED_PATH, 'utf-8')) as Record<string, unknown>;
  daemonShapes = parsed as Record<string, string[]>;
  daemonRequired = (parsed.required as Record<string, string[]> | undefined) ?? {};
  daemonSse = (parsed.sse as Record<string, string[]> | undefined) ?? {};
  daemonSseRequired = (parsed.sseRequired as Record<string, string[]> | undefined) ?? {};
}, 120_000);

// --- Capturing what `mock.ts` actually puts on the wire, before anything decodes it -----------
//
// `vi.mock` replaces `./stream-events` for every importer sharing this module graph, `mock.ts`
// included, since both resolve the same specifier to the same file. `vi.hoisted` is required
// because `vi.mock`'s factory itself is hoisted above this file's other top-level statements —
// a plain `const` declared after it would not exist yet when the factory runs.
const captured = vi.hoisted(() => ({
  tap: [] as { event: string; data: string }[],
  log: [] as { event: string; data: string }[],
}));

vi.mock('./stream-events', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./stream-events')>();
  return {
    decodeTapFrame: (frame: SseFrame) => {
      captured.tap.push({ event: frame.event, data: frame.data });
      return actual.decodeTapFrame(frame);
    },
    decodeLogFrame: (frame: SseFrame) => {
      captured.log.push({ event: frame.event, data: frame.data });
      return actual.decodeLogFrame(frame);
    },
  };
});

// Imported after the `vi.mock` above only for readability — `vi.mock` calls are hoisted above
// every import in this file regardless of source position, so this already sees the mocked
// `./stream-events` no matter where it is written.
import { createTap, getNodeInfo, getService, getServiceErrors, listServices, streamLogs, streamTap } from './mock';

// --- Rule 1 / rule 2, applied to one flattened field set ---------------------------------------

/** Recursively dots a parsed JSON value's own keys, the same shape `crates/cli/tests/
 * response_shapes.rs`'s `flatten` produces on the daemon side (and `schema-parity.test.ts`'s
 * `flattenInterface` produces from `types.ts`'s AST): every property name, dotted for a nested
 * plain object, so a field that moved *inside* another object (`ServiceSummary.error.detail`) is
 * compared at the path it actually lives at. Does not recurse into an array — `NodeInfo
 * .capabilities` is a leaf the same way both of those flatteners already treat it, and nothing
 * this file exercises nests an array of objects.
 *
 * Also does not recurse into a value whose path the daemon's own schema declares no *children*
 * of — `ApiError.detail` (`Option<serde_json::Value>`, DAEMON §9.2's deliberately opaque
 * "per-slug structured data") is a real, allowed field name with no declared shape underneath
 * it, and `crates/cli/tests/response_shapes.rs`'s `flatten` never descends into it either
 * (`Schema::Object` with no declared `properties`), so nothing here should invent a shape it
 * doesn't have. This was found the hard way: an earlier version of this function recursed
 * unconditionally and reported `error.detail.instance`/`error.detail.block` (the attic-fan
 * fixture's own `detail` payload) as invented fields, which they are not — the daemon's schema
 * simply never described *any* shape for `detail`, so no shape a mock puts there can be wrong by
 * this check's own rules. Deriving "stop here" from `allowed` rather than hard-coding the field
 * name `detail` is what keeps this general rather than a second exception list. */
function flattenValue(value: unknown, prefix: string, depth: number, allowed: ReadonlySet<string>, out: Set<string>): void {
  if (depth <= 0 || value === null || typeof value !== 'object' || Array.isArray(value)) return;
  const declaresChildren = prefix === '' || [...allowed].some((field) => field.startsWith(`${prefix}.`));
  if (!declaresChildren) return;
  for (const [key, child] of Object.entries(value as Record<string, unknown>)) {
    const path = prefix ? `${prefix}.${key}` : key;
    out.add(path);
    flattenValue(child, path, depth - 1, allowed, out);
  }
}

/** The flat, dotted field set a JSON value would actually carry over the wire, judged against
 * `allowed` (the daemon's own field-name set for this shape — see [`flattenValue`] for why it is
 * needed during flattening itself, not only afterward). Round-tripped through
 * `JSON.stringify`/`JSON.parse` first: `mock.ts`'s functions hand back plain JS objects rather
 * than pre-serialized text (unlike its SSE frames, already real `JSON.stringify` output inside
 * `sseFrame`), and a key whose value is `undefined` (`ServiceSummary.error` on a service with
 * none) is dropped by real serialization the same way a `fetch().json()` caller would never see
 * it — reading it via a bare `Object.keys()` instead would make that legitimate absence look
 * like an invented field with no value. */
function wireFields(value: unknown, allowed: readonly string[]): Set<string> {
  const out = new Set<string>();
  const serialized: unknown = JSON.parse(JSON.stringify(value));
  flattenValue(serialized, '', 4, new Set(allowed), out);
  return out;
}

/** Rule 1: every field `label` carries must be one the daemon's own schema names for this shape.
 * Not set equality — `allowed` may (and usually does) contain fields `label` legitimately omits. */
function assertNoInventedFields(label: string, fields: ReadonlySet<string>, allowed: readonly string[]): void {
  const allowedSet = new Set(allowed);
  const invented = [...fields].filter((field) => !allowedSet.has(field)).sort();
  expect(invented, `${label} invents field(s) the daemon never sends: ${JSON.stringify(invented)}`).toEqual([]);
}

/** Rule 2: every field the daemon's schema marks as always-present must actually be present in
 * `label`. `required` comes from `crates/cli/tests/response_shapes.rs`'s `required`/`sseRequired`
 * (`Schema::Object.required` — non-`Option` fields only), never a second hand-typed list here. */
function assertRequiredFieldsPresent(label: string, fields: ReadonlySet<string>, required: readonly string[]): void {
  expect(required.length, `no required-field list for ${label} — check response_shapes.rs's target/event list`).toBeGreaterThan(0);
  const missing = required.filter((field) => !fields.has(field)).sort();
  expect(missing, `${label} omits field(s) the daemon always sends: ${JSON.stringify(missing)}`).toEqual([]);
}

// --- Non-SSE responses: listServices (ServiceSummary), getNodeInfo (NodeInfo), --------------
// --- and getService's own structured error (ApiError) ---------------------------------------

describe('mock.ts response shapes vs. the daemon\'s own schemas (eieio-m9s.15)', () => {
  it('getNodeInfo matches NodeInfo: no invented field, every daemon-required field present', async () => {
    const info = await getNodeInfo('node-porch');
    const fields = wireFields(info, daemonShapes.NodeInfo ?? []);
    assertNoInventedFields('getNodeInfo("node-porch")', fields, daemonShapes.NodeInfo ?? []);
    assertRequiredFieldsPresent('getNodeInfo("node-porch")', fields, daemonRequired.NodeInfo ?? []);
  });

  it('listServices matches ServiceSummary for every fixture service, including one with a structured error', async () => {
    for (const nodeId of ['node-porch', 'node-attic', 'node-closet']) {
      const services = await listServices(nodeId);
      expect(services.length, `no fixture services for "${nodeId}" — nothing to check`).toBeGreaterThan(0);
      for (const service of services) {
        const fields = wireFields(service, daemonShapes.ServiceSummary ?? []);
        const label = `listServices("${nodeId}")'s "${service.name}"`;
        assertNoInventedFields(label, fields, daemonShapes.ServiceSummary ?? []);
        assertRequiredFieldsPresent(label, fields, daemonRequired.ServiceSummary ?? []);
      }
    }
  });

  it('an errored service\'s structured error, on the listing, the detail and /errors, matches ApiError', async () => {
    const listed = await listServices('node-attic');
    const atticFan = listed.find((s) => s.name === 'attic-fan');
    expect(atticFan?.error, 'the attic-fan fixture is supposed to be errored with a structured reason').toBeDefined();

    const detail = await getService('node-attic', 'attic-fan');
    expect(detail.error).toBeDefined();

    // eieio-m9s.18: `getServiceErrors` used to answer a fabricated `{service, errors: [...]}`
    // wrapper unrelated to `ApiError` — it now answers the same value `.error` above does, so
    // this is checked here rather than only in `mock.test.ts`'s behavioural suite, which does
    // not compare against the live daemon schema at all.
    const viaErrorsEndpoint = await getServiceErrors('node-attic', 'attic-fan');

    for (const [label, error] of [
      [`listServices("node-attic")'s "attic-fan".error`, atticFan?.error],
      [`getService("node-attic", "attic-fan").error`, detail.error],
      [`getServiceErrors("node-attic", "attic-fan")`, viaErrorsEndpoint],
    ] as const) {
      const fields = wireFields(error, daemonShapes.ApiError ?? []);
      assertNoInventedFields(label, fields, daemonShapes.ApiError ?? []);
      assertRequiredFieldsPresent(label, fields, daemonRequired.ApiError ?? []);
    }
  });
});

// --- SSE frames: streamTap's signals/expr_failure/lagged, streamLogs's log ---------------------

describe('mock.ts SSE frames vs. the daemon\'s own Observation/What wire shapes (eieio-m9s.15)', () => {
  const reachedEvents = new Set<string>();

  beforeAll(async () => {
    // `createTap` goes through the mock's own `delay()`, on real timers — the same ordering
    // `mock-taps.test.ts` already uses, and for the same reason (fake timers installed before
    // this resolves would hang it forever).
    const tap = await createTap('node-porch', 'kitchen', 'b7k2.out -> f3m9.in');
    vi.useFakeTimers();
    const tapHandle = streamTap('node-porch', tap.tap_id, { onEvent: () => {}, onStatus: () => {} });
    // Long enough to cross the mock's own scripted disconnect/resume (8500ms + 2500ms after
    // "open") and reach a tick count divisible by 5 (signals + expr_failure, the missing-field
    // case), 7 (discarded) and 11 (lagged) at least once each — proven empirically here by
    // asserting `reachedEvents` below rather than trusted from the arithmetic alone.
    for (let i = 0; i < 20; i++) {
      await vi.advanceTimersByTimeAsync(1000);
    }
    tapHandle.close();
    vi.useRealTimers();

    // Fake timers installed *before* `streamLogs` schedules its own opening `setTimeout` — the
    // reverse order silently starves it: a real timer set before the switch keeps running on the
    // wall clock while `advanceTimersByTimeAsync` only advances the fake one, so it fires (if at
    // all) long after `logHandle.close()` already tore the stream down. Found by running this
    // file and getting zero captured `log` frames despite the advance below.
    vi.useFakeTimers();
    const logHandle = streamLogs('node-porch', {}, { onEvent: () => {}, onStatus: () => {} });
    // The five-line synchronous backlog fires on `streamLogs`'s own 100ms open timer; a couple
    // of live ticks past that (1100ms apart) are just insurance.
    await vi.advanceTimersByTimeAsync(2500);
    logHandle.close();
    vi.useRealTimers();

    for (const frame of [...captured.tap, ...captured.log]) reachedEvents.add(frame.event);
  }, 30_000);

  it('reaches signals, expr_failure, discarded and lagged on the tap stream, and log on the log stream', () => {
    // eieio-m9s.17: `discarded` used to be the one SSE event name `mock.ts` never dispatched
    // anywhere; `mock.ts`'s `streamTap` now manufactures one on the same tap connection, so this
    // is the proof it is genuinely reachable rather than only present in the source.
    expect([...reachedEvents].sort()).toEqual(['discarded', 'expr_failure', 'lagged', 'log', 'signals']);
  });

  for (const event of ['signals', 'expr_failure', 'discarded', 'lagged'] as const) {
    it(`every captured "${event}" tap frame: no invented field, every daemon-required field present`, () => {
      const frames = captured.tap.filter((frame) => frame.event === event);
      expect(frames.length, `no "${event}" frame was captured`).toBeGreaterThan(0);
      for (const frame of frames) {
        const payload = JSON.parse(frame.data) as unknown;
        const fields = wireFields(payload, daemonSse[event] ?? []);
        assertNoInventedFields(`a mock "${event}" frame`, fields, daemonSse[event] ?? []);
        assertRequiredFieldsPresent(`a mock "${event}" frame`, fields, daemonSseRequired[event] ?? []);
      }
    });
  }

  it('every captured "log" frame: no invented field, every daemon-required field present', () => {
    const frames = captured.log.filter((frame) => frame.event === 'log');
    expect(frames.length, 'no "log" frame was captured').toBeGreaterThan(0);
    for (const frame of frames) {
      const payload = JSON.parse(frame.data) as unknown;
      const fields = wireFields(payload, daemonSse.log ?? []);
      assertNoInventedFields('a mock "log" frame', fields, daemonSse.log ?? []);
      assertRequiredFieldsPresent('a mock "log" frame', fields, daemonSseRequired.log ?? []);
    }
  });
});

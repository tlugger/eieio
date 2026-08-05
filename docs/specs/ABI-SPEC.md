# Block ABI Specification

**Status:** Draft 1 **Depends on:** SCOPE.md (decision record). OPEN items referenced here are tracked there, not re-litigated here.

This document specifies the binary interface between a **host** (daemon-class runtime on Linux-class devices; leaf-class runtime on MCUs) and a **guest** (a block compiled to a core WASM module). It is the contract that the daemon, the leaf runtime, the block SDK, the registry manifest, and the conformance suite are all built against.

The key words MUST, MUST NOT, SHOULD, and MAY are used as in RFC 2119.

---

## 1. Design invariants

These follow from SCOPE §3.2–3.5 and are restated here because every rule below derives from one of them:

1. **Core WASM modules only.** Target `wasm32-unknown-unknown`. No WASI, no component model, no threads, no multi-value returns, no reference types beyond MVP. A module MUST validate against WASM MVP (+ nothing) so that wasmtime, WAMR, and wasm3 are all viable hosts.
2. **Single-threaded actor model.** One WASM instance per block-instance-in-a-service. The host serializes all calls into an instance. The host MUST NOT call into a guest that is mid-call. Guest→host calls MUST NOT re-enter the guest.
3. **Copies, not shared references.** Every boundary crossing copies bytes between host memory and guest linear memory. No pointers outlive the call that carries them unless this spec says otherwise.
4. **CBOR everywhere.** All structured payloads crossing the boundary are CBOR (SCOPE §3.4). Same encoding on the wire and across the ABI.
5. **The import section is the capability system.** A module's imports are cross-checked against its manifest at load time. A module importing functions outside the `eio:*` namespaces MUST be rejected.
6. **Traps are death.** Any WASM trap invalidates the instance. Recoverable conditions are status codes.

---

## 2. Terminology

|Term|Meaning|
|---|---|
|Host|The runtime embedding the WASM engine (daemon or leaf runtime)|
|Guest / block|A WASM module implementing this ABI|
|Block instance|One instantiation of a block within one service, with its own linear memory, state, and configuration|
|Callback|Any host→guest call after instantiation (`configure`, `start`, `process_signals`, `on_*`, `stop`)|
|Signal|One CBOR map (dict-shaped, schemaless)|
|Batch|An ordered sequence of signals; the unit of delivery and emission, encoded as a CBOR array of maps|
|Port|A named input or output terminal of a block, addressed at runtime by a `u32` index|
|Property|A named, typed configuration value of a block instance, always defined by an expression (SCOPE §3.5 and §4-amendment: option (b), per-signal pull model)|

---

## 3. Numeric conventions

- Pointers and lengths are `i32`, interpreted as unsigned offsets into the guest's linear memory. `(ptr, len)` pairs denote a byte range that MUST lie within linear memory; out-of-range access by the host is a host bug, by the guest is a trap.
- Identifiers (`prop_id`, port indices, `timer_id`, `req_id`, `watch_id`) are `u32` carried as `i32`.
- Status/size returns are `i32` (see §8).
- Reserved sentinel values:

|Constant|Value|Meaning|
|---|---|---|
|`SIGNAL_NONE`|`0xFFFF_FFFF`|No signal context (property evaluation outside `process_signals`)|
|`PORT_ERR`|`0xFFFF_FFFE`|The reserved error output port (§6.4)|

---

## 4. Module layout

### 4.1 Required exports

|Export|Signature|Purpose|
|---|---|---|
|`memory`|linear memory|The instance's memory; host reads/writes payloads here|
|`eio_abi_version`|`() -> i32`|Packed ABI version (§12)|
|`eio_alloc`|`(size: i32) -> i32`|Guest allocator; returns ptr or 0 on failure|
|`eio_free`|`(ptr: i32, size: i32) -> ()`|Releases a `eio_alloc` allocation|
|`eio_configure`|`(ptr: i32, len: i32) -> i32`|Receives the instance descriptor (§5.2)|
|`eio_start`|`() -> i32`|Transition to running|
|`eio_stop`|`() -> i32`|Transition to stopped|
|`eio_process_signals`|`(input_port: i32, ptr: i32, len: i32) -> i32`|Deliver a batch on an input port|

### 4.2 Optional exports (callbacks)

Present only if the block uses the corresponding capability. The host MUST verify that a module importing `eio:timer` exports `eio_on_timer`, etc., at load time.

|Export|Signature|Paired capability|
|---|---|---|
|`eio_on_timer`|`(timer_id: i32) -> i32`|`timer`|
|`eio_on_gpio`|`(watch_id: i32, value: i32) -> i32`|`gpio`|
|`eio_on_http`|`(req_id: i32, status: i32, ptr: i32, len: i32) -> i32`|`http`|

### 4.3 Imports

All imports MUST be from `eio:*` namespaces (§7). Anything else fails validation. The set of imported namespaces MUST be a subset of the capabilities declared in the manifest (§11); the import section is authoritative, the manifest is advisory, and a mismatch in either direction where imports exceed manifest MUST be a load-time rejection.

### 4.4 Custom section

A module SHOULD embed its manifest as a custom section named `eio:manifest` (UTF-8 JSON, identical to the registry manifest). A `.wasm` file is then self-describing without registry metadata. If both are present the registry manifest and embedded manifest MUST be identical; hosts MAY reject on mismatch.

---

## 5. Lifecycle

### 5.1 State machine

```
instantiate → CONFIGURED → RUNNING → STOPPED
                  ↑            |
                  └── (restart = new instance; no re-start of a stopped instance)
trap (any state) → DEAD
```

1. **Instantiate.** Host creates the instance, validates exports/imports/ABI version. No guest code runs except start-function-free module init (a module MUST NOT rely on a WASM start function).
2. **`eio_configure(ptr, len)`.** Host allocates via `eio_alloc`, writes the instance descriptor, calls configure, guest frees. Guest MAY read properties (with `SIGNAL_NONE`), allocate internal state, and validate configuration. Non-zero return = configuration rejection; instance is discarded and the error surfaced to the deployer.
3. **`eio_start()`.** Guest MAY arm timers, register GPIO watches, emit initial signals. After a zero return, the host begins delivering batches.
4. **Running.** Host invokes callbacks, serialized, in host-determined order. The host SHOULD deliver batches on one input port in arrival order; ordering across ports and across instances is unspecified (delivery guarantees are OPEN, SCOPE §3.4).
5. **`eio_stop()`.** Host cancels outstanding timers/watches/requests after stop returns. Guest SHOULD flush state via `eio:state` before returning. A stopped instance is never restarted; service restart creates fresh instances (SCOPE §3.13 hot-reload posture).
6. **DEAD.** Any trap, fuel exhaustion, or deadline violation. The host discards the instance and applies supervision policy (OPEN, SCOPE §3.13). Re-instantiation implies a fresh `eio_configure`; blocks MUST NOT assume linear-memory continuity across lives. Durable state goes through `eio:state`.

### 5.2 Instance descriptor

The CBOR document passed to `eio_configure`. Properties are NOT included (they are pulled via `prop`, §7.1). Fields:

```
{
  "instance_id": tstr,          ; unique within the service
  "block": tstr,                ; block ref (registry name)
  "inputs":  [tstr],            ; input port names; index in array = port index
  "outputs": [tstr],            ; output port names; index in array = port index
  "props":   [tstr],            ; property names; index in array = prop_id
  "limits": {
    "max_payload": uint,        ; largest (ptr,len) the host will accept or deliver
    "max_batch": uint           ; largest signal count per batch
  }
}
```

Port and property indices are fixed for the life of the instance. Blocks resolve names to indices once, in configure; all runtime calls use indices (MCUs do not hash strings per signal).

---

## 6. Signal flow

### 6.1 Delivery

Host: `eio_alloc(len)` → write CBOR batch → `eio_process_signals(input_port, ptr, len)` → guest processes → guest returns status → **guest owns the buffer and MUST `eio_free` it** (before or after returning; before the next callback at the latest).

### 6.2 Emission

Guest, inside any callback: encode CBOR batch into guest memory → `emit(output_port, ptr, len)` → host **copies out during the call** → guest frees its buffer whenever it likes after `emit` returns.

`emit` is **enqueue, not delivery** (§2-amendment from design discussion): the host buffers the batch and routes it after the current callback returns. Consequences the spec guarantees:

- Emitting N batches to M downstream instances cannot recurse into this instance or any other mid-call.
- Backpressure, fan-out duplication, cross-node publication, and signal tapping (SCOPE §3.12) are host concerns invisible to the guest.
- `emit` failure (queue full / payload too large) is a status code to the _emitter_, policy is host-defined (OPEN: backpressure, SCOPE §3.4).

A block MAY emit zero, one, or many batches per callback; timer-driven blocks (simulators) emit with no inbound batch at all.

### 6.3 Batch encoding

A batch is a CBOR array of CBOR maps. Keys are text strings. An empty batch (`[]`) is legal and MUST be delivered/routable like any other.

The value space is **exactly** the following, not a minimum: signed 64-bit integer, float64, text string, byte string, bool, null, array, map. Hosts and the expression engine MUST support all of them and MUST reject everything else. (EXPR §2 already states the value space is exactly this set; the closed reading is the normative one.) Integers are signed 64-bit: a CBOR integer outside `i64` is outside the data model, whichever major type carries it.

#### 6.3.1 Canonical form

There is **exactly one** valid encoding of any given batch. Encoders MUST emit it; decoders MUST reject anything else. Two independent host implementations have to agree byte for byte (§13), and a decoder that quietly normalised non-canonical input would let a divergent encoder ship unnoticed — so strictness here is what makes conformance testable at all.

For every input a decoder accepts, re-encoding the decoded batch MUST reproduce that input byte for byte.

1. **Definite lengths only.** Indefinite-length arrays, maps, text strings, and byte strings MUST be rejected.
2. **Preferred serialization.** Every integer value, and every length of major types 2–5, MUST use the shortest head that carries it (RFC 8949 §4.2.1).
3. **Integers** MUST lie within `i64`. Major type 0 above `i64::MAX` and major type 1 below `i64::MIN` MUST be rejected.
4. **Floats are `binary64` only.** Encoders MUST write a `binary64` (`0xfb`) head; `binary16` and `binary32` MUST be rejected. *This is a deliberate deviation from RFC 8949 §4.2.1's shortest-float rule:* the data model has exactly one float type, and shortest-float would make a value's encoded width depend on its magnitude.
5. **NaN and ±Infinity MUST be rejected** on decode. EXPR §2 forbids operations from *producing* them; refusing them on arrival makes "no NaN/inf escape" (EXPR §9) a property of the type rather than an obligation on every builtin, and keeps equality, ordering, and canonical rendering (EXPR §7.6, which pins no NaN spelling) total.
6. **Negative zero** is a distinct encoding and MUST be preserved. It is a finite value; rejecting or normalising it is not permitted.
7. **Map keys** MUST be text strings, MUST be unique, and MUST appear in ascending bytewise order **of their UTF-8 content**. *This is a deliberate deviation from RFC 8949 §4.2.1*, which orders keys by their encoded bytes and therefore sorts `"z"` (`0x617a`) before `"aa"` (`0x626161`). Ordering by content gives the platform a single ordering: the same one EXPR §2 exposes as map iteration order and EXPR §7.5's `(keys m)` returns. Duplicate keys are rejected rather than collapsed, because collapsing would re-encode to different bytes than arrived.
8. **Tags, `undefined`, and simple values** other than `false`/`true`/`null` MUST be rejected.
9. **Nesting depth** MUST be bounded, and exceeding the bound MUST be a decode error rather than a host crash. Decoding is naturally recursive, and at this boundary a stack overflow kills the *host*, which the "traps are death" rule (§1, §8) does nothing to contain. The bound is **host configuration**, like every other budget in the system (EXPR §9), subject to two constraints: it MUST be at least EXPR §9's `MAX_DEPTH` **floor**, and it MUST be at least that host's own configured expression `MAX_DEPTH` — otherwise an expression could construct a value the boundary then refuses. A leaf host running its expression engine near the floors therefore need not accept, and need not find stack for, the depth a daemon accepts.
10. **Trailing bytes** after the batch MUST be rejected. A concatenated or truncated payload is corruption, not a batch carrying extra data.
11. **Declared lengths are not allocation instructions.** A decoder MUST NOT pre-allocate on a declared collection length before the corresponding items are present: a nine-byte head can claim `u64::MAX` elements in a payload of ten bytes.

Rules 4 and 7 are the only two deviations from RFC 8949 §4.2.1's core deterministic encoding requirements; both are recorded here rather than being left to implementations to rediscover.

**Which** rule a host rejects under is diagnostic, not normative. Two implementations MUST agree on *whether* input is canonical; they need not agree on how they classify or describe a violation, and a conformance suite MUST NOT require identical rejection reasons. Hosts SHOULD nonetheless make the reason machine-readable, because it is what a deploy-time error message and a signal tap have to show.

### 6.4 Error port

`PORT_ERR` is a reserved output port on every block, absent from the manifest's `outputs` list. A guest MAY `emit(PORT_ERR, ...)` signals it cannot process. Routing of the error port is a service-level concern (host/Designer); unrouted error emissions are logged and counted. This gives failure a data path without inventing new mechanisms.

---

## 7. Host interface

Import namespaces, with signatures. `-> i32` follows the status/size convention of §8 unless stated.

### 7.0 `eio:core` — always available, requires no manifest capability

|Import|Signature|Notes|
|---|---|---|
|`log`|`(level: i32, ptr: i32, len: i32) -> ()`|UTF-8 message; levels 0=trace..4=error|
|`emit`|`(port: i32, ptr: i32, len: i32) -> i32`|§6.2|
|`prop`|`(prop_id: i32, signal_idx: i32, buf: i32, cap: i32) -> i32`|§7.1|
|`error`|`(code: i32, ptr: i32, len: i32) -> ()`|Structured error detail accompanying a non-zero callback return|
|`time_unix_ms`|`() -> i64`|Wall clock. Host-mediated deliberately: determinism/replay lever|
|`time_mono_ms`|`() -> i64`|Monotonic|
|`rand`|`(buf: i32, len: i32) -> i32`|Host RNG, same rationale|

### 7.1 Property access protocol

Properties are always expressions, evaluated **host-side, per-signal, on demand** (pull model — SCOPE §3.5, design discussion §4 option (b)).

`prop(prop_id, signal_idx, buf, cap) -> i32`

- `signal_idx` identifies a signal **within the batch of the current `eio_process_signals` call**, explicitly — no hidden cursor. Outside `process_signals`, or for signal-independent evaluation, pass `SIGNAL_NONE`.
- Result is the CBOR-encoded evaluated value, written to `(buf, cap)`.
- Return convention (§8): `0..=cap` bytes written; `> cap` = required size, nothing written, guest grows buffer and retries; `< 0` = error.
- The host MUST cache evaluation results keyed by `(instance, prop_id, signal_idx)` for the duration of the current callback, so the grow-and-retry path does not re-evaluate.
- **Constant folding:** the host MUST parse all property expressions at configure time and SHOULD detect signal-independence statically; signal-independent expressions are evaluated once and served from cache regardless of `signal_idx`.
- **No-context error:** evaluating a signal-dependent expression with `SIGNAL_NONE` MUST return `ERR_NO_SIGNAL_CONTEXT`, never a null value.
- **Per-signal failure:** an expression that fails against a particular signal (missing attribute, type mismatch) returns `ERR_EXPR` _for that call only_; the instance is unaffected. The block chooses: skip the signal, substitute a default, or route it to `PORT_ERR`. The host MUST log the failure and SHOULD surface it in signal taps.

`prop` calls with `signal_idx` outside the current batch (and not `SIGNAL_NONE`) return `ERR_INVALID_ARG`.

### 7.2 `eio:state` — capability `state`

Durable KV, scoped to the block instance (namespaced by host: system/service/instance).

|Import|Signature|
|---|---|
|`state_get`|`(key: i32, key_len: i32, buf: i32, cap: i32) -> i32` (size convention)|
|`state_put`|`(key: i32, key_len: i32, val: i32, val_len: i32) -> i32`|
|`state_del`|`(key: i32, key_len: i32) -> i32`|

Durability is host-decided. On leaf hosts, `state_put` MAY return `ERR_THROTTLED` (flash wear budgets); blocks MUST treat persistence as best-effort and not as a message queue.

### 7.3 `eio:timer` — capability `timer`

|Import|Signature|
|---|---|
|`timer_set`|`(delay_ms: i64, repeat: i32) -> i32` (returns `timer_id` ≥ 0 or error)|
|`timer_cancel`|`(timer_id: i32) -> i32`|

Fires as `eio_on_timer(timer_id)`, serialized with all other callbacks. `repeat != 0` = periodic until cancelled or stop. Timer resolution and drift are host-defined; timers are not real-time guarantees.

### 7.4 `eio:gpio` — capability `gpio`

|Import|Signature|
|---|---|
|`gpio_mode`|`(pin: i32, mode: i32) -> i32` (0=input, 1=output, 2=input_pullup, 3=input_pulldown)|
|`gpio_read`|`(pin: i32) -> i32` (0/1 or error)|
|`gpio_write`|`(pin: i32, value: i32) -> i32`|
|`gpio_watch`|`(pin: i32, edge: i32) -> i32` (1=rising, 2=falling, 3=both; returns `watch_id`)|
|`gpio_unwatch`|`(watch_id: i32) -> i32`|

Edges fire as `eio_on_gpio(watch_id, value)`. Pin numbering is host/platform-defined and surfaced through node configuration, not the ABI. Hosts without GPIO reject the capability at deploy validation (SCOPE §3.3).

### 7.5 `eio:i2c` — capability `i2c`

|Import|Signature|
|---|---|
|`i2c_write`|`(bus: i32, addr: i32, ptr: i32, len: i32) -> i32`|
|`i2c_read`|`(bus: i32, addr: i32, buf: i32, cap: i32) -> i32` (size convention)|
|`i2c_write_read`|`(bus: i32, addr: i32, wptr: i32, wlen: i32, buf: i32, cap: i32) -> i32`|

Synchronous by design: I2C transactions are microseconds-to-milliseconds and fall within callback deadlines. (SPI/UART/BLE follow this pattern later; additive, minor version — §12.)

### 7.6 `eio:http` — capability `http`

|Import|Signature|
|---|---|
|`http_request`|`(ptr: i32, len: i32) -> i32` (returns `req_id`)|

Request is a CBOR map: `{method, url, headers?, body?, timeout_ms?}`. Completion fires `eio_on_http(req_id, status, ptr, len)` where `(ptr, len)` is a host-allocated (via `eio_alloc`) CBOR map `{headers, body}` the guest MUST free. `status` < 0 = transport error; ≥ 0 = HTTP status. Async request-id pattern; the same shape applies to any future async capability.

---

## 8. Status and size return convention

All `-> i32` returns follow one convention:

- **Status calls** (no data out): `0` = OK, negative = error code.
- **Size calls** (data out into `(buf, cap)`): `0..=cap` = bytes written; `> cap` = required size, buffer untouched, retry with larger buffer; negative = error code.
- **Id-returning calls** (`timer_set`, `gpio_watch`, `http_request`): `≥ 0` = id, negative = error.

Error codes (guest-visible; hosts MUST use these values):

|Code|Name|Meaning|
|---|---|---|
|`-1`|`ERR_INVALID_ARG`|Bad index, pointer, or parameter|
|`-2`|`ERR_NO_SIGNAL_CONTEXT`|Signal-dependent expression with `SIGNAL_NONE`|
|`-3`|`ERR_EXPR`|Expression evaluation failed for this signal|
|`-4`|`ERR_CAPABILITY`|Capability not granted / not present on this host|
|`-5`|`ERR_LIMIT`|Payload/batch/queue limit exceeded|
|`-6`|`ERR_THROTTLED`|Temporarily refused (e.g. flash wear budget); retry later|
|`-7`|`ERR_NOT_FOUND`|Key/id does not exist|
|`-8`|`ERR_IO`|Underlying device/transport failure|
|`-9`|`ERR_UNSUPPORTED`|Valid call, unimplemented on this host|

Callback returns (guest→host): `0` = OK; non-zero = block-level error. The host logs it (with any detail passed via `core.error`), counts it, and continues; a non-zero callback return is NOT a trap and does not kill the instance. Killing is reserved for traps, fuel, and deadlines.

---

## 9. Memory rules (normative summary)

1. `eio_alloc`/`eio_free` are the only allocation channel across the boundary. Host never writes to guest memory it did not just allocate, except into `(buf, cap)` ranges the guest passed in the current call.
2. **Inbound payloads** (`eio_configure`, `eio_process_signals`, `eio_on_http`): host allocates, guest owns after the call begins, guest MUST free.
3. **Outbound payloads** (`emit`, `state_put`, `i2c_write`, `http_request`, `log`, `error`): guest allocates, host copies out during the call, guest owns and frees afterward. Host MUST NOT retain guest pointers past the call.
4. **Guest-supplied out-buffers** (`prop`, `state_get`, `i2c_read`): guest allocates `(buf, cap)`; grow-and-retry per §8.
5. `eio_alloc` returning 0 = allocation failure; a guest failing to allocate SHOULD return an error status rather than trap where possible.
6. Alignment: `eio_alloc` MUST return 8-byte-aligned pointers.
7. `max_payload` (instance descriptor): host rejects `emit` beyond it with `ERR_LIMIT` and never delivers batches beyond it. Discoverable, so MCU limits are visible to blocks and to deploy-time validation.

---

## 10. Execution limits

- Every callback runs under a host-enforced budget: fuel (wasmtime), epoch interruption, or watchdog (WAMR/wasm3). Exhaustion is a trap (→ DEAD).
- The contract stated plainly: **callbacks MUST return promptly. Blocking is a defect. Long work is chunked via timers.**
- Budgets are host configuration, not ABI constants; leaf hosts will be tighter. The conformance suite (§13) includes a hostile-block test that spins.

---

## 11. Manifest schema

JSON. Published in the registry alongside the OCI artifact (SCOPE §3.6) and embedded as the `eio:manifest` custom section (§4.4).

```json
{
  "name": "filter",
  "version": "1.2.0",
  "abi": { "major": 1, "minor": 0 },
  "description": "Route signals by predicate",
  "capabilities": [],
  "inputs":  [ { "name": "in" } ],
  "outputs": [ { "name": "true" }, { "name": "false" } ],
  "properties": [
    {
      "name": "predicate",
      "type": "bool",
      "description": "Evaluated per signal",
      "default": "(true)",
      "required": true
    }
  ],
  "targets": [ "wasm32-unknown-unknown" ],
  "aot": [ "esp32s3" ]
}
```

Notes:

- **Every property is an expression** (design discussion §4, option (b)). There is no static/expression kind split. `type` declares what the expression must evaluate to (`bool | int | float | string | bytes | any`); the host type-checks the evaluated value and returns `ERR_EXPR` on mismatch. Constants are trivial expressions; the Designer MAY render simple literals as plain input fields — a UI affordance, not an ABI distinction.
- `default` is an expression string in the platform's micro-Lisp (SCOPE §3.5; expression language grammar is specified separately).
- `capabilities` ⊇ imported `eio:*` namespaces minus `eio:core` (§4.3).
- Port order in `inputs`/`outputs` defines port indices (§5.2).
- `aot` lists prebuilt AOT artifacts for leaf targets published alongside the portable module.
- Property order defines `prop_id` — appending properties is backward compatible, reordering or removing is not; this is the block's (not the ABI's) versioning concern.

Full JSON Schema for the manifest ships in the repo as `manifest.schema.json`; this section is the normative prose. The manifest is also the Designer's config-panel source and the agent-tooling surface (SCOPE §4) — descriptions are user-facing documentation and SHOULD be written as such.

---

## 12. ABI versioning

- `eio_abi_version() -> i32` returns `(major << 16) | minor`. This document specifies **1.0**.
- Host policy: reject `major` mismatch; accept `minor` ≤ host's minor (pure-additive guarantee).
- Additive changes (new host namespaces/functions, new optional exports, new error codes): minor bump. Old blocks never import the new functions, so nothing breaks.
- Changes to memory rules, lifecycle, calling conventions, sentinels, or the status convention: major bump.
- The manifest's `abi` field MUST match the module's exported version; hosts MAY reject on mismatch (the module is authoritative).

---

## 13. Conformance

The monorepo carries:

1. **Reference harness** — a minimal host (wasmtime-based) that drives a module through the full lifecycle with scripted deliveries, property tables, and fault injection (undersized buffers, `ERR_THROTTLED` state, capability denial).
2. **Golden blocks** — small blocks exercising each contract area: pure transform, multi-port routing (filter), timer emitter (simulator), stateful counter, GPIO echo, hostile blocks (spinner, allocator-liar, reentrancy-prober, oversize-emitter).

Both the daemon and the leaf runtime MUST pass the harness against the golden blocks. Divergence between the two hosts is a conformance bug by definition.

---

## 14. SDK requirement (informative)

Almost no one writes against this ABI raw. The `block-sdk` Rust crate is developed in lockstep with this spec:

- Derive macro over a config struct + a `Block` trait → generated exports, allocator, CBOR (de)serialization, typed property accessors wrapping `prop` with the grow-and-retry loop, safe wrappers for every host namespace.
- All `unsafe` in the block ecosystem lives inside the SDK's audited glue; block authors write safe Rust exclusively (design discussion §3 resolution).
- **Litmus rule: if a contract in this spec is awkward to wrap ergonomically in the SDK, the spec is wrong.** SDK friction findings feed back as spec amendments before 1.0 freezes.

---

## 15. OPEN items (tracked in SCOPE.md)

This spec deliberately does not decide, and is compatible with any resolution of:

- Cross-device delivery guarantees, ordering, backpressure policy — SCOPE §3.4, §3.9. (ABI touchpoints: `emit` enqueue semantics and `ERR_LIMIT` already accommodate all candidate policies.)
- Pub/sub transport and broker topology — SCOPE §3.9. (Publisher/subscriber blocks are ordinary blocks; transport is a host concern behind `emit`/delivery.)
- Supervision policy on instance death — SCOPE §3.13. (ABI defines only: trap = death, re-instantiation = fresh configure.)
- Transport security / node auth — SCOPE §3.11. (No ABI surface.)
- Metrics — SCOPE §3.12. (Likely additive `eio:core` functions or pure host-side counters; minor version either way.)
- Expression language grammar — SCOPE §3.5 specifies the constraints (pure, bounded, `no_std`, per-signal); the grammar gets its own spec. This document only fixes the _evaluation protocol_ (§7.1).

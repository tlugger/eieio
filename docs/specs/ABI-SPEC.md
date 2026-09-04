# Block ABI Specification

**Status:** Draft 1 **Depends on:** SCOPE.md (decision record). OPEN items referenced here are tracked there, not re-litigated here.

This document specifies the binary interface between a **host** (daemon-class runtime on Linux-class devices; leaf-class runtime on MCUs) and a **guest** (a block compiled to a core WASM module). It is the contract that the daemon, the leaf runtime, the block SDK, the registry manifest, and the conformance suite are all built against.

The key words MUST, MUST NOT, SHOULD, and MAY are used as in RFC 2119.

---

## 1. Design invariants

These follow from SCOPE §3.2–3.5 and are restated here because every rule below derives from one of them:

1. **Core WASM modules only.** Target `wasm32-unknown-unknown`. No WASI, no component model, no threads. A module MUST validate against **core WASM 1.0 plus the portable subset of the six proposals §4.3 lists** — what the guest toolchain emits by default *and* the leaf interpreter executes, which for two of the six is less than the whole proposal — so that wasmtime, WAMR and wasm3 are all viable hosts. Nothing beyond that set: SIMD, tail calls, exceptions, GC, multi-memory and memory64 are all refused.
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

**The `memory` export carries a declared minimum, and a host MAY refuse it.** The module's memory declaration fixes the pages an instantiation has to be able to supply before any guest code runs; one WASM page is 64 KiB and a module cannot declare less than one. A host MAY bound that minimum with a **per-instance page ceiling**, which is host configuration and not an ABI constant, and a host whose ceiling a module's declared minimum exceeds **MUST refuse the module at load time**, in the same place and the same sense as §4.3's cross-check. A host that bounds nothing here refuses nothing here, and both answers are conforming — the daemon gives the first (DAEMON §4), a leaf the second at one page (LEAF §4.2). Three things are fixed about the refusal:

- **It is an instantiation refusal and never a trap.** The instance is never created (§5.1 step 1), so there is nothing to kill and nothing for §8's death kinds to classify. A host that admitted the module and killed it afterwards would be reporting a property of the *module* as a fault of the instance.
- **A host MUST NOT instead grant less than the module declared.** An instance given a smaller memory than it asked for is not a smaller instance, it is a different one: it fails at whatever allocation first crosses a line the guest was never told about, at whatever moment the traffic happens to reach it. Refusing is the only answer that fails where a deployer can see it.
- **It bounds admission, not growth.** `memory.grow` is core WASM and a module MAY grow past its declared minimum; what bounds that is the module's declared *maximum* and the engine enforcing it. A host that must bound an instance's whole footprint bounds it there, not here.

What a *block* may assume about any host's ceiling is §9.7 rule 10: nothing.

### 4.2 Optional exports (callbacks)

Present only if the block uses the corresponding capability. The host MUST verify that a module importing `eio:timer` exports `eio_on_timer`, etc., at load time.

|Export|Signature|Paired capability|
|---|---|---|
|`eio_on_timer`|`(timer_id: i32) -> i32`|`timer`|
|`eio_on_gpio`|`(watch_id: i32, value: i32) -> i32`|`gpio`|
|`eio_on_http`|`(req_id: i32, status: i32, ptr: i32, len: i32) -> i32`|`http`|

The pairing is required in **both** directions: a module that exports `eio_on_timer` while importing no `eio:timer` MUST also be rejected. Such an export is a callback the host can never invoke, which means the block — or the code generator that produced it — believes it holds a capability it never asked for. Silence there is how a block ships with a timer handler that simply never runs.

### 4.3 Imports

All imports MUST be from `eio:*` namespaces (§7). Anything else fails validation. Within a namespace, an imported name MUST be one of the functions §7 defines for it; a module importing `eio:core` `frobnicate` is rejected at load time rather than at instantiation, so the rejection can name the offending import. The set of imported namespaces MUST be a subset of the capabilities declared in the manifest (§11); the import section is authoritative, the manifest is advisory, and a mismatch in either direction where imports exceed manifest MUST be a load-time rejection.

Import *signatures* are checked when the host links the module, by the engine. The load-time check above is a superset in namespaces and names only.

**Whole-proposal conformance (§1) is enforced by the engine, not by manifest validation.** A host configures its engine to accept the proposals below and nothing more; a module reaching for a seventh proposal is refused at instantiation. This is deliberate placement: WASM feature gating is a moving target that engines already track, and duplicating it in the loader would create a second, slower-moving definition of what is accepted.

A host MUST refuse such a module. Where the engine is what refuses it, the rejection SHOULD name the offending proposal, and MUST name it where the host's engine reports which one it objected to — because a host that refuses a module without saying which feature it objected to leaves a deployer with a working block, a passing manifest, and nothing to act on. **Where the loader is what refuses it, naming the proposal is a MUST**, unconditionally: that message is written in this repository rather than reported by an engine, so there is nothing for a host to be excused from.

**Why the engine's half is a SHOULD and not the MUST it was.** Because measured against real engines it was unsatisfiable, and a MUST no conformant host can meet is not a requirement but a bug in this document. Of the nine refused proposals, wasmtime names eight and does not name **extended const** — a module whose global initialiser is `i32.add` is refused with `constant expression required: non-constant operator: i32.add`, which describes the instruction and never the proposal. wasm3 names **none** of the nine: its refusals are `unknown opcode`, `restricted opcode`, `out of order Wasm section`, `malformed Wasm binary`. A host cannot invent a name its engine does not give it, and a loader that recomputed one would be the second definition of the accepted set this section spends its length refusing.

So the obligation is placed where it can be met: name it when you know it, and the conformance vectors assert the name only for the proposals an engine actually reports (§13.1) — except for the three the loader refuses, below, where every host names it.

**The accepted set.** Core WASM 1.0, plus exactly these six:

|Proposal|Why it is in|Accepted|
|---|---|---|
|bulk memory|`memory.copy`/`memory.fill`; rustc emits them for any sizeable move|in part|
|sign extension|`i32.extend8_s` and friends|whole|
|reference types|the `call_indirect` table-index *encoding*, not `externref` in a guest|in part|
|multi-value|multiple block and function results|whole|
|non-trapping float-to-int|saturating casts, which Rust's `as` requires|whole|
|mutable globals|exported mutable globals|whole|

Every one is enabled by default by rustc for `wasm32-unknown-unknown`. **Two are accepted only in part**, because the leaf interpreter implements only part of them — and it is the leaf interpreter, not the proposal document, that decides what a block may contain.

#### The portable subset

Measured on wasm3 (`crates/conformance/tests/wasm3.rs`), instruction by instruction, with each case returning a value only correct execution produces. Of the six proposals, four run whole. The other two do not, and their remainder is **carved out of the accepted set**: a module using anything in the right-hand column below is non-conformant and MUST be refused, with a rejection naming both the instruction and its proposal.

**The subset is the floor across leaf engines, not a description of one.** WAMR — the other engine SCOPE §3.2 names for the leaf tier, measured the same way in `crates/conformance/tests/wamr.rs` — runs the *whole* of bulk memory and reference types, every instruction in the right-hand column included. That widens nothing. The carve-out stays where it is because a block is portable or it is not: a module using `table.copy` runs on WAMR and fails on wasm3, and the accepted set is what runs on **every** engine the platform claims. An entry leaves the right-hand column when *no* named leaf engine refuses it, not when one of them stops.

|Proposal|Accepted|Carved out — wasm3 refuses|
|---|---|---|
|bulk memory|`memory.copy`, `memory.fill`|`memory.init`, `data.drop`, `table.init`, `table.copy`, `elem.drop`|
|reference types|`call_indirect` with table index `0`|`ref.null`, `ref.is_null`, `ref.func`, `table.get`, `table.set`, `table.size`, `table.grow`, `table.fill`, the `externref`/`funcref` value type outside a table, and any module declaring more than one table|
|sign extension|all five `extend*_s`|—|
|non-trapping float-to-int|all eight `trunc_sat`|—|
|multi-value|multi-result functions, `block`/`loop`/`if` parameters and multiple results|—|
|mutable globals|exported|imported globals are already unreachable: every import MUST be an `eio:*` *function* (§7), so the import rules above refuse one first|

#### The measured gaps

The engine owns the seventh proposal *when it refuses one*. Measured, wasm3 does not always. Three proposals outside the six are not refused by it at all — it loads, compiles and **runs** them, while wasmtime refuses each by name:

|Proposal|What wasm3 runs (WAMR refuses all three)|
|---|---|
|tail call|`return_call` compiles and executes, returning what a correct implementation returns|
|memory64|`(memory i64 1)` is accepted and instantiated|
|threads|`(memory 1 1 shared)` is accepted and instantiated|

For the two memory flags it is almost certainly reading the encoding and dropping it rather than implementing the proposal — an `i64` index quietly truncated, a shared memory that is not shared. That is a silent misinterpretation, and worse than an honest refusal: the block works on the daemon and is wrong on the leaf. A hand-written `.wat` or a future non-Rust SDK (SDK §7) is all it takes to produce one.

A gap in an engine's refusals is not a gap in the platform, so **the loader refuses these three** — for them and for nothing else. That bound is what keeps this from becoming the second, slower-moving definition of the accepted set: an entry earns its place by being *measured*, and leaves the day the engine refuses it and `crates/conformance/tests/wasm3.rs` fails. Every other proposal outside the six is refused by every measured engine, which is where this section leaves it: a loader that answered for SIMD as well would be claiming to validate MVP, which it does not. WAMR refuses all nine, these three included — so this carve-out is wasm3's alone, and it stays for exactly as long as wasm3 is a named leaf engine.

Threads is also refused for a reason no engine fix would remove: §1.2 gives an instance one caller at a time, so a second thread reaching into guest memory has no place in this ABI.

**Both of those are enforced by the loader, and neither is a second definition of the accepted set** — between them they are the only place the real one can be stated. An engine's feature configuration is per *proposal*: neither wasmtime's `Config` nor any other engine's can express "bulk memory minus `memory.init`", so a host that admits `memory.copy` admits `table.copy` with it, and would run a block the leaf tier cannot flash. Nor can any configuration make an engine refuse what it does not implement refusing. The loader scan states what the engine cannot; it never restates what the engine does. Hosts therefore enforce the accepted set in two layers, and both are mandatory:

1. **the engine** refuses a seventh proposal (`crates/daemon/src/engine.rs`);
2. **the loader** refuses what the engine cannot or does not (`eio_manifest::validate`) — the carved-out instructions within the six, which no feature switch can express, and the three proposals above that the leaf engine runs rather than refuses.

Layer 2 is a fixed, measured list on both counts, and that is what makes it a narrowing rather than a rival: it never grows by argument, only by a measurement, and every entry names the proposal it refuses.

**Layer 2 stays quiet about what it cannot decode — but only where layer 1 is still to come.** The scan walks function bodies, so it meets instructions from proposals it has no business in, and the right answer to one is silence: the engine refuses such a module and names the proposal, and a *not a readable WASM module* from the loader would replace that sentence with one nobody can act on. That is sound exactly while an engine follows. It is not sound in a flow that reports a verdict and then compiles nothing — a build tool that prints its success, a registry endpoint that answers *the block is in the cache* — because there the silence is the last word, and the module was never judged at all.

So the obligation follows the flow. **A host that reports a module acceptable without an engine having compiled it MUST refuse a body it cannot finish decoding**, saying where decoding stopped. It MUST NOT name a proposal there: it does not know one, and guessing would be the second definition of the accepted set this section spends its length refusing. Naming the offset and saying that nothing downstream will explain it is the honest answer, and it is a strictly better one than a build that succeeded on a module no engine has ever read.

The consequence is intended and is not a loss: a module reaching for a seventh proposal is non-conformant either way, and on such a flow no named refusal was ever coming. Which of the two a given flow will actually meet differs, and it is worth saying which. A registry endpoint answering for a pulled artifact meets both — the bytes are foreign, so corruption is real. A build tool meets only the proposal: the module it validates is one it has just produced, so the honest thing it gains is refusing a block whose compiler emitted an instruction outside the six, at the build rather than at the deploy.

**Both cost a block author nothing**, which is why they are affordable. ABI §13.2's five golden blocks, built by stock rustc with no flags, contain `memory.copy`, `memory.fill`, one table, one 32-bit unshared memory and numeric locals only — not one refused construct between them. Rust reaches for the rest only under `-Z build-std` with shared memory, or through `externref`, or through a tail-call feature it does not emit, none of which a block does.

Every half is pinned by tests, not by these tables. `crates/daemon/src/engine.rs` asserts a conformant host accepts each of the six, that it refuses the three measured gaps by name, and that the loader refuses both what this engine would have run: the carve-out and those three. `crates/conformance/tests/wasm3.rs` runs every accepted instruction on wasm3 checking the value it produces, asserts wasm3 refuses every carved-out one — and asserts it still *runs* all three measured gaps, so that a real refusal from it fails the suite and says so.

**Producers need no feature flags**, and this is a correction. Earlier drafts of this section required `-C target-feature=-bulk-memory` and called it "the only flag needed". Measured on rustc 1.97.1, that flag changes nothing: the offending `memory.copy` lives in `alloc::string::String::clone` inside the precompiled `rust-std`, which no `RUSTFLAGS` rebuilds — nor does `-Z build-std`. Strict MVP was therefore not merely unnecessary, it was unreachable, and the rule made every Rust block unloadable while protecting a constraint wasm3 does not have. An ordinary `cargo build --release --target wasm32-unknown-unknown` produces a conformant module; `crates/conformance/tests/wasm3.rs` and `crates/conformance/tests/wamr.rs` each build one with no flags at all and drive it through §5.1.

Widening this set further is a measurement, not a judgement call: a proposal — or an instruction within one — belongs here when the toolchain emits it *and* the leaf tier runs it, and the suite is where that is established. A carved-out instruction moves into the accepted set the day wasm3 executes it correctly and the negative test above starts failing; that failure is the notification.

### 4.4 Custom section

A module SHOULD embed its manifest as a custom section named `eio:manifest` (UTF-8 JSON, identical to the registry manifest). A `.wasm` file is then self-describing without registry metadata. If both are present the registry manifest and embedded manifest MUST be identical; hosts MAY reject on mismatch.

"Identical" means the two **parsed manifests** are equal, not the two byte sequences. A registry entry reformatted by a publishing tool, or serialized with different whitespace, describes exactly the same block, and refusing to load it would make the manifest's meaning depend on its formatting. Byte-level identity is not required and MUST NOT be relied on.

WASM permits repeated custom sections with the same name. A module carrying more than one `eio:manifest` section MUST be rejected: it describes itself twice, and choosing one silently is the same last-wins resolution §11.1 forbids for duplicate JSON keys.

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
4. **Running.** Host invokes callbacks, serialized, in host-determined order. The host SHOULD deliver batches on one input port in arrival order; ordering across ports and across instances is unspecified, and deliberately so — a block that needs two inputs correlated correlates them itself. Cross-*node* delivery is a separate promise and a weaker one: SCOPE §3.4 makes it at-most-once, ordered per publisher per topic and no wider.
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
    "max_batch": uint,          ; largest signal count in a DELIVERED batch
    ? "max_emission_bytes": uint ; OPTIONAL. largest total payload `emit` accepts within
                                 ; one callback (§9.7 rule 9)
  }
}
```

`max_emission_bytes` is **absent, not zero and not a sentinel**, on a host that does not bound what one callback may emit: `0` would read as "emit nothing", and a maximal integer would be a number every block has to be told means something else. Absence is therefore a statement — this host will not refuse an emission for the queue's sake — and it is the only optional key in the document.

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
- `emit` failure (queue full / payload too large) is a status code to the _emitter_, policy is host-defined (OPEN: backpressure, SCOPE §3.4). **"Queue full" has a number and the descriptor publishes it**: `max_emission_bytes` (§9.7 rule 9). Host-defined means the host chooses the size, not that a block cannot find out what it is — a limit a conforming payload can hit, invisible to the block that hits it, is the divergence §13 exists to prevent.

Three refusals are **not** host-defined, because a guest that hears a different code from two hosts cannot be written against either:

|What the guest emitted|Code|
|---|---|
|Bytes that are not a canonical batch (§6.3.1)|`ERR_INVALID_ARG`|
|An `output_port` that is neither an index into the instance descriptor's `outputs` nor `PORT_ERR`|`ERR_INVALID_ARG`|
|A `len` beyond `max_payload` (§9.7)|`ERR_LIMIT`|

The first two are §8's "bad index, pointer, or parameter" and the third is §9.7 stated as a code. A host MUST check the port and the length before reading the payload: the length check is the one that makes an oversized `(ptr, len)` cheap to refuse, and refusing on a length the host never read is what stops a guest from choosing how much memory the host touches.

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

**The rules above are pinned by vectors, and every host MUST pass them identically.** `expr-tests/cbor/` carries them: encoded bytes paired either with the batch they decode to or with the fact that they are refused, covering each of the eleven rules in both directions — rule 6 excepted, which mandates preservation and so forbids no bytes — and both deviations in both forms. They are data files rather than a test written in any host's language, for the reason EXPR §11 gives about its own corpus — a suite written in Rust could only ever measure the Rust implementation, and it is a host reaching for a *stock* canonical-CBOR library that rules 4 and 7 exist to catch. **`expr-tests/README.md` is the vector format's normative description.**

Two properties of that corpus follow from this section rather than from convenience. A rejecting vector asserts only that the bytes are refused, never a reason, because of the paragraph above. And every accepting vector additionally asserts that re-encoding the decoded batch reproduces its input byte for byte, because that is this section's second sentence — and because it is the only thing that catches a decoder which reads the right values and normalises them, negative zero being the case where no value comparison can.

### 6.4 Error port

`PORT_ERR` is a reserved output port on every block, absent from the manifest's `outputs` list. A guest MAY `emit(PORT_ERR, ...)` signals it cannot process. Routing of the error port is a service-level concern (host/Designer); unrouted error emissions are logged and counted. This gives failure a data path without inventing new mechanisms.

A service file addresses it as `err`, and §11.1 reserves that name in both `inputs` and `outputs` so that no block can declare a port competing with it.

---

## 7. Host interface

Import namespaces, with signatures. `-> i32` follows the status/size convention of §8 unless stated.

**Three of these namespaces have no host today, and a block author has to be told so here.** `eio:core` (§7.0), `eio:state` (§7.2) and `eio:timer` (§7.3) are implemented by every host in this repository. `eio:gpio`, `eio:i2c` and `eio:http` (§7.4–§7.6) are implemented by none of them: a daemon lists exactly `state` and `timer` in its `GET /node` capabilities and **refuses at validation** a block declaring any other (DAEMON §3 step 2, SCOPE §3.3), and the leaf runtime implements the same two. The only code answering those imports anywhere is two test fixtures — the conformance harness's scriptable stand-in and `test-host`'s.

That is a deliberate state rather than a gap, and the reason is architectural: a daemon runs on a server, which has no pins and no bus, so the tier those namespaces belong to is the leaf (LEAF §1). **Their specification here is what makes a leaf implementable, not a promise that a node will run them now.** The rest of the vertical is built and honest about this: `manifest` validates their import signatures, `block-sdk` ships `Gpio`/`I2c`/`Http` wrappers, `examples/blocks/gpio-echo` is one of §13.2's golden blocks, and §13's suite has scenarios for all three — so a block using them compiles, passes conformance against the reference harness, and is refused by every node that exists. The Designer says so before a deploy rather than after: it compares a block's manifest capabilities against the target node's reported list and marks the block in both the library and on the canvas, naming the missing capability — so an author meets SCOPE §3.3's check when choosing a block, not when a service fails to start.

### 7.0 `eio:core` — always available, requires no manifest capability

|Import|Signature|Notes|
|---|---|---|
|`log`|`(level: i32, ptr: i32, len: i32) -> ()`|UTF-8 message; levels 0=trace..4=error|
|`emit`|`(port: i32, ptr: i32, len: i32) -> i32`|§6.2|
|`prop`|`(prop_id: i32, signal_idx: i32, buf: i32, cap: i32) -> i32`|§7.1|
|`error`|`(code: i32, ptr: i32, len: i32) -> ()`|Structured error detail accompanying a non-zero callback return|
|`time_unix_ms`|`() -> i64`|Wall clock. Host-mediated deliberately: determinism/replay lever|
|`time_mono_ms`|`() -> i64`|Monotonic|
|`rand`|`(buf: i32, len: i32) -> i32`|Host RNG, same rationale. **Status** convention, not size: the parameter is a `len`, not a `cap`, so `0` means exactly `len` bytes were written and there is no shorter answer to grow and retry from|

### 7.1 Property access protocol

Properties are always expressions, evaluated **host-side, per-signal, on demand** (pull model — SCOPE §3.5, design discussion §4 option (b)).

`prop(prop_id, signal_idx, buf, cap) -> i32`

- `signal_idx` identifies a signal **within the batch of the current `eio_process_signals` call**, explicitly — no hidden cursor. Outside `process_signals`, or for signal-independent evaluation, pass `SIGNAL_NONE`.
- Result is the CBOR-encoded evaluated value, written to `(buf, cap)`. The value MUST satisfy the property's declared `type` (§11.1) and MUST be encoded as that type — an int promoted to a `float` property is encoded as a float, so the guest decodes what was declared. A value that does not satisfy it is `RESULT_TYPE` (EXPR §8), returned as `ERR_EXPR`.
- Return convention (§8): `0..=cap` bytes written; `> cap` = required size, nothing written, guest grows buffer and retries; `< 0` = error.
- The host MUST cache evaluation results keyed by `(instance, prop_id, signal_idx)` for the duration of the current callback, so the grow-and-retry path does not re-evaluate. The cache MUST NOT outlive the callback: `signal_idx` numbers signals within *this* call's batch, so a value carried into the next callback would answer a different question than the one asked.
- **Constant folding:** the host MUST parse all property expressions at configure time and SHOULD detect signal-independence statically; signal-independent expressions are evaluated once and served from cache regardless of `signal_idx`. That result is the expression's for the life of the instance, and a folded expression that *fails* is folded too — expressions are pure and terminating (EXPR §1), so re-evaluating one would spend budget to reach the same error, and the failure is reported once rather than once per call.
- **No-context error:** evaluating a signal-dependent expression with `SIGNAL_NONE` MUST return `ERR_NO_SIGNAL_CONTEXT`, never a null value.
- **No value at all:** a property the service did not supply and whose manifest has no `default` returns `ERR_NOT_FOUND`, for every `signal_idx` including `SIGNAL_NONE`. §11.1 admits any combination of `required` and `default`, so this is a valid declaration and not an omission: the property keeps its `prop_id` — that number is its position in the manifest (§5.2), and skipping it would renumber every property after it — and the block hears that the deployer configured nothing, which is the one thing it can act on by falling back to a value of its own. It is neither `ERR_INVALID_ARG` (which means the `prop_id` was out of range) nor `ERR_NO_SIGNAL_CONTEXT` (which means the expression needed a signal): there is no expression here for a signal to be the context of.
- **Per-signal failure:** an expression that fails against a particular signal (missing attribute, type mismatch) returns `ERR_EXPR` _for that call only_; the instance is unaffected. The block chooses: skip the signal, substitute a default, or route it to `PORT_ERR`. The host MUST log the failure and SHOULD surface it in signal taps.

`prop` calls with `signal_idx` outside the current batch (and not `SIGNAL_NONE`) return `ERR_INVALID_ARG`, and so do calls with a `prop_id` outside the manifest's `properties` list (§8: a bad index). The `signal_idx` check applies whatever the property is: a signal-independent expression served from the fold MUST still refuse an out-of-range index, or two properties of one block would answer the same bad argument differently.

### 7.2 `eio:state` — capability `state`

Durable KV, scoped to the block instance. The host composes the namespace and the guest never sees it: a block writes `count` and the host keys that under the instance it belongs to. Scoping by **service and instance** is required, because a block instance's state is its own (SERVICE §2 makes an id unique only within its service file); a `system` component is the host's option and not an obligation — a node that does not know which System it is in does not have one to key by (DAEMON §10).

|Import|Signature|
|---|---|
|`state_get`|`(key: i32, key_len: i32, buf: i32, cap: i32) -> i32` (size convention)|
|`state_put`|`(key: i32, key_len: i32, val: i32, val_len: i32) -> i32`|
|`state_del`|`(key: i32, key_len: i32) -> i32`|

Durability is host-decided. On leaf hosts, `state_put` MAY return `ERR_THROTTLED` (flash wear budgets); blocks MUST treat persistence as best-effort and not as a message queue.

**`state_del` answers `0` whether or not the key was present.** Deleting a key that was never written is not an error: the call states the intended end state, not a transition, so a block clearing state it may or may not have written needs no read first and no special case for the empty case. A host therefore MUST NOT report absence here — §8's `ERR_NOT_FOUND` is not an answer `state_del` gives — and a block that needs to know whether a key existed reads it before deleting. Pinned by `29_state_del_missing_key`, so the two hosts cannot drift on it.

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
5. `eio_alloc` returning 0 = allocation failure; a guest failing to allocate SHOULD return an error status rather than trap where possible. The same applies in the other direction: a host that cannot allocate an **inbound** payload because the guest refused MUST NOT kill the instance. The delivery fails and is reported as `ERR_LIMIT`, counted like any other block-level error (§8) — a guest that is briefly out of memory has told the truth about itself, and killing it for that would make a transient memory spike fatal.
6. Alignment: `eio_alloc` MUST return 8-byte-aligned pointers. A pointer that is misaligned, zero-but-nonzero-length, or outside linear memory is a *different* matter from a refusal: the guest has told the host something untrue about its own memory, nothing the host does next is trustworthy, and the instance MUST be discarded.
7. `max_payload` (instance descriptor): host rejects `emit` beyond it with `ERR_LIMIT` and never delivers batches beyond it. Discoverable, so MCU limits are visible to blocks and to deploy-time validation. Both it and `max_batch` are host configuration with **no floor**: a block reads them from its descriptor and may assume nothing about their size (SCOPE §3.4 OPEN).
8. `max_batch` bounds the batches a host **delivers** to a guest, and only those. It does **not** bound emission: a block MAY emit a batch carrying more signals than `max_batch`, and the host routes it. §6.2's three refusals stay the whole list — a fourth would make the one answer §6.2 fixes vary by host, and a guest-side check would report a code no host produced. The asymmetry is deliberate: `max_payload` is about bytes the host must hold, which it must refuse in both directions, while a signal *count* costs the host nothing on the way out. Pinned by `30_emit_exceeds_max_batch`.
9. `max_emission_bytes` (instance descriptor, OPTIONAL) bounds the payload bytes a host accepts from `emit` **within one callback**. It is §6.2's "queue full" given a number: `emit` enqueues rather than delivers, so everything a callback emits is held by the host until the callback returns and routing happens, and this is the bound on what is held. Past it a host MUST answer `ERR_LIMIT`, and the instance **lives** — a status code, never a death (§8). Only *accepted* payloads count against it (a refused emission is not held, so it is not charged), and the count starts again at every callback. Four things follow, and the last is the one a block author has to hold:
    - **It is not a fourth entry in §6.2's fixed table.** That table is the refusals whose *occurrence* cannot vary by host; this one is the host-defined policy §6.2 already licenses, and what the ABI fixes about it is the code (`ERR_LIMIT`) and the fact that the number is published. Rule 8 is untouched: `max_batch` still does not bound emission.
    - **It is a bound on wire bytes, not on host memory, and MUST NOT be read as one.** A batch's decoded footprint expands over its CBOR length by a factor no length predicts — measured between 1.19× and 22.1× (LEAF §4.3) — so a host holding batches on a fixed heap needs a reservation before it decodes as well as this bound, and this rule does not supply it.
    - **Host configuration with no floor**, like the other two: a block may assume nothing about its size (SCOPE §3.4 OPEN). A host that bounds emission publishes the number in its descriptor; a host that does not publishes no key (§5.2).
    - **A block MUST NOT assume the bound is absent.** A callback that emits 10 KiB succeeds where nothing bounds it and is refused on a leaf publishing 4 096 (LEAF §4.3). The block sees `ERR_LIMIT`, which is exactly the answer §10 already tells it what to do about: long work is chunked via timers.

    Pinned by `33_emission_budget`, on every host the suite runs.
10. **Declared minimum linear memory** (§4.1) is the fourth host-configured number here and the only one a block cannot read. A host's per-instance page ceiling is **not** an instance-descriptor key and MUST NOT be published as one: the three above are numbers a live instance reacts to, and this one is answered before the instance exists — a module past the ceiling never runs, so there is no descriptor to carry it and no `ERR_LIMIT` for anyone to see. A block **MAY** declare more than one page and **MUST** assume nothing about any host's ceiling. Three things follow for a block author:
    - **The refusal is a deploy-time fact and never a runtime one.** It reaches whoever loads or bakes the block — a load-time rejection on a node, a failed firmware build on a leaf (LEAF §4.2) — which is the whole reason §4.1 puts it at admission rather than leaving it to an allocation failure at 2 a.m.
    - **The number is usually the linker's, not the block's.** All five of §13.2's golden blocks declared 17 pages before SDK §5.2 fixed the shadow-stack default, and not one of them needed more than one. A block refused for its memory is worth re-linking before it is worth rewriting.
    - **Two hosts answering differently is licensed here rather than tolerated.** §13's "divergence between the two hosts is a conformance bug" binds them to the same *rule*, and the rule is that the ceiling is configuration — as it already is for the three limits above. A block that must run on both tiers stays inside the smallest ceiling it targets, and has no way to discover at runtime what that was.

    Pinned by `34_memory_ceiling`, whose refusal is the loader's on every host (§13.1).

---

## 10. Execution limits

- Every callback runs under a host-enforced budget: fuel (wasmtime), epoch interruption, or watchdog (WAMR/wasm3). Exhaustion is a trap (→ DEAD).
- The contract stated plainly: **callbacks MUST return promptly. Blocking is a defect. Long work is chunked via timers.**
- Budgets are host configuration, not ABI constants; leaf hosts will be tighter. The conformance suite (§13) includes a hostile-block test that spins.

**What a budget mechanism owes, whatever it is.** Fuel, epoch interruption and a hardware watchdog have nothing in common but the job below, and these two obligations are that job. They are requirements on the *host*, identical on every host, and they are stated here rather than in each host's own document because a host that derives them again in its own words has written a second copy of them to drift from.

1. **A terminated call MUST return to the host as a trap, not as a status code.** §8 is explicit in both directions: a budget death kills the instance, and a non-zero callback return does not. A mechanism that unwound a terminated call as an ordinary return would turn a death into life, and the host would have nothing left to tell a killed callback apart from a block reporting an error with a status.
2. **The gap between the decision to terminate and the return MUST be bounded**, and a mechanism that cannot bound it does not enforce a deadline, it requests one. Where the bound comes from is the mechanism's own: an interpreter bounds it by checking for the request at least once per loop back-edge and once per call, and a host driving the engine's clock from outside bounds it by that clock's resolution (DAEMON §5.1's epoch tick, LEAF §4.4's watchdog stage). Without a bound the host's next move is whatever it does about a call that never came back — on a leaf that is a node reset (LEAF §4.6), which is the divergence this obligation keeps off the normal path.

**A host whose binding cannot meet both does not have a budget, and MUST say so rather than report one.** §13.1 gives the harness a skip class for exactly that: the scenarios expecting a budget death are skipped by name, because the only other thing an unbudgeted host can do with a block that never returns is hang.

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
      "default": "true",
      "required": true
    }
  ],
  "targets": [ "wasm32-unknown-unknown" ],
  "aot": [ "riscv32imc-unknown-none-elf" ]
}
```

Notes:

- **Every property is an expression** (design discussion §4, option (b)). There is no static/expression kind split. `type` declares what the expression must evaluate to (`bool | int | float | string | bytes | any`); the host type-checks the evaluated value and returns `ERR_EXPR` on mismatch. §11.1 says which values satisfy which type, and is where the one implicit conversion — int to float, when exact — is stated. Constants are trivial expressions; the Designer MAY render simple literals as plain input fields — a UI affordance, not an ABI distinction.
- `default` is an expression string in the platform's micro-Lisp (SCOPE §3.5; expression language grammar is specified separately).
- `capabilities` ⊇ imported `eio:*` namespaces minus `eio:core` (§4.3).
- Port order in `inputs`/`outputs` defines port indices (§5.2).
- `aot` lists prebuilt AOT artifacts for leaf targets published alongside the portable module. The list stays an open name pattern here — a registry may carry artifacts for targets this platform has not defined — but **an entry naming an eieio leaf target is spelled as that target's Rust triple**, which LEAF §6.2.1 fixes and LEAF §6.2 enumerates.
- Property order defines `prop_id` — appending properties is backward compatible, reordering or removing is not; this is the block's (not the ABI's) versioning concern.

The JSON Schema for the manifest ships in the repo as **`schemas/manifest.schema.json`** (draft 2020-12), beside the schemas of the other published formats. It lives at the repository root rather than inside a crate because its consumers are not Rust: the Designer's config panels, agent tooling, and editor autocomplete all read it directly.

**This section and §11.1 are the normative prose; the schema is a structural gate derived from them.** Five §11.1 rules cannot be expressed in JSON Schema and the schema therefore does not enforce them: uniqueness of port and property names — `uniqueItems` compares whole items rather than a chosen property, so it would coincidentally catch two *identical* ports and stop doing so as soon as a port carries a second field; rejection of duplicate JSON object keys; whether a property `default` parses as an expression; whether a signal-independent `default` evaluates to a value its declared `type` admits; and the document size bound. A manifest that validates against the schema MAY still be rejected for one of those, and a host MUST apply the prose regardless of whether a document validated. The repository tests the boundary in both directions, so the subset stays a documented one rather than an unnoticed gap.

The manifest is also the Designer's config-panel source and the agent-tooling surface (SCOPE §4) — descriptions are user-facing documentation and SHOULD be written as such, in the schema as much as in the manifest.

### 11.1 Validation rules

A manifest that violates any rule below is invalid and MUST be rejected. "Reject" means refuse the whole document: a partially accepted manifest would leave port indices and `prop_id`s ambiguous, and those are load-bearing (§5.2).

**Presence.** `name`, `version`, and `abi` are REQUIRED; within a property, `name` and `type` are REQUIRED. Every other field is OPTIONAL, and an absent one means exactly:

|Field|Absent means|
|---|---|
|`description` (block and property)|no description|
|`capabilities`, `inputs`, `outputs`, `properties`, `aot`|empty|
|`targets`|`["wasm32-unknown-unknown"]`|
|`required` (property)|`false`|
|`default` (property)|no default; see the semantics below|

**Strictness.** Unknown fields MUST be rejected, at the top level and within every nested object (`abi`, and `inputs`/`outputs`/`properties` entries). A typo'd `"capabilites"` that silently granted nothing is the failure this prevents. This costs no forward compatibility: additive schema growth is a minor bump (§12), and a manifest declaring a minor above the host's is rejected regardless.

Duplicate JSON object keys MUST be rejected rather than resolved last-wins. A present field MUST hold a value of its declared type, and `null` is not a spelling of absence: `"default": null` MUST be rejected, and a property with no default omits the field. One way to say a thing.

**Names.**

|What|Pattern|Bound|
|---|---|---|
|`name` (block), `targets[]`, `aot[]`|`^[a-z0-9]([a-z0-9._-]*[a-z0-9])?$`|≤64 bytes|
|port names, property names|`^[a-z0-9]([a-z0-9_-]*[a-z0-9])?$`|≤64 bytes|

Port and property names exclude `.` deliberately: service files address connections as `from.port -> to.port` and carry property names as TOML bare keys (DAEMON §2), and a dot is ambiguous in both. The block `name` admits `.` because it is a registry reference component (SCOPE §3.6).

These are stated as regexes so that one rule reaches every surface: `manifest.schema.json` publishes them as `pattern`, and the SDK, `cargo eio`, and the Designer validate against the same expression rather than each inventing an approximation.

**`err` is RESERVED as a port name**, in `inputs` and in `outputs` alike, and a manifest declaring one MUST be rejected. §6.4 gives every block an error port it does not declare, and a service file addresses that port by this name; a block declaring its own would make the name mean two things there, and neither reading is safe to guess.

Reserved in both directions even though §6.4's port is an output, because the collision is symmetric. A host resolves a connection's destination by name *before* consulting the block's declared inputs — it has to, since `err` is never among them — so an input called `err` is one no service file could ever wire to. Refusing the block says so at build time instead of shipping it with a port that silently does nothing.

The reservation is a forbidden string rather than an exclusion folded into the pattern above, deliberately: the patterns stay statements about a name's *shape*, which is what keeps them publishable verbatim as a schema `pattern`. Property names are unaffected — properties are their own namespace and reserve nothing.

`version` MUST be a valid Semantic Versioning 2.0.0 string: `MAJOR.MINOR.PATCH`, each numeric component without leading zeros, with an optional `-<pre-release>` and `+<build>`.

**Uniqueness.** Names MUST be unique within `inputs`, within `outputs`, and within `properties`. Those are three separate namespaces — port indices are per-direction (§5.2) — so a block MAY have an input and an output that share a name. `capabilities`, `targets`, and `aot` MUST NOT contain duplicates.

**Closed sets.** `capabilities` entries MUST each be one of `state`, `timer`, `gpio`, `i2c`, `http`. `core` MUST NOT appear: `eio:core` is always available and requires no capability (§7.0). A property's `type` MUST be one of `bool`, `int`, `float`, `string`, `bytes`, `any`.

**Targets.** `targets` lists the triples a block's compiled artifact was built for, and a **non-empty** `targets` MUST contain `wasm32-unknown-unknown`: every block that ships bytes ships the portable module (§1), and `aot` entries are additions published alongside it, never replacements.

`targets: []` is a distinct, legal claim rather than a shorter list — it says **there is no compiled artifact**. That is what a **host-implemented** block looks like: DAEMON §6's `publisher` and `subscriber` are real blocks in the palette with real manifests, and there is no `.wasm`, no triple they were built for, and nothing for a leaf tier to flash. A host therefore **MUST refuse to load a module whose manifest declares `[]`**, because a manifest claiming no artifact cannot be describing the bytes it arrived with. The requirement attaches to being loaded from bytes, not to being a manifest: the document is valid, and the contradiction only exists once something hands it a module.

**Default expressions.** A `default`, when present, MUST parse and MUST pass static analysis (EXPR §10) — the same configure-time gate a service-supplied expression gets, so a manifest cannot ship a default naming a function that does not exist. A default MAY be signal-dependent; it is an expression like any other property value, not a constant.

**Property types.** A property's declared `type` is a constraint on the *evaluated* value, checked every time the expression is evaluated (§7.1). These values satisfy it:

|`type`|Satisfied by|
|---|---|
|`bool`|a bool|
|`int`|an int|
|`float`|a float, **or an int whose value is exactly representable in `binary64`**|
|`string`|a text string|
|`bytes`|a byte string|
|`any`|any value in the §6.3 space|

A host MUST encode a promoted int as a float, so a guest reading a `float` property always decodes a float and never has to handle both. Failure is `RESULT_TYPE` (EXPR §8), surfaced through the ABI as `ERR_EXPR` (§7.1).

Promotion goes one way and only when it is exact. A float never satisfies `int`: the conversion loses the fractional part, and `(int x)` is how an expression asks for it. An int satisfies `float` only when no information is lost, which is a question about significant bits rather than about magnitude — a `binary64` significand holds 53 of them and the exponent absorbs trailing zeros, so `2^62` is exactly representable while `2^53 + 1` is not. Hosts MUST decide it that way rather than by converting and converting back: `i64::MAX` rounds up to `2^63`, which a saturating float→int conversion returns as `i64::MAX`, reporting an exactness that did not happen. Where an int is not exactly a float, `(float n)` is how an expression asks for the rounding, which is where EXPR §7.3 documents the loss. This is the same shape as every other conversion decision in EXPR §7.8: one implicit rule, exact, with the lossy reading spelled out by hand.

**Default type-checking.** A `default` that is **signal-independent** (EXPR §10) MUST be evaluated at manifest-validation time, and its value MUST satisfy the declared `type`; a manifest whose folded default contradicts its own declaration MUST be rejected. `"type": "int"` with `"default": "true"` cannot ever produce an int, so it is a defect in the document rather than a configuration failure waiting to happen.

Two limits on that, both deliberate:

- A **signal-dependent** default MUST NOT be evaluated — there is no signal to evaluate it against — and is checked per signal at run time like any other property expression.
- A default that **fails to evaluate** is NOT a manifest defect. An evaluation failure is a per-signal outcome (§7.1), and budgets are host configuration (EXPR §9), so rejecting a manifest for one would make a document's validity depend on which host read it — two hosts MUST agree on whether a manifest is valid. `"default": "(/ 1 0)"` is therefore a valid declaration that fails with `ERR_EXPR` at configure time.

One consequence follows from EXPR rather than from anything here: the expression language has no `bytes` literal and no builtin that produces one (EXPR §3.2, §7), so a `bytes` property cannot have a signal-independent default at all. Its default, if it has one, reads the signal.

**`required` and `default`.** `default` is the value the property takes when the service does not supply one; it is what makes an instance configurable without the deployer touching every field. `required: true` means configuration MUST fail when the property has no value at configure time — from the service file or from `default`, either satisfies it. `required` is therefore the enforceable half of the pair and the two do not constrain each other in the manifest: any combination of `required` and `default` is a valid declaration, including both (a required property with a suggested starting value, as in the example above). The remaining combination — not required, no default, and nothing supplied — is equally valid, and what `prop` answers for it is in §7.1: `ERR_NOT_FOUND`, with the `prop_id` unchanged.

**Size.** A manifest document larger than the host's configured maximum MUST be rejected before parsing. The bound is host configuration like every other budget (EXPR §9): hosts MUST accept documents of at least 8 KiB, and 64 KiB is the reference default. Manifests are read from registries and from module custom sections, so the bound is a trust-boundary limit, not a style guide.

---

## 12. ABI versioning

- `eio_abi_version() -> i32` returns `(major << 16) | minor`. This document specifies **1.0**.
- Host policy: reject `major` mismatch; accept `minor` ≤ host's minor (pure-additive guarantee).
- Additive changes (new host namespaces/functions, new optional exports, new error codes): minor bump. Old blocks never import the new functions, so nothing breaks.
- An **optional instance-descriptor key whose absence is meaningful** (§5.2's `max_emission_bytes`) is not even that: a host that omits it is saying the thing a host without the key would have meant, and a block that never looks for it reads the same document it always did. So it is additive without a bump, and this document is still **1.0**. A key whose absence had *no* meaning would be a different matter — a block could not tell "not bounded" from "not told" — and would need one.
- **A host limit written down that hosts already had** is likewise not a change to this document's number. §4.1's page ceiling is the case: a host has always been free to fail an instantiation it could not allocate memory for, and what §4.1 adds is *when* it must say so and what it must not do instead. No export moves, no import appears, and no conforming block becomes non-conforming — a block refused by a ceiling was already a block that host could not run, and the difference is that it is now refused at load rather than discovered later. Still **1.0**.
- Changes to memory rules, lifecycle, calling conventions, sentinels, or the status convention: major bump.
- The manifest's `abi` field MUST match the module's exported version; hosts MAY reject on mismatch (the module is authoritative).

---

## 13. Conformance

The monorepo carries:

1. **Reference harness** (§13.1) — a minimal host (wasmtime-based) that drives a module through the full lifecycle with scripted deliveries, property tables, and fault injection (undersized buffers, `ERR_THROTTLED` state, capability denial).
2. **Golden blocks** (§13.2) — small blocks exercising each contract area: pure transform, multi-port routing (filter), timer emitter (simulator), stateful counter, GPIO echo, hostile blocks (spinner, allocator-liar, reentrancy-prober, oversize-emitter).

Both the daemon and the leaf runtime MUST pass the harness against the golden blocks. Divergence between the two hosts is a conformance bug by definition.

### 13.1 Reference harness

The harness drives a **host**; the wasmtime reference implementation is one host and not the subject. A conformant host MUST therefore be drivable by it, which costs exactly two things: a way to instantiate a module, and a way to call its exports and read and write its linear memory. Anything more the harness needed of a host would be a requirement this specification does not make.

A host also states which capability namespaces (§7.2–§7.6) it implements, and the harness asks *before* instantiating. Only `eio:core` is promised unconditionally (§7.0); every other namespace is a question about the device, settled at deploy validation (SCOPE §3.3). A scenario needing one the host does not implement is reported **skipped, with the namespace named** — never passed over, because a suite that counted an unreachable scenario as a pass would claim coverage the platform does not have. Asking beforehand is also what keeps the report legible: a module importing an unimplemented namespace fails to *link*, and a link failure reads as "this module is broken".

A host also states **whether it enforces §10's budget**, and one that does not has the scenarios expecting a budget death reported **skipped, with the death named**. §10 requires a budget of every host, but a budget is built on an engine's ability to stop a call that is already running, and an engine binding may have no such entry point — §10's two obligations name what one costs. This skip is the same kind as the capability skip above it and the unrefused-proposal skip below: not an excuse and not a pass, a divergence made visible, and §13's rule applies to it in full. It exists because the alternative for an unbudgeted host is not a red suite but a hung one — a block that never returns never returns — and a suite that hangs reports nothing at all. LEAF §4.5 is a binding that answers so today, and says what it is missing.

The reference binding is written **independently of any production host's**, deliberately. A harness sharing the daemon's engine binding could only ever report that the binding agrees with itself, and "both hosts MUST pass" would be a statement about one implementation.

**Scenarios are data, not code.** A scenario is a document a host in any language can read, because the leaf runtime and every later host MUST run the same ones — a suite written in the host's own language can only test that host. What the *harness* is written in is not constrained; only the suite is.

A scenario fixes:

- The module, and the manifest to validate it against when the module carries none (§4.4).
- `instance_id`, the `limits` the descriptor publishes (§5.2, §9.7), and the property values a service would supply. **Ports and `prop_id`s come from the manifest**, resolved by §11.1's `required`/`default` rule; a scenario restating them would be a second numbering free to disagree with the first.
- The execution budget (§10) — fuel and wall-clock deadline — because exhaustion is a fault a scenario injects rather than a property of the machine it runs on.
- A sequence of **steps**: one lifecycle call each, walking §5.1 from `eio_configure` to `eio_stop`.

#### Load-time refusal

A scenario may instead assert that the module is **never loaded at all**. It then carries `refuses` and no steps, and publishes no `limits` — a module that never instantiates has no descriptor to read them from, and §9.7's rule that a host may not choose those numbers is untouched by a scenario that has none.

```json
{ "refuses": { "proposal": "tail call", "names": "tail call", "layer": "loader" } }
```

`proposal` is the feature §4.3 refuses, and is what the report and any skip say. `names` is optional: when present, the rejection MUST contain it, matched case-insensitively as a substring so an engine stays free to rephrase the sentence around it. It is omitted for a proposal no engine names — **extended const** today — because a vector asserting a name nothing produces would fail every conformant host, and the scenario's `note` is where that is recorded.

**A refusal is not always about what a module contains.** Where it is about how much memory the module declares (§4.1), `refuses` carries `memory_pages` — the per-instance page ceiling the scenario configures the host's loader with — in place of `proposal`, and exactly one of the two is present:

```json
{ "refuses": { "memory_pages": 1, "names": "2 page(s)", "layer": "loader" } }
```

Such a scenario is always a `loader` refusal and therefore always names its numbers, because no engine has an opinion about a ceiling that is host configuration: the check is the loader's, the same code on every host, and no host has a gap to declare. It is also the only place a scenario configures a host limit that no descriptor publishes, which is §9.7 rule 10 and not an inconsistency — a ceiling a block could read would be a ceiling a module had already got past.

`layer` is which of §4.3's two mandatory layers must do the refusing: `engine` (the default) or `loader`. It is stated rather than inferred from whichever layer answered first, because "either one refused it" is the assertion a creeping second definition of the accepted set would satisfy — a loader that began refusing SIMD would pass the SIMD scenario while §4.3 still said the engine owns that proposal, and nothing would say otherwise. A `loader` scenario therefore also asserts what an `engine` scenario asserts of its fixture: that the *other* layer had no opinion.

The layer decides two things:

- **A `loader` refusal is never skipped, and always names the proposal.** It is the same code on every host, so no host has an engine gap to declare and none is excused from the name — which is why the three measured gaps of §4.3 are the only refusals in the suite whose *name* is asserted on the leaf engine.
- **An `engine` refusal turns on two answers a host gives about its engine**, neither of them an opinion about this specification:
  - **A host whose engine does not refuse the proposal at all** reports the scenario **skipped, with the proposal named**, exactly as an unimplemented capability is. This is not an excuse and it is not a pass: it is a divergence made visible, and §13's "divergence between the two hosts is a conformance bug by definition" applies to it in full. It is recorded here because the alternative — a suite that goes red on a known, tracked gap — gets muted, and a muted suite pins nothing. No host answers so today; wasm3 did, for the three proposals that are now refused in the loader's layer instead.
  - **A host whose engine names no proposal** has `names` skipped for it while the refusal itself is still asserted. wasm3 is that host for all six it refuses.

What a refusal scenario must not be is a module that is broken in some *other* way, or the vector would pass for the wrong reason. Every refusal fixture is therefore a block valid under §4.1 and §11 — every export present with the right signature, an `eio:manifest` section that agrees — so that the named proposal is the only thing left to object to. The suite asserts that of each fixture rather than trusting it: for an `engine` scenario by requiring `validate` to accept the module, and for a `loader` scenario by requiring the refusal to name the proposal, which no other flaw in a fixture would produce.

**Batches are canonical CBOR, written as hex.** §6.3.1 admits exactly one encoding of any batch, and pinning bytes is half of what this suite is for. A JSON spelling of a batch would be a second, lossier data model — it has no byte string, and it resolves duplicate keys before rule 7 can reject them.

**The host is deterministic.** `time_unix_ms`, `time_mono_ms` and `rand` are fixed or seeded by the scenario. §7.0 mediates all three precisely so that a host holds this lever; a conformance run that is not reproducible cannot pin a divergence to a change.

#### Observation vocabulary

What a scenario may assert about a step, and therefore what a host MUST make observable to whatever embeds it:

|Observation|Section|
|---|---|
|The status a callback returned, or the code it was rejected/refused with|§8, §5.1|
|The death and its kind: trap, fuel, deadline, engine|§8, §10|
|A delivery the *host* declined — distinct from any status, because the guest was never called|§9.7|
|Emissions, per port, in order, as canonical bytes|§6.2, §6.3.1|
|Guest→host calls, in order, by name|§7|
|Property **evaluations**, counted separately from `prop` calls|§7.1|
|`log` lines and `error` details|§7.0|
|The allocation ledger below|§9|

`prop` calls and property evaluations are separate numbers on purpose: §7.1 requires the result to be cached for the duration of the callback, so grow-and-retry is two calls and one evaluation. A single count could not tell a compliant host from one that re-evaluates.

#### Fault injection

A scenario may inject any of these, and a host MUST behave as the cited section says under each:

|Fault|How it is injected|Expected|
|---|---|---|
|Undersized guest buffer|an answer larger than the guest's first `(buf, cap)`|grow-and-retry (§8), one evaluation (§7.1)|
|`ERR_THROTTLED` state|scripted refusal of `state_put`|§7.2; the block backs off, the instance lives|
|Capability denial|every function of a namespace answers `ERR_CAPABILITY`|§8, and the instance lives|
|Oversize delivery|`max_payload`/`max_batch` set below the batch|refused, guest never called (§9.7)|
|Oversize emission|the block emits past `max_payload`|`ERR_LIMIT` to the emitter (§6.2)|
|Budget exhaustion|fuel or deadline set below what the callback needs|a trap; the instance is DEAD (§10)|
|A lying allocator|`eio_alloc` answers misaligned, zero, or out of bounds|`0` is a refusal and survivable (§9.5); the other two are death (§9.6)|

The undersized-buffer fault is worth stating exactly, because it cannot be what it sounds like: a host does not choose a guest's buffer size and cannot make one too small. What it can do is answer with a value that does not fit the buffer it was handed, which is the condition §8's grow-and-retry exists for and the only way to reach that path from the host side.

#### The allocation ledger, and what it cannot see

Every run records every `eio_alloc` the host made for an inbound payload (§6.1): the size asked for, the pointer returned, and whether that pointer was accepted, refused as `0` (§9.5), or rejected as a lie (§9.6). Two host-side invariants are checked on every run without a scenario asking for them:

- **The host MUST NOT call `eio_free`.** Rule §9.2 makes the guest the owner from the moment the callback begins, and a host-side free would be the second owner that rule exists to prevent.
- **The host MUST NOT write into guest memory it did not allocate** (§9.1). The other writing path — a `(buf, cap)` the guest supplied in the current call — is bounded by `cap` under the size convention, so it needs no ledger to check.

What no harness can see is the *guest's* frees. `eio_free` is an export (§4.1), so a guest releasing an inbound payload calls it as an ordinary intra-module call, which no engine surfaces to its embedder. A harness claiming to check §6.1's "the guest MUST `eio_free` it" from the outside would be claiming knowledge no host has. That obligation is therefore tested from the *inside*, by a fixture that counts its own allocations and frees and refuses to stop unbalanced — a hand-written one, for the reason §13.2 gives: a block written through the SDK never sees either call, so it has nothing to count. The harness's part is to run enough deliveries for a leak to show and to report the guest's linear-memory growth across the run.

### 13.2 Golden blocks

Small blocks, each exercising one contract area:

|Block|Exercises|
|---|---|
|Pure transform|§6.1, §6.2, §7.1 — a batch in, a batch out, one property per signal|
|Filter|§5.2, §6.2, §6.4 — multi-port routing by an expression-valued predicate, and what it cannot route to the error port|
|Timer emitter|§4.2, §7.3 — emission with no inbound batch at all, and §3's `SIGNAL_NONE` property read|
|Stateful counter|§7.2, §5.1 — durable state, across an instance that did not survive|
|GPIO echo|§4.2, §7.4 — a watch, an edge, an output|

They live in `examples/blocks/`, and they are **written with the SDK** — ordinary blocks a block author could have written, built by the ordinary toolchain with no flags. That is deliberate and it is what makes them worth running: a fixture hand-written to the raw ABI proves a host can drive *that fixture*, while these prove a host can drive what the platform actually produces. It also makes them the SDK's acceptance tests, and ABI §14's litmus applies in that direction too — friction found while writing one is a defect in the SDK or in this specification, not in the block.

They are **not** the fixtures that test the harness itself. A scenario that injects a fault needs a guest that misbehaves on demand: a first buffer too small for the answer (§8's grow-and-retry), an emission past `max_payload` or to an undeclared port (§6.2's three refusals), an allocator that lies (§9.6). The SDK exists to make all three unwritable — `Ctx::emit` refuses an oversized batch before the host sees it, and an undeclared port is a compile error — so those scenarios keep hand-written `.wat` fixtures, and a golden block would make the suite test the harness with the harness's own output. `crates/conformance/scenarios/blocks/` holds them.

The guest-side allocation ledger belongs there for the same reason. §13.1 requires §6.1's "the guest MUST `eio_free` it" to be checked from the inside, and a block written with the SDK cannot check it: every allocation and free is the SDK's audited glue (SDK §4), which is the point of the SDK. The fixture that counts its own allocations and refuses to stop unbalanced is therefore a hand-written one; what the golden blocks contribute to the same question is the leak signal, linear-memory growth across a run.

And the hostile blocks, which are conformant modules behaving badly on purpose — a host MUST survive every one of them with the outcome named:

|Block|Behaviour|Host MUST|Pinned by|
|---|---|---|---|
|Spinner|never returns from a callback|kill it on fuel or deadline (§10) and survive|`blocks/spinner.wat`|
|Allocator-liar|`eio_alloc` returns misaligned, out-of-bounds, or zero pointers|discard the instance on the first two (§9 rule 6), fail the delivery on the third (§9 rule 5)|`blocks/liar.wat`, `blocks/oob.wat`, `blocks/refuser.wat`|
|Reentrancy-prober|emits from inside a callback and looks for delivery before it returns|never deliver mid-call (§6.2); the probe MUST observe nothing|`blocks/prober.wat`|
|Oversize-emitter|emits past `max_payload` and on undeclared ports|answer `ERR_LIMIT` and `ERR_INVALID_ARG` (§6.2) without reading the payload|`blocks/harness.wat`, its `probe` port|

The last column names the fixture, because a behaviour and a file are not the same thing and
two of these are not one file each. The allocator-liar is three: §9's rules 5 and 6 are three
different answers with three different outcomes, and two of them require the instance to
survive `configure` first, so one fixture switching between them would hide which case a
failure came from. The oversize-emitter is a *port* on the fixture that already probes §6.2's
refusals from the guest's side — a second module making the same three assertions would be a
second place for them to drift.

"Without reading the payload" is stated as a pointer outside linear memory, which is what
makes it checkable: a host that consulted the range before the length would fault on the read
instead of answering `ERR_LIMIT`, and the block would say so.

---

## 14. SDK requirement (informative)

Almost no one writes against this ABI raw. The `block-sdk` Rust crate is developed in lockstep with this spec:

- Derive macro over a config struct + a `Block` trait → generated exports, allocator, CBOR (de)serialization, typed property accessors wrapping `prop` with the grow-and-retry loop, safe wrappers for every host namespace.
- All `unsafe` in the block ecosystem lives inside the SDK's audited glue; block authors write safe Rust exclusively (design discussion §3 resolution).
- **Litmus rule: if a contract in this spec is awkward to wrap ergonomically in the SDK, the spec is wrong.** SDK friction findings feed back as spec amendments before 1.0 freezes.

---

## 15. OPEN items (tracked in SCOPE.md)

This spec deliberately does not decide, and is compatible with any resolution of:

- ~~Cross-device delivery guarantees, ordering, backpressure~~ — **settled**: at-most-once, per-publisher ordering, no cross-device backpressure (SCOPE §3.4). No ABI change was needed, which is the point: `emit`'s enqueue semantics and `ERR_LIMIT` accommodated every candidate, so the decision cost this document nothing.
- ~~Pub/sub transport and broker topology~~ — **settled**: MQTT behind DAEMON §7's bridge, elected daemon-class broker (SCOPE §3.9). Still no ABI surface — `publisher`/`subscriber` remain ordinary blocks and the transport stays behind `emit`/delivery, which is why a transport swap cannot reach this spec.
- Supervision policy on instance death — SCOPE §3.13. (ABI defines only: trap = death, re-instantiation = fresh configure.)
- Transport security / node auth — SCOPE §3.11. (No ABI surface.)
- Metrics — SCOPE §3.12. (Likely additive `eio:core` functions or pure host-side counters; minor version either way.)
- Expression language grammar — SCOPE §3.5 specifies the constraints (pure, bounded, `no_std`, per-signal); the grammar gets its own spec. This document only fixes the _evaluation protocol_ (§7.1).

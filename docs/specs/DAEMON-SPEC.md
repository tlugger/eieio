# Daemon Specification

**Status:** Draft 1 — high-level; intended for in-depth expansion per-subsystem. **Depends on:** SCOPE.md, ABI-SPEC.md, EXPR-SPEC.md. **Markers:** Settled decisions are stated plainly. **PROPOSED** = drafted here, awaiting ratification. **OPEN** = tracked in SCOPE.md.

The daemon is the daemon-class node runtime (SCOPE §3.7): a single compiled Rust binary that establishes a node, executes services, and exposes the management API. This spec covers its internal architecture; the leaf runtime shares the starred (★) subsystems and is specified separately later.

---

## 1. Crate architecture

Monorepo workspace. The load-bearing split is **host-core vs daemon**: everything the leaf runtime will also need lives in `host-core` and MUST stay `no_std`-compatible (alloc allowed).

```
crates/
  host-core/     ★ ABI implementation: lifecycle driver, memory conventions,
                   status/size protocol, capability validation, router core,
                   property resolution (ABI §11.1's required/default rule)
  expr/          ★ Expression language: parser, static analysis, interpreter,
                   budgets (EXPR-SPEC). no_std. Also compiled to WASM for
                   Designer in-browser linting (DESIGNER-SPEC §5)
  signal/        ★ CBOR batch/signal encode-decode (minicbor). no_std
  manifest/      ★ Manifest schema types, parsing, import-section cross-check
  daemon/          Binary: tokio runtime, wasmtime engine, OCI client,
                   management API, state store, pub/sub bridge
  block-sdk/       Guest-side (SDK-SPEC); published as `eio-sdk`
  cargo-eio/       Block build/publish tooling (SDK-SPEC §5)
  conformance/     Reference harness + golden blocks (ABI §13)
```

The expression conformance vectors are **not** here: they are data files at the repository root in `expr-tests/` (EXPR §11), because a host written in another language must be able to consume them without building a Rust crate. `conformance/` holds the ABI harness, which is Rust by nature — it drives a WASM engine.

★-marked crates are shared with the leaf runtime and MUST stay `no_std`-compatible (`alloc` allowed). `daemon` and `cargo-eio` are `std` binaries; `block-sdk` is `no_std` by necessity (it compiles into guests).

Conformance implication: `host-core` driven by wasmtime (daemon) and by WAMR (leaf) MUST pass the same harness — the shared crate is how divergence is prevented structurally, not just tested for.

**Where a rule lives follows from what it is about, not from who happens to call it.** ABI §11.1's `required`/`default` precedence is the worked example, because all three plausible homes were arguable. Not `manifest`: a manifest describes what a *block* says about itself, and a deployment's supplied values are not that. Not `daemon`: the rule is pure ABI semantics with no engine and no configuration *format* in it, and leaving it there would mean the leaf runtime — whose configuration source is shaped differently — reimplementing the precedence from scratch, which is the silent divergence this split exists to prevent. So `host-core`, which is also the only crate that *can* hold it: the rule consumes `manifest`'s `Manifest` and produces `host-core`'s `PropertySource`, and the dependency runs host-core → manifest. The daemon reaches it from `--prop` flags today and from service files later; the leaf reaches it from whatever it reads. One implementation, two hosts.

**Naming.** Directory names are exactly as listed above. Package names are `eio`-prefixed and imported with underscores:

|Directory|Package|Import path|
|---|---|---|
|`host-core/`|`eio-host-core`|`eio_host_core`|
|`expr/`|`eio-expr`|`eio_expr`|
|`signal/`|`eio-signal`|`eio_signal`|
|`manifest/`|`eio-manifest`|`eio_manifest`|
|`daemon/`|`eio-daemon`|—|
|`block-sdk/`|`eio-sdk`|`eio_sdk`|
|`cargo-eio/`|`cargo-eio`|—|
|`conformance/`|`eio-conformance`|`eio_conformance`|

`cargo-eio` is the sole exception: cargo discovers subcommands by binary name, so it cannot be prefixed differently. No crate overrides its `[lib] name` — package name and import path differ only by the `-`→`_` substitution cargo already performs.

The prefix is not cosmetic. `eio-sdk` publishes to crates.io (SCOPE §7.1), and a published crate cannot depend on path-only crates, so every crate reachable from it — `signal` (SDK-SPEC §2) and `expr` (SDK-SPEC §6.1, `TestHost` evaluates with the real interpreter) at minimum — MUST carry a publishable name. The bare names `signal`, `expr`, and `manifest` are already taken on crates.io.

**JSON parsing in `manifest`.** The manifest is JSON (ABI §11) and `manifest/` is ★, so the parser has to work with `alloc` and no `std`, on a target with no atomics. `serde` + `serde_json`, both with `default-features = false, features = ["alloc"]`, do: verified building for both `just check-nostd` targets. They are used with `deny_unknown_fields`, which is what makes ABI §11.1's strictness rules fall out of the derive rather than out of hand-written checks — duplicate keys, unknown fields, unknown enum variants and type mismatches are all reported with a line and column. `serde_json_core` was the obvious alternative and is unusable here: it deserializes into fixed-size buffers and cannot own strings, which a manifest of arbitrary port and property names needs. A hand-written parser would trade a well-tested dependency for the same feature list reimplemented, and the leaf runtime gains nothing from it.

The daemon depends on the same `serde_json` with `std` enabled (§12's JSON batch input). That does not reach `manifest`: `just check-nostd` builds each ★ crate for a bare-metal target as its own package, so cargo unifies features across that build and not across the workspace. The gate is what makes the claim true rather than an assumption about resolver behaviour.

---

## 2. On-disk layout (source of truth, SCOPE §3.8)

**PROPOSED:**

```
/etc/eieio/                      (or --data-dir)
  node.toml                    node identity, listen addr, limits, budgets
  auth/                        node token, TLS material (OPEN, SCOPE §3.11)
  services/
    <service>.toml             one service definition per file
  blocks/                      OCI pull cache: <name>/<version>/block.wasm
                               (+ precompiled wasmtime artifact, keyed by engine hash)
  state/                       eio:state backing store
```

- **Service definition format: TOML** (PROPOSED — human-first, comment-friendly, agents handle it fine; JSON Schema published for the equivalent structure regardless). One file = one service = the deployable unit.
- Service file contents: service name, block instances (`name`, `block` OCI ref, properties as expression strings), connections (`from.port -> to.port`), an optional `[ui]` table the daemon MUST ignore (Designer layout annotations — DESIGNER-SPEC §4).
- The management API is a thin CRUD layer over these files plus lifecycle commands. `PUT` writes the file, validates, and reports; it never holds state the file doesn't. Editing files directly on disk (or via git) and calling `POST /services/{s}/reload` is a first-class, supported path — this is the GitOps/agent story.

## 3. Boot sequence

1. Load `node.toml`; bind API listener.
2. For each service file: parse → resolve block refs against cache (pull missing, §4) → validate (manifest/import cross-check per ABI §4.3, capability-vs-node check per SCOPE §3.3, expression static analysis per EXPR §10, connection graph check: ports exist, no dangling refs).
3. Start services marked `autostart = true`. Validation failure of one service MUST NOT prevent the daemon or other services from starting; the failed service surfaces as errored via API.

## 4. Block manager

- Pulls OCI artifacts (SCOPE §3.6) by reference; verifies digest; **PROPOSED:** verifies cosign signature when the registry entry carries one, policy knob in `node.toml` (`require_signed = true|false`).
- Load-time validation is exactly ABI §4: exports present, imports ⊆ `eio:*`, imports ⊆ manifest capabilities, ABI version accepted, embedded manifest (if present) matches registry manifest.
- Caches wasmtime-precompiled modules keyed by (digest, engine config hash) — cold-start matters on a Pi.
- Airgap/offline: cache is authoritative when the registry is unreachable; a service whose blocks are cached starts fine offline.

## 5. Executor

The runtime embodiment of ABI §1 invariants:

- One wasmtime `Store` + instance per block instance. **One tokio task per block instance** owning the store (stores are `!Sync`; ownership model and serialization requirement align perfectly): the task loops over a bounded mpsc mailbox of work items (`Deliver{port, batch}`, `Timer{id}`, `GpioEdge{..}`, `HttpDone{..}`, `Stop`), invoking guest callbacks strictly sequentially. Serialized invocation falls out of the architecture rather than a lock.
- Fuel **and** epoch interruption per callback (ABI §10); budget from `node.toml`. Trap/exhaustion → instance DEAD → supervision (§8).
- Host functions (`eio:*`) implemented against the mailbox/router: `emit` enqueues to the router (never delivers inline — ABI §6.2); `prop` hits the expression engine with the callback's current-batch context; async capabilities post completions back into the mailbox.

**Placement: one OS thread per instance.** §5.1's store affinity note left this as "a `LocalSet` or a thread each"; it is a thread each, and each such thread runs a current-thread tokio runtime with a `LocalSet` carrying the instance's one task — so the task above is a task, and the thread is what bounds a hostile block's blast radius. The deciding case is ABI §10's spinner: a guest that spins holds its thread until a budget kills it, and on a shared `LocalSet` that is every other instance and the management API held with it. The cost is a thread per instance, which is a daemon-class cost; the leaf tier has its own runtime and is not bound by this choice.

**Mailbox bound and what a full one means.** The mailbox is bounded and its depth is host configuration, with no floor. The executor offers a sender both answers to a full one and takes neither on the sender's behalf: a *waiting* send (backpressure, which propagates to whoever is producing too fast) and a *refusing* send that hands the work item back. Which one a connection uses is §6's per-connection overflow policy; the cross-device question is OPEN (SCOPE §3.4) and is not settled by the executor having a bound.

**Every sender gone is a stop.** A mailbox no sender can reach again cannot receive work, so the instance runs `eio_stop` (ABI §5.1 step 5) rather than idling. An instance is therefore never left running with nothing that can reach it. This is why a routing instance holds a sender only for the receivers it actually emits into (§6): were it given the whole service's mailboxes, every instance would keep every other reachable and no serviced instance could ever stop this way. Instances in a cycle do hold each other's, and stop on an explicit `Stop`.

**Inbound is bounded; outbound observation is not.** What an instance produces — callback statuses, `error` details, expression failures, emissions, its death or its clean stop — leaves on an unbounded stream, because an observer that could stall a guest by reading slowly would be a worse defect than a queue that grows. Backpressure belongs on the inbound side, where slowing the sender down is a correct response. Routed emissions are not this stream: they travel through the *destination's* bounded mailbox (§6), which is where a slow consumer should be felt.

**Death is an event, not a log line.** An instance that dies reports the trap and its kind (ABI §5.1 step 6, §10) on that stream; supervision (§8) is its consumer. Until §8 exists it is logged and the thread ends.

### 5.1 Engine binding

`host-core` drives a guest through its `Engine` trait; this is the daemon's only implementation of it, and nothing outside it knows the engine is wasmtime. The leaf runtime writes the equivalent file against WAMR or wasm3, and the driver above both is the same code — that is what makes "divergence between the two hosts is a conformance bug" (ABI §13) enforceable rather than aspirational.

**Feature set.** wasmtime is depended on with `default-features = false` and `cranelift, runtime, std, anyhow, backtrace`. Threads, the component model and GC are therefore *compiled out* rather than switched off, which is a stronger reading of ABI §1's "core WASM only": with the features absent, the corresponding `Config` setters do not exist to be forgotten. `anyhow` gives `wasmtime::Error` its `From` conversion into `anyhow::Error`, so `?` works throughout the daemon — a conversion, not an alias: `anyhow::Context` does not apply to a wasmtime result, so a wasmtime error is given context by converting it first. `backtrace` earns its place because a trap is an instance's death (ABI §8) and the log line is all anyone gets.

**Core WASM MVP, and nothing past it.** ABI §4.3 places MVP conformance on the engine and nowhere else — `manifest` does no WASM feature gating — so this configuration is the only thing standing between a block that uses a post-MVP proposal and a leaf runtime that will refuse it at flash time.

The configuration states it *subtractively*: every proposal wasmtime knows of is disabled, and then exactly the MVP set is re-enabled. Not a list of `wasm_*(false)` calls, for two reasons.

- A list is a closed statement about a moving target. Whatever proposal a later wasmtime enables by default is admitted silently on the next `cargo update`, and blocks using it would run here and be refused by wasm3 — the divergence §1 exists to prevent, arriving through the one door the shared crates do not watch. Subtracting from "everything" refuses it instead, on a host nobody has touched.
- A list is order-sensitive in a way nothing checks. wasmtime rejects a `Config` that disables `simd` while `relaxed_simd` is still enabled, so the setters would have to be called in dependency order; the subtractive form never meets the question.

The MVP set is wasmparser's own, less its `GC_TYPES` flag. That flag gates the `externref`/`anyref` *types* rather than any proposal, and wasmparser folds it into `MVP` only so the wider sets need not repeat it; a wasmtime built without the `gc` cargo feature — this one, per the feature set above — refuses to build an engine at all while it is set. The two decisions agree only once it is removed. `FLOATS` stays enabled: MVP has floating point, and so does `expr`.

So what a block may use is the 2017 MVP. Refused, among everything else: SIMD and relaxed SIMD, bulk memory, multi-value, tail calls, sign extension, saturating float-to-int, reference types, multi-memory, memory64, extended const, exceptions, GC, threads, and the component model — the last three by the feature set as well, which is why no `Config` call can restore them.

**What MVP-only costs a block author.** A stock `cargo build --target wasm32-unknown-unknown` emits `memory.copy` for any sizeable move, which is the bulk-memory proposal, so an unadorned Rust block is refused here. `-C target-feature=-bulk-memory` is enough to bring it back to MVP — measured on rustc 1.97, and the only flag needed — which makes it `cargo eio build`'s to supply rather than a block author's to remember. Hand-written `.wat` needs nothing.

**Host functions reach the engine through the store, not the linker.** wasmtime wants host functions before instantiation and wants each of them `Send + Sync`; `host-core`'s `HostFn` is a boxed `FnMut` over `Rc`-shared state and is neither, because ABI §1.2 gives an instance one caller at a time and nothing needs atomics. So the linker defines the `eio:core` functions once, with ABI §7.0's exact signatures, and each definition captures only a slot index; the real handlers live in the store's data and `register` puts them there. Two consequences worth stating:

- `register` works *after* instantiation, which is the order `host-core` expects — build an instance, register what its capabilities call for, hand the whole thing to the lifecycle driver.
- Import signatures are checked by the engine at link time, which is exactly where ABI §4.3 puts them. The `manifest` cross-check is a superset "in namespaces and names only"; a module importing `eio:core` `log` with the wrong arity fails to instantiate.

A host function reaches guest memory through `Memory::data_and_store_mut`, which yields the bytes and the store's data from one disjoint borrow. The memory borrow ends with the call, which is ABI §9.3 — "host MUST NOT retain guest pointers past the call" — as a lifetime rather than as a rule.

**Export presence is resolved once, at instantiation.** `Engine::has_export` takes `&self` while wasmtime's export lookup needs `&mut Store`, and the answer cannot change for the life of an instance. The exported functions and their result arities are read off the module and kept.

**Trap classification.** Every arm discards the instance — ABI §5.1 offers no state to return to that is not "discard it" — but which death it was is what supervision (§8) and the operator's log need:

|wasmtime|`host-core`|ABI|
|---|---|---|
|`Trap::OutOfFuel`|`TrapKind::Fuel`|§10, execution budget exhausted|
|`Trap::Interrupt`|`TrapKind::Deadline`|§10, wall-clock deadline (epoch interruption)|
|any other `Trap`|`TrapKind::Trap`|§8, a guest trap|
|not a trap|`TrapKind::Engine`|§5.1 step 6, the engine or a host function failed|

**Budgets are armed inside `Engine::call`.** ABI §10's per-callback budget is refreshed by the engine binding rather than by the lifecycle driver, because the driver is `host-core`'s and knows nothing about fuel. `call` is the one place every guest entry passes through, so arming it there is exhaustive by construction — `eio_alloc` included, which is a guest call like any other and just as capable of spinning. Instantiation is armed too: a store with fuel metering enabled starts with none, and module initialisation (ABI §5.1 step 1) is guest code, so an unarmed store kills every block on the way in. Each entry gets the whole budget rather than a share of one: §10 budgets a *callback*, so nothing is banked and nothing is carried over.

**Both budgets, not either.** Fuel bounds *work* and is deterministic — the same block given the same batch dies at the same instruction on every run, which is what makes a fuel death reproducible rather than a thing that happens on a busy machine. It says nothing about the leaf tier, whose watchdog counts something else entirely; ABI §10 does not make budgets comparable across hosts, only mandatory. Epoch interruption bounds *wall-clock time*, which is what an operator actually promised, and it is the only one that sees a callback blocked in a host function, where no fuel is consumed at all. Implementing one would leave half the trap table above unreachable. Epoch interruption needs the epoch advanced by somebody, so the engine owns one ticker thread — per engine, not per instance — holding a weak handle, so that dropping the last engine is what ends it. Its period is the resolution of every deadline: a deadline is rounded up to whole ticks, and the ticker's phase is unrelated to when a guest was entered, so a deadline is enforced within one tick either side of what was asked for.

**Store affinity.** `Store<T>` is `!Send` here, because the handlers and the property context are `Rc`-shared. That is the ABI showing through rather than an accident, and it is what forces §5's placement decision: an instance must be *built* on the thread it will live on, so the executor hands a thread the ingredients rather than a finished instance. Never a work-stealing pool.

## 6. Router

Owns the service graph: the connection table, fan-out (duplicate batch per receiver — nio semantics), and delivery into destination mailboxes.

**The table is ★-shared; the delivery is not.** Which `(instance, output port)` reaches which `(instance, input port)`, the resolution of a service's *names* into the port indices ABI §5.2 fixes, and the duplication of a batch per receiver have no engine and no queue in them, so they live in `host-core` (§1) and the leaf runtime routes with the same code. What is host-specific is what a queue is and what a full one means.

Endpoints are indices, resolved once at build time, because ABI §5.2 makes the descriptor's name lists *be* the numbering and a table carrying names would re-derive it on every emission. Resolution refuses, rather than warns about: a name nothing declares; the error port as a *destination*, since ABI §6.4 makes it an output; and the same connection declared twice, which would deliver one batch twice. Fan-out order is declaration order.

What resolution does *not* check is whether a block declares a port named `err`. ABI §11.1 reserves that name in both directions, so such a manifest is rejected at load and no descriptor carrying one can reach the router. Checking it here as well would be a second statement of a manifest rule, in the crate whose whole purpose is that there is only one.

**`PORT_ERR` is routable and unrouted by default** (ABI §6.4). A service may wire it like any other output; one that does not gets §6.4's "logged and counted" for every error emission, and nothing else — an *ordinary* output nobody wired is an ordinary shape and says nothing.

### 6.1 Where routing happens

**On the emitting instance's own thread, after its callback returned.** ABI §6.2 fixes the *when*; this fixes the *where*, and the two together are what make backpressure real: an instance waiting for room in a full destination is an instance not draining its own mailbox, so the pressure reaches whoever is feeding it. Routing from a central task draining §5's outbound event stream would look equivalent and quietly delete that, because that stream is unbounded on purpose (§5). An emission is therefore reported on the event stream **and** routed, through two different queues, deliberately.

Two consequences worth stating:

- **`eio_start` may emit** (ABI §5.1 step 3), so every mailbox in a service exists before its first instance is spawned. That is also the only order in which a *cyclic* graph can be wired at all.
- **A callback that trapped still has its emissions routed.** `emit` already returned zero, so the host has taken those batches; the guest dying afterwards does not un-take them. The instance is discarded (ABI §5.1 step 6) either way.

### 6.2 Bounded mailboxes and the overflow policy

**Bounded mailboxes; overflow policy per connection.** The default is to **block the emitter's queue-drain** — natural backpressure within a node. **Drop-oldest** is available as an opt-in for sensor-style flows. The cross-*device* question — delivery guarantees, ordering, and backpressure between nodes — is a different one and stays OPEN (SCOPE §3.4).

The two policies are the two answers §5's mailbox offers a sender, and neither is free-standing:

- **Backpressure** is the waiting send. Nothing is lost; a saturated graph slows down.
- **Drop-oldest** is the refusing send plus a **one-batch slot on the connection**. When the destination is full the newest batch takes the slot, and the batch it finds there is the one dropped; the slot is retried ahead of the next round of emissions. The batch a connection discards is always one of *its own*: a per-connection policy MUST NOT discard work another connection put in the shared mailbox, so a control flow set to backpressure keeps its guarantee even when a sensor flow into the same block does not.

**A connection whose destination is its own source never waits**, however it is configured. An instance is the only drain of its own mailbox, so waiting there cannot succeed — it is a deadlock rather than backpressure, and the batch is discarded and counted instead. Longer cycles are not locally detectable: a saturated cycle of two or more instances stalls those instances, which is the cost of in-node backpressure and is stated here rather than papered over. Every discard — unrouted error emission, drop-oldest replacement, full self-connection, gone receiver — is logged and counted.

### 6.3 Taps and system blocks

- **Taps** (SCOPE §3.12): any connection can be tapped at runtime via API — sampled copies into a per-tap ring buffer, streamed over the API (§9) and/or published to a system topic. Zero cost untapped. Expression evaluation failures (EXPR §8) are injected into taps as annotated events.
- **System blocks (PROPOSED):** `publisher` and `subscriber` blocks are **host-native**, not WASM — they appear in the palette/manifest system like any block but their implementation is the router's pub/sub bridge (§7). Rationale: they need transport internals and credentials no sandboxed block should hold; and every node class must have them even when it can't load WASM dynamically. The precedent is deliberate and narrow: system blocks are limited to transport endpoints (logger stays an ordinary WASM block).

## 7. Pub/sub bridge

Transport is OPEN (SCOPE §3.9). The bridge is the isolation layer that keeps it that way: a small trait (`publish(topic, batch)`, `subscribe(topic) -> stream`, connection lifecycle) implemented per transport candidate (MQTT first — **PROPOSED** rumqttc behind the trait), so the transport decision stays swappable until cross-node work forces it. Topic naming convention, QoS mapping, and retained-message posture are part of that later decision, not this spec.

## 8. Supervision

Policy is OPEN (SCOPE §3.13); the daemon ships the _mechanism_: per-instance restart with exponential backoff and a restart-count circuit breaker escalating to service-errored. Re-instantiation = fresh `eio_configure` (ABI §5.1); durable state via `eio:state` only. **PROPOSED default policy:** restart-instance up to N times per window, then stop service and surface. Callback error returns (ABI §8) are counted/logged, never restart-triggering.

## 9. Management API (SCOPE §3.10)

REST/JSON, OpenAPI spec generated from code and served at `/openapi.json` (the agent/MCP tool surface). Sketch:

```
GET    /node                          identity, capabilities, limits, versions
GET    /blocks                        cached blocks + manifests
POST   /blocks/pull                   {ref}
GET    /services                      list + status
GET    /services/{s}                  definition + runtime status
PUT    /services/{s}                  write definition (validate, report)
POST   /services/{s}/start|stop|reload
GET    /services/{s}/errors           validation/runtime error detail
POST   /taps                          {service, connection} -> tap_id
GET    /taps/{id}/stream              SSE/WS: sampled signals + expr-failure events
GET    /logs/stream                   SSE/WS, filterable by service/instance
GET    /state/{instance}              inspect eio:state KV (debug)
```

Auth: per-node bearer token (SCOPE §3.11), generated by the daemon on first boot, printed once / readable from `auth/`. Transport security OPEN.

## 10. State store

Backs `eio:state` (ABI §7.2), namespaced `service/instance/key`. **PROPOSED: redb** (pure-Rust embedded KV, single file, no compaction daemon). Leaf hosts implement the same host functions against flash with `ERR_THROTTLED` budgets — another host-core trait boundary.

## 11. Observability

- Structured logs (tracing): daemon subsystems + guest `log` calls tagged (service, instance).
- Taps per §6.
- Metrics OPEN (SCOPE §3.12); reserve `/metrics` (Prometheus text) — **PROPOSED** counters: delivered/emitted batches per connection, callback duration, instance restarts, expr failures.

## 12. `dev` commands

Commands for block authors, under a `dev` subcommand so that the top-level verbs stay the node's. They operate on a `.wasm` file directly and have no service, no persistence and no API behind them; that is what makes them useful for a block that is not deployable yet, and it is why they are not a way to run a node.

They are a separate thing from the conformance harness (ABI §13.1), which lives in `conformance/`, injects faults, and is run by CI rather than by a person.

```
eio-daemon dev run-block <WASM> [--manifest PATH] [--prop NAME=EXPR]... 
                                [--batch JSON | --batch-file PATH]
                                [--input-port N] [--instance ID] [--service NAME]
                                [--max-payload BYTES] [--max-batch SIGNALS]
```

`run-block` performs ABI §4 load-time validation, resolves the property table per ABI §11.1, instantiates, then walks ABI §5.1 once: `eio_configure`, `eio_start`, one optional `eio_process_signals`, `eio_stop`. Emitted batches are printed rather than routed — there is no graph to route into — using EXPR §7.6's canonical rendering, because a second way of rendering a value is a second definition of what one is.

`--max-payload` and `--max-batch` are stated rather than defaulted-into-invisibility: ABI §9.7 gives them no floor (SCOPE §3.4 OPEN), so the command has values a block can read from its descriptor and a deployer can change.

**The JSON batch mapping.** `--batch` is a debug input, **not** a wire format: the batch encoding is canonical CBOR and nothing else (ABI §6.3.1). The mapping exists so that trying a block does not require producing a `.cbor` file by hand, and it is deliberately one-way. Three things do not survive it, and all three are the JSON data model being smaller than ABI §6.3's:

- Byte strings have no JSON spelling, so a batch containing one cannot be written this way.
- Int and float are told apart *lexically*: `1` is an int, `1.0` and `1e0` are floats. An integer literal between `i64::MAX` and `u64::MAX` is refused rather than rounded into a float; beyond `u64::MAX` the JSON reader has already made it a float and nothing survives to act on.
- Duplicate object keys collapse rather than being rejected as §6.3.1 rule 7 requires, because the JSON parser resolves them first.

NaN and infinity need no rule here: a literal that overflows `binary64` is refused while parsing.

**Logging.** Every line a run produces — the daemon's own and the guest's `log` calls alike — is emitted inside a span carrying `(service, instance)` per §11. `run-block` has no service, so `--service` supplies the name, defaulting to `dev`.

**Capabilities.** A block whose manifest declares a capability the host does not implement is refused at load, by name. That is SCOPE §3.3's deploy-time question asked where a deployer can act on it; the engine's own answer would name a missing symbol rather than the capability that asked for it.

## 13. Expansion list (for the in-depth pass)

Per-subsystem deep specs needed: service file schema (normative), router semantics under reload (in-flight signal disposition), OCI auth for private registries, tap sampling strategy, mailbox sizing defaults, API error model, multi-arch AOT artifact selection, node.toml schema.

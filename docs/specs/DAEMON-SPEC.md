# Daemon Specification

**Status:** Draft 1 — high-level; intended for in-depth expansion per-subsystem. **Depends on:** SCOPE.md, ABI-SPEC.md, EXPR-SPEC.md. **Markers:** Settled decisions are stated plainly. **PROPOSED** = drafted here, awaiting ratification. **OPEN** = tracked in SCOPE.md.

The daemon is the daemon-class node runtime (SCOPE §3.7): a single compiled Rust binary that establishes a node, executes services, and exposes the management API. This spec covers its internal architecture; the leaf runtime shares the starred (★) subsystems and is specified separately later.

---

## 1. Crate architecture

Monorepo workspace. The load-bearing split is **host-core vs daemon**: everything the leaf runtime will also need lives in `host-core` and MUST stay `no_std`-compatible (alloc allowed).

```
crates/
  abi/           ★ The ABI's shared vocabulary: §8 status codes, §3 sentinels,
                   §9.6 alignment. No dependencies. Read by both host-core and
                   the guest SDK
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
  block-sdk-macros/  The `#[block]` attribute macro (SDK-SPEC §1). A separate
                   crate because the language requires it: a proc-macro crate
                   can export nothing but macros. Host-compiled, so not ★
  test-host/       Native in-process host for testing blocks (SDK-SPEC §6.1).
                   A *host*, so it depends on host-core; separate from block-sdk
                   so a guest never can
  cargo-eio/       Block build/publish tooling (SDK-SPEC §5)
  conformance/     Reference harness + golden blocks (ABI §13)
```

The expression conformance vectors are **not** here: they are data files at the repository root in `expr-tests/` (EXPR §11), because a host written in another language must be able to consume them without building a Rust crate. `conformance/` holds the ABI harness, which is Rust by nature — it drives a WASM engine.

★-marked crates are shared with the leaf runtime and MUST stay `no_std`-compatible (`alloc` allowed). `daemon` and `cargo-eio` are `std` binaries; `block-sdk` is `no_std` by necessity (it compiles into guests).

Conformance implication: `host-core` driven by wasmtime (daemon) and by WAMR (leaf) MUST pass the same harness — the shared crate is how divergence is prevented structurally, not just tested for.

The daemon's half of that is `crates/daemon/src/conformance.rs`: a `#[cfg(test)]` module taking `eio-conformance` as a **dev-dependency** and running the reference suite's scenario files through §5.1's binding. A dev-dependency and not a lib target on this crate, because the table above gives `eio-daemon` no import path on purpose — the reusable half of the host is `host-core`, and a lib target here would be a second answer to what another crate may link against. Scenarios needing a capability namespace the daemon implements no functions in (§5.1: only `eio:core`, today) are reported skipped by name, which is how that gap stays visible as the daemon grows.

**Where a rule lives follows from what it is about, not from who happens to call it.** ABI §11.1's `required`/`default` precedence is the worked example, because all three plausible homes were arguable. Not `manifest`: a manifest describes what a *block* says about itself, and a deployment's supplied values are not that. Not `daemon`: the rule is pure ABI semantics with no engine and no configuration *format* in it, and leaving it there would mean the leaf runtime — whose configuration source is shaped differently — reimplementing the precedence from scratch, which is the silent divergence this split exists to prevent. So `host-core`, which is also the only crate that *can* hold it: the rule consumes `manifest`'s `Manifest` and produces `host-core`'s `PropertySource`, and the dependency runs host-core → manifest. The daemon reaches it from `--prop` flags today and from service files later; the leaf reaches it from whatever it reads. One implementation, two hosts.

The same reasoning is why **`abi` is a crate and not a module of `host-core`**, which is where ABI §8's codes started. `host-core` is the *host* half of the boundary: it drives a guest through its lifecycle and resolves properties, and depends on `expr` and `manifest` to do it. But §8's codes, §3's sentinels and §9.6's alignment are not host rules — a guest compares against every one of them, and `eio-sdk` needs them too. Left in `host-core`, a block reaching for `ERR_THROTTLED` would compile the expression interpreter and the manifest parser into its `.wasm`; re-declared in the SDK, the platform would hold two hand-maintained copies of a table the two sides MUST agree on byte for byte. So they sit below both, in a crate with no dependencies at all — which is the property worth protecting, since anything added there is added to every block that ships. ABI §12's version is deliberately *not* among them: `eio_manifest::Abi` already holds the packed form together with the compatibility rule that gives it meaning, and a bare constant in `abi` would be a second spelling of the number sitting next to the one implementation that knows what to do with it. `host-core` re-exports the lot, so a host still has one import for the ABI and the move is invisible at its call sites.

The same reasoning puts **both halves of ABI §9.7 in `host-core`**, and it is worth stating because they arrived at different times and the split looked survivable. §9.7 is one rule read in two directions: the host "rejects `emit` beyond `max_payload` with `ERR_LIMIT`" and "never delivers batches beyond" the limits its descriptor published. Neither half has an engine or a queue in it — the numbers come from the instance descriptor (ABI §5.2) and the answer is a refusal, not a delivery — so both belong beside each other, and a leaf runtime that reimplemented the inbound half would be free to disagree with the daemon about which batches a block is entitled to never see. Concretely: the driver takes the batch *decoded* and encodes it itself, because the guest is handed canonical CBOR (§6.1) while `prop`'s `signal_idx` indexes the same call's signals (§7.1), and a host supplying those by two paths could supply two different batches. Refusing is therefore its own outcome rather than a status: the guest was never called, so nothing is counted against it (§8), and the daemon's part is only saying what the refusal means to an operator (§11).

For the same reason the **property scope is the driver's**, not its caller's. ABI §7.1 answers `prop` "for the duration of the current callback", so `host-core` holds the instance's property context and opens a scope around every guest call it makes. A host cannot forget to open one, cannot leave one open across callbacks, and cannot pair a callback with the wrong batch — which is the ABI rule most likely to be implemented twice and slightly differently.

**Naming.** Directory names are exactly as listed above. Package names are `eio`-prefixed and imported with underscores:

|Directory|Package|Import path|
|---|---|---|
|`abi/`|`eio-abi`|`eio_abi`|
|`host-core/`|`eio-host-core`|`eio_host_core`|
|`expr/`|`eio-expr`|`eio_expr`|
|`signal/`|`eio-signal`|`eio_signal`|
|`manifest/`|`eio-manifest`|`eio_manifest`|
|`daemon/`|`eio-daemon`|—|
|`block-sdk/`|`eio-sdk`|`eio_sdk`|
|`block-sdk-macros/`|`eio-sdk-macros`|`eio_sdk_macros`|
|`test-host/`|`eio-test-host`|`eio_test_host`|
|`cargo-eio/`|`cargo-eio`|—|
|`conformance/`|`eio-conformance`|`eio_conformance`|

`block-sdk-macros/` follows `block-sdk/`'s existing exception rather than the directory rule, so the pair reads as one thing: a block author sees `eio-sdk` and never names the macro crate, which `eio-sdk` re-exports.

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

**Where that stops scaling, and what replaces it.** Per instance: one thread, one current-thread runtime, one mailbox; per *runtime*, one shared epoch ticker, not one per instance. At Pi-class density — hundreds of instances, nearly all of them parked in a mailbox read — that is thread memory and nothing else, since a parked thread costs its stack and no scheduler attention. The ceiling is server-class density, thousands of instances on one node, where stack reservation and scheduler churn stop being free.

No such workload has been measured, and this decision stands until one is. It is recorded here so that when a node does hit the ceiling, the revisit starts from the trade-off rather than from scratch — and so that nobody pre-emptively pays for a density nobody has. Two candidate paths, in cost order:

- **Sharded `LocalSet`s** — M threads carrying K instances each. Cheapest change and the smallest departure: the store affinity, the serialization and the mailbox are all unchanged. What it gives up is exactly what the thread bought — a spinner's blast radius becomes its whole shard, for up to the deadline budget, rather than itself.
- **wasmtime async with epoch-yield** (`epoch_deadline_async_yield_and_update`) — a spinner yields rather than holding anything, at the price of a fiber stack per in-flight call. Two consequences make this the more expensive option than it looks: deadline attribution becomes load-dependent, so ABI §10's wall-clock budget stops meaning what an operator promised; and every host function must then not block a poll, which ABI §7.5 rules out for `i2c` up to milliseconds. It also reverses §5.1's decision to compile wasmtime's async machinery *out*, which is part of how "core WASM only" is enforced by absence here.

The choice is confined to this crate. `host-core` and the ABI know nothing of threads, and the leaf runtime has its own executor, so neither the shared driver nor a second host implementation is affected by whichever way it goes.

**Mailbox bound and what a full one means.** The mailbox is bounded and its depth is host configuration, with no floor. The executor offers a sender both answers to a full one and takes neither on the sender's behalf: a *waiting* send (backpressure, which propagates to whoever is producing too fast) and a *refusing* send that hands the work item back. Which one a connection uses is §6's per-connection overflow policy; the cross-device question is OPEN (SCOPE §3.4) and is not settled by the executor having a bound.

**Every sender gone is a stop, and a serviced instance stops on an explicit `Stop`.** A mailbox no sender can reach again cannot receive work, so the instance runs `eio_stop` (ABI §5.1 step 5) rather than idling; an instance is never left running with nothing that can reach it. That is the terminator for an instance with no service around it — the single-block path.

Inside a service it is not, and never was: the service holds a mailbox for every instance it owns, so "every sender gone" cannot become true while the service does. Cycles make the same point about the instances themselves. So a service stops its instances by posting `Stop` to each, and §6's delivery registry — which holds every instance's *current* mailbox so that §8 can replace one — does not change that. The rule a host must not break is the one above: an instance that nothing can reach must stop.

**Inbound is bounded; outbound observation is not.** What an instance produces — callback statuses, `error` details, expression failures, emissions, its death or its clean stop — leaves on an unbounded stream, because an observer that could stall a guest by reading slowly would be a worse defect than a queue that grows. Backpressure belongs on the inbound side, where slowing the sender down is a correct response. Routed emissions are not this stream: they travel through the *destination's* bounded mailbox (§6), which is where a slow consumer should be felt.

**Death is an event, not a log line.** An instance that dies reports the trap and its kind (ABI §5.1 step 6, §10) on that stream; supervision (§8) is its consumer. Until §8 exists it is logged and the thread ends.

### 5.1 Engine binding

`host-core` drives a guest through its `Engine` trait; this is the daemon's only implementation of it, and nothing outside it knows the engine is wasmtime. The leaf runtime writes the equivalent file against WAMR or wasm3, and the driver above both is the same code — that is what makes "divergence between the two hosts is a conformance bug" (ABI §13) enforceable rather than aspirational.

**Feature set.** wasmtime is depended on with `default-features = false` and `cranelift, runtime, std, anyhow, backtrace`. Threads, the component model and GC are therefore *compiled out* rather than switched off, which is a stronger reading of ABI §1's "core WASM only": with the features absent, the corresponding `Config` setters do not exist to be forgotten. `anyhow` gives `wasmtime::Error` its `From` conversion into `anyhow::Error`, so `?` works throughout the daemon — a conversion, not an alias: `anyhow::Context` does not apply to a wasmtime result, so a wasmtime error is given context by converting it first. `backtrace` earns its place because a trap is an instance's death (ABI §8) and the log line is all anyone gets.

**The accepted feature set, and nothing past it.** ABI §4.3 places feature conformance on the engine and nowhere else — `manifest` does no WASM feature gating — so this configuration is the only thing standing between a block using a proposal the leaf tier lacks and a leaf runtime that will refuse it at flash time.

The configuration states it *subtractively*: every proposal wasmtime knows of is disabled, and then exactly the MVP set is re-enabled. Not a list of `wasm_*(false)` calls, for two reasons.

- A list is a closed statement about a moving target. Whatever proposal a later wasmtime enables by default is admitted silently on the next `cargo update`, and blocks using it would run here and be refused by wasm3 — the divergence §1 exists to prevent, arriving through the one door the shared crates do not watch. Subtracting from "everything" refuses it instead, on a host nobody has touched.
- A list is order-sensitive in a way nothing checks. wasmtime rejects a `Config` that disables `simd` while `relaxed_simd` is still enabled, so the setters would have to be called in dependency order; the subtractive form never meets the question.

The base is wasmparser's own `MVP` set, less its `GC_TYPES` flag. That flag gates the `externref`/`anyref` *types* rather than any proposal, and wasmparser folds it into `MVP` only so the wider sets need not repeat it; a wasmtime built without the `gc` cargo feature — this one, per the feature set above — refuses to build an engine at all while it is set. The two decisions agree only once it is removed. `FLOATS` stays enabled: WASM 1.0 has floating point, and so does `expr`.

Added back on top are ABI §4.3's six: bulk memory, sign extension, reference types, multi-value, non-trapping float-to-int and mutable globals. Still refused, among everything else: SIMD and relaxed SIMD, tail calls, multi-memory, memory64, extended const, exceptions, GC, threads, and the component model — the last four by the cargo feature set as well, which is why no `Config` call can restore them.

**What the set costs a block author: nothing.** A stock `cargo build --release --target wasm32-unknown-unknown` produces a conformant module, with no flags and no post-processing. That is a correction rather than a convenience — this section previously said an unadorned Rust block was refused and that `-C target-feature=-bulk-memory` fixed it. Measured on rustc 1.97.1, the flag changes nothing: the `memory.copy` lives in `alloc::string::String::clone` inside the precompiled `rust-std`, which no `RUSTFLAGS` and no `-Z build-std` rebuilds. The restriction it was defending was itself the error (ABI §4.3, SCOPE §3.2). Hand-written `.wat` needs nothing either way.

**And the daemon is not the only host that says so.** `crates/conformance/tests/wasm3.rs` runs the same scenarios, and the same stock-built Rust block, on wasm3 — the leaf-class interpreter. That is what makes §1's "divergence between the two hosts is a conformance bug" a fact about this repository rather than an aspiration, and it is what the feature set above is measured against.

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

**Delivery goes through a per-service mailbox registry, not through senders resolved once.** The connection table fixes which `(instance, port)` reaches which; *where* an instance is reachable is a separate question with a changing answer, because §8 restarts an instance in place and a restarted instance has a new mailbox. So the registry holds one slot per instance index and an emitting instance reads its destination's slot at delivery time. Baking the senders into each outlet when the service was built would mean a restarted instance was routed to by nobody — every peer would still name the channel the dead thread took with it, and supervision would restart the block while silently severing it from the graph. The registry is also what makes §5's "every sender gone" not the terminator for a serviced instance.

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

Policy is OPEN (SCOPE §3.13); the daemon ships the _mechanism_: per-instance restart with exponential backoff and a restart-count circuit breaker escalating to service-errored. Re-instantiation = fresh `eio_configure` (ABI §5.1); durable state via `eio:state` only. **PROPOSED default policy:** restart-instance up to N times per window, then stop service and surface. Callback error returns (ABI §8) are counted/logged, never restart-triggering.

**Restarting one instance leaves the graph intact.** The old instance is stopped and joined before the new one is built, so no two lives of a block ever answer the same connections — ABI §1.2 admits one caller, not one per life. The replacement's mailbox is installed in §6's registry *before* it is spawned, for the same reason a service's mailboxes all exist before any of its instances do: a peer emitting during the gap queues its batch instead of finding a closed channel. Because every outlet reads the registry per delivery, no peer is rebuilt and none is consulted. The descriptor is unchanged, so the connection table resolved against it still describes the instance; a restart that renumbered a port would have rewired the service behind its own back.

**A restart re-instantiates, it does not recompile.** The service keeps the compiled module each instance was built from. That is not the block's bytes: compiled code is already resident for as long as any instance of it is, so a retained handle costs a refcount, where keeping the `.wasm` would mean every instance paying for its whole life for the moment it was compiled. Where the module comes from on a *cold* start is §4's, not this section's.

**Work the old instance had queued is gone with it.** That is what a restart is: the replacement did not run those callbacks and must not be told it did. Anything that had to survive was written through `eio:state` (ABI §7.2), which is the only continuity ABI §5.1 offers across lives.

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

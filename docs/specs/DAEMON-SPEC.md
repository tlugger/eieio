# Daemon Specification

**Status:** Draft 1 — high-level; intended for in-depth expansion per-subsystem. **Depends on:** SCOPE.md, ABI-SPEC.md, EXPR-SPEC.md. **Markers:** Settled decisions are stated plainly. **PROPOSED** = drafted here, awaiting ratification. **OPEN** = tracked in SCOPE.md.

The daemon is the daemon-class node runtime (SCOPE §3.7): a single compiled Rust binary that establishes a node, executes services, and exposes the management API. This spec covers its internal architecture; the leaf runtime shares the starred (★) subsystems and is specified separately later.

---

## 1. Crate architecture

Monorepo workspace. The load-bearing split is **host-core vs daemon**: everything the leaf runtime will also need lives in `host-core` and MUST stay `no_std`-compatible (alloc allowed).

```
crates/
  host-core/     ★ ABI implementation: lifecycle driver, memory conventions,
                   status/size protocol, capability validation, router core
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
- Fuel or epoch interruption per callback (ABI §10); budget from `node.toml`. Trap/exhaustion → instance DEAD → supervision (§8).
- Host functions (`eio:*`) implemented against the mailbox/router: `emit` enqueues to the router (never delivers inline — ABI §6.2); `prop` hits the expression engine with the callback's current-batch context; async capabilities post completions back into the mailbox.

### 5.1 Engine binding

`host-core` drives a guest through its `Engine` trait; this is the daemon's only implementation of it, and nothing outside it knows the engine is wasmtime. The leaf runtime writes the equivalent file against WAMR or wasm3, and the driver above both is the same code — that is what makes "divergence between the two hosts is a conformance bug" (ABI §13) enforceable rather than aspirational.

**Feature set.** wasmtime is depended on with `default-features = false` and `cranelift, runtime, std, anyhow, backtrace`. Threads, the component model and GC are therefore *compiled out* rather than switched off, which is a stronger reading of ABI §1's "core WASM only": with the features absent, the corresponding `Config` setters do not exist to be forgotten. `anyhow` makes `wasmtime::Error` an alias for `anyhow::Error` — without it wasmtime's own error type does not implement `std::error::Error`. `backtrace` earns its place because a trap is an instance's death (ABI §8) and the log line is all anyone gets.

**Post-MVP proposals** that the feature set does not remove are still enabled at their wasmtime defaults. ABI §4.3 places MVP conformance here and nowhere else, so this is a known gap rather than a decision; the disabled-proposal list belongs in this section when it lands. Two things measured while establishing that: wasmtime rejects a `Config` that disables `simd` while `relaxed_simd` is still enabled, so the proposals have to be turned off in dependency order; and `wasm_reference_types` no longer exists as a setter in wasmtime 47.

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

**Store affinity.** `Store<T>` is `!Send` here, because the handlers and the property context are `Rc`-shared. That is the ABI showing through rather than an accident, and it is a constraint on §5's executor: instances live on a `LocalSet` or on a thread each, never on a work-stealing pool.

## 6. Router

- Owns the service graph: connection table, fan-out (duplicate batch per receiver — nio semantics), delivery into destination mailboxes.
- **Bounded mailboxes; overflow policy per connection** (OPEN backpressure, SCOPE §3.4 — **PROPOSED default:** block the emitter's queue-drain, i.e. natural backpressure within a node; drop-oldest available as opt-in for sensor-style flows).
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

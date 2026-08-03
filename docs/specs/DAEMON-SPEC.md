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
  block-sdk/       Guest-side (SDK-SPEC)
  conformance/     Reference harness + golden blocks (ABI §13) + expr vectors (EXPR §11)
```

Conformance implication: `host-core` driven by wasmtime (daemon) and by WAMR (leaf) MUST pass the same harness — the shared crate is how divergence is prevented structurally, not just tested for.

---

## 2. On-disk layout (source of truth, SCOPE §3.8)

**PROPOSED:**

```
/etc/nio/                      (or --data-dir)
  node.toml                    node identity, listen addr, limits, budgets
  auth/                        node token, TLS material (OPEN, SCOPE §3.11)
  services/
    <service>.toml             one service definition per file
  blocks/                      OCI pull cache: <name>/<version>/block.wasm
                               (+ precompiled wasmtime artifact, keyed by engine hash)
  state/                       nio:state backing store
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
- Load-time validation is exactly ABI §4: exports present, imports ⊆ `nio:*`, imports ⊆ manifest capabilities, ABI version accepted, embedded manifest (if present) matches registry manifest.
- Caches wasmtime-precompiled modules keyed by (digest, engine config hash) — cold-start matters on a Pi.
- Airgap/offline: cache is authoritative when the registry is unreachable; a service whose blocks are cached starts fine offline.

## 5. Executor

The runtime embodiment of ABI §1 invariants:

- One wasmtime `Store` + instance per block instance. **One tokio task per block instance** owning the store (stores are `!Sync`; ownership model and serialization requirement align perfectly): the task loops over a bounded mpsc mailbox of work items (`Deliver{port, batch}`, `Timer{id}`, `GpioEdge{..}`, `HttpDone{..}`, `Stop`), invoking guest callbacks strictly sequentially. Serialized invocation falls out of the architecture rather than a lock.
- Fuel or epoch interruption per callback (ABI §10); budget from `node.toml`. Trap/exhaustion → instance DEAD → supervision (§8).
- Host functions (`nio:*`) implemented against the mailbox/router: `emit` enqueues to the router (never delivers inline — ABI §6.2); `prop` hits the expression engine with the callback's current-batch context; async capabilities post completions back into the mailbox.

## 6. Router

- Owns the service graph: connection table, fan-out (duplicate batch per receiver — nio semantics), delivery into destination mailboxes.
- **Bounded mailboxes; overflow policy per connection** (OPEN backpressure, SCOPE §3.4 — **PROPOSED default:** block the emitter's queue-drain, i.e. natural backpressure within a node; drop-oldest available as opt-in for sensor-style flows).
- **Taps** (SCOPE §3.12): any connection can be tapped at runtime via API — sampled copies into a per-tap ring buffer, streamed over the API (§9) and/or published to a system topic. Zero cost untapped. Expression evaluation failures (EXPR §8) are injected into taps as annotated events.
- **System blocks (PROPOSED):** `publisher` and `subscriber` blocks are **host-native**, not WASM — they appear in the palette/manifest system like any block but their implementation is the router's pub/sub bridge (§7). Rationale: they need transport internals and credentials no sandboxed block should hold; and every node class must have them even when it can't load WASM dynamically. The precedent is deliberate and narrow: system blocks are limited to transport endpoints (logger stays an ordinary WASM block).

## 7. Pub/sub bridge

Transport is OPEN (SCOPE §3.9). The bridge is the isolation layer that keeps it that way: a small trait (`publish(topic, batch)`, `subscribe(topic) -> stream`, connection lifecycle) implemented per transport candidate (MQTT first — **PROPOSED** rumqttc behind the trait), so the transport decision stays swappable until cross-node work forces it. Topic naming convention, QoS mapping, and retained-message posture are part of that later decision, not this spec.

## 8. Supervision

Policy is OPEN (SCOPE §3.13); the daemon ships the _mechanism_: per-instance restart with exponential backoff and a restart-count circuit breaker escalating to service-errored. Re-instantiation = fresh `nio_configure` (ABI §5.1); durable state via `nio:state` only. **PROPOSED default policy:** restart-instance up to N times per window, then stop service and surface. Callback error returns (ABI §8) are counted/logged, never restart-triggering.

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
GET    /state/{instance}              inspect nio:state KV (debug)
```

Auth: per-node bearer token (SCOPE §3.11), generated by the daemon on first boot, printed once / readable from `auth/`. Transport security OPEN.

## 10. State store

Backs `nio:state` (ABI §7.2), namespaced `service/instance/key`. **PROPOSED: redb** (pure-Rust embedded KV, single file, no compaction daemon). Leaf hosts implement the same host functions against flash with `ERR_THROTTLED` budgets — another host-core trait boundary.

## 11. Observability

- Structured logs (tracing): daemon subsystems + guest `log` calls tagged (service, instance).
- Taps per §6.
- Metrics OPEN (SCOPE §3.12); reserve `/metrics` (Prometheus text) — **PROPOSED** counters: delivered/emitted batches per connection, callback duration, instance restarts, expr failures.

## 12. Expansion list (for the in-depth pass)

Per-subsystem deep specs needed: service file schema (normative), router semantics under reload (in-flight signal disposition), OCI auth for private registries, tap sampling strategy, mailbox sizing defaults, API error model, multi-arch AOT artifact selection, node.toml schema.

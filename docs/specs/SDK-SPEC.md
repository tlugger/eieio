# Block SDK Specification

**Status:** Draft 1 — high-level; intended for in-depth expansion. **Depends on:** ABI-SPEC.md (especially §14's litmus rule: SDK friction = spec bug), EXPR-SPEC.md, SCOPE.md §3.2–3.6. **Markers:** **PROPOSED** = drafted here, awaiting ratification. **OPEN** = tracked in SCOPE.md.

`block-sdk` is the Rust crate block authors build against. Its contract: **block authors write 100% safe Rust; every `unsafe` in the block ecosystem lives in this crate's audited glue; the raw ABI is invisible.** It is developed in lockstep with the ABI — friction here amends the spec before 1.0 freezes.

---

## 1. Programming model

**PROPOSED** shape — a struct, a derive-style attribute macro, and a trait:

```rust
use nio_sdk::prelude::*;

#[block(
    name = "threshold_filter",
    description = "Route signals by comparing an attribute to a threshold",
    inputs(default),
    outputs(above, below),
    capabilities()          // none beyond nio:core
)]
struct ThresholdFilter {
    #[prop(ty = "float", desc = "Compared per signal", default = "(float $value)")]
    reading: Prop<f64>,
    #[prop(ty = "float", default = "50.0")]
    threshold: Prop<f64>,
}

impl Block for ThresholdFilter {
    fn process_signals(&mut self, ctx: &mut Ctx, input: InPort, batch: Batch) -> BlockResult {
        let mut above = ctx.batch();
        let mut below = ctx.batch();
        for (i, signal) in batch.iter().enumerated() {
            if self.reading.get(ctx, i)? > self.threshold.get(ctx, i)? {
                above.push(signal.clone());
            } else {
                below.push(signal.clone());
            }
        }
        ctx.emit(Out::Above, above)?;
        ctx.emit(Out::Below, below)?;
        Ok(())
    }
}
```

What the macro generates:

- All ABI exports (`nio_configure`, `nio_start`, `nio_stop`, `nio_process_signals`, optional `nio_on_*`, `nio_abi_version`, `nio_alloc`/`nio_free`) wrapping the trait impl. Lifecycle methods (`configure`, `start`, `stop`) have default no-op impls; only `process_signals` is required for transform blocks; timer/gpio/http callbacks are optional trait methods gated by declared capabilities.
- Port enums (`In`, `Out`) from the macro attributes — emitting to an undeclared port is a _compile_ error, not a runtime one.
- `prop_id` mapping from field order, and typed `Prop<T>` handles whose `get(ctx, signal_idx)` wraps the ABI `prop` call: grow-and-retry buffer loop (ABI §7.1), CBOR decode, declared-type check. `get_static(ctx)` = `SIGNAL_NONE` evaluation for use in `configure`/`start`/timers.
- **The manifest** (ABI §11): properties, ports, capabilities, ABI version are all derived from these same attributes and emitted as both `manifest.json` and the `nio:manifest` custom section at build time. Single source of truth in code; manifest/import mismatches become unrepresentable rather than merely validated.

## 2. Core types

- `Value` — the CBOR value enum (shared `signal` crate, no_std).
- `Signal` — map wrapper: `get/get_or/set/has`, serde-compatible via minicbor derive for typed extraction.
- `Batch` — owned Vec<Signal> with builder; `ctx.batch()` for capacity-hinted construction.
- `Ctx` — the only channel to the host: `emit`, `log` (also backing `log`-crate macros), `error` detail, `time_unix_ms`/`time_mono_ms`/`rand` wrappers, capability handles (§3).
- `BlockError` / `BlockResult` — errors map to non-zero callback returns + structured `error()` detail (ABI §8); `?` works throughout. `HostError` (from host-fn status codes) converts into `BlockError` with code preservation, so `ERR_THROTTLED` etc. remain matchable.

## 3. Capability wrappers

One safe wrapper per `nio:*` namespace, present on `Ctx` only when declared (macro gates them — using `ctx.gpio()` without `capabilities(gpio)` is a compile error):

- `ctx.state()` — `get/put/del` over typed CBOR values; grow-and-retry hidden; `ERR_THROTTLED` surfaced as a matchable error, per ABI §7.2's "best-effort, not a queue" posture.
- `ctx.timers()` — `set(Duration, Repeat) -> TimerId`, `cancel`. Fires `Block::on_timer(&mut self, ctx, TimerId)`.
- `ctx.gpio()` — mode/read/write/watch with typed enums. Fires `on_gpio(watch_id, Level)`.
- `ctx.http()` — `request(HttpRequest) -> ReqId`; completion fires `on_http(&mut self, ctx, ReqId, HttpResponse)`. **No async/await in guests** (PROPOSED, firmly): no runtime exists in the instance and the ABI is callback-shaped; correlating `ReqId -> purpose` is the block's job via its own fields. An SDK correlation-map sugar (`ctx.http().request_tagged(req, tag)`) is a candidate nicety for the in-depth pass, not core.

## 4. Guest internals (the unsafe budget)

- `#![no_std]` + `alloc`; **PROPOSED** allocator: `dlmalloc` (Rust's wasm default) behind `nio_alloc`/`nio_free` with the ABI's 8-byte alignment guarantee.
- The entire `unsafe` surface, enumerated for audit: allocator export glue, `(ptr,len) ↔ &[u8]` conversions at each export entry and host-fn call site, and the panic handler. Nothing else. Target: every `unsafe` block carries a `// SAFETY:` comment citing the ABI section that justifies it.
- **Panics abort → trap → instance death** (ABI §6 invariant 6). The SDK's job is making panics rare in safe code (`get_or`, checked ops in examples) — not catching them. `panic = "abort"` enforced via the build tooling.

## 5. Build and packaging tooling

**PROPOSED:** a `cargo nio` subcommand (separate `cargo-nio` crate):

```
cargo nio new <name>         template block repo (CI included)
cargo nio build              wasm32-unknown-unknown, panic=abort, opt for size,
                             embed nio:manifest section, emit manifest.json
cargo nio test               native tests + harness run (§6)
cargo nio aot --target esp32s3   WAMR AOT artifact for leaf targets
cargo nio publish            package OCI artifact (+ AOT variants), push, sign (cosign)
```

The template repo's CI runs build/test/publish on tag — this is the "block repos independently released to the registry" flow from SCOPE §3.6 made concrete.

## 6. Testing story

Two layers, both in-template:

1. **Native unit tests** — `TestHost`: a mock implementing the host side in-process (no WASM): scripted property tables (real `expr` crate evaluates them — same interpreter, honest semantics), signal delivery, emit capture, capability stubs (virtual GPIO/clock/state). Fast inner loop: `host.deliver("default", batch); assert_eq!(host.emitted("above").len(), 2);`
2. **Conformance run** — the same tests executed against the compiled `.wasm` under the reference harness (ABI §13), catching boundary bugs the native layer can't (memory conventions, encoding, limits).

## 7. Non-Rust authorship (deferred, SCOPE §6)

The ABI permits any language; the SDK does not chase this in v1. The conformance harness + golden blocks are the de facto spec for future SDKs (TinyGo, AssemblyScript, componentized Python for legacy nio-blocks migration). No design work now beyond keeping the harness language-agnostic.

## 8. Expansion list (for the in-depth pass)

Macro attribute grammar (normative), `Prop<T>` supported types and their CBOR/manifest-type mapping, HttpRequest/Response types, TestHost API, template repo contents, size-optimization defaults (opt-level, lto, strip, wasm-opt pass), SDK versioning vs ABI versioning policy, `request_tagged` correlation sugar decision.

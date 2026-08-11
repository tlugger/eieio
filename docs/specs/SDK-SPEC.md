# Block SDK Specification

**Status:** Draft 1 — high-level; intended for in-depth expansion. **Depends on:** ABI-SPEC.md (especially §14's litmus rule: SDK friction = spec bug), EXPR-SPEC.md, SCOPE.md §3.2–3.6. **Markers:** **PROPOSED** = drafted here, awaiting ratification. **OPEN** = tracked in SCOPE.md.

`block-sdk` is the Rust crate block authors build against. Its contract: **block authors write 100% safe Rust; every `unsafe` in the block ecosystem lives in this crate's audited glue; the raw ABI is invisible.** It is developed in lockstep with the ABI — friction here amends the spec before 1.0 freezes.

---

## 1. Programming model

A struct, an attribute macro, and a trait:

```rust
use eio_sdk::prelude::*;

#[block(
    name = "threshold_filter",
    description = "Route signals by comparing an attribute to a threshold",
    inputs(default),
    outputs(above, below),
    capabilities()          // none beyond eio:core
)]
struct ThresholdFilter {
    #[prop(ty = "float", desc = "Compared per signal", default = "(float $value)")]
    reading: Prop<f64>,
    #[prop(ty = "float", default = "50.0")]
    threshold: Prop<f64>,
}

impl Block for ThresholdFilter {
    fn process_signals(&mut self, ctx: &mut Ctx, _input: u32, batch: Batch) -> BlockResult {
        let mut above = Batch::new();
        let mut below = Batch::new();
        for (index, signal) in batch.iter().enumerate() {
            let index = index as u32;
            if self.reading.get(ctx, index)? > self.threshold.get(ctx, index)? {
                above.push(signal.clone());
            } else {
                below.push(signal.clone());
            }
        }
        ctx.emit(Out::Above, &above)?;
        ctx.emit(Out::Below, &below)?;
        Ok(())
    }
}
```

This example compiles and runs; `crates/block-sdk/tests/macro.rs` is it, verbatim, and that
is deliberate — ABI §14 makes SDK friction a spec bug, so a printed example that does not
compile is a defect in this document.

What the macro generates:

- All ABI exports (`eio_configure`, `eio_start`, `eio_stop`, `eio_process_signals`, optional `eio_on_*`, `eio_abi_version`) wrapping the trait impl, over the `eio_alloc`/`eio_free` the SDK already exports. **Every trait method has a default**: ABI §4.1 makes all the exports REQUIRED so the module carries them regardless, and what varies is whether there is anything behind one — a pure transform has no `start`, and a timer-driven emitter has no `process_signals` at all (§6.2 admits blocks that emit with no inbound batch).
- Port enums (`In`, `Out`) from the macro attributes — emitting to an undeclared port is a _compile_ error, not a runtime one. The enum's discriminant **is** ABI §5.2's port index rather than something kept in step with it.
- `prop_id` mapping from field order, and typed `Prop<T>` handles whose `get(ctx, signal_idx)` wraps the ABI `prop` call: grow-and-retry buffer loop (ABI §7.1), CBOR decode, declared-type check. `get_static(ctx)` = `SIGNAL_NONE` evaluation for use in `configure`/`start`/timers.
- **The manifest** (ABI §11): properties, ports, capabilities, ABI version are all derived from these same attributes and emitted as the `eio:manifest` custom section (ABI §4.4) at compile time — a `#[used]` `static` in a named `link_section`, so no build tooling is involved and a plain `cargo build` produces a self-describing module. `manifest.json` is `cargo eio build`'s (§5): writing a file is a build step, not a macro's. Single source of truth in code; manifest/import mismatches become unrepresentable rather than merely validated.

**One block per module.** The generated exports are `#[unsafe(no_mangle)]` and the manifest static has a fixed name, so a second `#[block]` in the same crate is a link error. That is the enforcement rather than a limitation: ABI §4.4 requires a module carrying more than one `eio:manifest` section to be rejected, because it describes itself twice.

### 1.1 Attribute grammar (normative)

```
#[block( <block-arg> ,* )]          on a struct with named fields
#[prop( <prop-arg> ,* )]            on a field of type Prop<T>
```

Each argument MAY appear at most once; a repeat is an error rather than last-wins, for the reason ABI §11.1 gives for duplicate JSON keys. Unknown arguments are rejected — a typo'd `capabilites` that silently granted nothing is the failure this prevents.

|`<block-arg>`|Form|Meaning|
|---|---|---|
|`name`|`name = "..."`|REQUIRED. The block's registry name; ABI §11.1's `^[a-z0-9]([a-z0-9._-]*[a-z0-9])?$`, ≤64 bytes|
|`version`|`version = "..."`|SemVer (ABI §11.1). Absent = the crate's `CARGO_PKG_VERSION`, which cargo already requires to be SemVer|
|`description`|`description = "..."`|Absent = no description|
|`inputs`|`inputs(a, b)`|Bare identifiers; **position is the port index** (ABI §5.2). Absent = none|
|`outputs`|`outputs(a, b)`|As `inputs`. `err` is REJECTED in both (ABI §6.4, §11.1)|
|`capabilities`|`capabilities(state, timer)`|ABI §11.1's closed set: `state`, `timer`, `gpio`, `i2c`, `http`. Absent = none. Declaring one generates its §4.2 callback export|

|`<prop-arg>`|Form|Meaning|
|---|---|---|
|`ty`|`ty = "float"`|REQUIRED. ABI §11.1's closed set: `bool`, `int`, `float`, `string`, `bytes`, `any`|
|`desc`|`desc = "..."`|Absent = no description|
|`default`|`default = "..."`|An expression string (ABI §11.1), checked by the manifest crate at parse time|
|`required`|`required`|A bare flag. Absent = `false`|

A field **without** `#[prop]` is the block's own state: it takes no `prop_id`, never reaches the manifest, and is initialized with `Default`.

ABI §11.1's rules are enforced at *expansion* time — reserved port name, duplicate port or property, name pattern, closed sets. Every one is something a host refuses at load, and §11.1 states them as regexes precisely so one rule reaches every surface; a block author should meet them at `cargo build`.

### 1.2 `Prop<T>` and the type mapping (normative)

|`ty`|Rust type|
|---|---|
|`bool`|`bool`|
|`int`|`i64`|
|`float`|`f64`|
|`string`|`String`|
|`bytes`|`Vec<u8>`|
|`any`|`Value`|

One-to-one, closed, and checked **at compile time**: a `Prop<f64>` field declared `ty = "int"` does not compile. The manifest's declared type and the field's Rust type are two statements about one property, and this is what stops them disagreeing — the run-time half (a host that sent something else) is a `BlockError` naming both.

There is deliberately no `i64` field satisfying a `float` property. ABI §11.1's int-to-float promotion is the *host's*, applied to an evaluated value and encoded as a float precisely so a guest never has to handle both — so an int arriving at a `float` field means the manifest declared `int`, and converting would hide that rather than report it.

`Prop<T>` holds only its `prop_id`. There is no guest-side cache: ABI §7.1 makes a property a pull, evaluated host-side per signal on demand, and the host already caches within a callback. A guest-side cache would answer a question about a signal the host has moved past.

## 2. Core types

- `Value` — the CBOR value enum (shared `signal` crate, no_std).
- `Signal` — map wrapper: `get/get_or/set/has`, serde-compatible via minicbor derive for typed extraction.
- `Batch` — owned Vec<Signal> with builder; `ctx.batch()` for capacity-hinted construction.
- `Ctx` — the only channel to the host: `emit`, `log` (also backing `log`-crate macros), `error` detail, `time_unix_ms`/`time_mono_ms`/`rand` wrappers, capability handles (§3).
- `BlockError` / `BlockResult` — errors map to non-zero callback returns + structured `error()` detail (ABI §8); `?` works throughout. `HostError` (from host-fn status codes) converts into `BlockError` with code preservation, so `ERR_THROTTLED` etc. remain matchable.

### 2.1 Where the ABI's shared vocabulary lives

ABI §8's error codes, §3's sentinels (`SIGNAL_NONE`, `PORT_ERR`) and §9.6's alignment are **not** the SDK's to define. They live in `eio-abi`, a dependency-free `no_std` crate that both `host-core` and `eio-sdk` read (DAEMON §1). `eio-sdk` re-exports what a block needs. ABI §12's version stays with `eio_manifest::Abi`, which owns the compatibility rule as well as the number (DAEMON §1).

This is stated normatively because the alternative is the obvious thing to do and is wrong twice over. Re-declaring the codes in the SDK would give the platform two hand-maintained copies of a table that hosts and guests MUST agree on. Depending on `host-core` for them would compile the expression interpreter and the manifest parser into every block — machinery a guest never runs, on targets measured in kilobytes. DAEMON §1's rule decides it: where a rule lives follows from what it is about, not from who happens to call it.

### 2.2 Error handling

`HostError` carries the ABI §8 code **as a matchable variant**, never flattened to a string, and names the import that returned it. Preservation is normative rather than a quality goal: ABI §7.2 tells a block to treat `state_put`'s `ERR_THROTTLED` as "retry later" and ABI §7.1 tells it that `prop`'s `ERR_NOT_FOUND` means the deployer configured nothing and the block should fall back to a value of its own. Neither instruction is actionable unless the block can branch on the code.

An unassigned negative code MUST be carried rather than collapsed: a foreign host on a later ABI minor can return one, and a block or an operator that loses the number has nothing to look up.

`BlockError` covers the block's own decisions and reports `ERR_INVALID_ARG` through `error()`. It MUST NOT borrow a host code for a failure the host did not report — that would put words in the host's mouth.

### 2.3 Limits are read, never assumed

ABI §9.7 gives `max_payload` and `max_batch` **no floor**: both are host configuration, and a block "may assume nothing about their size" (SCOPE §3.4 is OPEN on the policy around them, not on this). An MCU host may publish numbers a server host would consider unusable.

The SDK therefore surfaces both on `Ctx`, and checks the one the ABI makes checkable: `Ctx::emit` compares the batch's encoded length against `max_payload` before calling the host and refuses with `ERR_LIMIT` — the same code ABI §6.2 requires a host to return, so a block sees one answer whichever side noticed. The length is exact and known before the encode, so an oversized batch does not cost a serialization first.

`max_batch` is deliberately **not** checked. ABI §6.2's table of refusals whose code the spec fixes has three entries and the signal count is not among them, and §9.7's operative sentence about `max_batch` is that a host "never delivers batches beyond" it — the inbound direction. An SDK that refused locally would report an `ERR_LIMIT` no host produced, inventing a fourth refusal in the one place §6.2 says the answer must not vary. Whether `max_batch` bounds emissions at all is a genuine gap, tracked as eieio-7d8.13; until it closes, the limit is readable and the decision is the block's.

A block that hard-codes a size it believes is safe is a block that works on one tier and fails on another. There is no size that is safe to assume.

## 3. Capability wrappers

One safe wrapper per `eio:*` namespace, present on `Ctx` only when declared (macro gates them — using `ctx.gpio()` without `capabilities(gpio)` is a compile error):

- `ctx.state()` — `get/put/del` over typed CBOR values; grow-and-retry hidden; `ERR_THROTTLED` surfaced as a matchable error, per ABI §7.2's "best-effort, not a queue" posture.
- `ctx.timers()` — `set(Duration, Repeat) -> TimerId`, `cancel`. Fires `Block::on_timer(&mut self, ctx, TimerId)`.
- `ctx.gpio()` — mode/read/write/watch with typed enums. Fires `on_gpio(watch_id, Level)`.
- `ctx.http()` — `request(HttpRequest) -> ReqId`; completion fires `on_http(&mut self, ctx, ReqId, HttpResponse)`. **No async/await in guests** (PROPOSED, firmly): no runtime exists in the instance and the ABI is callback-shaped; correlating `ReqId -> purpose` is the block's job via its own fields. An SDK correlation-map sugar (`ctx.http().request_tagged(req, tag)`) is a candidate nicety for the in-depth pass, not core.

## 4. Guest internals (the unsafe budget)

- `#![no_std]` + `alloc`. The allocator is `dlmalloc` (Rust's own `wasm32` default, so a block gets the allocator it would have had from `std` without the `std`) behind `eio_alloc`/`eio_free`, with ABI §9.6's 8-byte alignment guarantee.
- The entire `unsafe` surface, enumerated for audit: allocator export glue, `(ptr,len) ↔ &[u8]` conversions at each export entry and host-fn call site, and the panic handler. Nothing else. Every `unsafe` block carries a `// SAFETY:` comment citing the ABI section that justifies it.
- **The enumeration covers generated code.** The `#[block]` macro emits `unsafe` — the instance statics ABI §1.2's single-threaded actor model permits, and the inbound-payload conversion at each export entry — and that code is compiled into every block. Which crate the text happens to sit in does not change whose `unsafe` it is, so the macro's templates are audited under this section like the rest.
- **Panics abort → trap → instance death** (ABI §6 invariant 6). The SDK's job is making panics rare in safe code (`get_or`, checked ops in examples) — not catching them. `panic = "abort"` enforced via the build tooling.

### 4.1 The allocator, and where it may be depended on

`eio_alloc` MUST return 8-byte-aligned pointers (ABI §9.6) and MUST return `0` rather than panicking or trapping when it cannot serve a request (ABI §9.5). Refusal is a legal answer and death is the wrong one, so the allocator path contains no panicking operation: a non-positive size and a size whose `Layout` cannot exist both return `0`.

**`dlmalloc` MUST be a target-gated dependency, not a target-gated `use`.** Its `global` feature has backends for wasm and unix only; on `thumbv7em-none-eabihf` and `riscv32imc-unknown-none-elf` it fails to compile outright. A `#[cfg]` at the use site does not prevent cargo from building the crate, so the gate belongs in `Cargo.toml`. This is recorded because the failure mode is a compile error in a dependency with no obvious connection to the flag that caused it.

`eio_alloc`/`eio_free` are exported on `wasm32-unknown-unknown` only. ABI §3 carries pointers as `i32`, which is exact where pointers are 32 bits and lossy everywhere else; the allocation *behaviour* is therefore reachable in native pointer width for testing, and only the `i32` conversion is guest-gated. A build asserts that the guest target's pointers really do fit.

### 4.2 What the SDK may depend on

A guest crate is constrained twice: no `std`, and — for the `no_std` gate to mean anything on the leaf tier — no assumption of atomic compare-and-swap, since `riscv32imc` has no `A` extension. `log`'s `set_logger` needs CAS and is unavailable there.

The SDK therefore compiles three ways, and a dependency MUST work in all three or be gated out of the ones it cannot serve: as a guest (`wasm32-unknown-unknown`, the only build that ships), against a hosted test target, and for a bare-metal target with no `std` and no atomics.

### 4.3 The panic handler reports before it traps

A trap reaches the operator as a host backtrace of WASM function indices: it says where, in a numbering nobody reads, and never says why. The Rust panic message exists at the moment of the panic and is gone the instant the trap fires.

The handler therefore formats the panic and calls `eio:core` `log` at level 4 (error) before trapping. The cost is real and is accepted: this is what pulls `core::fmt`'s formatting machinery into every guest. The alternative is a block that dies silently, which the platform's error posture exists to prevent.

The message MUST be formatted into a fixed buffer and truncated if it does not fit, never grown. A panic may be *from* the allocator, and a panic inside a panic handler is an abort with no message at all — strictly worse than the bare trap this improves on. Truncation is the right failure: half a message still names the file and line.

## 5. Build and packaging tooling

**PROPOSED:** a `cargo eio` subcommand (separate `cargo-eio` crate):

```
cargo eio new <name>         template block repo (CI included)
cargo eio build              wasm32-unknown-unknown, panic=abort, opt for size,
                             -C target-feature=-bulk-memory (ABI §4.3: a host
                             accepts MVP only, and rustc emits memory.copy),
                             embed eio:manifest section, emit manifest.json
cargo eio test               native tests + harness run (§6)
cargo eio aot --target esp32s3   WAMR AOT artifact for leaf targets
cargo eio publish            package OCI artifact (+ AOT variants), push, sign (cosign)
```

The template repo's CI runs build/test/publish on tag — this is the "block repos independently released to the registry" flow from SCOPE §3.6 made concrete.

## 6. Testing story

Two layers, both in-template:

1. **Native unit tests** — `TestHost`: a mock implementing the host side in-process (no WASM): scripted property tables (real `expr` crate evaluates them — same interpreter, honest semantics), signal delivery, emit capture, capability stubs (virtual GPIO/clock/state). Fast inner loop: `host.deliver("default", batch); assert_eq!(host.emitted("above").len(), 2);`
2. **Conformance run** — the same tests executed against the compiled `.wasm` under the reference harness (ABI §13), catching boundary bugs the native layer can't (memory conventions, encoding, limits).

## 7. Non-Rust authorship (deferred, SCOPE §6)

The ABI permits any language; the SDK does not chase this in v1. The conformance harness + golden blocks are the de facto spec for future SDKs (TinyGo, AssemblyScript, componentized Python for legacy nio-blocks migration). No design work now beyond keeping the harness language-agnostic.

## 8. Expansion list (for the in-depth pass)

HttpRequest/Response types, TestHost API, template repo contents, size-optimization defaults (opt-level, lto, strip, wasm-opt pass), SDK versioning vs ABI versioning policy, `request_tagged` correlation sugar decision.

Done since Draft 1: the macro attribute grammar and `Prop<T>`'s type mapping are normative in §1.1 and §1.2; §2 and §4 are expanded.

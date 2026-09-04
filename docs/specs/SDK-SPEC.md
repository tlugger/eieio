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
- Port enums (`In`, `Out`) from the macro attributes — emitting to an undeclared port is a _compile_ error, not a runtime one. The enum's discriminant **is** ABI §5.2's port index rather than something kept in step with it. `Out` carries one variant the block did not declare: **`Out::Err`**, ABI §6.4's reserved error port, whose discriminant is `PORT_ERR` rather than a position. Every block has that port whether or not it uses it, and §1.1 rejects `err` as a declared name, so there is nothing for it to collide with. It is generated rather than left to `eio_sdk::Out::ERR` because the generated enum *shadows* `eio_sdk::Out` in the block's own scope: making an author write a qualified path for the one port they did not have to declare is friction, and ABI §14 makes friction the wrapper's bug. `Out` is therefore never uninhabited, including on a block that declares no outputs at all — a sink can still report a signal it could not handle.
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
|`outputs`|`outputs(a, b)`|As `inputs`. `err` is REJECTED in both (ABI §6.4, §11.1) — the block gets `Out::Err` regardless, per §1|
|`capabilities`|`capabilities(state, timer)`|ABI §11.1's closed set: `state`, `timer`, `gpio`, `i2c`, `http`. Absent = none. Declaring one generates its §4.2 callback export|

|`<prop-arg>`|Form|Meaning|
|---|---|---|
|`ty`|`ty = "float"`|REQUIRED. ABI §11.1's closed set: `bool`, `int`, `float`, `string`, `bytes`, `any`|
|`desc`|`desc = "..."`|Absent = no description|
|`default`|`default = "..."`|An expression string (ABI §11.1). Parsed and statically analysed (EXPR §10) at expansion time, and — when it is signal-independent — folded and checked against `ty`|
|`required`|`required`|A bare flag. Absent = `false`|

A field **without** `#[prop]` is the block's own state: it takes no `prop_id`, never reaches the manifest, and is initialized with `Default`.

**ABI §11.1's rules are enforced at *expansion* time — all of them, and not a list of them.** The macro builds the manifest it is about to embed and hands it to the crate that decides whether a manifest is valid, so what `#[block]` accepts is by construction what a host accepts. That is the difference that matters: an enumerated set of checks is complete only until §11.1 grows a rule, and the rules that went missing that way were the two §11.1 judges as *content* rather than as shape — `version` against SemVer, and a `default` against EXPR §10 and its declared `type`. Neither falls out of a pattern, so neither was caught by a name check.

The macro still refuses, first and by hand, the rules it can point a *token* at — a reserved port name, a duplicate port or property, a name that fails its pattern. A verdict on a document cannot say which identifier was wrong, and a compile error that cannot underline the offending token is worth less than one that can. Where a rejection does come from the manifest, the macro maps it back to the span it recorded: the `version` literal, the offending `default`, the port.

Messages quote the pattern rather than paraphrasing it. §11.1 states these as regexes precisely so one rule reaches every surface, and a paraphrase in a compile error is a second statement of the rule that is free to drift from the first.

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
- `Batch` — owned Vec<Signal> with builder; `Batch::with_capacity(n)` for capacity-hinted construction. (Draft 1 wrote `ctx.batch()`. There is nothing on `Ctx` a batch's allocation needs — the guest has one allocator and `Ctx` holds no hint for it — so routing construction through the host handle would have been ceremony that suggested otherwise. Corrected here rather than implemented.)
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

ABI §9.7 gives `max_payload`, `max_batch` and `max_emission_bytes` **no floor**: all three are host configuration, and a block "may assume nothing about their size" (SCOPE §3.4 is OPEN on the policy around them, not on this). An MCU host may publish numbers a server host would consider unusable.

The SDK therefore surfaces both on `Ctx`, and checks the one the ABI makes checkable: `Ctx::emit` compares the batch's encoded length against `max_payload` before calling the host and refuses with `ERR_LIMIT` — the same code ABI §6.2 requires a host to return, so a block sees one answer whichever side noticed. The length is exact and known before the encode, so an oversized batch does not cost a serialization first.

`max_batch` is deliberately **not** checked. ABI §6.2's table of refusals whose code the spec fixes has three entries and the signal count is not among them, and §9.7's operative sentence about `max_batch` is that a host "never delivers batches beyond" it — the inbound direction. An SDK that refused locally would report an `ERR_LIMIT` no host produced, inventing a fourth refusal in the one place §6.2 says the answer must not vary. Whether `max_batch` bounds emissions at all is a genuine gap, tracked as eieio-7d8.13; until it closes, the limit is readable and the decision is the block's.

`max_emission_bytes` is surfaced and **not** checked either, for the reason above and one more. ABI §9.7 rule 9 bounds what one *callback* may emit, so a guest-side check would mean the SDK keeping its own running total and its own idea of where a callback begins — a second copy of the host's bookkeeping, in the guest, free to disagree with it. What the SDK does instead is carry the host's answer faithfully: `emit` returns `ERR_LIMIT`, a status code and therefore life (ABI §8), and a block that means to keep emitting chunks the rest across timers (ABI §10). `Option<u64>` and not a number, because ABI §5.2 makes the key's absence the statement — a block can tell "this host does not bound emission" from "this host did not say", and neither is a size it may assume.

A block that hard-codes a size it believes is safe is a block that works on one tier and fails on another. There is no size that is safe to assume.

## 3. Capability wrappers

One safe wrapper per `eio:*` namespace, present on `Ctx` only when declared. The gate is a
compile error, not a runtime `ERR_CAPABILITY`: `ctx.gpio()` in a block without
`capabilities(gpio)` does not name a method.

- `ctx.state()` — `get`/`put`/`del` over raw bytes or typed CBOR values; grow-and-retry hidden; an absent key is `None` rather than an error, and `ERR_THROTTLED` is a matchable error, per ABI §7.2's "best-effort, not a queue" posture.
- `ctx.timers()` — `once(delay_ms)` / `repeating(delay_ms)` returning a `TimerId`, and `cancel`. Fires `Block::on_timer`.
- `ctx.gpio()` — `mode`/`read`/`write`/`watch`/`unwatch` with typed `Mode`, `Edge` and `PinLevel` enums. Fires `Block::on_gpio`.
- `ctx.i2c()` — `write`/`read`/`write_read`, synchronous as ABI §7.5 requires.
- `ctx.http()` — `request(&HttpRequest) -> ReqId`; completion fires `Block::on_http`. **No async/await in guests**, firmly: no runtime exists in the instance and the ABI is callback-shaped; correlating `ReqId -> purpose` is the block's job via its own fields.

**`i2c` is a wrapper like the rest.** Draft 1 listed four and omitted it, which left ABI §11.1's fifth capability declarable and unusable — and a capability declared without being used produces a module every conformant host refuses, because ABI §4.2 requires the export/import pairing in both directions. A capability in the manifest's closed set with no wrapper is a trap, not an omission.

### 3.1 How the gate works, and why it is generated

The `#[block]` macro emits a `Capabilities` trait carrying only the declared accessors, and
implements it for `Ctx`. `Ctx` is one type in `eio-sdk` and cannot conditionally have
methods, so the alternative — the SDK owning the trait and the macro implementing a marker
per capability — is not available: both the trait and `Ctx` would be foreign to the block's
crate, which the orphan rule forbids. The error a block author sees is therefore `no method
named `gpio` found for `&mut Ctx``, which names the method and the type but does not suggest
the fix; that is the cost of the orphan rule rather than a choice.

A handle borrows the `Ctx` for its lifetime, so it cannot outlive the callback that took it
— ABI §1.2 gives an instance one caller at a time, and a handle held across callbacks would
be a way to pretend otherwise. The handles are zero-sized: ABI §7's capability functions are
free imports, not methods on state, so a handle exists to *scope* the calls and to be the
thing the macro can withhold.

**Declaring a namespace costs nothing until it is used.** WASM emits an import only for a
function something references, so the SDK declares all five namespaces unconditionally and a
block's import set is exactly what it calls — which is what ABI §4.3 requires, since imports
must not exceed declared capabilities.

### 3.2 No retries, anywhere

ABI §7.2 lets a leaf host answer `state_put` with `ERR_THROTTLED` to protect a flash wear
budget, and says blocks MUST treat persistence as best-effort and not as a message queue. The
wrapper therefore returns that code and never retries: a wrapper that retried would be
building the queue the spec refuses, and would hide from the block the one signal it can act
on. The same holds for every other capability — the SDK reports what the host said.

### 3.3 `HttpRequest` and `HttpResponse` (normative)

ABI §7.6 fixes the CBOR shapes; these are the Rust renderings, and the field names are those
keys exactly.

|`HttpRequest`|CBOR key|Absent when|
|---|---|---|
|`method: String`|`method`|never — required|
|`url: String`|`url`|never — required|
|`headers: Vec<(String, String)>`|`headers`|empty|
|`body: Vec<u8>`|`body`|empty|
|`timeout_ms: Option<i64>`|`timeout_ms`|`None` — the host's default applies|

Empty collections are **omitted** rather than encoded, which is ABI §11.1's posture
throughout: absent and empty say the same thing, and one way to say it is better than two.

`HttpResponse` carries the `status` the callback was given plus the decoded `{headers, body}`
map. **`status` is not normalized.** Below zero is a transport error and at or above zero is
the HTTP status (ABI §7.6); a 404 is an answer and a DNS failure is not, and a block retries
differently for each. `reached_a_server()` and `is_success()` name the two questions rather
than collapsing them.

**`request_tagged` is deferred**, and this records the decision. ABI §7.6's request-id
pattern makes correlation the block's job through its own fields, an SDK-side map would have
to guess at a lifetime for entries a block may never claim, and nothing about the sugar
changes the ABI — so it can be added whenever a block wants it, and nothing is foreclosed by
waiting.

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

A `cargo eio` subcommand (separate `cargo-eio` crate):

```
cargo eio new <name>         template block repo (CI included)          §5.1
cargo eio build              wasm32-unknown-unknown, panic=abort, opt for size,
                             a leaf-sized shadow stack, embed eio:manifest
                             section, emit manifest.json
                             (no feature flags: ABI §4.3's accepted set is what
                             rustc emits by default, and the flag this line
                             used to carry was measured to do nothing)   §5.2
cargo eio test               native tests + harness run (§6)             §5.3
cargo eio publish <registry> OCI artifact, push, optional cosign signature   §5.4
```

**PROPOSED**, and unimplemented:

```
cargo eio aot --target riscv32imc-unknown-none-elf   WAMR AOT artifact for a leaf target (LEAF §6.2)
```

`aot` stays marked because it depends on AOT variants the leaf tier has not specified yet.
`publish` no longer does: DAEMON §4 has a puller with a verified artifact shape, so there is
something concrete to produce rather than a place nobody has agreed on.

The template repo's CI runs build/test/publish on a `v*` tag — SCOPE §3.6's "block repos
independently released to the registry" made concrete. Signing is conditional on the signing
secrets being present, so a repo that has not set them still publishes.

### 5.4 `cargo eio publish <registry>`

Takes `<host>/<namespace>`; the reference is `<name>:<version>` from the manifest, so what is
published is what `build` already validated rather than anything restated on a command line.

**The artifact's shape is pinned to the puller, not to a specification of its own.** DAEMON
§4 verifies media types, the digest, and — when signed — a signature at the `sha256-<hex>.sig`
tag carrying `application/vnd.dev.cosign.simplesigning.v1+json` and the
`dev.cosignproject.cosign/signature` annotation (§4.2). A publish is correct when that puller
accepts it, which is a stronger and more useful test than any assertion about the pusher.

**Signing is optional**, because §4.2's `require_signed` defaults to false and a first publish
has no key. A missing `cosign` is an actionable error naming the alternative, never a panic —
the same posture §5.2 takes on `wasm-opt`.

**cosign's default shape, kept offline.** §4.2 verifies both of cosign's shapes — legacy
simplesigning and, since eieio-8yq.18, cosign 3.x's default Sigstore bundle — so `publish`
forces neither. Two flags remain, and both are about staying offline rather than about format:
`--use-signing-config=false`, because cosign otherwise fetches a TUF-provided signing config
over the network before signing anything, and `--tlog-upload=false`, which keeps the signature
off the public transparency log. The second is not independent — cosign 3.1.3 *refuses* it
unless the first is given too, which is worth knowing before anyone tries to drop one.

The bundle this produces carries no transparency-log inclusion proof and no signing-config
certificate: nothing §4.2 consults, and nothing a verifier would need a network call for.

### 5.1 The template (normative)

`cargo eio new <name>` writes a block repo that builds, tests and passes conformance with no
further editing. That is the whole requirement, and it is a requirement rather than a
courtesy: a template whose first run fails teaches a block author that the toolchain is
approximate.

```
<name>/
  Cargo.toml               the block crate; NOT a workspace member of anything
  .cargo/config.toml       §5.2's shadow-stack default, restated
  src/lib.rs               one #[block] struct and its Block impl (§1)
  tests/native.rs          the TestHost layer (§6.1)
  conformance/*.json       the harness layer (§6, ABI §13.1)
  .github/workflows/ci.yml build + test
  .gitignore
  README.md
```

Five things about it are normative, because each is load-bearing somewhere else:

- **`[lib] crate-type = ["cdylib", "rlib"]`.** The `cdylib` is the block; the `rlib` is what
  lets `tests/native.rs` name the block's type at all. A `cdylib`-only crate cannot be
  imported by its own integration test, which would put §6.1's whole layer out of reach.
  It does not change what the guest build emits.
- **The crate carries an empty `[workspace]` table.** A block repo is its own thing, and a
  template generated inside some other checkout must not silently join that checkout's
  workspace.
- **The dependency on `eio-sdk` is a registry dependency.** `cargo eio new --sdk-path <DIR>`
  rewrites it, and `eio-test-host`, to path dependencies against a local eieio checkout —
  which is what the tooling's own tests use, so "the template builds out of the box" is
  measured rather than asserted.
- **`[profile.release]` restates §5.2's defaults.** `cargo eio build` enforces them anyway;
  restating them is what makes a plain `cargo build --release --target wasm32-unknown-unknown`
  produce the same module. A block author who reaches for cargo directly should not get a
  different artifact.
- **`.cargo/config.toml` restates §5.2's shadow stack**, for the same reason and under
  `[target.wasm32-unknown-unknown]` rather than `[build]`. A rustflag cannot live in a
  `Cargo.toml`, so this is the file that gives the previous rule its other half — and the
  table is the target-specific one deliberately: it outranks the `build.rustflags` §5.2
  passes, so **this file is where a block raises its stack, and editing it moves both build
  paths together**. `[build]` here would move only the plain one. The size itself is
  *rendered*, not written into the template: §5.2 says which constant, and why.

The conformance scenarios name the built module by path, relative to the scenario file
(`../target/wasm32-unknown-unknown/release/<lib>.wasm`), because ABI §13.1 already says a
scenario names its module. Nothing about a block's suite is special: the same files run under
any host the harness can drive.

### 5.2 Size-optimization defaults (normative)

Blocks are pulled over networks onto devices measured in kilobytes of flash (SCOPE §3.7), so
the size posture is part of the contract rather than a preference. `cargo eio build` invokes:

```
cargo build --release --target wasm32-unknown-unknown
  --config 'profile.release.panic="abort"'
  --config 'profile.release.opt-level="z"'
  --config 'profile.release.lto=true'
  --config 'profile.release.strip=true'
  --config 'build.rustflags=["-C", "link-arg=-zstack-size=16384"]'
```

|Setting|Why|
|---|---|
|`panic = "abort"`|§4 requires it: a guest has no unwinder and a panic MUST become a trap. This one is a correctness rule wearing a profile's clothes.|
|`opt-level = "z"`|Size over speed. A block's work is bounded by its fuel budget (ABI §10), not by its instruction count.|
|`lto = true`|Fat LTO across the block and the SDK; it is what removes the capability wrappers a block never calls.|
|`strip = true`|Symbol names are the largest single component of an unstripped guest, and no host reads them: ABI §8's death report is a trap and a status code, and §4.3's diagnostics name imports and proposals rather than functions.|
|`-zstack-size=16384`|`wasm-ld` defaults the shadow stack to 1 MiB, which put every one of ABI §13.2's golden blocks at **17 declared pages, 1088 KiB** — 3.5× the whole of LEAF §4.2's target chip. 16 KiB takes all five to one page. The next paragraphs are why that number, why a default and not a ceiling, and why the module's *maximum* is left alone.|

They are passed as `--config` rather than left to the block's own manifest **deliberately, and
this is the point of the subcommand existing**: config-level profile settings override the
manifest's, so a block cannot ship with `panic = "unwind"` by editing a file. §4's rule is not
one a block author may opt out of on their own machine.

**The shadow stack is the one setting here that is a default rather than an enforcement, and
the difference is the point.** The profile rows above are enforced: `--config` outranks the
block's `Cargo.toml`, and §4's `panic = "abort"` is not negotiable. The stack size arrives as
`build.rustflags`, which is the **lowest**-priority of cargo's four rustflags sources — cargo
takes the first source that has any, in the order `RUSTFLAGS` env → `target.<triple>.rustflags`
→ `build.rustflags`, and does not merge them. So a block that genuinely needs a deeper stack
sets `[target.wasm32-unknown-unknown] rustflags` in its own `.cargo/config.toml` (§5.1 puts
the file there for exactly this) and gets it — from `cargo eio build` *and* from a plain
`cargo build`, identically, which is the property §5.1 asks of the profile.

**16 384 bytes, and the alternatives were measured rather than reasoned about.** 8 KiB, 16 KiB
and 32 KiB all produce a one-page module across the five golden blocks; 64 KiB produces two,
because the data section (~10 KiB) sits above the stack. Within the three that work, the
choice is how the first page is divided between stack and the guest's own heap, and 16 KiB is
the balance: it leaves ≈ 38 KiB of the page below `memory.grow`, which is more than nine times
LEAF §4.2's `max_payload` of 4 096 and therefore room for the inbound payloads `eio_alloc`
serves (ABI §9.5) without growing. 32 KiB halves that heap to buy stack a block has no reason
to use — a block author writes no recursion the SDK cannot bound — and 8 KiB gives the guest's
own frames no more room than LEAF §4.2 reserves for the *engine's* execution stack, which is a
smaller machine doing a larger job. Module size is unaffected: the five differ from their
1 MiB-stack builds by three bytes.

**Nothing here is a ceiling, and a ceiling does not belong here.** `cargo eio build` produces
ABI §11.1's portable module, the one that MUST always ship and that a daemon-class node runs
with no memory pressure worth the name. The number a declared minimum has to be checked
against is a *per-instance page budget*, and this command does not know one: it is a property
of a target, and LEAF §4.2 already places the refusal at firmware build time, where the budget
is known and where a refusal costs a build rather than a field failure. A second check here,
against a leaf's number hard-coded into the SDK, would be a second definition of the leaf's
memory budget — the drift this project's whole conformance argument exists to prevent. **A
block that declares more than one page is a valid block.** It is refused when, and only when,
somebody tries to put it on a leaf.

**The number is written once in Rust, and the copies it cannot reach are checked.**
`cargo-eio`'s `build::SHADOW_STACK_BYTES` is the single Rust definition: `cargo eio build`
formats the `--config` line above from it, and `cargo eio new` renders §5.1's
`.cargo/config.toml` from it as `{{stack_size}}`, so neither can drift from the other. Two
restatements remain that no constant can reach — this section, which is the decision rather
than a copy of it, and `examples/blocks/.cargo/config.toml`, which is a separate cargo
workspace. Both are pinned against the constant by `crates/cargo-eio/tests/end_to_end.rs`,
which extracts the invocation above out of this file the way `crates/manifest/tests/`
extracts ABI §11's example manifest. That test exists because the obvious one does not work:
a copy that drifted to 8 or 32 KiB would still produce a one-page module and still pass every
`min_pages == 1` assertion in the repository, silently.

What `build` does instead is **print the module's declared minimum linear memory** beside its
size, every time. That is the number LEAF §4.2 refuses on, and printing it is what turns a
displaced default — an author's `RUSTFLAGS`, which outranks everything above — into something
visible at the build that caused it rather than at somebody else's firmware build.

**`--max-memory` is not set, and that is a decision.** The shadow stack and the memory type's
*maximum* are different knobs pointed at different problems. The stack size moves the declared
**minimum**, which is the number a host must satisfy before an instance exists and the only one
it cannot negotiate — the whole of the defect. A maximum is a limit on `memory.grow`, and
setting one would change every block's behaviour rather than its size: a guest that could have
grown to serve a large batch would instead have `eio_alloc` return 0, which is ABI §9.5's
`ERR_LIMIT` path — a real, conforming, *different* outcome, imposed on the daemon tier to
encode a budget only the leaf tier has. It would also buy a host nothing it does not already
have, since a host that cares about growth must cap it itself; a maximum a module declares
about its own appetite is not a constraint a host may take on trust. So: the minimum is the
SDK's to keep small, the maximum is the host's to enforce.

**That last clause is now load-bearing rather than rhetorical**, and it is worth knowing what
each host does with it. LEAF §4.2's leaf enforces it at the engine: a block that declares no
maximum is capped at the leaf's per-instance reserve, and `eio_alloc` starts returning 0 past
it — exactly the `ERR_LIMIT` outcome the paragraph above describes, and no trap. A daemon
enforces nothing (DAEMON §4). Both are conforming (ABI §4.1). What has not changed is the
reason the SDK still sets no `--max-memory`: a block that declared one would be *refused* by a
leaf whose reserve is smaller rather than capped, since §4.1 forbids granting an instance less
than it declared — so the flag would convert a block that runs into a block that does not.

**No feature flags of any kind are passed.** ABI §4.3's accepted set is exactly what rustc
emits by default; the flag earlier drafts required here was measured to do nothing. Note
*emits*, not *enables*: rustc turns on six whole proposals and §4.3 accepts two of them only
in part, but the instructions in the carved-out remainder are ones rustc never reaches for —
measured across the five golden blocks, which contain none of them. So there is still no flag
to pass, and a block that trips the carve-out is one that went looking for it.

**`wasm-opt` is not invoked, and that is a decision rather than a gap.** Binaryen is a C++
toolchain, not a Rust dependency: requiring it would make `cargo eio build` fail on a machine
that has the Rust toolchain and nothing else, and would make the shipped artifact depend on
which version of a non-pinned external binary the builder happened to have — for a platform
whose two hosts must agree byte for byte. A block author who wants the last few percent runs
it themselves on the emitted module. If it is ever adopted it will be adopted as a pinned,
verified download in the release pipeline, where a reproducibility claim can be made about it.

After the build, `cargo eio build` reads the emitted module, validates it under the manifest
crate's full load-time checks (ABI §4: exports and their signatures, imports within declared
capabilities, capability-paired callbacks, the embedded manifest) and writes that manifest as
`manifest.json` beside the `.wasm`. The validation is the same one a host performs at load, so
a module that builds here is one a node will accept — that is what makes the build step worth
more than `cargo build` with flags.

### 5.3 `cargo eio test`

Both of §6's layers, in the order that makes a failure legible:

1. `cargo test` — the native `TestHost` layer, which is fast and fails with a Rust backtrace.
2. `cargo eio build` — because the harness layer needs the module the block actually ships.
3. Every scenario in `conformance/`, against the reference harness (ABI §13.1).

Native first because a block that is wrong is wrong more cheaply there, and a conformance
report on a block whose logic is broken says the same thing at ten times the length. A block
repo with no `conformance/` directory runs the first layer and says plainly that it ran only
one of two — never silently, since a suite nobody notices is missing is a suite nobody writes.

## 6. Testing story

Two layers, both in-template, and neither catches the other's bugs — which is why there are
two:

1. **Native unit tests** — `TestHost`: a host implementing the host side in-process (no
   WASM). Fast inner loop:
   `host.deliver("default", batch)?; assert_eq!(host.signals("above").len(), 2);`
2. **Conformance run** — the same block compiled to `.wasm` and driven under the reference
   harness (ABI §13.1), catching the boundary bugs the native layer cannot see: linear
   memory, `(ptr, len)`, CBOR crossing an engine, fuel and deadlines. The scenario a block
   author writes is a data file, not Rust (§13.1), so the same one runs on every host.

### 6.1 `TestHost` (normative)

`TestHost` lives in **`eio-test-host`**, not in `eio-sdk`. It is a *host* — it drives a
block the way a daemon does — so its dependency on `host-core` sits on the host side of the
boundary. Folding it into `eio-sdk` would put the expression interpreter one cargo feature
away from every guest, which is the coupling `eio-abi` was extracted to prevent (DAEMON §1).
Block templates take it as a dev-dependency.

**Properties are resolved by `host-core`'s `PropContext`** — the real `expr` interpreter,
the real per-callback cache, the real constant folding, the real declared-type check. Not
an evaluator that behaves like ABI §7.1 but the implementation of it. A stub evaluator
would make the fast layer a place where blocks pass and nodes fail, and ABI §13 calls that
divergence a conformance bug.

|Building|Meaning|
|---|---|
|`TestHost::<B>::builder()`|A fresh `B`, its `Prop<T>` fields bound by the `#[block]` macro through `Bound`. A test names the type and nothing else — binding a `prop_id` is the macro's job, and ABI §5.2 fixes it as the field's position.|
|`.inputs([..])` / `.outputs([..])`|Port names, position = index (ABI §5.2)|
|`.property(name, ty, source)`|A property and its expression. A literal is a trivial expression (ABI §11)|
|`.unset_property(name, ty)`|ABI §7.1's "no value at all": keeps its `prop_id`, answers `ERR_NOT_FOUND`|
|`.limits(max_payload, max_batch)`|What the descriptor publishes (ABI §9.7). Neither has a floor, and setting them small is the only way to find out whether a block reads them. `max_emission_bytes` (rule 9) is not offered: this host records emissions rather than queueing them, so it would be publishing a bound it never enforces|
|`.scripted(..)`|Capability answers, before the lifecycle runs — `configure` and `start` use capabilities too|
|`.seed_state(key, value)`|What `eio:state` already holds before the block runs, as raw bytes — the same standing state a conformance scenario's `state` field sets up (ABI §7.2)|
|`.configure()` / `.start()`|ABI §5.1's lifecycle, stopping after the first or running both|

|Driving|Meaning|
|---|---|
|`deliver(port, batch)` / `deliver_one(port, signal)`|ABI §6.1, by port *name*. A batch beyond `max_batch` is refused before the block is called, as §9.7 requires of a host|
|`fire_timer(id)`, `fire_gpio(watch, level)`, `complete_http(req, status, body)`|ABI §4.2's callbacks. The host drives these because that is which side they happen on|
|`stop()`|ABI §5.1|

|Asserting|Meaning|
|---|---|
|`emitted(port)` / `signals(port)` / `emissions()`|What the block emitted, by port name. `err` reaches `PORT_ERR` (ABI §6.4)|
|`property_failures()`|ABI §7.1's per-signal failures. How a test tells "the block skipped that signal deliberately" from "the expression was wrong" — identical from the emissions alone|
|`reported_errors()`|Detail the block sent through `eio:core` `error` (ABI §8)|
|`block()`|The block itself, for asserting on its own state|

Scripted capability answers are **queued, not set**: a block that reads twice gets two
answers, which is what lets a test script a sensor that changes between polls. A refusal
(`Throttle::Throttled`) is scriptable for the same reason it has to be — ABI §7.2's flash
wear budget is a property of the hardware, so a block's back-off path is otherwise
unreachable in a test.

**`eio:state` is additionally a real store.** `state_put` writes it, `state_get` reads it
back and `state_del` removes from it, so a block that writes on one delivery and reads on
the next reads what it wrote — the most obvious test of what ABI §7.2 is *for*, and one this
layer could not express at all while every answer came from a queue. Scripted answers still
win: a queued answer or a refusal is consulted first, and only an empty queue falls through
to the store. That order is what keeps §7.2's flash-wear back-off reachable, since a store
that always answered would make a refusal unscriptable.

The store belongs to the host, not the process, so two `TestHost`s on one thread never read
each other's keys. It answers `state_del` for a key that was never written the way
`host-core`'s store does, so what a block sees here is what it sees on a node.

**One block per test crate.** `#[block]` generates `#[unsafe(no_mangle)]` exports and a
single `EIO_MANIFEST`, so a second block in the same crate is a link error — §1's
one-block-per-module rule, met from the other side. In practice each block gets its own
file under `tests/`, which cargo compiles as its own crate.

**What `TestHost` does not do.** It runs the block as native Rust. There is no linear
memory, no `(ptr, len)`, no engine, no fuel and no deadline — so a block that passes here
can still fail the harness, and that is the division of labour §6 describes rather than a
gap in either layer.

## 7. Non-Rust authorship (deferred, SCOPE §6)

The ABI permits any language; the SDK does not chase this in v1. The conformance harness + golden blocks are the de facto spec for future SDKs (TinyGo, AssemblyScript, componentized Python for legacy nio-blocks migration). No design work now beyond keeping the harness language-agnostic — which ABI §13.1 makes structural rather than aspirational: it consumes a `.wasm` and a manifest, its scenarios are data, and `eio-conformance` does not depend on `eio-sdk`.

## 8. Versioning

**`eio-sdk` versions on its own Rust API surface, independently of ABI §12's number.** A breaking change to `Ctx`, to `Prop<T>`, or to what `#[block]` accepts bumps the crate the way any Rust crate bumps: it is what a cargo user reads a version as, and a scheme where wrapper breakage could not move major would be lying to `cargo update`.

The ABI number is not the wrapper's to carry. §12 already owns the compatibility rule — reject a `major` mismatch, accept a `minor` at or below the host's — and that number describes the *boundary*, not the crate that happens to wrap it. Two blocks built against different `eio-sdk` versions are interchangeable to a host if they declare the same ABI, and that is the property worth keeping legible.

**Which ABI a block declares is derived, never declared twice.** `#[block]` emits `abi: Abi::CURRENT`, read from `eio_manifest`, so the manifest's ABI version is a fact about the `eio-manifest` the SDK was compiled against rather than a number someone maintains in prose. There is therefore nothing for a release to get wrong here and no check to add: the drift this section would otherwise have to guard against is unconstructible, which is a better answer than a test.

What a release *can* get wrong is the dependency order. The crates published under one `sdk-vX.Y.Z` tag share a version because crates.io publishes them together (SCOPE §7.2), and `cargo publish` needs each crate's `eio-*` dependencies already on the registry — so the order is normative, not alphabetical, and `just publish-dry-run` is where it is stated once for both the release workflow and ordinary CI. Note what a pre-first-publish dry run cannot do: packaging resolves a path dependency against the live registry, so only the crates with no `eio-*` dependency of their own can be dry-run until the first tag ships.

## 9. Expansion list (for the in-depth pass)

Nothing outstanding.

Done since Draft 1: the macro attribute grammar and `Prop<T>`'s type mapping are normative in §1.1 and §1.2; the `HttpRequest`/`HttpResponse` types are normative in §3.3, which also records the `request_tagged` decision; the `TestHost` API is normative in §6.1; the template's contents are normative in §5.1 and the size-optimization defaults in §5.2, which also records the `wasm-opt` decision; §2, §3, §4 and §6 are expanded.

# Leaf Runtime Specification

**Status:** Draft 1 — architecture and contracts; expects per-subsystem expansion (§11). **Depends on:** SCOPE.md, ABI-SPEC.md, EXPR-SPEC.md, SERVICE-SPEC.md, DAEMON-SPEC.md. **Markers:** Settled decisions are stated plainly. **PROPOSED** = drafted here, awaiting ratification. **OPEN** = tracked in SCOPE.md §3, never decided here.

The leaf runtime is the leaf-class node runtime (SCOPE §3.7): a `no_std` Rust firmware image for MCU-class hardware that executes a service graph fixed at build time. DAEMON-SPEC's preamble defers to this document; this is that deferral answered.

---

## 1. What a leaf is, and what it is not

**A leaf is not a smaller daemon.** Both run the same blocks against the same ABI, and that is the whole of what they share operationally. The difference is *when the graph is decided*:

|                | Daemon-class | Leaf-class |
|---|---|---|
| Service graph | read from a file at boot, changeable at runtime | compiled into the image |
| Blocks | pulled from a registry, hot-loaded | AOT-compiled and linked in |
| Deploy | `PUT` a file, start it | build firmware, flash it |
| Management API | the whole of DAEMON §9 | none (§7) |
| Filesystem | a data directory (DAEMON §2) | none required |

Every one of those follows from a single physical fact: an MCU has no room to compile WASM and nowhere to put a block registry. **The design consequence that matters is that a leaf has no configuration surface at runtime** — there is no file to edit and no endpoint to call, so everything a daemon reads from `node.toml` and a service file is instead *baked* (§6).

**What is identical, and MUST stay identical:** the ABI a block sees (ABI-SPEC in full), the expression language and its semantics (EXPR-SPEC), the canonical CBOR encoding (ABI §6.3.1), the manifest schema (ABI §11), and the wire vocabulary of a signal. A block author targets one platform, not two. SCOPE §3.7 puts it as "extra flashing steps are acceptable; a different design flow is not"; this section is the runtime half of that sentence.

## 2. Architecture

The leaf runtime is a Rust binary crate — `crates/leaf`, which exists and is built for the host — that links the ★ crates unchanged:

```
  eio-abi        status codes, sentinels, alignment (ABI §8, §3, §9.6)
  eio-signal     CBOR value/signal/batch types (ABI §6.3)
  eio-expr       the expression language (EXPR-SPEC)
  eio-manifest   manifest schema and the ABI §4.3 load-time cross-check
  eio-host-core  lifecycle driver, memory conventions, property resolution,
                 router core, the StateStore and Timers traits
```

**This is the entire reason those crates are `no_std`.** DAEMON §1 calls the host-core/daemon split "load-bearing"; a leaf runtime is the load it bears. `just check-nostd` compiles them for `thumbv7em-none-eabihf` and `riscv32imc-unknown-none-elf` on every gate run, which is the guard that keeps this section implementable rather than aspirational. **The two gate targets are not both leaf targets** — §6.2 says which one v1 builds a leaf for, and why the other keeps earning its place in the gate anyway.

What the leaf adds on top, and what it MUST NOT:

- **Adds:** an engine binding (§3), a `StateStore` against flash (§5), a `Timers` implementation against a hardware timer, a transport client (§8), and a generated `main` that constructs the baked graph (§6).
- **MUST NOT add:** a second lifecycle driver, a second property-resolution rule, a second router, a second expression interpreter, a second CBOR encoder, or **a second implementation of `eio:core`'s host functions** (DAEMON §1.1 — a leaf supplies a clock and an entropy source and shares the rest). Every one of those exists in a ★ crate precisely so that two hosts cannot disagree, and reimplementing one for size or speed is the divergence ABI §13 calls a conformance bug by definition.

**No allocator is not an option.** The ★ crates permit `alloc`, and ABI §6.3's batches are dynamically sized. A leaf therefore ships a global allocator over a fixed heap. Which allocator, and the heap's size, are per-target build configuration.

## 3. The engine

**WAMR** (WebAssembly Micro Runtime), in AOT mode for deployment (SCOPE §3.2, SDK §5). Its interpreter is also a supported mode and is what a bring-up or a debugging build uses.

This choice is **measured, not assumed** — `crates/conformance/tests/wamr.rs` runs the full ABI §13 scenario suite and the ABI §4.3 instruction checks against WAMR's fast interpreter. What that measurement established, and what this spec therefore inherits:

- **31 of 32 scenarios pass.** The one skip is `07_budget_exhausted`, for the reason in §4.
- **WAMR refuses all nine proposals outside the accepted six**, including tail call, memory64 and threads — the three wasm3 *runs* (ABI §4.3's measured gaps). A leaf on WAMR needs no loader carve-out of its own for those three; the carve-out stays because it is wasm3's.
- **WAMR runs the whole of bulk memory and reference types**, where wasm3 runs part. This widens nothing: ABI §4.3's portable subset is the floor across leaf engines, not a description of one, and a module using `table.copy` runs on WAMR and fails on wasm3. The loader refuses it on both.
- **WAMR's rejections do not name the proposal they objected to.** They are opcode- and section-level parse errors (`unsupported opcode fd`, `invalid limits flags`). ABI §4.3 makes naming a MUST only where the engine reports it; a host cannot invent a name its engine does not give.

**wasm3** is the second measured interpreter (`crates/conformance/tests/wasm3.rs`) and remains a valid leaf engine. It has no AOT path, so a wasm3 leaf is interpreted throughout.

### 3.1 The engine is the only place the feature set is enforced (ABI §4.3)

ABI §4.3 splits refusal across two layers, and a leaf implements both. Stated concretely, because "configure the engine to the accepted set" is not an instruction anyone can follow twice the same way:

- **WAMR** selects features at *build* time through its CMake configuration, not at runtime through a config object. A leaf's WAMR build MUST enable `bulk-memory` and `reference-types` and MUST NOT enable SIMD, tail call, multi-memory, memory64, threads, exceptions or GC. The default build already refuses all nine (measured, above), so the requirement is that a leaf does not go *adding* them for a convenience.
- **wasm3** has no feature switches; what it accepts is what it was compiled with. Its acceptance was measured instruction by instruction (ABI §4.3) rather than read off a list, and that measurement is the specification of what a wasm3 leaf accepts.

**Neither engine can express the carve-out**, because a proposal is one switch and the accepted set is part of two of them. `eio_manifest::validate` is the second layer and runs on every host, leaf included — it is a ★ crate for exactly this reason. **A leaf MUST run it**, at firmware build time where a refusal costs a build rather than a field failure.

## 4. Budgets

ABI §10 requires every callback to run under a host-enforced budget. **WAMR cannot supply a fuel counter as built**: `wasm_runtime_set_instruction_count_limit` exists in its C API but is compiled out behind `WASM_ENABLE_INSTRUCTION_METERING`, confirmed by a linker error rather than by reading documentation. wasm3 has no counter at all.

So a leaf's budget is a **watchdog**, not fuel: a hardware timer armed before entering a guest callback and disarmed on return, whose expiry kills the instance exactly as ABI §8 requires of a deadline violation. This is the leaf runtime's to add rather than the interpreter's to provide, which is why the conformance harness lets a host answer `enforces_budgets = false` and have `07_budget_exhausted` skipped by name — a binding without a watchdog is honest about it rather than hanging.

**A leaf's own budgets sit near EXPR §9's floors** (`MAX_FUEL` 10 000, `MAX_DEPTH` 32, `MAX_RANGE` 1 000, `MAX_VALUE_BYTES` 4 096, `MAX_EXPR_BYTES` 1 024), which is what §9 already tells leaf hosts to do. They are *floors*, so a conforming expression may rely on that much and a leaf MUST NOT go below them.

**That the floors are adequate is now measured, not assumed** (eieio-x7g.6). The whole of `expr-tests/` runs at them: 484 language vectors, 29 property vectors through the leaf's own `compile_with_limits` call, and 46 canonical-CBOR vectors — 559 in all, none failing. Before this, every vector in the repository ran at the reference defaults, so "a budget floor that only holds on a generous host is not a floor" (§9) was a rule nothing had tested. A vector that failed here would have meant one of two things, and they need different fixes: the vector relies on more budget than §9 says a conforming expression may rely on, or the floors are set too low. Neither happened. ABI §9.7's `max_payload` and `max_batch` remain **OPEN** in SCOPE §3 — a leaf supplies both explicitly and blocks may assume nothing about them.

### 4.1 Decode depth is coupled to `MAX_DEPTH`

A CBOR decoder needs a nesting bound or a hostile batch is a stack overflow, and a stack overflow on an MCU is not a caught error. **That bound MUST be at least the configured `MAX_DEPTH`.** Setting it lower makes a value the expression language is required to handle undecodable, which turns an EXPR §9 budget into a decode failure with a different error code, on one host only — the shape of divergence ABI §13 exists to prevent.

**The MUST is a floor, and a leaf should not sit on it** (eieio-x7g.7). A leaf's *evaluation* budgets belong at EXPR §9's floors, but its decode bound is not the same kind of number: it decides which values a leaf can receive at all, so lowering it toward `MAX_DEPTH` makes a batch that a daemon routes without complaint undecodable on a leaf — divergence again, in the other direction, bought for stack headroom. **A leaf therefore matches the daemon's decode bound rather than its own floors**, and both pass `eio-signal`'s `MAX_DEPTH` today. That may be the wrong trade on a real target, where 128 levels of recursive decode is exactly the overflow this section exists to bound; but it is a decision about interoperability as well as safety, and it needs a measured stack, so it is §11's memory-budget item and not a knob to turn early.

## 5. State on flash

`eio:state` (ABI §7.2) is backed by flash through `host-core`'s `StateStore` trait — the same three functions, `get`/`put`/`del`, the daemon implements against redb (DAEMON §10). The trait is the boundary, so the host functions that decode `(key, key_len, buf, cap)` and apply ABI §8's size convention are shared code and cannot diverge.

**Wear is the difference, and `ERR_THROTTLED` is how it is spoken.** ABI §7.2 permits a leaf host to refuse a `state_put` for a wear budget; the daemon never does, and the variant is plumbed on both so a block's back-off branch is the same code either way. Two obligations follow:

- A leaf MUST NOT silently drop a write. Refusing with `ERR_THROTTLED` is the contract; succeeding and not persisting is not.
- ABI §7.2's "blocks MUST treat persistence as best-effort and not as a message queue" is what makes the refusal safe. A block that cannot tolerate a refused write is a block that cannot run on a leaf, and that is a property of the block.

**Namespacing is `(service, instance)`**, as DAEMON §10 establishes and for the same reason: a node does not know its System. On a leaf there is exactly one service, so the service component is constant — it is kept anyway, because dropping it would make a leaf's key layout differ from a daemon's for no gain, and `eieio`'s whole conformance argument is that the two agree.

The **wear budget policy** — how much writing is too much, over what window, and what a leaf does when a block ignores repeated refusals — is **OPEN** (SCOPE §3.7).

## 6. What is baked, and what a build produces

A daemon reads `node.toml` (DAEMON §2.1) and a service file (SERVICE-SPEC) at boot. A leaf has neither at runtime, so the firmware build resolves both and bakes the results:

- **The service graph**: instances, their block AOT artifacts, their resolved property expressions, and the connection table `host-core`'s router consumes.
- **The node's identity and limits**: what `node.toml` would have carried.
- **The transport configuration** (§8): what `pubsub.toml` would have carried.

**The service file is still the source, and stays the portable artifact.** The same file deploys to a daemon; SERVICE-SPEC parses it; the firmware build is one more consumer. It is not parsed *on* the leaf — `eio-service` is a `std` crate and deliberately so (CLAUDE.md: nothing parses a service file on a leaf tier) — it is parsed by the build host, which then emits Rust.

**Property expressions are baked as source text, not as a compiled form.** ABI §11 makes every property an expression evaluated per signal, and EXPR-SPEC's parser is a ★ crate that runs on the leaf. Pre-parsing to an AST at build time is a plausible optimisation and is explicitly **not** specified here: it would put a second representation of an expression into the platform, and the first thing to measure is whether parse cost matters at all when properties are parsed once at configure time (ABI §5.1) rather than per signal.

### 6.1 The AOT artifact

`cargo eio aot --target <leaf>` produces a WAMR AOT artifact per block, and ABI §11.1's manifest carries an `aot` list naming the prebuilt targets published alongside the portable module. §6.2 says which targets those are and §6.2.1 how each is spelled. The portable `wasm32-unknown-unknown` module **MUST always ship** (ABI §11.1): an AOT artifact is an optimisation for one target, never a replacement for the thing every host can run.

**AOT artifacts are version-sensitive, and the pairing is normative.** A WAMR AOT artifact is tied to the WAMR version that compiled it and to the LLVM that WAMR was built against — **WAMR 2.4.5 pins LLVM `release/18.x`**. A leaf image and the artifacts it loads MUST come from the same WAMR version. Recording the pair is not bookkeeping: a mismatched artifact is a load failure in the field, after flashing.

This section is **PROPOSED and unimplemented**: `wamrc` has not been built on any developer machine here (six distinct blockers recorded on `eieio-7d8.21`), so the artifact layout is specified from WAMR's documentation rather than from something this repository has produced. **It ratifies when a leaf loads an artifact this pipeline built**, and not before. The interpreter path (§3) needs none of it and is what a first leaf bring-up should use.

### 6.2 The v1 target list

**v1 is one target**, and naming one rather than three is the decision rather than a shortening of it: every remaining §11 item — heap sizing, watchdog mechanics, flash layout, the transport client — is a *per-target* question, so a second target does not add a target, it doubles four unanswered questions before any of them has been answered once.

|eieio leaf target|Rust triple|Toolchain|Exemplar silicon|
|---|---|---|---|
|ESP32-C3 class|`riscv32imc-unknown-none-elf`|stock rustup, prebuilt `core`/`alloc`|ESP32-C3, ESP32-C2|

Why this one:

- **It is the tier SCOPE §3.7 names.** "Leaf-class (MCU: ESP32 etc.)" names a *family*, never a triple; this section is where the family becomes a target.
- **Stock rustup reaches it**, so `rust-toolchain.toml`'s single exact pin keeps meaning what it means. That pin exists because "clippy and rustfmt change behaviour between releases, and every host implementation has to agree byte for byte"; a target needing a second toolchain buys a chip at the cost of that property.
- **The ★ crates already compile for it** on every gate run (`just check-nostd`), and it is the leg with **no atomics at all** — rv32imc lacks the `A` extension — which is what caught `log::set_logger` being unavailable without compare-and-swap. A leaf target whose constraints the gate already enforces is one where a `no_std` regression fails in CI rather than at flash time.
- **One vendor supplies the whole stack.** §5 needs a flash driver, §4 a hardware timer, §8 a network stack; here they come from one ecosystem and one decision instead of four independent ones.

**`riscv32imc-unknown-none-elf` and not `riscv32imc-esp-espidf`**, and the difference is not cosmetic: the `esp-espidf` triple is a `std` target. Taking it would make this document's opening sentence — "a `no_std` Rust firmware image" — false, and would make `check-nostd` a gate that no longer describes the thing it guards. **The cost of that choice is recorded here rather than discovered later**: WAMR's platform ports (`esp-idf`, `zephyr`, `nuttx`, `riot`, `freertos`) are OS-shaped and none of them is bare metal, so a `no_std` leaf owes the engine a platform shim, and it forgoes ESP-IDF's MQTT client and flash API that a `std` build would inherit. That is §11's engine-binding and transport work, not something a target list can hand it. If the shim proves prohibitive when someone measures it, the honest response is to reopen this section with `riscv32imc-esp-espidf` as the named alternative — not to weaken §2.

**The WAMR half of the target, stated as far as it is honest to state it.** §3.1 already fixes the feature set and it is not per-target; what a target adds is `WAMR_BUILD_TARGET=RISCV32_ILP32` — soft float, matching `imc`'s absent `F` — a `WAMR_BUILD_PLATFORM` port, and for the AOT path a `wamrc --target=riscv32 --target-abi=ilp32`. **Those three strings are read from WAMR's documentation and have not been run here**, for the reason §6.1 already records: `wamrc` has never been built on a machine in this project. They are §11's pipeline and engine-binding items to confirm, and nothing in this section depends on them — §3's interpreter is what a bring-up uses, and it needs no `wamrc` at all.

**No board is recorded anywhere in this repository or its tracker.** A target list nobody can flash is a list, not a decision, so this one is chosen partly for being the cheapest devkit to put on a desk; the first image that boots is the milestone that makes the choice real, and it is the milestone that reports what it cost in flash and RAM.

**Deferred, with the reason**, because "not v1" and "rejected" are different claims:

|Family|Triple|Why not v1|
|---|---|---|
|Cortex-M4F|`thumbv7em-none-eabihf`|Stays a `check-nostd` **gate** target and is not a leaf target. The gate's value — rejecting `std`, rejecting hard-float assumptions — is unaffected by whether a leaf is built for it. As a *leaf* target the triple names a CPU and nothing else: no board, no RTOS, no network stack, no flash part, and WAMR's MCU ports are RTOS-shaped, so adopting it is really adopting Zephyr, NuttX or RIOT — a decision nothing in this repository has made and no issue owns.|
|ESP32-C6 / H2|`riscv32imac-unknown-none-elf`|The **cheapest second target**, and the migration path below is written for it: same vendor stack, and `rustup target add` reaches it. It is a separate entry rather than free coverage because the ISAs differ — C6's native target has the `A` extension and C3's does not — which is exactly why an `aot` entry is spelled as a triple and not as a chip name.|
|Classic ESP32, S2, S3|`xtensa-esp32*-none-elf`|Xtensa, and **measured rather than assumed**: on the pinned 1.97.1 toolchain `rustup target list` offers no `xtensa` entry at all, and `rustc --target xtensa-esp32-none-elf` answers `can't find crate for 'std' … the target may not be installed` — the target *spec* is known, the standard library for it is not shipped. Upstream LLVM carries no enabled Xtensa backend, so these need the esp-rs fork (`espup`) and `-Zbuild-std` — a second toolchain channel, with its own `clippy` and `rustfmt`, against the one pin that makes two host implementations agree byte for byte. Deferred rather than refused: S3 is the most capable part in the family and the criteria below say what adding it costs.|

#### 6.2.1 How an `aot` entry is spelled

**An `aot` entry naming an eieio leaf target MUST be that target's Rust triple, verbatim as the table above gives it** — `"aot": ["riscv32imc-unknown-none-elf"]`. It names *the leaf the artifact is loadable by*, which is why a Rust triple is the right key even though `wamrc` and not `rustc` produced the bytes: a leaf image is a Rust build for a triple, and the match is a string comparison against a value the firmware build already holds.

- **One vocabulary per document.** ABI §11.1's `targets` already carries triples and already requires `wasm32-unknown-unknown` among them. Two adjacent lists in one manifest spelled two different ways is a trap, and the difference between the lists should be *portable module versus AOT artifact*, not *triple versus nickname*.
- **A triple states the facts that decide whether an artifact loads** — instruction subset, atomics, float ABI. A chip name does not: one `esp32c6` string cannot say whether the artifact assumes the `A` extension, and the answer decides whether it also runs on a C3.
- **No mapping table to drift.** rustup, `rust-toolchain.toml`, `check-nostd` and `cargo eio` all speak triples already, so `cargo eio aot --target riscv32imc-unknown-none-elf` needs no lookup that could disagree with this section.
- **They fit the contract as written.** ABI §11.1's name pattern and 64-byte bound admit both triples above unchanged, so this convention needs no schema change.

**The name is a necessary key, not a sufficient one.** §6.1's WAMR-and-LLVM pairing is not carried in the string and MUST still be checked; *where* that pair is recorded is §11's pipeline item.

ABI §11.1 is unchanged in force: `aot[]` stays an open name pattern rather than a closed set, because the registry may carry artifacts for targets this platform has not defined. This section binds the spelling of the ones it *has* defined.

**This spelling is robust to a question §11 has not answered.** Whether a leaf links its AOT artifacts in at firmware build time (§1's table) or loads them from flash at runtime (§6.1's wording) changes *who compares the name and when*, and not what the name is. That contradiction is real and is tracked; it does not reach this section.

#### 6.2.2 Adding a target

Adding one is a checklist, not an argument. A family joins the list when all five hold:

1. A Rust triple `rustup` ships, or a written reason it cannot be one.
2. A WAMR build for it at §3.1's feature set — `WAMR_BUILD_TARGET` plus a platform port — that passes the ABI §4.3 instruction checks as built.
3. A `Timers` against a hardware timer, a `StateStore` against its flash, and a transport client (§8).
4. A board someone can flash, running all three of §9's suites at the leaf's own budgets.
5. An entry in §6.2's table and the triple added to `check-nostd`'s target list, so the `no_std` claim is gated for it too.

## 7. There is no management API

A leaf serves no HTTP. DAEMON §9's entire surface is absent, and that is a design decision rather than a gap:

- **It cannot be authenticated safely enough to be worth it.** SCOPE §3.11 leaves transport security OPEN, and an MCU is the tier least able to carry a TLS stack and a credential lifecycle.
- **It would have nothing to serve.** Two thirds of §9 mutates a service file or a block cache, and a leaf has neither.

**Consequences that other specs already encode**, restated here so a leaf implementer meets them in one place:

- DESIGNER §3.1: the Designer's proxy and its node probe both **refuse a leaf by name** rather than dialling it. A leaf's address over HTTP would give a connection error indistinguishable from a node that is down, reporting a fault against a node working exactly as designed. (The `eio` CLI now has the same guard, for the same reason: `nodes.toml` carries an optional `class` per node, absent meaning `"daemon"`, and `eio` refuses a `"leaf"` entry by naming the class. SCOPE §3.7.)
- DAEMON §7.1: only a daemon-class node is eligible to be the pub/sub broker. A leaf is never a candidate.
- Observability is the wire protocol's (§8), not an endpoint's.

## 8. Transport

A leaf participates in the same pub/sub as a daemon: MQTT behind the same conceptual bridge boundary (SCOPE §3.9, DAEMON §7). The guarantees are the platform's own vocabulary — at-most-once, never-retained (SCOPE §3.4) — and are *mapped* onto QoS at the bridge, not stated in QoS terms.

`publisher` and `subscriber` remain host-native system blocks (DAEMON §6): they need credentials and transport internals, which is the whole reason that precedent exists and the whole reason it does not extend to anything else.

**The bus pre-shared key (SCOPE §3.11) applies unchanged.** It was chosen over mTLS with a System CA *because* of this tier — a CA lifecycle plus a TLS stack on every node is the weight that deletes the embedded north star. A leaf presenting the bus key is the case that decision was made for.

Which MQTT client a leaf links, and how it behaves across reconnects on a constrained device, is an **§11 expansion item and is deliberately unnamed here**: `rumqttc` is the daemon's choice and is `std`, so a leaf needs a `no_std` client or an `embedded-nal` stack, and nothing has been measured. This section endorses no client.

## 9. Conformance

**A leaf MUST pass what a daemon passes, and divergence is a conformance bug by definition** (ABI §13). Not "as far as is practical on an MCU": the suites are the contract, and a leaf that cannot pass one has found either a bug in itself or a rule the platform should not have.

Three suites, all of which already exist:

1. **The ABI §13 scenario suite**, driven through `host-core`'s `Engine` trait exactly as the daemon and the reference harness drive it. A capability a leaf does not implement is reported **skipped by name**, never passed over.
2. **`expr-tests/`** — the expression language, property types, and canonical CBOR. Run **at the leaf's own budget settings** (§4), not at the reference defaults: a budget floor that only holds on a generous host is not a floor.
3. **The ABI §4.3 instruction checks**, against the engine as the leaf actually builds it (§3.1). One table, shared — `crates/conformance/tests/support/wasm3_instructions.rs`, which the conformance wasm3 test and the leaf's own test both read. A second copy of the accepted set would be the divergence this whole section exists to prevent, written down twice.

   **What this catches is narrower than "the leaf's engine is configured correctly", and the difference is worth knowing** (eieio-x7g.8). Every case calls its export, so a knob that only changes *load-time* behaviour is invisible to it: `CompilationMode::Eager` and `Lazy` both pass, because calling forces per-function compilation either way. Knobs that change behaviour under a call are caught — a deliberately tiny interpreter stack fails the suite naming `memory.copy`. So this suite pins what the engine *executes*, not how it got there, which is the right thing for a set defined by what the toolchain emits and the interpreter runs.

### 9.1 Canonical CBOR: a stock encoder is wrong for this platform

ABI §6.3.1 deviates from RFC 8949 §4.2.1 in **two** places, and both are easy to violate by reaching for a well-regarded CBOR library:

- **Floats are `binary64` always.** Shortest-float encoding is forbidden. RFC 8949's preferred serialization would shrink some floats to `binary16`/`binary32`, making a value's encoded width depend on its magnitude.
- **Map keys sort by UTF-8 *content*, not by encoded bytes.** RFC 8949 orders by the encoded key, which sorts `"z"` before `"aa"`. This platform orders by content, so that one ordering serves the encoding, EXPR §2's map iteration order, and `(keys m)`.

A leaf uses `eio-signal`, which implements both, and MUST NOT substitute another encoder for size. The canonical-CBOR vectors in `expr-tests/cbor/` cover both deviations and are part of §9's obligation.

## 10. The deploy contract with the Designer

DESIGNER §7 gives the Designer's half: same canvas, same service file, the deploy button does the right thing per node class, and extra flash steps are surfaced as steps rather than as a different design flow. This is the other half.

A leaf deploy is a **build**, and the contract is what the build promises the Designer:

- **It is offered the same service file** a daemon would be `PUT`. If the file is valid for a daemon and every block it names has an AOT artifact for the target, the build is expected to succeed.
- **Validation happens before the build, not during it.** SERVICE §7's stages and ABI §4.3's load-time check both run on the build host, so a rejection is a message about a service file rather than a compiler error. A block whose manifest requires a capability the target lacks is refused here — the same check DESIGNER §5 surfaces at design time, enforced where it is binding.
- **It produces a flashable image and the steps to flash it.** Whether the Designer drives the flash tool directly is DESIGNER §7's business, not this document's.
- **A failed build changes nothing on the device.** The running firmware is untouched until a flash succeeds, which is the one operational advantage this tier has over a hot-loading daemon.

The build pipeline's own mechanics — how the toolchain is pinned, where AOT artifacts are cached, how a build is reproduced — are **§11 expansion items**, not settled here.

## 11. Expansion list (for the in-depth pass)

Needed before implementation, and deliberately not guessed at in this draft:

- ~~**The target list.**~~ — **resolved** (eieio-x7g.2.2): §6.2 names it. v1 is one target, `riscv32imc-unknown-none-elf` (ESP32-C3 class); `thumbv7em-none-eabihf` stays a `check-nostd` gate target and is not a leaf target; `riscv32imac-unknown-none-elf` (ESP32-C6/H2) is the named next candidate; Xtensa is deferred for the esp-rs toolchain fork, measured rather than assumed. §6.2.1 fixes how an `aot` entry is spelled and §6.2.2 what adding a target costs.
- **Firmware build pipeline mechanics** — toolchain pinning, AOT artifact caching, reproducibility, and how `cargo eio aot` is invoked from a Designer deploy.
- **The generated `main`**: what the baked graph looks like as Rust, and whether it is generated source or a const table.
- **Memory budget**: heap sizing per target, and what a leaf does when a batch will not fit.
- **The transport client** (§8), once one has been measured.
- **Watchdog mechanics** (§4): which timer, what granularity, and how a killed instance is reported when there is no log stream to report it on. **A host bring-up cannot stand in for this one**, measured rather than assumed: `wasm3x` 0.1.0 exposes no interruption, abort or termination entry point at all, so nothing outside a running guest call can end it, and the host leaf therefore answers `enforces_budgets = false` and has ABI §13's budget scenario skipped by name — which is §4's honest-binding rule working as intended, not a gap in the leaf. The watchdog becomes implementable at the same moment the target does: a hardware timer and an engine that can be told to stop.
- **Observability without an API** (§7): what a leaf publishes about itself, and on which topic.
- ~~**A class-aware CLI**~~ — **resolved** (eieio-x7g.5): it learns the class. A node entry in `nodes.toml` carries an optional `class`, `"daemon"` or `"leaf"`, absent meaning `"daemon"`, and `eio` refuses a leaf by naming the class rather than reporting a failed request. SCOPE §3.7 records the decision and why the two alternatives — requiring the key, or inferring the class from a refused connection — are each worse.
- **Flash layout**: where AOT artifacts, state and configuration sit, and how a firmware update treats existing state.

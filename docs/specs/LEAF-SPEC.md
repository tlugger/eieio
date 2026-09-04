# Leaf Runtime Specification

**Status:** Draft 1 — architecture and contracts; expects per-subsystem expansion (§11). **Depends on:** SCOPE.md, ABI-SPEC.md, EXPR-SPEC.md, SERVICE-SPEC.md, DAEMON-SPEC.md. **Markers:** Settled decisions are stated plainly. **PROPOSED** = drafted here, awaiting ratification. **OPEN** = tracked in SCOPE.md §3, never decided here.

The leaf runtime is the leaf-class node runtime (SCOPE §3.7): a `no_std` Rust firmware image for MCU-class hardware that executes a service graph fixed at build time. DAEMON-SPEC's preamble defers to this document; this is that deferral answered.

---

## 1. What a leaf is, and what it is not

**A leaf is not a smaller daemon.** Both run the same blocks against the same ABI, and that is the whole of what they share operationally. The difference is *when the graph is decided*:

|                | Daemon-class | Leaf-class |
|---|---|---|
| Service graph | read from a file at boot, changeable at runtime | compiled into the image |
| Blocks | pulled from a registry, hot-loaded | AOT-compiled and linked into the image (§6.3) |
| Deploy | `PUT` a file, start it | build firmware, flash it |
| Management API | the whole of DAEMON §9 | none (§7) |
| Filesystem | a data directory (DAEMON §2) | none required |

Every one of those follows from a single physical fact: an MCU has no room to compile WASM and nowhere to put a block registry. **The design consequence that matters is that a leaf has no configuration surface at runtime** — there is no file to edit and no endpoint to call, so everything a daemon reads from `node.toml` and a service file is instead *baked* (§6).

**What is identical, and MUST stay identical:** the ABI a block sees (ABI-SPEC in full), the expression language and its semantics (EXPR-SPEC), the canonical CBOR encoding (ABI §6.3.1), the manifest schema (ABI §11), and the wire vocabulary of a signal. A block author targets one platform, not two. SCOPE §3.7 puts it as "extra flashing steps are acceptable; a different design flow is not"; this section is the runtime half of that sentence.

## 2. Architecture

The leaf runtime is a Rust binary crate — `crates/leaf`, which exists, has a `no_std` boundary drawn through it (§2.1) and is still built and run on the host — that links the ★ crates unchanged:

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

- **Adds:** an engine binding (§3), a `StateStore` against flash (§5), a `Timers` implementation against a hardware timer, a transport client (§8), and a `main` that hands the baked graph (§6.4) to `spawn` — the graph is generated, the `main` is not.
- **MUST NOT add:** a second lifecycle driver, a second property-resolution rule, a second router, a second expression interpreter, a second CBOR encoder, or **a second implementation of `eio:core`'s host functions** (DAEMON §1.1 — a leaf supplies a clock and an entropy source and shares the rest). Every one of those exists in a ★ crate precisely so that two hosts cannot disagree, and reimplementing one for size or speed is the divergence ABI §13 calls a conformance bug by definition.

**No allocator is not an option.** The ★ crates permit `alloc`, and ABI §6.3's batches are dynamically sized. A leaf therefore ships a global allocator over a fixed heap. Which allocator, and the heap's size, are per-target build configuration.

### 2.1 The `no_std` boundary through `crates/leaf`

§1 calls a leaf "a `no_std` Rust firmware image", and the crate is one crate rather than two: the boundary is a cargo feature, `std`, on by default. `cargo build -p eio-leaf --no-default-features` builds the runtime half for a bare-metal target, and `just check-nostd` runs exactly that for both of §2's targets on every gate — the leaf is in `nostd_crates` beside the ★ crates for the same reason they are.

**What the boundary follows is the adds/MUST-NOT list above.** Everything the leaf *shares* with a daemon crosses it; everything the leaf *adds* is per-platform and does not:

|Crosses (`no_std` + `alloc`)|Behind `std`, because it is a platform|
|---|---|
|`spawn` — ABI §5.1 steps 0–3, generic over the engine, clock, entropy source and `StateStore`|the wasm3 binding (§3): `wasm3x` is `std`|
|the `eio:timer` scheduler, generic over its clock|the `eio:state` flat-file stand-in (§5): `std::fs`, and flash is §11's|
|the leaf's budgets (§4) and the router wiring a baked graph needs (§6)|the host clock and entropy source (DAEMON §1.1)|
||the golden-block fixtures, the demo binary and `tests/` (§9 runs on the host build)|

**A leaf's own `main` is not part of this**, and the crate's `[[bin]]` is skipped on a bare-metal target rather than compiled: a firmware image needs a global allocator and a `#[panic_handler]`, and which ones is the per-target build configuration the paragraph above already defers. They arrive with the target, not before it.

What this therefore does **not** claim: no cross-compiled image has been linked, flashed or run. The boundary is a measurement of how much of a leaf is already written — the portable half is — and it is the input to picking a target, not a substitute for it.

## 3. The engine

**WAMR** (WebAssembly Micro Runtime), in AOT mode for deployment (SCOPE §3.2, SDK §5). Its interpreter is also a supported mode and is what a bring-up or a debugging build uses.

This choice is **measured, not assumed** — `crates/conformance/tests/wamr.rs` runs the full ABI §13 scenario suite and the ABI §4.3 instruction checks against WAMR's fast interpreter. What that measurement established, and what this spec therefore inherits:

- **31 of 32 scenarios pass.** The one skip is `07_budget_exhausted`, for the reason in §4.
- **WAMR refuses all nine proposals outside the accepted six**, including tail call, memory64 and threads — the three wasm3 *runs* (ABI §4.3's measured gaps). A leaf on WAMR needs no loader carve-out of its own for those three; the carve-out stays because it is wasm3's.
- **WAMR runs the whole of bulk memory and reference types**, where wasm3 runs part. This widens nothing: ABI §4.3's portable subset is the floor across leaf engines, not a description of one, and a module using `table.copy` runs on WAMR and fails on wasm3. The loader refuses it on both.
- **WAMR's rejections do not name the proposal they objected to.** They are opcode- and section-level parse errors (`unsupported opcode fd`, `invalid limits flags`). ABI §4.3 makes naming a MUST only where the engine reports it; a host cannot invent a name its engine does not give.

**wasm3** is the second measured interpreter (`crates/conformance/tests/wasm3.rs`) and remains a valid leaf engine. It has no AOT path, so a wasm3 leaf is interpreted throughout.

**`crates/leaf` binds both** (`src/wasm3.rs` and `src/wamr.rs`, eieio-x7g.2.5). The WAMR binding is the interpreter, and it costs no `wamrc` and no C++ toolchain: WAMR's core is C, so the mode this section calls "what a bring-up uses" is reachable on a machine where §6.1's AOT compiler is not. It is written against `wamrx-sys`'s raw FFI rather than the `wamrx` safe wrapper, for the reason `crates/conformance/tests/wamr.rs` established first — `wamrx::Linker`'s closures never see the calling instance, which every ABI §7 function touching guest memory needs — and it is the fifth sanctioned `unsafe` site in the repository (CLAUDE.md), with a `// SAFETY:` comment on every block.

### 3.1 The engine is the only place the feature set is enforced (ABI §4.3)

ABI §4.3 splits refusal across two layers, and a leaf implements both. Stated concretely, because "configure the engine to the accepted set" is not an instruction anyone can follow twice the same way:

- **WAMR** selects features at *build* time through its CMake configuration, not at runtime through a config object. A leaf's WAMR build MUST enable `bulk-memory` and `reference-types` and MUST NOT enable SIMD, tail call, multi-memory, memory64, threads, exceptions or GC. The default build already refuses all nine (measured, above), so the requirement is that a leaf does not go *adding* them for a convenience.
- **wasm3** has no feature switches; what it accepts is what it was compiled with. Its acceptance was measured instruction by instruction (ABI §4.3) rather than read off a list, and that measurement is the specification of what a wasm3 leaf accepts.

**Neither engine can express the carve-out**, because a proposal is one switch and the accepted set is part of two of them. `eio_manifest::validate` is the second layer and runs on every host, leaf included — it is a ★ crate for exactly this reason. **A leaf MUST run it**, at firmware build time where a refusal costs a build rather than a field failure.

### 3.2 One engine per image, two in a host build

**A leaf image links exactly one engine.** Which one is a build-time choice — Cargo features `wasm3` and `wamr` on `crates/leaf` — and it is a choice rather than a default because §3.1's feature enforcement, §4's watchdog and §6.1's AOT artifact are all per-engine.

Nothing in this document names an engine at a call site, and the code MUST NOT either: `spawn` takes the `instantiate` function, so an engine is an argument. That is what makes the following possible, and it is a *host build's* shape rather than a leaf's:

**The host build links both, so that §9's engine-driven suites can be run against each in one process.** ABI §13 makes divergence between hosts a conformance bug by definition, and until this crate linked two engines that rule could only ever be checked between the leaf and the daemon — never between two leaves. It now is: `crates/leaf/tests/` runs suite 1, suite 3, the end-to-end graph and the timer scheduler once per engine and asserts the same answers. A firmware build enables one feature and gets one engine.

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

**Those three are *what* is baked; §6.4 is the form they take.** §6.3 settles the question underneath both — a block's compiled artifact is part of the firmware image rather than something a leaf reads out of flash — because that choice decides the shape of the baked graph, the flash layout (§11) and what the pipeline produces.

**The service file is still the source, and stays the portable artifact.** The same file deploys to a daemon; SERVICE-SPEC parses it; the firmware build is one more consumer. It is not parsed *on* the leaf — `eio-service` is a `std` crate and deliberately so (CLAUDE.md: nothing parses a service file on a leaf tier) — it is parsed by the build host, which then emits Rust.

**Property expressions are baked as source text, not as a compiled form.** ABI §11 makes every property an expression evaluated per signal, and EXPR-SPEC's parser is a ★ crate that runs on the leaf. Pre-parsing to an AST at build time is a plausible optimisation and is explicitly **not** specified here: it would put a second representation of an expression into the platform, and the first thing to measure is whether parse cost matters at all when properties are parsed once at configure time (ABI §5.1) rather than per signal.

### 6.1 The AOT artifact

`cargo eio aot --target <leaf>` produces a WAMR AOT artifact per block, and ABI §11.1's manifest carries an `aot` list naming the prebuilt targets published alongside the portable module. §6.2 says which targets those are and §6.2.1 how each is spelled. The portable `wasm32-unknown-unknown` module **MUST always ship** (ABI §11.1): an AOT artifact is an optimisation for one target, never a replacement for the thing every host can run.

**AOT artifacts are version-sensitive, and the pairing is normative.** A WAMR AOT artifact is tied to the WAMR version that compiled it and to the LLVM that WAMR was built against — **WAMR 2.4.5 pins LLVM `release/18.x`**. A leaf image and the artifacts it links MUST come from the same WAMR version. Recording the pair is not bookkeeping: a mismatched artifact would be a load failure in the field, after flashing. **§6.3 makes that pairing a property of a single build** — one `wamrc` produces the artifacts, one toolchain builds the engine they are linked beside, and the mismatch becomes a thing the build host can refuse rather than a check the device would have to carry. *Where* the pair is recorded is still §11’s pipeline item.

This section is **PROPOSED and unimplemented**: `wamrc` has not been built on any developer machine here (six distinct blockers recorded on `eieio-7d8.21`), so the artifact layout is specified from WAMR's documentation rather than from something this repository has produced. **It ratifies when a leaf image links an artifact this pipeline built and runs it** — §6.3 settles that linking, and not runtime loading, is what a leaf does with an artifact — and not before. The interpreter path (§3) needs none of it and is what a first leaf bring-up should use.

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

**That question is now answered, and this spelling was robust to it.** §6.3 settles that a leaf links its AOT artifacts into the image at firmware build time rather than loading them from flash at runtime, so the comparison is the *build host's*: the generator selects the artifact whose `aot` entry equals the triple the image is being built for, by string equality against a value the build already holds, and the image carries no target string and does no lookup. What the name is did not change — only who compares it and when, which is what this section predicted it would be.

#### 6.2.2 Adding a target

Adding one is a checklist, not an argument. A family joins the list when all five hold:

1. A Rust triple `rustup` ships, or a written reason it cannot be one.
2. A WAMR build for it at §3.1's feature set — `WAMR_BUILD_TARGET` plus a platform port — that passes the ABI §4.3 instruction checks as built.
3. A `Timers` against a hardware timer, a `StateStore` against its flash, and a transport client (§8).
4. A board someone can flash, running all three of §9's suites at the leaf's own budgets.
5. An entry in §6.2's table and the triple added to `check-nostd`'s target list, so the `no_std` claim is gated for it too.

### 6.3 A block's code is linked into the image, not loaded from flash

Three sections said this three ways: §1's table said a leaf's blocks are "AOT-compiled and linked in", §6.1 said the section ratifies when a leaf "loads an artifact this pipeline built", and §11's flash-layout item asked where AOT artifacts "sit". Those are not three spellings of one design. They are two designs — artifacts *in* the firmware image, versus artifacts in a flash region the runtime reads at boot — and the difference decides the shape of the baked graph (§6.4), the flash layout (§11) and what the pipeline produces.

**The answer is §1's.** Every block's compiled artifact is part of the firmware image, reached from the baked graph as a `&'static [u8]`, and **a leaf never reads a block's code out of a flash region it did not link**. §6.1 and §11 are amended to agree.

Why, in the order the reasons bind:

- **There is no channel that could deliver a block on its own.** §7 removes the management API by design rather than by omission — it "cannot be authenticated safely enough to be worth it" and "would have nothing to serve". §8's bus is the only other thing a leaf listens to, and it is at-most-once, never-retained and authenticated by a per-*bus* pre-shared key (SCOPE §3.11); pushing executable code down it would make every holder of the bus key a code-execution path on every device in the System, which is the surface §7 declined, reinvented. So the only way bytes reach a leaf is a flash operation, and once that is true a separate artifact region buys a smaller *write*, never a new capability. §1's deploy row — "build firmware, flash it" — was already the whole of it.

- **A separate region is a configuration surface, which is the one thing a leaf does not have.** §1's stated design consequence is that "a leaf has no configuration surface at runtime". An artifact region writable independently of the image is exactly that: state the device holds that the image did not decide. It also manufactures a mismatch class this tier cannot check — image built against one WAMR, region written by another — which is the failure §6.1 names as the one that matters, "a load failure in the field, after flashing". Linked in, that pair is a property of a single build and the check moves to the build host.

- **The graph and the code must not be able to disagree, and only one design makes that structural.** §6.4's instances carry each block's port names *in index order*, and that order is the manifest's (ABI §5.2). They are computed on the build host from the manifest that accompanied the artifact. If the code arrived separately, the baked port numbering and the loaded module could disagree with nothing on the device able to notice — a silent misdelivery rather than a load error. Linked in, the description and the bytes it describes are one artifact by construction.

- **The manifest may not survive the AOT compile, so the cross-check is a build-host act either way.** §3.1 already requires `eio_manifest::validate` at firmware build time "where a refusal costs a build rather than a field failure", and that check reads the module's import section and its `eio:manifest` custom section (ABI §4.3, §11) — neither of which a `.aot` is guaranteed to carry. **That is read from WAMR's documentation and has not been measured here**, for the reason §6.1 records. It cannot make the case *for* loading in any event: a device that cannot re-derive a manifest from the artifact it loaded has no way to check that it is the artifact its graph describes.

- **It costs less code on the tier least able to pay.** Loading needs a region reader, an index format and a parser for it, an integrity check and the version check above — all in the image, all `no_std`. Linking needs none of them: `include_bytes!` puts the artifact in `.rodata`, which on the v1 target (§6.2) is memory-mapped flash, so the `&'static [u8]` the baked graph carries is already a pointer into flash and no RAM is spent holding it. Whether the engine then copies the text into executable RAM is the engine's question and is identical under both designs, so it is not a discriminator between them.

**What is given up, stated plainly:** a leaf cannot be updated one block at a time, and a one-line property change reflashes the whole image. That is not a cost this section introduces — §1's deploy row and SCOPE §3.7's "extra flashing steps are acceptable" already said it — but it is the cost, and the argument that would reverse it is the argument for giving a leaf a management surface. It would be taken in §7, not here.

**This is engine-independent, and that is what makes §6.4 buildable now.** A bring-up leaf (§3) links the portable `wasm32-unknown-unknown` module through the same include; an AOT leaf links a `.aot` for §6.2's triple. Nothing else in the baked graph differs between the two, so the generator and the representation it emits can be written and tested against the interpreter today, with no `wamrc` — which is otherwise what §6.1's PROPOSED status blocks. **This section does not ratify §6.1**: no pipeline has produced an artifact, and none has been linked.

**What it settles for §11's flash-layout item.** A leaf's flash holds exactly two regions with distinct lifecycles: the **image** — runtime, baked graph and every block artifact, replaced wholesale by a flash — and the **state region** (§5's `StateStore`), which is not part of the image and which the image must be able to find. What is left to settle is where that region sits, how the image finds it, and what an update does to what is in it. An artifact partition, an on-device artifact index and a cross-region version pairing are no longer among the questions.

**And for §6.2.1's open note:** the artifact-name comparison is the *build host's*, always. The generator selects, for each instance's block, the artifact whose `aot` entry equals the Rust triple the image is being built for — string equality against a value the build already holds — and the image carries no target string and performs no lookup. §6.2.1 predicted its spelling was robust to this decision, and it is: the name is unchanged, and ABI §11.1's `aot` list stays what it was, a build-host input describing what a registry can supply rather than a runtime key.

### 6.4 The baked graph

§11 asked what the baked graph looks like as Rust, "and whether it is generated source or a const table". The two are not alternatives: **it is generated Rust source containing one `static` of hand-written types.** The types live in `crates/leaf` and are versioned like any other code; the generated file declares data of them, and nothing else.

**The generated file contains no `fn` and no control flow.** That is the rule that keeps §2's MUST-NOT list true — generated logic is where a second lifecycle driver, a second router and a second property-resolution rule are born, one convenience at a time. **A leaf's `main` is not generated.** It is hand-written per target, and what it does with the graph is hand it to `spawn`. §11's item is called "the generated `main`", and that name is the thing this section corrects.

#### 6.4.1 The one rule a generator obeys

**Everything in the baked graph that could have been computed is `host-core`'s own output, serialised.** A generator does not read a manifest, does not number ports, and does not apply ABI §11.1's required/default rule. It calls `eio_manifest::validate`, `Descriptor::from_manifest` and `eio_host_core::resolve` on the build host — the same functions on the same crates the daemon calls — and prints what they returned. Anything it computed for itself would be a second implementation of a ★ crate's job, running at a different time on a different machine with nothing comparing the two: §2's MUST-NOT list evaded by being early rather than by being different.

The converse is equally load-bearing. **What is cheap to derive on the device stays underived**, and two things are:

- **Connections are baked as names and resolved on the device** by `Routes::resolve`, exactly as `crates/leaf`'s own demo does today. Precomputing `Endpoint` pairs would put the router's numbering into generated code. What resolution refuses — an unknown id or port, `err` as a destination, a duplicate edge — is refused on the build host too, because the service file was validated there (SERVICE §7, §10); a refusal on the device therefore means the generator is wrong, and a leaf treats it as fatal at boot rather than running a partial table.
- **Property expressions are compiled on the device**, at configure time, by `PropContext::compile_with_limits` under §4's budgets. §6 already settles that they are baked as source text and says why pre-parsing is not specified.

#### 6.4.2 The shape

```rust
pub struct BakedGraph {
    pub node: BakedNode,
    pub instances: &'static [BakedInstance],
    pub connections: &'static [BakedConnection],
    pub overflow: Overflow,                        // SERVICE §5: one policy per service
    pub transport: Option<BakedTransport>,         // None = no bridge (DAEMON §7.1)
}

pub struct BakedNode {
    pub id: &'static str,                          // DAEMON §2.1's node id — see §6.4.3
    pub name: Option<&'static str>,                // a label; nothing resolves by it
    pub service: &'static str,                     // the service's name; §5's key component
    pub limits: Limits,                            // ABI §9.7, per instance
}

pub struct BakedInstance {
    pub id: &'static str,                          // SERVICE §2: the id, never the name
    pub block: &'static str,                       // the registry reference, for diagnostics
    pub module: &'static [u8],                     // the artifact itself (§6.3)
    pub inputs: &'static [&'static str],           // manifest order is the port index
    pub outputs: &'static [&'static str],          // (ABI §5.2)
    pub props: &'static [PropertySource<'static>], // manifest order is the prop_id
    pub capabilities: &'static [Capability],
}

pub struct BakedConnection {
    pub from: (&'static str, &'static str),        // (instance id, port name)
    pub to: (&'static str, &'static str),
}

pub struct BakedTransport {
    pub bus: &'static str,                         // DAEMON §7.1's pubsub.toml, baked
    pub candidates: &'static [&'static str],
    pub pinned: Option<&'static str>,
    pub key: Option<&'static [u8]>,                // SCOPE §3.11's bus pre-shared key
}
```

and the whole of a generated file is the module byte arrays plus

```rust
pub static GRAPH: BakedGraph = BakedGraph { /* … */ };
```

The notes below are normative, not commentary:

- **`Limits`, `Overflow`, `PropertySource` and `Capability` are `host-core`'s and `eio-manifest`'s own types**, used directly rather than mirrored. `PropertySource` in particular has `const` constructors and `&'a str` fields, so a resolved property list is expressible as a `static` with no conversion at all — which is why §6.4.1's "serialise what `resolve` returned" is a shape a generator can actually emit. `Descriptor` and `Connection` own their strings and so cannot be `static`; `BakedInstance` and `BakedConnection` are their borrowed mirrors, and a leaf builds the owned forms once at boot.
- **Instance order is the numbering.** `Routes::resolve` indexes descriptors positionally, so a `BakedInstance`'s position *is* its `Endpoint::instance`. The order is therefore part of the artifact and not an implementation detail: it is ascending instance-id order, which is what `eio-service` already yields (its `blocks` is a `BTreeMap`), so rebuilding the same file numbers the same instances the same way.
- **One artifact, one `static`.** Instances of the same block share one byte array, and a generator emits each distinct artifact exactly once. Three instances of `filter` are three `BakedInstance`s pointing at one module, not three copies of it in flash.
- **`include_bytes!` alone is not enough.** It yields an align-1 array, and both engines read multi-byte fields directly out of the buffer they are handed; §6.2's target gives no unaligned-access guarantee. `crates/leaf` therefore provides the macro that wraps the include in an over-aligned type, and a generator MUST emit that rather than a bare `include_bytes!`. **The alignment requirement is read from the engines' documentation and has not been measured on hardware** — §6.1's caveat, in the one place where being wrong about it is a fault at boot rather than a build error.
- **The generated file is a build artifact and MUST NOT be checked in.** It is derived from the service file, the manifests and the artifacts, and a checked-in copy is a second source of truth for a graph whose whole point is that the service file is the source (§6). It is written into the build directory and `include!`d, which is why every path inside it — the module includes above — MUST be absolute.

#### 6.4.3 Identity, limits, and the rest of what `node.toml` carried

DAEMON §2.1 mints a node `id` on first boot and writes it into `node.toml`, reasoning that "an id that changed per boot would identify nothing". A leaf has no first boot that can write anything. **The node id is therefore a required input to the firmware build, and a generator MUST NOT mint one.** A build that minted would hand a device a new identity every time it was reflashed — DAEMON §2.1's failure one level up, with a Designer registry entry, a state namespace and a bus identity all quietly ceasing to refer to the same thing. Where the id is *kept* between builds is the pipeline's question (§11), not this section's.

The rest of `node.toml` divides cleanly, and the divisions are why this section carries four fields rather than a file:

|`node.toml`|On a leaf|
|---|---|
|`id`, `name`|Baked, above. `id` is a required build input|
|`[limits]`|Baked. ABI §9.7 says a block "may assume nothing about their size", so a leaf states both; *what* they should be is §11's memory-budget item|
|`[budgets.expr]`|**Not a build input.** §4 fixes a leaf's evaluation budgets at EXPR §9's floors and its decode bound at the daemon's, and a service file is the wrong place to weaken either|
|`[budgets]` fuel, `deadline_ms`|Not a build input either: a leaf's budget is a watchdog rather than fuel (§4), and its granularity is §11's watchdog item — a property of the runtime and its target, not of the graph|
|`[api] listen`|Nothing to configure; there is no API (§7)|
|`[blocks]` pull policy|No registry to pull from at runtime (§1). Digest and signature verification happen on the build host (§10)|
|`[executor] mailbox`|A host's own queueing. `host-core`'s router core stops at the table (DAEMON §6), and what a leaf does with a full one is §11's|

`autostart` is ignored: a leaf has one service and nothing else it could be doing. `[ui]` never reaches the image — SERVICE §6 makes it annotations no host interprets, and a generator drops it rather than carrying it.

#### 6.4.4 What a generator has to prove

A generator is correct when, for every service file it accepts:

1. every `BakedInstance`'s `inputs`, `outputs` and `props` equal the fields `Descriptor::from_manifest` and `eio_host_core::resolve` produce from that instance's manifest and its service-supplied properties; and
2. `Routes::resolve` over the emitted instances and connections succeeds, and yields the table a daemon resolves from the same file.

Both are testable on the host build against `examples/services/`, with no target, no board and no `wamrc` (§6.3), and together they are what makes §6.4.1's "serialise, do not compute" rule checkable rather than merely stated.

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

Three suites, all of which already exist. **Suites 1 and 3 are per-engine and MUST be run against every engine the image may link** (§3.2); suite 2 is not — it drives no engine at all, so running it "on WAMR" and "on wasm3" would be the same run twice.

1. **The ABI §13 scenario suite**, driven through `host-core`'s `Engine` trait exactly as the daemon and the reference harness drive it. A capability a leaf does not implement is reported **skipped by name**, never passed over.

   **A leaf's skips are the leaf's, not its engine's**, and the count makes that visible: `crates/leaf` reaches **28 of 32 scenarios on each of its two engines**, with the same four skipped by the same four names — the budget scenario (§4: neither interpreter has a usable counter, so the watchdog is what closes it) and `eio:gpio`/`eio:i2c`/`eio:http`, which this crate has no host functions for. `crates/conformance/tests/wamr.rs` reaches 31 on the *same engine* because the reference harness supplies its own capability stand-ins; the difference between 28 and 31 is three namespaces, not three engine behaviours.
2. **`expr-tests/`** — the expression language, property types, and canonical CBOR. Run **at the leaf's own budget settings** (§4), not at the reference defaults: a budget floor that only holds on a generous host is not a floor.
3. **The ABI §4.3 instruction checks**, against the engine as the leaf actually builds it (§3.1). One table, shared — `crates/conformance/tests/support/wasm3_instructions.rs`, which the conformance wasm3 test and the leaf's own test both read, the latter running it through each of its bindings. A second copy of the accepted set would be the divergence this whole section exists to prevent, written down twice.

   **The two halves of that table are not the same kind of claim, and only one of them is universal.** The portable subset is the floor ABI §4.3 requires of *any* leaf engine, and both of `crates/leaf`'s clear it. The carved-out remainder is not: §3 already records that wasm3 refuses it and WAMR runs it whole, because a proposal is one feature switch and the accepted set is part of two of them, so no engine can be configured to hold the carve-out. **That is not a divergence to fix, because the carve-out is not the engine's.** It is `eio_manifest::validate`'s — a ★ crate every host shares, which a leaf runs before any engine is asked to compile a module (§3.1) — so a block using `table.copy` is refused identically on both, and `crates/manifest/tests/portable.rs` is where that is checked once. What the leaf's own suite 3 adds for the refusal half is therefore one measured fact per engine: the engine this crate builds behaves like the one the reference suite measured, and a case that stopped holding is notice that a build changed underneath it.

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
- ~~**The generated `main`**~~ — **resolved** (eieio-x7g.2.3): §6.4 gives the baked graph as one `static` of hand-written types, emitted as generated Rust source with no `fn` and no control flow in it, and §6.4.1 fixes the rule that keeps it from becoming a second router or a second property-resolution rule — a generator serialises what `host-core` computed on the build host and computes nothing itself. The `main` is not generated at all: it is hand-written per target and hands `GRAPH` to `spawn`. §6.3 settles the ambiguity underneath it — a block's artifact is linked into the image, never loaded from a flash region.
- **Memory budget**: heap sizing per target, and what a leaf does when a batch will not fit.
- **The transport client** (§8), once one has been measured.
- **Watchdog mechanics** (§4): which timer, what granularity, and how a killed instance is reported when there is no log stream to report it on. **A host bring-up cannot stand in for this one**, measured rather than assumed, and now on *both* of §3.2's engines: `wasm3x` 0.1.0 exposes no interruption, abort or termination entry point at all, so nothing outside a running guest call can end it, and WAMR's `wasm_runtime_set_instruction_count_limit` is compiled out behind `WASM_ENABLE_INSTRUCTION_METERING` with no `wamrx-sys` toggle to set it (confirmed by a linker error). Both bindings therefore answer `enforces_budgets = false` and have ABI §13's budget scenario skipped by name — which is §4's honest-binding rule working as intended, not a gap in the leaf, and it is the one skip that a second engine did *not* close. The watchdog becomes implementable at the same moment the target does: a hardware timer and an engine that can be told to stop.
- **Observability without an API** (§7): what a leaf publishes about itself, and on which topic.
- ~~**A class-aware CLI**~~ — **resolved** (eieio-x7g.5): it learns the class. A node entry in `nodes.toml` carries an optional `class`, `"daemon"` or `"leaf"`, absent meaning `"daemon"`, and `eio` refuses a leaf by naming the class rather than reporting a failed request. SCOPE §3.7 records the decision and why the two alternatives — requiring the key, or inferring the class from a refused connection — are each worse.
- **Flash layout**: where the state region sits, how the image finds it, and how a firmware update treats what is in it — including state left under a `(service, instance)` key that the new image's graph has no instance for (§5, §6.4.2: a build may change the instance set). **AOT artifacts and configuration are no longer part of this question** (§6.3): both are in the image, which a flash replaces wholesale, so a leaf's flash has two regions with distinct lifecycles rather than four.

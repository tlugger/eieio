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

- **Adds:** an engine binding (§3), a `StateStore` against flash (§5), a `Timers` implementation against a hardware timer, a transport client (§8), and a `main` that hands the baked graph (§6.4) to `spawn_graph` — the graph is generated, the `main` is not, and `spawn_graph` is `crates/leaf`'s own hand-written loop over `spawn` and `Routes::resolve` rather than anything a generator writes.
- **MUST NOT add:** a second lifecycle driver, a second property-resolution rule, a second router, a second expression interpreter, a second CBOR encoder, or **a second implementation of `eio:core`'s host functions** (DAEMON §1.1 — a leaf supplies a clock and an entropy source and shares the rest). Every one of those exists in a ★ crate precisely so that two hosts cannot disagree, and reimplementing one for size or speed is the divergence ABI §13 calls a conformance bug by definition.

**No allocator is not an option.** The ★ crates permit `alloc`, and ABI §6.3's batches are dynamically sized. A leaf therefore ships a global allocator over a fixed heap. **§4.2 says which allocator and how the heap is sized**; it is `embedded-alloc`'s TLSF heap, shared with the engine rather than beside it, given the linker's remainder against a per-target floor.

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

- **32 of 33 scenarios pass.** The one skip is `07_budget_exhausted`, for the reason in §4.
- **WAMR refuses all nine proposals outside the accepted six**, including tail call, memory64 and threads — the three wasm3 *runs* (ABI §4.3's measured gaps). A leaf on WAMR needs no loader carve-out of its own for those three; the carve-out stays because it is wasm3's.
- **WAMR runs the whole of bulk memory and reference types**, where wasm3 runs part. This widens nothing: ABI §4.3's portable subset is the floor across leaf engines, not a description of one, and a module using `table.copy` runs on WAMR and fails on wasm3. The loader refuses it on both.
- **WAMR's rejections do not name the proposal they objected to.** They are opcode- and section-level parse errors (`unsupported opcode fd`, `invalid limits flags`). ABI §4.3 makes naming a MUST only where the engine reports it; a host cannot invent a name its engine does not give.

**wasm3** is the second measured interpreter (`crates/conformance/tests/wasm3.rs`) and remains a valid leaf engine. It has no AOT path, so a wasm3 leaf is interpreted throughout.

**`crates/leaf` binds both** (`src/wasm3.rs` and `src/wamr.rs`, eieio-x7g.2.5). The WAMR binding is the interpreter, and it costs no `wamrc` and no C++ toolchain: WAMR's core is C, so the mode this section calls "what a bring-up uses" is reachable on a machine where §6.1's AOT compiler is not. It is written against `wamrx-sys`'s raw FFI rather than the `wamrx` safe wrapper, for the reason `crates/conformance/tests/wamr.rs` established first — `wamrx::Linker`'s closures never see the calling instance, which every ABI §7 function touching guest memory needs.

**There is exactly one WAMR binding, and it is `crates/wamr-host`** (eieio-7d8.34). It was written twice — the harness proved the shape, `crates/leaf/src/wamr.rs` copied it — and ~640 of the two files' ~880 non-blank, non-comment lines were identical, including the whole of `impl Engine for Guest`. That block is not FFI plumbing: it carries ABI §8's `TrapKind::Trap`-versus-`TrapKind::Engine` classification, and no ABI §13 scenario pins `dead: "trap"`, so two copies of it could disagree with nothing in either suite noticing. The copy had already cost one real defect, the 8 MiB execution stack §4.2 records below. `crates/wamr-host` depends on `eio-host-core`, `eio-manifest` and `wamrx-sys` and on **neither of its two callers**: `crates/leaf` takes it behind its `wamr` feature, `crates/conformance` takes it as a *dev*-dependency, and no edge runs between those two — which is what keeps the reference harness's fourth-engine measurement from becoming a test of the leaf's own code. That harness keeps every *measurement* it had: ABI §4.3's instruction table, the carved-out remainder and all nine refused proposals still drive raw `wamrx-sys` inside `tests/wamr.rs`; what is shared is the instrument. The shared crate is the fourth sanctioned `unsafe` site in the repository and the harness's remaining fixtures the fifth (CLAUDE.md), each with a `// SAFETY:` comment on every block; `crates/leaf/src/wamr.rs` now has none.

The shared binding takes WAMR's process-global runtime lock **per operation**, never for a guest's lifetime, and that is a requirement rather than a detail: a leaf runs a graph, so a lifetime-held guard would deadlock the second instantiation against the first live instance. It is sound because re-entering it is unconstructible — a host function is handed an `eio_host_core::Memory`, which carries no way back into the engine (ABI §1.2). The engine execution stack is *not* a constant of that crate but an argument to it, because §4.2's 8 KiB reserve is the leaf's budget and the harness's deliberately generous desktop number is not; a shared constant would be one of those two imposed on the other, which is the defect this merge exists to prevent repeating.

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

So a leaf's budget is a **watchdog**, not fuel: a hardware timer armed before entering a guest callback and disarmed on return, whose expiry kills the instance exactly as ABI §8 requires of a deadline violation. This is the leaf runtime's to add rather than the interpreter's to provide, which is why ABI §13.1 gives the harness a skip class for a host that enforces no budget: a binding without a watchdog answers `enforces_budgets = false` and has `07_budget_exhausted` skipped by name, honest about it rather than hanging. **§4.4 fixes which timer and how long, §4.5 what an engine binding must expose for any of it to work, and §4.6 how a kill is reported on a node with no log stream.**

**A leaf's own budgets sit near EXPR §9's floors** (`MAX_FUEL` 10 000, `MAX_DEPTH` 32, `MAX_RANGE` 1 000, `MAX_VALUE_BYTES` 4 096, `MAX_EXPR_BYTES` 1 024), which is what §9 already tells leaf hosts to do. They are *floors*, so a conforming expression may rely on that much and a leaf MUST NOT go below them.

**That the floors are adequate is now measured, not assumed** (eieio-x7g.6). The whole of `expr-tests/` runs at them: 484 language vectors, 29 property vectors through the leaf's own `compile_with_limits` call, and 46 canonical-CBOR vectors — 559 in all, none failing. Before this, every vector in the repository ran at the reference defaults, so "a budget floor that only holds on a generous host is not a floor" (§9) was a rule nothing had tested. A vector that failed here would have meant one of two things, and they need different fixes: the vector relies on more budget than §9 says a conforming expression may rely on, or the floors are set too low. Neither happened. Whether `max_payload` and `max_batch` should have *floors* remains **OPEN** in SCOPE §3 (ABI §9.7); what a leaf supplies for them is not that question, and §4.2 supplies it.

### 4.1 Decode depth is coupled to `MAX_DEPTH`

A CBOR decoder needs a nesting bound or a hostile batch is a stack overflow, and a stack overflow on an MCU is not a caught error. **That bound MUST be at least the configured `MAX_DEPTH`.** Setting it lower makes a value the expression language is required to handle undecodable, which turns an EXPR §9 budget into a decode failure with a different error code, on one host only — the shape of divergence ABI §13 exists to prevent.

**The MUST is a floor, and a leaf should not sit on it** (eieio-x7g.7). A leaf's *evaluation* budgets belong at EXPR §9's floors, but its decode bound is not the same kind of number: it decides which values a leaf can receive at all, so lowering it toward `MAX_DEPTH` makes a batch that a daemon routes without complaint undecodable on a leaf — divergence again, in the other direction, bought for stack headroom. **A leaf therefore matches the daemon's decode bound rather than its own floors**, and both pass `eio-signal`'s `MAX_DEPTH` today. That may be the wrong trade on a real target, where 128 levels of recursive decode is exactly the overflow this section exists to bound; but it is a decision about interoperability as well as safety, and it needs a measured stack, so it *was* §11's memory-budget item and not a knob to turn early.

**Now measured, and the bound stays at 128** (eieio-x7g.2.7). The stack this paragraph was waiting for was read off the v1 target's own object code rather than estimated. `eio-signal` built for `riscv32imc-unknown-none-elf` at the workspace release profile gives `Value::decode_at` — the directly self-recursive function, one frame per level of nesting — a **160-byte** frame (`addi sp, sp, -0xa0`), with `Batch::decode_from` at 208 bytes and `Signal::decode_from` at 80 bytes once each above it. The price of the bound is therefore linear and known:

|Decode bound|Worst-case decode stack|Of the v1 target's 313 KiB of SRAM (§4.2)|
|---|---|---|
|`MIN_DEPTH`, 32|≈ 5 KiB|1.6 %|
|`MAX_DEPTH`, 128|≈ 20 KiB|6.5 %|

**Host parity costs 15 KiB of stack, and 15 KiB is worth paying.** EXPR §9 names "the 16 KiB-of-stack tier" as the one that dies first, and the honest reading of the measurement is that the v1 target is *not* that tier: a part with 313 KiB of SRAM can afford a 32 KiB stack outright, and buying interoperability with 5 % of RAM is a better trade than buying 5 % of RAM with a value a daemon routes and a leaf cannot read.

So the bound is a **constant, not a per-target one**: `eio_signal::MAX_DEPTH`, the same number the daemon passes, on every leaf target. What varies per target is the *stack reserved for it*. That is the point of the resolution — it moves the variation to the side of the trade where it costs a linker number instead of an interoperability guarantee, and it keeps `leaf_budgets()` free of a target `cfg`.

**The stack rule, stated as the MUST that makes the bound safe:** a leaf MUST reserve at least **`decode_bound × 192` bytes** of native stack for the context that runs the graph. 192 is the measured 160 plus the `Batch`/`Signal`/minicbor frames above it and margin for the engine's own host-call frames beneath. At 128 that is 24 KiB, which §4.2 rounds up to a 32 KiB reserve.

**Evaluation is not the term that sizes the stack, and that was measured too.** On the same target `Evaluator::eval`, `eval_operand` and `apply` are 112, 144 and 80 bytes, and one level of evaluation is a chain through all three — but a leaf's `EvalLimits::FLOORS` bounds that recursion at 32, so evaluation costs ≈ 10.5 KiB. `Parser::parse_expr` is 224 bytes, bounded at 32 by `ParseLimits::FLOORS`, so parsing costs ≈ 7 KiB, and it happens once at configure time (ABI §7.1) rather than per signal. Neither nests inside a decode. Decode at 128 is the deepest walk in the system and it is what the reserve is sized against.

**What §11's bring-up must report back, and what would reopen this.** The 160-byte frame is a *static* measurement of one crate compiled alone: link-time optimisation may inline `decode_at`'s callees into it and grow the frame, and the recursive `Drop` of a 128-deep `Value` — the walk EXPR §9 names explicitly, and the only one with no budget check in it, because dropping cannot fail — has not been measured at all. eieio-x7g.2.11 paints the stack at boot and reports the high-water mark after decoding the deepest vector in `expr-tests/cbor/`. **If the measured high-water mark exceeds half the reserve, grow the reserve**; this section reopens only if the grown reserve will not fit, and reopening means an explicitly per-target bound with the interoperability cost stated in the same breath — never a quiet 32.

### 4.2 The memory budget

`no_std`, one heap, and a size the linker computes against a floor this section fixes.

**The v1 target's real number is 313 KiB, not 400 KiB.** The ESP32-C3 datasheet gives 400 KB of on-chip SRAM, of which 16 KB is configured as cache, and 384 KB of ROM that is not usable memory at all. What a bare-metal image actually gets is smaller again: `esp-hal`'s ESP32-C3 linker script gives `DRAM` `ORIGIN = 0x3FC80000, LENGTH = 313K`, with a further ~66 KiB in `dram2_seg` that is only free once the second-stage bootloader has finished. **313 KiB is the number this section budgets against**, because it is the one that holds for the whole life of the image. The instruction window over the same SRAM (`IRAM`) is an alias, so anything a leaf places in RAM to execute comes out of the same 313 KiB; a leaf executes from flash through the cache and places nothing there by choice.

**The allocator is `embedded-alloc`'s TLSF heap.** Three properties decide it, and only the third is about this platform:

- **O(1) worst-case allocation and free.** §4.4's watchdog bounds a callback in wall-clock time, and a first-fit free list has an unbounded search on a fragmented heap — the variance is what breaks a deadline, not the mean. An allocator whose worst case is a constant is the one that makes a wall-clock budget arguable.
- **Bounded fragmentation**, which is a published property of TLSF rather than a hope. A leaf runs for months without a reboot and the ★ crates allocate a `Batch` per signal, so the steady-state allocation pattern is exactly the churn that defeats a naive free list.
- **It reports its free and used bytes**, which §4.3's reservation rule requires. An allocator that cannot answer "how much is left" makes the difference between a refusal and a reset unimplementable.

**Not `esp-alloc`, and the reason is not a criticism of it.** §6.2 buys a vendor for the *peripherals* — a flash driver, a timer, a network stack — because those are the target. Nothing about a heap is. Keeping the allocator vendor-neutral means the same allocator can be run on the host under the same workload, which is how the numbers below get checked without a board. `critical-section` is a dependency either way and arrives with the target.

**There is exactly one heap, and the engine allocates from it.** WAMR is configured with the global allocator rather than its own pool. Two pools means two ways to run out and a fixed split that is wrong for every graph, and the split would have to be chosen by a build that cannot know which of the two a given service leans on.

**The heap is the linker's remainder, checked against a floor.** A leaf does not carve a `static mut HEAP: [u8; N]`: it gives the allocator everything between the end of `.bss` and the top of `DRAM`, and the build **fails** if that remainder is below the floor. Sizing it as a constant would mean choosing between wasting RAM on a small graph and failing to link a large one; sizing it as the remainder means the graph that does not fit is refused at build time, which is §10's posture already ("validation happens before the build, not during it").

**The floor for the v1 target is 192 KiB**, and it is derived rather than picked:

|Reserve|Size|Why|
|---|---|---|
|Guest linear memory|1 × 128 KiB|**Two** 64 KiB pages, and one instance rather than two — both of those are corrections, and the paragraph below the table is the measurement that forced them. This is the dominant per-instance cost by an order of magnitude.|
|Engine execution stack|1 × 8 KiB|The stack `wasm_runtime_create_exec_env` allocates **and zeroes** per instance, held for that instance's whole life. **Measured** — see the paragraph below the table.|
|Signal working set|48 KiB|One decoded batch in flight, the bounded emission queue, and one mailbox slot per connection (DAEMON §6.2). **Shared, not per-instance**: a leaf runs one callback at a time, so only the running instance's batch is live.|
|Subtotal|184 KiB||
|**Floor**|**192 KiB**|Rounded up, and the 8 KiB is the only picked number in the table. It is kept because the floor is what a build *fails* against, and a floor sitting exactly on a measurement fails on the first block that is a hair larger than a golden one.|

**The engine execution stack is 8 KiB because it was measured there, not because a wrapper said so.** This row was the one number in the table with nothing behind it, and the binding underneath it proved the point: `crates/leaf/src/wamr.rs` asked WAMR for **8 MiB** per instance — `wamrx::InstanceConfig`'s desktop default, copied verbatim from `crates/conformance/tests/wamr.rs`, which is 42× this whole heap floor, per instance, and `wasm_runtime_create_exec_env` `memset`s all of it. It is the same defect as the 17-page shadow stack below, one layer down: a host default inherited into the code that exists to fit an MCU. Harmless while `crates/leaf` is a host build, fatal on the first cross-compile. It is also why §3's shared binding takes this number as an argument rather than owning a constant: the same code now serves both the leaf and the desktop harness, and this row is the leaf's budget line, not the harness's.

What it costs is now bisected on every `just ci`, by `crates/leaf/tests/exec_stack.rs`, over every ABI §13 scenario WAMR's interpreter reaches — all five golden blocks, the four hostile blocks, and the hand-written fixtures — by shrinking the stack until a scenario stops passing:

|Block|Deepest scenario|Bytes|
|---|---|---|
|`transform`|`03_property_failure` (`eio_configure`, property evaluation, failure path)|**3 252**|
|`counter`|`11_state_throttled` / `12_capability_denied`|3 000|
|`filter`|`16_filter_routing`|2 304|
|`emitter`|`13_timer_emitter`|2 292|
|`gpio-echo`|`14_gpio_echo`|2 292|
|hostile blocks and `.wat` fixtures|`31_i2c_write_read_and_read` is the largest|≤ 340|

**So 8 KiB held, at 2.5× the worst golden block.** The number stays 8 KiB rather than dropping to the measured 3 252 bytes, and the margin is the reason: the golden blocks are small by construction, a field block need not be, and WAMR's own `DEFAULT_WASM_STACK_SIZE` (`core/config.h`) is 12–16 KiB for a general-purpose host — so 8 KiB is already below what upstream picks, and the margin is what makes that defensible rather than merely small. What the bisection measures is bytes and not frames: WAMR's `wasm_exec_env_alloc_wasm_frame` bumps a pointer by a per-function frame size and refuses when twice that size will not fit, so "the depth a block needs" only has one answer in bytes.

Two things this does **not** settle, both §11's. It is the *interpreter*'s frame layout, and §3 deploys **AOT**, whose frames are the compiler's; and it is a 64-bit host, where WAMR's frame headers carry 64-bit pointers. Both point the same way — a 32-bit target's frames are smaller — but "probably smaller" is not a measurement, which is why the bring-up still has this on its list.

Outside the heap and additional to it: §4.1's **32 KiB native stack reserve**, and whatever `.data`/`.bss` the image and WAMR's runtime globals need, which is measured from the linked image rather than chosen. 192 + 32 = 224 KiB of 313 KiB, leaving ≈ 89 KiB for statics and the loaded modules.

**So a v1 leaf image sizes for one block instance, and that is the headline rather than a footnote.** It said two until eieio-x7g.2.27 measured the row above; what changed is not the arithmetic but the number going into it. Two instances at the measured footprint want 2 × 128 + 2 × 8 + 48 = **320 KiB**, which is more than the 313 KiB of DRAM this section budgets against — so two is not a matter of loosening the floor, and no build can compute its way to it. The levers, none of them free and none of them decided here: the ~66 KiB of `dram2_seg` this section deliberately excludes; a part with more SRAM; or a guest heap smaller than `dlmalloc`'s, which is a block-side question the SDK has never been asked.

**One page is what a golden block *declares*, and for most of this platform's history it was not what one *needed*.** The two questions are different, and this row has had the wrong answer to the second one twice, in both directions. `wasm-ld` puts a block's statics and its shadow stack in the declared minimum and nothing else, leaving ≈ 38 KiB of that page unused; `dlmalloc` at its default 64 KiB granularity **declined** that remainder — its wasm backend donates the span between `__heap_base` and `__heap_end` only when the span is at least one granule, and 38 KiB is not 64 KiB — and took every byte it handed out from `memory.grow` instead. So the *first* `eio_alloc` a block ever served left the page it declared, and this row read **two**. At a one-page bound `counter` failed `eio_configure` with `ERR_LIMIT` before a single signal was routed.

**SDK §4.1 closed that, and it is the reason the row reads one again.** `dlmalloc` configured with a 4 096-byte granularity takes the remainder it was rejecting only for being smaller than a granule. Nothing about a block's behaviour changed — it grows exactly when it needs to, and this reserve still bounds that, at both ends and by both mechanisms below — and nothing about a leaf's budget went into ABI §11.1's portable module: what the SDK changed is *where a block's first allocations come from*, which is address space its own memory section already declared. SDK §4.1 has the sweep, and the three alternatives that were measured against it.

`crates/leaf/tests/memory_growth.rs` is where the one page comes from, and it re-takes the measurement on every `just ci` rather than quoting it: it runs §9's suite 1 on WAMR's interpreter at every page bound from one upward and prints the smallest each scenario holds at. **Every scenario in the suite needs exactly one** — SDK-built golden blocks and hand-written `.wat` fixtures alike, where before the SDK change the golden blocks needed two and the fixtures one. It asserts both directions — nothing over the reserve, and something *at* it — so the number cannot quietly become slack, which is the property that caught the second page in the first place.

**Unlike the execution-stack row, this one is not a property of the host it was measured on.** Linear memory is the guest's own address space, so a 32-bit target sees the same pages; the frame-layout and pointer-width caveats two paragraphs down do not apply to it. It is one of the few rows here the MCU bring-up does not have to re-take.

**The measurement that should worry an implementer most: every golden block declared 17 pages.** Built as `examples/blocks/` built them before SDK §5.2 carried a link default, all five of ABI §13.2's golden blocks declared a minimum linear memory of **17 pages, 1088 KiB** — three and a half times the whole chip. The cause was not the blocks: `wasm-ld` defaults the shadow stack to 1 MiB, and `-C link-arg=-zstack-size=16384` brings all five to **1 page, 64 KiB** with no source change and no measurable size difference. Two consequences:

- **A leaf's per-instance page ceiling is one page for v1**, and this is where the leaf supplies its number rather than where the rule lives. ABI §4.1 states the rule for every host — a host MAY bound a module's declared linear memory, and one whose ceiling a module exceeds at either end MUST refuse it at load time, never grant it less, and never turn the refusal into a trap — and ABI §9.7 rule 10 states what a block may assume about any host's ceiling, which is nothing. A daemon bounds nothing (DAEMON §4); a leaf bounds one page, because the table above reserves 64 KiB per instance. **A leaf refuses at firmware build time rather than at load time** because that is where a leaf's admission happens at all: nothing is loaded on a leaf (§6.3), so §4's load-time refusal falls where the module is baked, and a refusal costs a build rather than a field failure. **That place is the generator** (§6.4.5), which takes the ceiling as a build input rather than a constant — it is derived from *this* table and so is a property of the target's heap, not of the platform — and passes it to `eio_manifest::validate_against`, the same call that already does ABI §4.3's cross-check, so a build cannot do the one and forget the other. The 17 pages above are now measured by that generator's own suite rather than quoted: baking any service made of golden blocks against a budget below what they declare is a refusal, and the refusal names the link flag.
- **Making the golden blocks pass that check was a change to how `cargo eio build` links, not to any block.** SDK §5.2 owns that default and now states it, with the reasoning for the size, for its being a default rather than a ceiling, and for leaving the module's *maximum* alone. The ceiling stays here: this is the only document that knows a per-instance page budget, and a copy of it in the SDK would be a second definition of it.

#### The reserve bounds growth as well as admission, and it takes two mechanisms

ABI §4.1 is explicit that a page ceiling bounds **admission, not growth**: `memory.grow` is core WASM, a module may leave the page it declared, and what bounds that is the module's declared *maximum* and the engine enforcing it. Everything above is an admission bound, so until eieio-x7g.2.27 **nothing on a leaf bounded growth at all** — a guest could reach 65 536 pages as far as either engine was concerned, and unlike a daemon there is no OS to absorb it. What follows is that decision and its reasoning.

**Both mechanisms are used, and they are not alternatives — they cover disjoint modules.** The question was posed as a choice between refusing at firmware build time and capping at the engine. It is not one:

1. **A module whose declared maximum exceeds the reserve is refused by the generator**, in the same call and the same walk as its declared minimum (§6.4.5, `eio_manifest::validate_against`). It has stated an appetite this target cannot meet. Capping it at the engine instead is not a gentler answer, it is a *wrong* one: ABI §4.1 forbids granting an instance less than it declared at either end, because the instance then fails at whatever allocation first crosses a line the guest was never told about, at whatever moment the traffic reaches it. That an engine will do exactly this if asked is measured, not supposed — `crates/leaf/tests/memory_growth.rs` hands WAMR a module declaring four pages and watches it silently receive the reserve, with no diagnostic anywhere. A build refusal names the block, names both numbers, and costs a build.
2. **A module that declares no maximum is bounded at the engine**, at instantiation, because there is nothing for a loader to refuse — and this is the case that actually occurs. `wasm-ld` emits no maximum unless asked and SDK §5.2 deliberately does not ask, so **every block this platform builds** declares one page and nothing on the right, which an engine reads as the WASM default of 65 536 pages. Refusing those instead would mean refusing every block the SDK produces; requiring a `--max-memory` from the SDK would put this target's page budget into every block the daemon tier runs, which SDK §5.2 declines for reasons that have not changed. Capping a module that declared nothing grants it nothing less than it declared, so §4.1's prohibition does not reach it: the host is choosing a maximum the module declined to choose.

**What a guest observes is core WASM's own answer, and no new surface anywhere.** `memory.grow` returns `-1` — neither a trap nor a status code, and nothing this platform defines. A guest allocator reads that as a failed allocation, and it reaches the ABI only where an allocation failure always did: `eio_alloc` returning `0`, which ABI §9.5 already makes `ERR_LIMIT`, counted like any other block-level error. **ABI §8's death kinds are a closed set and this adds nothing to it.** A leaf therefore does *not* kill an instance for having grown, before or after the fact, and the post-hoc variant — checking `memory.size` after each callback and discarding the instance — was considered and rejected for exactly that reason: it would be a fourth kind of death, and it would report a property of the host's budget as a fault of the guest.

**The rule above is ABI's, not this document's, and it lives there.** Which end of a declaration a host refuses, and what a guest sees when growth is bounded, are things a daemon and a leaf must agree on or a block author cannot write against either — ABI §13's divergence, in the one place a block cannot see it coming. ABI §4.1 carries it, pinned by `34_memory_ceiling` and `35_memory_maximum`; what stays here is the number and which engine can enforce it.

**Only WAMR can, measured.** `crates/wamr-host` passes the reserve to `wasm_runtime_instantiate_ex`, whose `wasm_runtime_get_max_mem` has exactly §4.1's semantics: it takes the smaller of the host's number and the module's own, and refuses to override below the module's declared minimum, so it can bound growth without ever handing out a crippled instance. **wasm3 has no equivalent** — its only linear-memory ceiling is `d_m3MaxLinearMemoryPages`, a compile-time define of the published `wasm3x-sys` crate, and its only per-runtime knob is `M3Runtime::memoryLimit`, which is internal to wasm3 and clamps *bytes* while leaving the page count, which would be worse than no bound at all. So a wasm3 leaf is bounded by the generator and by the heap itself, and not per instance. The guest-visible behaviour is identical — wasm3's `op_MemGrow` also answers `-1` when `ResizeMemory` fails, whether from a declared maximum or from a `realloc` the heap could not satisfy — so what is missing is the *isolation*: one instance's ability to eat the reserve another was budgeted out of. **The fix is §11's and belongs to a firmware build**, which compiles wasm3's sources itself and can define `d_m3MaxLinearMemoryPages` as this section's reserve; a global compile-time constant is the right shape for a leaf, which has one number. The host build links a published crate and cannot. `crates/leaf/tests/memory_growth.rs` asserts the gap as it stands, so the day `wasm3x` grows the knob, the suite says so.

**`max_payload` is 4096 and `max_batch` is 8** for the v1 target. ABI §9.7 makes both host configuration with no floor and SCOPE §3 keeps the *question of a floor* OPEN; supplying values is what §4 already said a leaf does, and this is that.

- **4096 is EXPR §9's `MAX_VALUE_BYTES` floor**, and choosing it there is the whole argument: a conforming expression may build a value whose canonical encoding is 4 096 bytes, and a leaf whose `max_payload` were smaller would make a value the language guarantees can be *built* impossible to *emit* — the §4.1 shape of divergence, in a third place. Framing means a batch of exactly one maximal value does not fit, which is the honest cost of not sizing above the floor.
- **8 delivered signals** is a delivery bound only (ABI §9.7 rule 8): a leaf block may still emit a larger batch and the leaf routes it. It is deliberately small because §4.4's deadline is derived from it — `max_batch` is the one number that appears in both budgets, and a leaf that raises it pays in wall-clock time as well as in RAM.

**What §11's bring-up must report back:** the linked image's `.data`/`.bss` and WAMR's runtime globals, which decide whether 89 KiB of headroom is real; the engine execution stack a golden block actually needs **on the target and in AOT mode** — the table above answers it for WAMR's interpreter on a 64-bit host, which is the half that can be measured without a board; the expansion factor §4.3 defines, which is measured there on a 64-bit host and is the least certain number in either section; and a per-instance growth bound for wasm3, which this host build cannot supply and a firmware build can. The linear-memory reserve itself is **not** on that list, and the paragraph above the 17-page one says why: it is the guest's own address space, so the number holds on a 32-bit target unchanged.

### 4.3 When a batch will not fit

**`max_payload` does not bound host memory, and that measurement is the centre of this section.** A batch's decoded footprint is not a bounded multiple of its CBOR length, because canonical CBOR is dense for small scalars and `BTreeMap<String, Value>` is not. Measured with a counting allocator over `eio_signal::Batch::from_cbor`:

|Batch shape|CBOR|Decoded|Expansion|
|---|---|---|---|
|One 4 000-byte string value|4 007 B|4 761 B|**1.19×**|
|1 636 three-character keys with small integer values|8 184 B|180 588 B|**22.1×**|

Both are legal, canonical batches. So a host that has checked `len` against `max_payload` has learned nothing about how much memory decoding will take, and a leaf that decodes on the strength of that check alone is one hostile batch away from `handle_alloc_error` — which on `no_std` is a panic, and on a leaf a panic is a reset (§4.6). ABI §9.5 already says what should happen instead: a host that cannot allocate for a delivery **MUST NOT** kill the instance, the delivery fails and is counted. This section makes that implementable.

**The rule: a leaf reserves before it decodes.** Before decoding any batch — inbound from the router or outbound from `emit` — a leaf computes `len × expansion_factor` and refuses if the allocator's free bytes will not cover it. The check is arithmetic on a length the host already holds against a number the allocator already knows, so it costs nothing and, decisively, it happens **before** the allocation it is protecting against. That is ABI §6.2's own principle — refuse on a length you have not read — applied one layer in.

`expansion_factor` is a per-target build constant. **v1 builds with 16**, from the 22.1× measured above scaled down for the target's 32-bit pointers (`Value` is 32 bytes on the host and the shrink is in its pointer-sized halves). It is the least certain number in §4, it is deliberately a build constant rather than a literal so that eieio-x7g.2.11 can correct it from a real measurement, and it is a *reservation*, not a limit: a batch that reserves 16× and uses 1.19× returns the difference immediately.

**What a leaf does, in ABI §8's vocabulary and no other.** Nothing here is new; the value of the table is that it is complete.

|What will not fit|What the leaf does|Whose vocabulary|
|---|---|---|
|An inbound batch longer than `max_payload`, or carrying more than `max_batch` signals|Refused **at the router, before the guest is called**. The batch is dropped and counted. No status code reaches anyone: the emitter already got `0`.|ABI §9.7 ("never delivers batches beyond it"), §13.1's table ("refused, guest never called")|
|An inbound batch whose reservation the heap cannot meet|The same: dropped at the router, counted, guest not called.|ABI §9.5's rule, moved one step earlier so no allocation is attempted|
|An inbound batch that will not decode — deeper than §4.1's bound, or not canonical|The same: dropped at the router, counted.|ABI §6.3.1|
|`eio_alloc` returning 0 for an inbound payload, because the *guest* is out of memory|Delivery fails, reported `ERR_LIMIT`, counted as a block-level error. The instance lives.|ABI §9.5, verbatim — "a guest that is briefly out of memory has told the truth about itself"|
|`emit` with `len` beyond `max_payload`|`ERR_LIMIT` to the emitter, payload never read.|ABI §6.2, row 3|
|`emit` whose reservation the heap cannot meet, or which would exceed the callback's emission budget|`ERR_LIMIT` to the emitter.|ABI §9.7 rule 9 for the budget, which is §6.2's "queue full … policy is host-defined" with a number this section supplies; the reservation is this section's own|
|`emit` of bytes that are not a canonical batch, or on an undeclared port|`ERR_INVALID_ARG`.|ABI §6.2, rows 1–2|
|A guest pointer that is misaligned, zero-but-nonzero-length, or outside linear memory|The instance is discarded. Not a memory-pressure case at all: the guest has said something untrue about itself.|ABI §9.6|

**So: refuse outbound, drop inbound, never truncate, and never die.** Truncation is the one of the four that is rejected on principle rather than on arithmetic. A Signal is a *batch* (SCOPE §5), and half a batch is a value nobody wrote: a block that emitted eight readings and had five delivered has been told a lie about its own output, in a platform whose entire conformance argument is that two hosts do the same thing. Dying is rejected because ABI §8 reserves death for traps, fuel and deadlines, and running out of room to hold a signal is none of the three.

**The emission budget, because `emit` enqueues.** ABI §6.2 routes after the callback returns, so every batch a callback emits is held — `host-core` holds it *decoded*, as `Emission { port, batch }` — until the callback is over. On a daemon that queue is a `Vec` and grows; on a leaf an unbounded `Vec` inside one callback is the leak this section exists to close. **A leaf supplies `max_emission_bytes` = 4 096** for v1: one payload's worth out for one payload's worth in. Past it, `emit` answers `ERR_LIMIT`, which is a status code and therefore life (ABI §8) — the block sees the refusal, and ABI §10's own advice for a block with more to say is already "long work is chunked via timers".

**The rule is ABI §9.7 rule 9's, and only the number is this document's**, which is a correction to what this section used to say rather than a restatement of it. The bound was stated here as a leaf obligation, and it was unimplementable as stated: `emit` is `host-core`'s (`crates/host-core/src/core_fns.rs`), the queue it pushes onto is shared by both hosts, and §2's MUST-NOT list forbids a leaf a second implementation of `eio:core`'s host functions — so honouring it meant either amending a ★ crate, which makes it a both-hosts change, or breaking §2 two paragraphs earlier. Worse, it was invisible: a callback emitting 10 KiB succeeded on a daemon and was refused here, with no ABI text telling a block author the limit existed. It is now a third field of `eio_host_core::Limits`, checked once in `host-core`'s `emit`, published in the instance descriptor when a host bounds it, and pinned on every host by `33_emission_budget`. A leaf's part is the number — the same part it plays for `max_payload` and `max_batch` — and a daemon's part is to state that it does not bound the queue at all (DAEMON §6.2).

**4 096 rather than something derived from §4.2's 48 KiB working set**, because the working set is shared and this bound is per instance: one instance's callback may hold a payload's worth, and the graph beneath it holds the rest. The bound is on the bytes the guest passed to `emit`, not on what holding them costs — the expansion measured above is exactly why those are different numbers — so it is the *reservation* rule, not this one, that stands between a leaf and `handle_alloc_error`. Both are needed and neither substitutes for the other.

**Reaching `handle_alloc_error` is a leaf defect, not a policy.** The reservation is what makes it unreachable; if it is reached, the `#[panic_handler]` resets the node, and §4.6 says what that costs and why it is nonetheless the honest last resort. It is not a design answer to "the batch did not fit" — that answer is the table above.

**What is not decided here.** What a leaf does with an instance that has been *killed* — restart it, leave it dead, tear down the graph — is SCOPE §3's block-failure-policy **OPEN** item, and this section neither answers it nor needs to: every row above leaves the instance running.

### 4.4 The watchdog

§4 settles *that* a leaf's budget is a watchdog. This is which timer, how long, and how it stops a guest.

**The timer is TIMG0's digital watchdog, MWDT0, in two stages.** Not a general-purpose alarm plus a separate watchdog: the ESP32-C3's digital watchdogs are already multi-stage, each stage carrying its own timeout and its own action, with interrupt actions in the earlier stages stepping up to a system reset in the later ones. That is the mechanism this section needs, built into the part, so a leaf configures one peripheral rather than composing two:

|Stage|Timeout|Action|What it means|
|---|---|---|---|
|0|the callback deadline|**interrupt**|The ISR asks the engine to stop the running guest call. One instance dies, exactly as a daemon's would (ABI §8).|
|1|4 × the deadline|**system reset**|Stage 0 did not get the call back. The node restarts. This is the divergence, and §4.6 states it.|

It is **armed on entry to a guest callback and disarmed on return**, not left running: a leaf spends most of its time idle between signals, and a watchdog that ran through the idle would be measuring the wrong thing.

**MWDT0 rather than MWDT1 or the RTC watchdog**, and the reasons are not interchangeable. MWDT1 is by ESP-IDF convention the interrupt watchdog; a bare-metal leaf has no ESP-IDF and both are free, but leaving MWDT1 alone keeps that convention available to anyone who later puts an RTOS underneath. The RTC watchdog is the wrong instrument for a different reason: **its reset reaches the RTC domain**, and §4.6 depends on the record of a kill surviving the reset that a kill causes. MWDT's system reset covers the CPU and peripherals and leaves the RTC domain standing, which is the property that makes §4.6 implementable. That reading is from the datasheet's reset-source table and has not been confirmed on hardware; §11's bring-up confirms it, and if it is wrong the record has to go to flash instead, at §5's wear cost.

**The deadline is 250 ms for stage 0 and 1 000 ms for stage 1**, and the number is a floor-driven derivation rather than a feel. What makes it a decision is the constraint, not the value:

> **A conforming block MUST NOT be killable for spending budget the platform promised it.**

That is the same argument §4 already makes for EXPR §9's floors, applied to wall-clock time. A single callback may resolve a property for every signal in the batch, and a conforming expression may burn `MAX_FUEL` steps doing it, so the worst case a *conforming* block can reach is

```
max_batch × properties × MAX_FUEL × t_step
```

— `8 × properties × 10 000` evaluation steps, times the target's per-step cost. `t_step` has not been measured on the target and is the number this derivation is least sure of; v1 builds with **250 ns**, which gives 160 ms for an eight-property block and leaves 250 ms as a deadline with margin rather than a coincidence.

**The firmware build checks the product, and fails rather than shipping a leaf that can kill a conforming block.** It knows `max_batch`, it knows each instance's property count from its manifest, and `t_step` is a per-target build constant beside `expansion_factor` (§4.3). If the product exceeds the deadline, the build fails and names the instance. **The lever is `max_batch`, not the deadline**: lowering the number of signals a leaf delivers at once shortens the worst case linearly and shrinks §4.2's working set at the same time, whereas raising the deadline forever ends with a watchdog that fires after the network has already given up. This is the one number that appears in both budgets, and that is why §4.2 set it low.

**The watchdog is fed across host calls whose duration is the host's, not the guest's.** A flash sector erase on this part is tens of milliseconds and an I²C transaction is bounded by the bus, and neither is the guest failing to return. So a leaf resets stage 0's counter around `state_get`/`state_put`, `i2c_*` and `http_*`, and **does not** feed it around `prop`, `emit`, `log` or the clocks, whose cost is the guest's own choice. The budget exists to bound a guest that will not come back; charging it for the host's device drivers would make the deadline a measurement of the flash part.

**Granularity is not the timer's resolution, and saying so avoids a false precision.** The systimer behind MWDT counts in microseconds, so the deadline is exact to far better than it needs to be. The real granularity is **how often the engine looks**: an interpreter can only abandon a call at a point where it checks, and ABI §10's second obligation on a budget mechanism makes "at least once per loop back-edge and once per call" a requirement on every such binding rather than a hope about one (§4.5). With that requirement met, the overrun past the deadline is bounded by one straight-line instruction sequence, which WASM makes finite by construction — there is no unbounded straight-line code in a module the loader accepted. Without it, stage 1 is the only bound, and the answer is a reset.

### 4.5 What an engine binding MUST expose for the watchdog to work

This is a requirement on **every** engine binding, present and future — including the WAMR interpreter binding of eieio-x7g.2.5 — because §4.4's stage 0 is unimplementable without it, and a binding that cannot meet it makes a leaf's only budget a reset.

1. **A termination entry point callable from outside the running call**, taking the instance or its execution environment and asking the in-progress guest call to stop. This is the one `wasm3x` 0.1.0 does not have — measured before the bring-up was written, and the reason `crates/leaf` answers `enforces_budgets = false` and has `07_budget_exhausted` skipped by name.
2. **It MUST be callable from an interrupt context, or the binding MUST document a safe deferral.** This is the requirement that decides whether the watchdog can be a hardware alarm at all. A termination that takes a lock, allocates, or is otherwise not ISR-safe forces stage 0's ISR to do nothing but set a flag someone else must poll — which is a fine answer, but it has to be *stated*, because the polling interval then becomes the granularity §4.4 attributes to the engine.
3. **ABI §10's two obligations on any budget mechanism**, which a binding here meets rather than restates: the terminated call returns to the host as a **trap and not a status code**, and the **gap between the request and the return is bounded** — for an interpreter, by checking the request at least once per loop back-edge and once per call, which is what §4.4's granularity claim rests on. Both were written out here before they were written into ABI §10, and that was the error: they are what *every* host's budget owes, epoch interruption included (DAEMON §5.1), so a copy of them in this document is a copy free to drift from the daemon's.
4. **A binding that cannot do 1–3 answers `enforces_budgets = false`** and has the budget scenario skipped by name, rather than hanging or pretending. The skip class is ABI §13.1's, beside the unimplemented capability and the unrefused proposal, and this is a leaf binding taking it. It is not a concession: a suite that reports a skip is a suite you can read, and one that reports a pass on an unenforced budget is worse than one that reports nothing.

**Nothing here is WAMR-specific on purpose.** The list is what a *watchdog* needs from an interpreter — items 1 and 2 are the leaf's own, an ISR being the only thing that will be holding the deadline on a node with no scheduler — so it is the acceptance criterion the interpreter binding is written against rather than a description of what one interpreter happens to offer, and it is what `07_budget_exhausted` stops being skipped on (eieio-x7g.2.13).

### 4.6 Reporting a kill, when there is no log stream

§7 removes the whole of DAEMON §9, and it does so deliberately, so a leaf that kills an instance has nowhere obvious to say so. **The mechanism belongs to §8 and to §7.1; what this section fixes is the requirement that mechanism has to meet.** Stating it here rather than designing a topic here is the point: the transport decision (eieio-x7g.2.10) and this one have to agree, and the way to make them agree is for the watchdog to name what it needs rather than to invent a channel for itself.

Three obligations, and the second is the one that is easy to miss:

1. **A kill MUST be recorded, not merely emitted.** The normal condition of a leaf is that nobody is listening — no operator, possibly no broker reachable. A report that exists only as a message sent at the moment of the kill is a report that is usually lost. The record is state that outlives the callback and is readable by whatever §8 publishes, so the fact is still there at the next connect.
2. **A kill MUST survive the reset a kill can cause.** §4.4's stage 1 resets the node, and a fact that the reset erases cannot explain the reset — which is the failure mode most worth avoiding, because a leaf that reboots for an unrecorded reason is indistinguishable from a leaf with a hardware fault. This is why §4.4 chose MWDT over the RTC watchdog: the record goes in RTC-retained memory, which the digital watchdog's system reset does not clear. The chip's own reset-reason register is the backstop for the backstop — even with the record lost, "this node rebooted because of a watchdog" is recoverable from the silicon.
3. **The minimum record**, which is what §7.1 must carry and what a firmware build must leave room for: the instance id, the callback that overran (ABI §5.1's step, or the export name), the deadline in force, and which stage fired. Which stage is not a detail: stage 0 means one instance died and the graph kept running, and stage 1 means the graph did not.

**The divergence, argued rather than arrived at** (ABI §13). Stage 0 is not a divergence: a leaf kills one instance and keeps running, exactly as a daemon does, and a block cannot tell the two hosts apart. **Stage 1 is a divergence and a large one** — a daemon kills one instance while the node carries on, and a resetting leaf restarts the whole graph, so every instance re-runs ABI §5.1 from step 0, in-memory state is gone, `eio:state` survives because it is flash (§5), and armed timers do not. Three things make it the right answer anyway:

- **The alternative is worse and is not observable.** A leaf whose engine will not stop a spinning guest, and which does not reset, is a node that runs no blocks, answers no HTTP because §7 gave it none, and cannot be distinguished from a dead device. A reset at least produces a node that comes back and says why.
- **Stage 1 firing is a defect in the engine binding, not a normal path.** With §4.5's requirements met, stage 0 ends the call and stage 1 never runs. The divergence is therefore on the *failure path of the mechanism* rather than in the mechanism, which is a different claim from "a leaf behaves differently", and it is the claim ABI §13 can live with.
- **It is bounded and it is reported.** Stage 1 is a fixed multiple of a deadline the build checked, and obligation 2 above makes the reset explain itself. A divergence that announces itself every time is one an operator can act on; a silent one is the kind ABI §13 exists to prevent.

**What a leaf does with the dead instance afterwards — restart it, leave it dead, tear the graph down — is SCOPE §3's block-failure-policy OPEN item** and is not settled here. This section says how the instance dies and how the fact travels; it deliberately says nothing about what replaces it.

## 5. State on flash

`eio:state` (ABI §7.2) is backed by flash through `host-core`'s `StateStore` trait — the same three functions, `get`/`put`/`del`, the daemon implements against redb (DAEMON §10). The trait is the boundary, so the host functions that decode `(key, key_len, buf, cap)` and apply ABI §8's size convention are shared code and cannot diverge.

**Wear is the difference, and `ERR_THROTTLED` is how it is spoken.** ABI §7.2 permits a leaf host to refuse a `state_put` for a wear budget; the daemon never does, and the variant is plumbed on both so a block's back-off branch is the same code either way. Two obligations follow:

- A leaf MUST NOT silently drop a write. Refusing with `ERR_THROTTLED` is the contract; succeeding and not persisting is not.
- ABI §7.2's "blocks MUST treat persistence as best-effort and not as a message queue" is what makes the refusal safe. A block that cannot tolerate a refused write is a block that cannot run on a leaf, and that is a property of the block.

**Namespacing is `(service, instance)`**, as DAEMON §10 establishes and for the same reason: a node does not know its System. On a leaf there is exactly one service, so the service component is constant — it is kept anyway, because dropping it would make a leaf's key layout differ from a daemon's for no gain, and `eieio`'s whole conformance argument is that the two agree. §5.1 says what the constant *is*, which is what turns that sentence into a claim something can check.

The **wear budget policy** — how much writing is too much, over what window, and what a leaf does when a block ignores repeated refusals — is **OPEN** (SCOPE §3.7). §5.2 is the layout underneath it and does not decide it: where the region is and how big it may be are separable from how fast it may be written, and only the first is settled here.

### 5.1 What the constant service component is

**The service component is the service file's `name`** (SERVICE §3), verbatim as the file spells it and with no normalisation — the same string, read from the same field, that a daemon composes into the same position of the same key (DAEMON §10's composition takes it from the parsed service's `name`). §6.4.2 bakes it as `BakedNode::service`, which is where a leaf's copy comes from and why nothing on the device computes it.

**Naming the value is the whole of the parity claim**, and until it was named there was no parity to have. "Keep the component, because dropping it would make a leaf's key layout differ from a daemon's" holds only if the component *carries what a daemon's carries*. A leaf that kept a constant `"service"`, or the node id, or the empty string would have a daemon's key shape and none of its content — and would read in a dump as though it agreed. So the rule is not "keep a constant". It is:

> Given one service file, a daemon and a leaf compose the same `(service, instance, key)` triple for the same block.

That is checkable on the build host, from the same file, with no device — the shape §6.4.4 already gives a generator's other obligations.

Two consequences, stated because the stronger version of each is what a reader will otherwise assume:

- **The parity is of key composition, not of the store's bytes.** A daemon's redb tuple encoding and a leaf's flash records are each their own host's, and nothing here makes a state store portable between tiers. What is shared is the trait, the composition, and the three host functions above it — which is what stops the two hosts *disagreeing*, and is all ABI §13's argument needs.
- **A service's name is therefore a state identity, on both tiers.** Renaming a service in the file leaves every namespace the old name wrote behind and starts new ones — on a daemon at the next reload, on a leaf at the next flash. That is DAEMON §10's "nothing garbage-collects a namespace" seen from the other end; §5.3 says what a leaf does with what is left.

`crates/leaf`'s flat-file stand-in does not do this: it gives each instance a file at `state/<instance_id>.bin` and drops the service component entirely. That was defensible while the component had no stated value, and it is a divergence now that it has one — one a bring-up with no service file behind it cannot close, and a real flash-backed store must (eieio-x7g.2.14).

### 5.2 Flash layout: two regions, and how the image finds the second

§6.3 settles what is *in* the image — the runtime, the baked graph and every block artifact — and with it that a leaf's flash holds two regions and not four:

|Region|What is in it|Lifecycle|
|---|---|---|
|the **image**|the runtime, the baked graph (§6.4), every block artifact (§6.3), and everything else a build decided (§6)|replaced wholesale by a flash|
|the **state region**|`eio:state`'s keys and values, and nothing else|written only by a running leaf; untouched by a flash (§5.3)|

**There is no third region, and the two absences are the load-bearing part.** Configuration is in the image because §6 bakes it. The node id is in the image because §6.4.3 makes it a required build input. Both are worth saying rather than leaving to follow, because the embedded reflex is to put identity in a writable key-value store beside the state — and a device with a baked id *and* a stored one has two answers to who it is, which is the failure §6.4.3's whole argument is about not having. **What a build decided is in the image; what a block wrote is in the state region; nothing is in both.**

**§4.6's kill record is not a counter-example, and it MUST NOT be moved into the state region.** It lives in RTC-retained memory — SRAM the digital watchdog's system reset does not clear — which is not flash and so is not a region here at all. Keeping it there rather than in flash is not an accident of what survives a reset: a kill is a failure path, the state region is wear-budgeted and refusable (§5), and a crash loop that wrote to flash on every iteration would turn a defect in an engine binding into a worn-out part. **The state region holds what a block wrote through `eio:state` and nothing else** — not the runtime's own facts about itself, whose durability requirement §4.6 states and whose channel §7.1 owns.

**The image MUST NOT carry the state region's absolute address.** It finds the region *by name*, through whatever description of its own flash the target already has, and an offset compiled into the runtime is forbidden — not for elegance, but because it would be a second place the layout is written down, and the failure when the two disagree is a write into somebody else's sectors rather than an error.

On the v1 target (§6.2) that description is the ESP-IDF partition table, and these are the platform's figures rather than this document's choices:

- The partition table lives at flash offset **`0x8000`** and occupies one **4 KB** sector, so the first partition may begin no earlier than `0x9000`. Every partition offset MUST be 4 KB aligned, and an app partition additionally 64 KB aligned. ([ESP-IDF: Partition Tables](https://docs.espressif.com/projects/esp-idf/en/stable/esp32c3/api-guides/partition-tables.html))
- **A bare-metal image boots through that table exactly as an ESP-IDF one does** — ROM bootloader, second-stage bootloader at `0x0`, partition table at `0x8000`, app at `0x10000` — because `espflash` bundles an ESP-IDF second-stage bootloader and synthesises an ESP-IDF partition table whether or not the ELF it is given came from ESP-IDF ([The Rust on ESP Book: Bootloader](https://docs.espressif.com/projects/rust/book/application-development/bootloader.html)). §6.2 chose a `no_std` triple; it did not buy a different boot path, and this is the place that matters.
- **The table is therefore already the one place the layout is written down**, which is why the image reads it rather than carrying a copy of what it says.

**How much flash there is to divide** ([ESP32-C3 Datasheet](https://documentation.espressif.com/esp32-c3_datasheet_en.html) §1, §4.1.2): the recommended embedded-flash part, `ESP32-C3FH4X`, carries **4 MB** in package, as do the `ESP32-C3-MINI-1` module and the `ESP32-C3-DevKitM-1` built on it; `ESP32-C3FH8X` and the `-H8` modules carry 8 MB, and the chip supports **16 MB** of external flash at most (§4.1.2.2). Of that, **8 MB maps into the instruction address space and 8 MB into the data address space, in 64 KB blocks** (§4.1.2.2) — which is the ceiling under which §6.3's linked-in artifacts are read straight out of `.rodata`, and it is not a ceiling a service graph is likely to reach on a 4 MB part. Against 4 MB, the 16 KB floor below is 0.4% of the device; the image gets the rest, and this layout costs a leaf essentially nothing.

**Erase granularity is 4 KB and a program page is 256 bytes** on the SPI NOR parts this class uses ([Winbond W25Q32JV](https://www.winbond.com/resource-files/w25q32jv%20revg%2003272018%20plus.pdf)), rated at 100 000 program/erase cycles per sector. **That last number is class-representative and not a measurement of any device this project will ship**: on a `-MINI-1` the die is in package and Espressif's own module datasheet declines to name its vendor. That is corroboration for SCOPE §3.7 keeping the wear budget **OPEN** rather than an argument for closing it — the endurance figure a policy would have to be built on is not yet knowable for the part this tier will actually ship.

What this document fixes is only what is its own:

- **The state region is one partition of type `data`, named `eio_state`.** The name is the contract between the image and the flash tool, and it is the whole of the contract: nothing else about the entry is this specification's.
- **It is not an NVS partition and MUST NOT be declared as one.** A leaf's store is `host-core`'s `StateStore` (§5), not a second key-value library, and an `nvs` subtype invites another stack's tooling to write into a region it does not own.
- **Its size is a build input, with a floor of 16 KB.** Erase granularity is one 4 KB sector, so a store that must not lose what it already holds needs somewhere to write a replacement before erasing the original: three sectors is the smallest arrangement in which that is possible at all, and a fourth gives a compaction somewhere to go. ESP-IDF's own NVS puts its minimum at 12 KB for the same arithmetic ([NVS](https://docs.espressif.com/projects/esp-idf/en/stable/esp32c3/api-reference/storage/nvs_flash.html)) — corroboration, not the rule.
- **A leaf build supplies its own partition table.** `espflash`'s default one is `nvs` at `0x9000`, `phy_init` at `0xf000`, and a `factory` app partition from `0x10000` filling the flash to the end — so accepting it leaves no room for a state region at all, and its `nvs` is a partition nothing on a leaf uses. The table is a build output like the image, and a leaf's declares `eio_state` and need declare no `nvs`.

**What the region's *contents* look like is deliberately not settled here** — the record format, how a write survives a power cut, how erases are spread across sectors. That is an implementation (eieio-x7g.2.14) under a policy that is **OPEN** (SCOPE §3.7's wear budget). This section fixes where the region is, how it is found, how small it may not be, and what may not be in it.

**Where each figure comes from, because §4.2 shows the difference matters.** That section budgets against `esp-hal`'s linker script (313 KiB) rather than the datasheet's headline SRAM (400 KiB), because the number that binds is the one the toolchain computes. The same rule is applied here: what the *chip* can address — flash sizes, the 8 MB mapping windows, the 64 KB block size — is the datasheet's to state, and what the *tool* does — three segments, per-segment erase, the default partition table — is read from `espflash`'s source rather than its book, because a book describes intent and the source is what runs. **No board has run any of it.** §6.2 records that no board is named anywhere in this repository or its tracker; this section inherits that caveat unchanged, and the first image that boots is what turns these figures into measurements.

### 5.3 What a firmware update does to what is already there

Three rules, and they are one rule seen three ways: **a flash replaces the image and nothing else.**

**1. State survives a firmware update.** The state region is not part of the image, and the flash step a build emits (§10) MUST write only the image's own regions. **A deploy MUST NOT be a full-chip erase**, and a build MUST NOT emit one as a step.

§10 calls "a failed build changes nothing on the device" the one operational advantage this tier has over a hot-loading daemon. A *successful* flash that silently wiped a counter would spend that advantage on the deploy path instead of the failure path — and state is the one thing on a node that cannot be rebuilt from a file (DAEMON §10).

**On the v1 target this rule costs nothing, because the ordinary flash already obeys it.** `espflash flash` writes exactly three segments — the bootloader at `0x0`, the partition table at `0x8000`, and the app at the `factory` partition's offset — and the erase it asks for before each is derived from that segment's own length rounded up to a sector, so a partition none of the three covers is not touched. Erasing more than that is a different command with its own name (`erase-flash`, `erase-parts`, `erase-region`), and `flash`'s own `--erase-parts` / `--erase-data-parts` flags are opt-in. **A build MUST NOT emit any of them.** (Read from `espflash`'s source rather than its book: `flash_segments` and `default_partition_table` in `espflash/src/image_format/idf.rs`, `write_segment` in `espflash/src/target/flash_target/esp32.rs`, and the `Commands` enum in `espflash/src/bin/espflash.rs` — [esp-rs/espflash](https://github.com/esp-rs/espflash).)

The exception is the operator's and is named as one: **moving or resizing the state region is a reprovision, not an update.** Its contents do not survive it, nothing on the device can migrate them, and a build that changes either offset or size MUST say so among the steps it emits (§10) rather than let a flash discover it.

**2. A changed graph orphans state, and nothing reclaims it.** A firmware build may change the instance set — an id removed, an id renamed, an instance added — and §6.4.2's numbering is rebuilt from whatever the new service file says. State written under a `(service, instance)` key that the new image's graph has no instance for is **left exactly where it is**. A leaf does not delete it at boot, does not delete it during a flash, and has no way of being asked to.

That is the daemon's rule (DAEMON §10: "nothing removes a namespace as a side effect"), kept for the daemon's reasons and one of the leaf's own:

- **The failure that matters is deleting live state, not retaining dead state.** Renaming an id is an ordinary edit; a leaf that reclaimed on a graph change would discard — silently, once, unrecoverably — state a block is about to want.
- **A leaf cannot tell "removed" from "not yet".** A daemon can: it compares the store against every service file on the node, which is what makes DAEMON §9's orphan question answerable at all. A leaf knows one graph, the one it was built with, so "no instance claims this key" is exactly as true of a device half way through a rollout as of one whose block was deleted on purpose.
- **Divergence.** What a deploy does to state is not something the two hosts may answer differently (§9, ABI §13).

**What replaces `DELETE /state/orphans/{namespace}` is the flash tool.** A daemon's reclamation is an operator naming one namespace over an API a leaf does not have (§7). A leaf's is the operator erasing the region — the same act, explicit and named and never a side effect, through the only channel this tier has. It is a coarser instrument, and the cost is stated rather than hidden: **a leaf cannot reclaim one namespace.** It reclaims all of them or none.

**A full region answers `ERR_IO`, and being full is not a licence to reclaim.** A `state_put` with no room left fails with `StateError::Io` — ABI §8's `ERR_IO` — because that is the vocabulary `host-core`'s `StateStore` has, and its two variants are deliberately the whole of it. It MUST NOT be `ERR_THROTTLED`: that code means "retry later" (ABI §8), which is true of a wear budget refusing a burst and false of a region with nothing left. And **a leaf MUST NOT free space by evicting a namespace**, orphaned or otherwise — that is the reclamation this rule refuses, arrived at through pressure instead of through a decision.

**3. An instance that survives keeps its state — through a changed block and through changed properties.** Neither the block nor its properties is a key component, on either host.

- **A new version of a block, or a different block entirely, under the same id inherits the namespace.** That is what makes ABI §13.2's stateful counter survive a firmware update at all; a host that keyed by block would reset every counter on a version bump, which is the opposite of what a durable store is for. The obligation therefore lands on the block author, and ABI §7.2 already gives it: values are opaque bytes to the host, nothing in the platform versions their encoding, and **a block that changes how it encodes its state is responsible for reading what its predecessor wrote** — or for changing its keys. A build host cannot check this and MUST NOT try: mangling a key so the mismatch became impossible would have to be the daemon's behaviour too, and it would trade a rare visible bug for a permanent one.
- **An id reused for an unrelated block is the one case where the safe default is surprising**, and it stays the default. Reusing an id is the author saying these are the same instance; a service file has no other way to say otherwise, and giving it one is SERVICE-SPEC's decision and not this document's.
- **A property change does nothing at all.** Properties are configuration, evaluated per signal (ABI §11), and live in the image. On this tier a one-line property edit reflashes the whole image (§6.3) — and it costs nothing in state, which is worth stating precisely because that reflash is the most alarming-looking thing this tier does.

**The observable contract, in one sentence.** Flash an image built from a changed service file, and every instance whose id survived finds the state it left, every instance whose id did not is gone with its keys still on the device, and nothing is removed that an operator did not explicitly erase.

Whether a leaf *says* any of this — how much of the region is spent, how many namespaces no instance claims — is §7.1, which is where everything a leaf reports about itself is decided — and it is not among the three record kinds settled there.

## 6. What is baked, and what a build produces

A daemon reads `node.toml` (DAEMON §2.1) and a service file (SERVICE-SPEC) at boot. A leaf has neither at runtime, so the firmware build resolves both and bakes the results:

- **The service graph**: instances, their block AOT artifacts, their resolved property expressions, and the connection table `host-core`'s router consumes.
- **The node's identity and limits**: what `node.toml` would have carried.
- **The transport configuration** (§8): what `pubsub.toml` would have carried.

**Those three are *what* is baked; §6.4 is the form they take.** §6.3 settles the question underneath both — a block's compiled artifact is part of the firmware image rather than something a leaf reads out of flash — because that choice decides the shape of the baked graph, the flash layout (§5.2) and what the pipeline produces.

**The service file is still the source, and stays the portable artifact.** The same file deploys to a daemon; SERVICE-SPEC parses it; the firmware build is one more consumer. It is not parsed *on* the leaf — `eio-service` is a `std` crate and deliberately so (CLAUDE.md: nothing parses a service file on a leaf tier) — it is parsed by the build host, which then emits Rust.

**Property expressions are baked as source text, not as a compiled form.** ABI §11 makes every property an expression evaluated per signal, and EXPR-SPEC's parser is a ★ crate that runs on the leaf. Pre-parsing to an AST at build time is a plausible optimisation and is explicitly **not** specified here: it would put a second representation of an expression into the platform, and the first thing to measure is whether parse cost matters at all when properties are parsed once at configure time (ABI §5.1) rather than per signal.

### 6.1 The AOT artifact

`cargo eio aot --target <leaf>` produces a WAMR AOT artifact per block, and ABI §11.1's manifest carries an `aot` list naming the prebuilt targets published alongside the portable module. §6.2 says which targets those are and §6.2.1 how each is spelled. The portable `wasm32-unknown-unknown` module **MUST always ship** (ABI §11.1): an AOT artifact is an optimisation for one target, never a replacement for the thing every host can run.

**AOT artifacts are version-sensitive, and the pairing is normative.** A WAMR AOT artifact is tied to the WAMR version that compiled it and to the LLVM that WAMR was built against — the WAMR this repository links pins LLVM `release/18.x` (`build-scripts/build_llvm.py`), and so does the `iwasm` installed beside it. A leaf image and the artifacts it links MUST come from the same WAMR version. **§6.3 makes that pairing a property of a single build** — one `wamrc` produces the artifacts, one toolchain builds the engine they are linked beside, and the mismatch becomes a thing the build host can refuse rather than a check the device would have to carry. **§6.1.1 says where the pair is recorded, which of §6.3's mismatch classes survived it, and what "can refuse" costs to make true.**

This section is **PROPOSED and unimplemented**: `wamrc` has not been built on any developer machine here (six distinct blockers recorded on `eieio-7d8.21`), so the artifact layout is specified from WAMR's documentation rather than from something this repository has produced. **It ratifies when a leaf image links an artifact this pipeline built and runs it** — §6.3 settles that linking, and not runtime loading, is what a leaf does with an artifact — and not before. The interpreter path (§3) needs none of it and is what a first leaf bring-up should use.

#### 6.1.1 Where the pair is recorded

**Nowhere in eieio. `wamrc` records it in the artifact, and WAMR's loader checks it.** This platform adds no version field — not to ABI §11.1's manifest, not to §6.2.1's artifact name, not to the image — because a second record of a fact the bytes already carry is a second thing that can disagree with the first, and the disagreement would be silent on the side that nobody checks.

**What the artifact already carries, read from the WAMR this repository links rather than from documentation** (`wamrx-sys` 0.3.0's vendored tree, `core/iwasm/aot/aot_loader.c`). A `.aot` opens with a magic number and a `uint32` version, and the loader refuses on four distinct grounds:

- `AOT_MAGIC_NUMBER` — "magic header not detected".
- the version against `AOT_CURRENT_VERSION` (`6` in that tree), by **exact equality and not a range** — `aot_compatible_version` is `return version == AOT_CURRENT_VERSION;` — "unknown binary version".
- a target-info section carrying `arch`, `e_machine`, endianness, bit width and `e_type`, checked against the runtime's own build — "invalid target type, expected %s but got %s".
- a `feature_flags` word checked against the proposals the runtime was *compiled* with — "SIMD is not enabled in this build", and one such line per proposal. That is §3.1's accepted set enforced mechanically against an artifact, where `eio_manifest::validate` enforces it against a module's imports.

So an artifact states its own loadability, and in more detail than a version string would: the third and fourth grounds are exactly the facts §6.2.1 says a triple encodes and a chip name does not. What it does **not** state is the WAMR *release* or the LLVM behind it. `AOT_CURRENT_VERSION` is a file-format number that many releases share, so a match proves the format did not move and proves nothing about the tree.

**What §6.3 removed, and what it did not.** It removed the failure this section was written about. An image built by one WAMR and an artifact region written by another cannot both exist on a leaf, because nothing can write the second one: §7 declines a management API, §8's bus carries Signals and not code, and §5.2 leaves exactly two regions of which neither holds a block. That mismatch class is unconstructible now rather than unlikely, and no device records or compares a WAMR version.

It did not remove the pairing; it moved it. An artifact reaches a firmware build as an *input* — §6.4.5's "one artifact per block reference" — and may come from a registry (ABI §11.1's `aot` list is a statement about what a registry can supply) or from a cache, while the engine linked beside it is whatever `wamrx-sys` vendors. Those are still two independently versioned things. They now meet on the build host instead of on a device, which is the whole of the improvement and is a large one.

**But WAMR's four checks are a *loader's*.** Linking makes the mismatch build-time *detectable*, not build-time *detected*: left alone, the earliest one fires is the first boot of a flashed image — the expensive place §6.3 said the check would move away from. So: **the firmware build MUST refuse an artifact whose header does not match the engine it is linking**, reading the fields `wamrc` already wrote. *Which* fields it reads, *when* it reads them, and how an artifact cache is keyed so a stale entry cannot outlive a WAMR bump are §11's pipeline item; this section fixes only that the record is the artifact and the reader is the build host, never the device.

**The pin above is not enforced by anything today, and that is this section's own worked example.** The engine a leaf links is not chosen in this repository: it is `wamrx-sys` 0.3.0's vendored tree, WAMR **2.4.3** (`core/version.h`), locked in `Cargo.lock`. The `iwasm` on the machine this section was written on is **2.4.5**, and it is where an earlier draft's version number came from. So the first `wamrc` built here will come from a different tree than the engine it compiles for, and whether the two agree on `AOT_CURRENT_VERSION` is unmeasured. Pinning one tree for both is the concrete form §11's toolchain-pinning row takes, and it is not something a spec can assert into being.

This subsection is inside §6.1 and inherits its **PROPOSED** status: the loader behaviour above is read from source, but nothing here has compiled an artifact or linked one.

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

**The name is a necessary key, not a sufficient one.** §6.1's WAMR-and-LLVM pairing is not carried in the string and MUST still be checked. **§6.1.1 answers where it is recorded — in the artifact, by `wamrc` — and confirms this spelling rather than disturbing it.** The facts that decide whether an artifact loads are in the artifact's own target-info section, checked by the loader against the runtime's build, so the triple here is a *selection* key and never a safety one. Putting a WAMR version into the name would make a registry's answer to "what did you publish" depend on the toolchain of whoever consumes it, and would require every block republished on a version bump — for a fact the bytes already carry.

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

Three sections said this three ways: §1's table said a leaf's blocks are "AOT-compiled and linked in", §6.1 said the section ratifies when a leaf "loads an artifact this pipeline built", and §11's flash-layout item asked where AOT artifacts "sit". Those are not three spellings of one design. They are two designs — artifacts *in* the firmware image, versus artifacts in a flash region the runtime reads at boot — and the difference decides the shape of the baked graph (§6.4), the flash layout (§5.2) and what the pipeline produces.

**The answer is §1's.** Every block's compiled artifact is part of the firmware image, reached from the baked graph as a `&'static [u8]`, and **a leaf never reads a block's code out of a flash region it did not link**. §6.1 and §11 are amended to agree.

Why, in the order the reasons bind:

- **There is no channel that could deliver a block on its own.** §7 removes the management API by design rather than by omission — it "cannot be authenticated safely enough to be worth it" and "would have nothing to serve". §8's bus is the only other thing a leaf listens to, and it is at-most-once, never-retained and authenticated by a per-*bus* pre-shared key (SCOPE §3.11); pushing executable code down it would make every holder of the bus key a code-execution path on every device in the System, which is the surface §7 declined, reinvented. So the only way bytes reach a leaf is a flash operation, and once that is true a separate artifact region buys a smaller *write*, never a new capability. §1's deploy row — "build firmware, flash it" — was already the whole of it.

- **A separate region is a configuration surface, which is the one thing a leaf does not have.** §1's stated design consequence is that "a leaf has no configuration surface at runtime". An artifact region writable independently of the image is exactly that: state the device holds that the image did not decide. It also manufactures a mismatch class this tier cannot check — image built against one WAMR, region written by another — which is the failure §6.1 names as the one that matters, "a load failure in the field, after flashing". Linked in, that pair is a property of a single build and the check moves to the build host.

- **The graph and the code must not be able to disagree, and only one design makes that structural.** §6.4's instances carry each block's port names *in index order*, and that order is the manifest's (ABI §5.2). They are computed on the build host from the manifest that accompanied the artifact. If the code arrived separately, the baked port numbering and the loaded module could disagree with nothing on the device able to notice — a silent misdelivery rather than a load error. Linked in, the description and the bytes it describes are one artifact by construction.

- **The manifest may not survive the AOT compile, so the cross-check is a build-host act either way.** §3.1 already requires `eio_manifest::validate` at firmware build time "where a refusal costs a build rather than a field failure", and that check reads the module's import section and its `eio:manifest` custom section (ABI §4.3, §11) — neither of which a `.aot` is guaranteed to carry. **That is read from WAMR's documentation and has not been measured here**, for the reason §6.1 records. It cannot make the case *for* loading in any event: a device that cannot re-derive a manifest from the artifact it loaded has no way to check that it is the artifact its graph describes.

- **It costs less code on the tier least able to pay.** Loading needs a region reader, an index format and a parser for it, an integrity check and the version check above — all in the image, all `no_std`. Linking needs none of them: `include_bytes!` puts the artifact in `.rodata`, which on the v1 target (§6.2) is memory-mapped flash, so the `&'static [u8]` the baked graph carries is already a pointer into flash and no RAM is spent holding it. Whether the engine then copies the text into executable RAM is the engine's question and is identical under both designs, so it is not a discriminator between them.

**What is given up, stated plainly:** a leaf cannot be updated one block at a time, and a one-line property change reflashes the whole image. That is not a cost this section introduces — §1's deploy row and SCOPE §3.7's "extra flashing steps are acceptable" already said it — but it is the cost, and the argument that would reverse it is the argument for giving a leaf a management surface. It would be taken in §7, not here.

**This is engine-independent, and that is what makes §6.4 buildable now.** A bring-up leaf (§3) links the portable `wasm32-unknown-unknown` module through the same include; an AOT leaf links a `.aot` for §6.2's triple. Nothing else in the baked graph differs between the two, so the generator and the representation it emits can be written and tested against the interpreter today, with no `wamrc` — which is otherwise what §6.1's PROPOSED status blocks. **This section does not ratify §6.1**: no pipeline has produced an artifact, and none has been linked.

**What it settled for the flash layout.** A leaf's flash holds exactly two regions with distinct lifecycles: the **image** — runtime, baked graph and every block artifact, replaced wholesale by a flash — and the **state region** (§5's `StateStore`), which is not part of the image and which the image must be able to find. An artifact partition, an on-device artifact index and a cross-region version pairing were removed from the question here; where the region sits, how the image finds it and what an update does to what is in it are answered in §5.2 and §5.3.

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
    pub service: &'static str,                     // the service's name; §5.1's key component
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
- **`include_bytes!` alone is not enough.** It yields an align-1 array, and both engines read multi-byte fields directly out of the buffer they are handed; §6.2's target gives no unaligned-access guarantee. `crates/leaf` therefore provides the macro that wraps the include in an over-aligned type — `eio_leaf::include_module!`, a `#[repr(C)]` struct whose first field is a zero-length `[u128; 0]`, contributing **16-byte alignment** and no bytes — and a generator MUST emit that rather than a bare `include_bytes!`. **Both the requirement and the value are read from the engines' documentation and have not been measured on hardware** — §6.1's caveat, in the one place where being wrong about it is a fault at boot rather than a build error. 16 is chosen for being a superset of any field either engine plausibly reads out of a module or an AOT artifact: the cost of over-aligning is at most fifteen bytes of `.rodata` per artifact, and the cost of under-aligning is a fault after flashing.
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

Both are testable on the host build against `examples/services/`, with no target, no board and no `wamrc` (§6.3), and together they are what makes §6.4.1's "serialise, do not compute" rule checkable rather than merely stated. `crates/leaf-gen/tests/parity.rs` is that test, and it recomputes rather than reusing: everywhere the generator called a ★ crate, the suite calls it again independently from the same inputs and compares, because a test that reused the generator's own intermediate values would prove only that it is self-consistent. It runs both rules over every file in `examples/services/`, and adds the thing §6.4.4 does not ask for and the tier needs anyway — that the baked graph *runs*, on both of §3.2's engines, reaching the routed result `crates/leaf`'s hand-written demo table reaches.

#### 6.4.5 Where the generator lives, and what a build states

**`crates/leaf-gen` — package `eio-leaf-gen`, a `std` build-host crate with a library and a command-line binary.** Three placements were available and this is the one with the fewest consequences:

- **Not a module of `crates/leaf`.** Reading a service file needs `eio-service`, which is `std` and does not compile without atomics, and §2.1 draws a `no_std` boundary through the runtime crate. A separate crate keeps "nothing parses a service file on a leaf tier" (SCOPE §3.7) true by construction rather than by a feature flag nobody may turn on for the wrong target.
- **Not a `cargo eio` subcommand.** `cargo eio` is the block author's toolchain (SDK §5); a firmware build is a node operation.
- **Not a `crates/leaf` build script.** A build script fixes *when* and *by whom* the generator runs, and how a firmware build is invoked — by a Designer deploy (§10), by CI, by a person — is §11's pipeline contract. A command-line binary settles nothing about it.

**What a build states.** These are the generator's inputs, and each is here because §6 says it is baked or §6.4.3 says it cannot be minted:

|Input|Why|
|---|---|
|the service file|The source, and the portable artifact (§6). Parsed here and never on the device.|
|the **node id**|Required. §6.4.3: a generator MUST NOT mint one.|
|the node name|Optional; a label (§6.4.3).|
|one **artifact per block reference**|§6.3: every block's code is linked into the image, so a block the build was not given is a build failure. Selecting *which* artifact — the portable module, or the `.aot` whose `aot` entry equals the triple being built for (§6.2.1) — is the build's decision and reaches the generator as this map.|
|the **per-instance page budget**|§4.2's MUST, checked here. An input rather than a constant because §4.2 derives it from the target's heap.|
|the **bus configuration**|§8, DAEMON §7.1. Stated field by field — bus, ranked candidates, pin, pre-shared key — and **not read from a `pubsub.toml`**.|

**Why the transport configuration is fields and not a file**, since a daemon reads exactly such a file: the reader for `pubsub.toml` is private to `crates/daemon`, and a second reader of one format is how two nodes come to disagree about what that format means — the same argument §2 makes about a second router. Passing the fields keeps one reader in the platform and puts the question of *where a firmware build gets them* — a `pubsub.toml`, a Designer deploy, a secret store — where it belongs, in §11's pipeline item. A build that gives no bus bakes `transport: None`, which DAEMON §7.1 already calls the normal case.

**Every refusal is a message about a service file, never a compiler error** (§10). The generator runs SERVICE §7's stage 1 and stage 2, ABI §4.3's load-time cross-check per artifact (§3.1's MUST, here because here is where a refusal costs a build), §4.2's page budget, ABI §11.1's required/default rule, and finally `Routes::resolve` — whose result it *discards*, keeping only the refusal, because §6.4.1 makes a table that would not resolve on the device evidence that the generator is wrong.

**The generated file is written into the build directory and never checked in** (§6.4.2). Its module paths are absolute, and it compiles against `eio-leaf` with default features off — the shape a firmware build has.

## 7. There is no management API

A leaf serves no HTTP. DAEMON §9's entire surface is absent, and that is a design decision rather than a gap:

- **It cannot be authenticated safely enough to be worth it.** SCOPE §3.11 leaves transport security OPEN, and an MCU is the tier least able to carry a TLS stack and a credential lifecycle.
- **It would have nothing to serve.** Two thirds of §9 mutates a service file or a block cache, and a leaf has neither.

**Consequences that other specs already encode**, restated here so a leaf implementer meets them in one place:

- DESIGNER §3.1: the Designer's proxy and its node probe both **refuse a leaf by name** rather than dialling it. A leaf's address over HTTP would give a connection error indistinguishable from a node that is down, reporting a fault against a node working exactly as designed. (The `eio` CLI now has the same guard, for the same reason: `nodes.toml` carries an optional `class` per node, absent meaning `"daemon"`, and `eio` refuses a `"leaf"` entry by naming the class. SCOPE §3.7.)
- DAEMON §7.1: only a daemon-class node is eligible to be the pub/sub broker. A leaf is never a candidate.
- Observability is the wire protocol's (§8), not an endpoint's. **§4.6 is the first concrete demand on that**: a leaf that kills an instance still has to be diagnosable, so a kill is recorded rather than merely emitted, and it survives the reset §4.4's stage 1 causes. §7.1 is the channel; §4.6 is the requirement it meets.

### 7.1 What a leaf says about itself

§7 leaves a leaf one channel, so everything DAEMON §9 would have served — that the node is there, that an instance died, why the node restarted — travels on the bus or not at all. Three decisions, and the first is the one the other two follow from.

**A leaf's self-report is a Signal.** Not a status document, not a diagnostic frame, not a second encoding: one `Batch` of `eio_signal` signals in ABI §6.3.1's canonical CBOR, published exactly the way `publisher` publishes, decoded exactly the way `subscriber` decodes. Nothing about it is special except the topic it lands on.

That is the whole of the answer to the objection this decision was written against — *a leaf publishing a bespoke status schema is a second management API wearing a topic name*. A bespoke schema would need a parser on every client, a version of its own, and a place in the Designer and the CLI that nothing else occupies. A `Batch` needs none of those: it is already what every host decodes, what a tap already renders, what `expr-tests/cbor/` already pins, and what a `subscriber` block already turns into signals a graph can act on. **A leaf gets observability by having none of its own.**

**It lands on the node's own topic: `eieio/<bus>/<node-id>`.** `<bus>` is `BakedTransport::bus` (§6.4.2) and `<node-id>` is `BakedNode::id` (§6.4.3), which DAEMON §2.1 mints as opaque and stable and which already matches ABI §11.1's name pattern — so this is DAEMON §7's topic rule unchanged, with the node's own identity as the topic, and it needs no new syntax.

**No segment is reserved for it, and refusing to reserve one is the decision.** A `$`-prefixed segment, or any multi-segment topic, would be unclaimable by a `publisher` — ABI §11.1's pattern admits neither `$` nor `/` — but it would be equally unreachable by a `subscriber`, because a topic property is one pattern in both directions. A report no `subscriber` can consume is a report that reaches only a client holding a raw MQTT connection, and that would put a leaf's diagnosis outside everything SCOPE §4's peer-client rule guarantees an agent: no `GET /services/{s}/logs`, no MCP tool, no tap. Leaving it on an ordinary topic means a daemon anywhere on the bus can wire a `subscriber` to a leaf's id and the report arrives in DAEMON §9's surface with nothing added to it. The cost is that a topic property spelled exactly like a node id would collide; a minted opaque id makes that deliberate rather than accidental, and DAEMON §7 already permits two publishers on one topic on purpose.

**Every record carries `node`, and that is forced rather than redundant.** DAEMON §7's boundary means a topic never reaches anything above the bridge, so a `subscriber` emits a batch with no idea where it came from. A record that identified its node only by its topic would be unreadable by the one mechanism designed to read it.

#### 7.1.1 The records

Three kinds, and a leaf publishes nothing else. Each is one signal — a map — and every field named for a kind is always present, because **missing data is an error, not null** (EXPR §6) and a report is the last place to make a consumer guess.

|Field|In|Type|What it is|
|---|---|---|---|
|`node`|all|`Str`|`BakedNode::id` (§6.4.3)|
|`event`|all|`Str`|`"boot"`, `"kill"` or `"gone"`|
|`service`|`boot`|`Str`|`BakedNode::service` — §5.1's constant key component, so a reader can tie the report to the state namespace|
|`reset`|`boot`|`Str`|the chip's own reset-reason register, rendered lowercase. §4.6 calls this "the backstop for the backstop"; this is where it surfaces|
|`dropped`|`boot`|`Int`|how many `kill` records were evicted before this connect. `0` normally, never absent|
|`instance`|`kill`|`Str`|the instance id (SERVICE §2: the id, never the name)|
|`callback`|`kill`|`Str`|the guest export that overran (ABI §5.1's step)|
|`deadline_ms`|`kill`|`Int`|the deadline in force (§4.4)|
|`stage`|`kill`|`Int`|`0` or `1`|

`instance`, `callback`, `deadline_ms` and `stage` **are §4.6's minimum record, field for field** — "the instance id, the callback that overran (ABI §5.1's step, or the export name), the deadline in force, and which stage fired". §4.6 fixed those four and deliberately left the topic and the encoding here; this section spends exactly that freedom and adds nothing to the four.

`stage` is the field §4.6 singles out — "stage 0 means one instance died and the graph kept running, and stage 1 means the graph did not" — so it is an `Int` and not a flag, and a `boot` record arriving beside a `stage: 1` kill is a reset explaining itself.

**Nothing else is published. No heartbeat, no metrics, no counters, no log stream.** A heartbeat on an at-most-once, never-retained bus proves less than the MQTT keepalive already proves to the broker, and it would put a fixed traffic floor on the tier least able to pay one. Metrics are OPEN in SCOPE §3.12 for the whole platform and a leaf is not the place to settle them. And a log stream is DAEMON §9.6's, which §7 removed on purpose: this is three record kinds, not the beginning of one.

#### 7.1.2 Recorded, not emitted

§4.6's first obligation is that a kill is **recorded**, because "the normal condition of a leaf is that nobody is listening". The mechanism is a fixed-capacity ring in **RTC-retained memory**, which is why §4.4 chose MWDT over the RTC watchdog: MWDT's system reset leaves the RTC domain standing.

- **The ring holds `max_batch` − 1 records — seven, for v1** (§4.2 sets `max_batch` to 8). That is not a round number, it is the number that makes a drain **exactly one Signal**: one `boot` record plus a full ring is eight signals, which is precisely what a leaf is allowed to deliver at once. A report that needed two batches would need an order between them, on a transport that promises none.
- **A `boot` record is not in the ring**, so it can never be the thing evicted. The record that explains a reset is the one a full ring would otherwise lose first.
- **On overflow the oldest `kill` is dropped and counted** into the next `boot` record's `dropped`. That is DAEMON §6.2's posture and SCOPE §3.4's, unchanged: a leaf loses observability the same way it loses signals, rather than in a new way that needs its own explanation.
- **Sizing**: seven records of an instance id (≤ 64 bytes, SERVICE §2), a callback name, a deadline and a stage, plus one boot record — under 1 KiB of RTC-retained memory. §4.6 said a firmware build "must leave room for" this; that is the figure. Whether the part's RTC domain survives an MWDT reset as §4.4 reads the datasheet to say **has not been confirmed on hardware**, and §4.4 already records what happens if it is wrong: the ring goes to flash instead, at §5's wear cost.

**The drain is one publish on every successful connect**, and it is **at-most-once like everything else on this bus.** The ring is cleared once the batch has been handed to the transport, and a leaf does not retry it. That is deliberate and it is not a hole in §4.6: the record exists to close the gap between a kill and the next connection, which is the gap §4.6 says is normally open forever. Once a connection exists, a report is a Signal and gets a Signal's guarantee. Retrying would make it the one message on the bus with a delivery promise nothing else has, and would need an acknowledgement the platform has no vocabulary for.

#### 7.1.3 Liveness is the connection, and the will says so

A daemon answers "are you there" with `GET /node`. A leaf's answer is that it holds a connection, and the bus already knows: **a leaf's bridge sets an MQTT will on `eieio/<bus>/<node-id>` whose payload is a one-signal batch, `{node, event: "gone"}`, QoS 0 and not retained.** The broker publishes it when the connection drops without a clean disconnect, so a leaf that loses power or wedges says so without executing anything.

A will is a transport concept and lives where transport concepts live — at the bridge, below DAEMON §7's boundary. Nothing above it names one; a `subscriber` receives an ordinary Signal on an ordinary topic and cannot tell that a broker rather than a node published it.

**The asymmetry, stated so it is not discovered.** A daemon's bridge sets no will today, because DAEMON §9's API is its liveness. So a graph subscribed to node topics sees `gone` from leaves and not from daemons. That is not the kind of divergence ABI §13 forbids — no block can distinguish the two hosts by anything in a batch it is delivered, and this is observability rather than signal delivery — but it is an inconsistency, and the resolution is the daemon's bridge doing the same rather than a leaf doing less. It is filed, not done here.

## 8. Transport

A leaf participates in the same pub/sub as a daemon: MQTT behind the same conceptual bridge boundary (SCOPE §3.9, DAEMON §7). The guarantees are the platform's own vocabulary — at-most-once, never-retained (SCOPE §3.4) — and are *mapped* onto QoS at the bridge, not stated in QoS terms.

`publisher` and `subscriber` remain host-native system blocks (DAEMON §6): they need credentials and transport internals, which is the whole reason that precedent exists and the whole reason it does not extend to anything else.

**The bus pre-shared key (SCOPE §3.11) applies unchanged.** It was chosen over mTLS with a System CA *because* of this tier — a CA lifecycle plus a TLS stack on every node is the weight that deletes the embedded north star. A leaf presenting the bus key is the case that decision was made for.

**The client is `minimq` 0.13.** §8.1 is the measurement behind that, §8.2 what it costs against §4.2's budget and what sits underneath it, §8.3 how "the same bytes as a daemon" is checked and the one place it does not hold today, and §8.4 what it does across a reconnect. The earlier draft of this section endorsed no client because nothing had been measured; that is no longer true, and what replaces it is numbers rather than a preference.

### 8.1 The client, and the measurement behind it

Three `no_std` MQTT clients exist on crates.io that build for §6.2's target. All three were built for `riscv32imc-unknown-none-elf` on the pinned 1.97.1 toolchain at `opt-level = "z"`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, each driving one program that connects with the bus key, subscribes, publishes and receives, against a stub byte stream and a stub `embassy-time` driver. Footprint is the linked ELF's allocated sections against a 10-byte do-nothing baseline; "own static RAM" is `.bss` + `.data` minus the caller-owned buffers the program itself declares.

|Client|Version|MQTT|`.text`|`.rodata`|Flash|Own static RAM|Client state|Driving frame|
|---|---|---|---|---|---|---|---|---|
|`mountain-mqtt`|0.2.0|v5|16 498|1 175|**17 673 B**|**0**|—|656 B|
|**`minimq`**|**0.13.0**|**v5**|**20 582**|**1 184**|**21 766 B**|**0**|`Session` = 728 B|1 968 B|
|`rust-mqtt`|0.5.1|v5|34 418|1 872|**36 290 B**|**0**|`Client` = 112 B|816 B|

**None of the three carries hidden static RAM**, and that is the first thing the measurement settles rather than the last: every one reported `.bss` exactly equal to the buffers the test program declared, so all three are caller-owned-buffer designs and a leaf's transport RAM is a number a leaf chooses rather than one a crate imposes.

**`mountain-mqtt` is the smallest and is disqualified, on a fact read from its source rather than from its reputation.** `ConnectionSettings` carries private `username` and `password` fields and exposes exactly one public constructor, `unauthenticated(client_id)`, with no setter for either. A leaf built on it **cannot present the bus pre-shared key**, and SCOPE §3.11 chose that key over mTLS *because of this tier* — it is not an optional extra a leaf may skip. Its own README also describes it as "in very early development… not yet stable or feature complete". 4.0 KiB of flash is not worth the one credential this document's security posture rests on.

**`rust-mqtt` costs 14.2 KiB more flash than `minimq` for machinery at-most-once never uses**: QoS 1 and 2 retransmission, session recovery, flow control, topic aliases. It also requires a `BufferProvider` — its `bump` feature is a second allocator beside §4.2's one heap, which is the shape §4.2 rejected for the engine and rejects again here for the same reason. It is the right client for a bus with delivery guarantees; SCOPE §3.4 does not have one.

**So `minimq`, and the reasons are properties rather than a ranking:**

- **Caller-owned buffers and no allocator of its own.** `Buffers::new(rx, tx)` is the whole of its memory model, so §4.2's single TLSF heap stays single and a reconnect cannot fail for want of memory.
- **It presents the bus key.** `ConfigBuilder::auth(user_name, password)` is MQTT username/password, which is exactly what DAEMON §7.1's `MQTT_USERNAME` constant and `Key` already put on the wire.
- **QoS 0 is a first-class path, not a degenerate case of QoS 1.** Its own `publish` documentation: QoS 0 "bypasses retained outbound state, encodes into temporary TX scratch space, and writes directly to the transport… does not consume replay/in-flight slots". Nothing accumulates across a disconnect, which is what at-most-once means made structural.
- **It carries a will**, which §7.1 spends.

**Two costs, recorded because neither is visible from the table.** `minimq`'s *default* feature set enables `defmt`, which cascades into `embassy-time`, `heapless` and `embedded-io` and fails to link with four undefined `_defmt_*` symbols on a target with no global logger — a leaf builds it `default-features = false`. And it depends on `embassy-time`, so a leaf owes an `embassy-time-driver` implementation for the target; that is not free, but it is the same driver the network stack needs, so it is one debt and not two.

**What was not measured, stated so it is not read as measured**: nothing ran on hardware, because §6.2 already records that no board exists in this repository. The numbers above are a linker's, and the timing half of "how it behaves across reconnects on a constrained device" is §8.4's, read from source and left for §11's bring-up to confirm.

### 8.2 What it costs against §4.2's budget, and the thing underneath it that costs more

§4.2 budgets 313 KiB of SRAM against a 192 KiB heap floor, leaving ≈ 89 KiB for statics and the linked image outside a 32 KiB native stack reserve. The transport's claim on that:

|Item|RAM|Where the number comes from|
|---|---|---|
|`minimq` own static|0|measured, §8.1|
|RX + TX packet buffers|2 × 4 352 B|§4.2's `max_payload` of 4 096, plus MQTT fixed header, topic and properties|
|`Session`|728 B|measured, §8.1|
|Driving stack frame|1 968 B|measured, §8.1|
|TCP socket buffers|2 × 4 096 B|one connection at that payload size|
|`embassy-net` `StackResources<3>`|1 504 B|measured|
|**Total**|**≈ 20.6 KiB**||

Flash: 21.3 KiB for `minimq`, plus **31.1 KiB measured for `embassy-net` 0.7 with `smoltcp` 0.12** (TCP, IPv4, DHCP, Ethernet medium, built for the same target through a stub `Driver`) — ≈ 52 KiB for the whole stack above the radio. Both fit.

**The radio driver does not obviously fit, and this is the finding §4.2 should hear.** `esp-wifi` 0.15.1 and `esp-hal` 1.0.0-rc.0 do compile for `riscv32imc-unknown-none-elf` (measured: they build, with `esp-hal`'s `unstable` feature, which its build script *requires* when `esp-wifi` depends on it). But `esp-wifi`'s own `esp_config.yml` — the file its build script consumes, so this is its configuration and not a third party's summary of it — defaults to **10 static RX buffers of "approximately 1.6KB" each, allocated at `esp_wifi_init` and not freed until deinit**, plus up to **32 dynamic RX** and **32 dynamic TX** buffers taken from the heap as traffic arrives. That is ≈ 16 KiB standing and an unbounded-in-practice dynamic claim, from the same heap §4.2 floors at 192 KiB for one block instance — and §4.2 reserved nothing for it.

Two things follow, and neither is this section's to settle:

- **§4.2's floor is a floor for the graph, not for the image.** The radio's buffers are outside it, so the honest reading of "one block instance" is "one, once the transport's ≈ 20 KiB and the radio's ≈ 16 KiB standing claim have been taken out of the ≈ 89 KiB of headroom". That still leaves room; what it does not leave is room for a second instance to be assumed — and since eieio-x7g.2.27 measured the linear-memory row at two pages, a second instance is not merely unassumed, it is 320 KiB against a part that has 313.
- **The dynamic buffer counts are a knob a firmware build must set rather than inherit.** Ten static RX buffers is an ESP-IDF default sized for throughput on a part with more RAM than this one, and a leaf that carries a batch every few seconds needs nothing like it. Which values, and what they cost in throughput, is a per-target measurement on a board — §11's bring-up item, named here so it is not discovered at flash time.

### 8.3 The same bytes as a daemon, and how that is checked

**Divergence between hosts is a conformance bug by definition (§9, ABI §13), and a transport is the one place where "the same behaviour" means literally the same octets.** Four of the five things that make up a wire message are identical by construction, and the fifth is not identical today.

1. **The payload is a `Batch` in canonical CBOR and nothing else.** A daemon's bridge publishes `batch.to_cbor()` and decodes with `Batch::from_cbor`; a leaf calls the same two functions on the same ★ crate. §9.1 already forbids substituting another encoder for size, and ABI §6.3.1's two deviations from RFC 8949 are pinned by `expr-tests/cbor/`, which both hosts run. There is no second encoder that could disagree.
2. **The topic is `eieio/<bus>/<topic>`,** DAEMON §7's rule unchanged. What differs is only where `<bus>` is read from: a daemon reads `pubsub.toml`, a leaf reads `BakedTransport::bus` (§6.4.2), which the firmware build wrote from the same file. Both components follow ABI §11.1's name pattern, so neither host escapes or normalises anything.
3. **The delivery guarantee is at-most-once, never retained** (SCOPE §3.4), which maps to QoS 0 and `retain = false` at both bridges. Stated as a mapping in both places, never as the guarantee.
4. **The credential is the bus key** under DAEMON §7.1's fixed `eieio` username, from `BakedTransport::key` on a leaf and from `pubsub.toml` on a daemon. The key is scoped to a bus and carries no per-node identity, which is the whole of SCOPE §3.11 and is why a leaf needs nothing minted for it.
5. **The protocol version is not the same, and this is the one place the claim fails today.** `crates/daemon` builds `rumqttc::MqttOptions`, which is **MQTT 3.1.1**. Every `no_std` client measured in §8.1 is **MQTT v5 only** — `rust-mqtt`'s `v3` feature is documented in its own README as "Unused", and `mountain-mqtt` lists v3 support under non-goals. And the two versions are not interchangeable at the broker either: `rumqttd` 0.20, the daemon's own fixture broker, binds v4 and v5 on **separate listeners** (`v4` and `v5` are separate `ServerSettings` maps, each spawning its own `Server` with its own protocol), so one address does not serve both — a leaf and a daemon pointed at the same `candidates` entry would not be on the same protocol.

**The fix is the daemon's, not a leaf special case,** which is what §9's rule requires: `rumqttc` 0.25 ships `pub mod v5` unconditionally, with no feature to enable and no new dependency, so a daemon moves to MQTT v5 by swapping a module and the bus becomes one version everywhere. Choosing instead to find a 3.1.1 `no_std` client would mean writing one — the only 3.1.1 crate on offer, `mqttrs`, is a codec and not a client — and would leave the platform's two hosts disagreeing about the protocol in order to keep the smaller host's choices free. **This section states the requirement: one bus is one MQTT version, and the version is part of the bus contract.** Making the daemon's bridge speak it is filed, not done here, and until it lands a leaf cannot talk to a daemon.

**How the remainder is verified, since four "by construction" arguments are not a test.** Points 1–4 are checkable by reading two call sites; point 5 was found by reading a third. What none of them proves is that two *different client libraries* put the same CONNECT, SUBSCRIBE and PUBLISH on the wire for one bus. That needs a case that runs a daemon `Bridge` and a leaf's `minimq` configuration against one embedded `rumqttd` and asserts a batch published by either arrives unchanged at the other, both directions, with the bus key required. It is named here rather than written because driving a `no_std` async client on a host needs an executor and a `std` byte stream behind `embedded-io-async`, which is the work; **until it runs, the claim this section makes is a claim.**

### 8.4 Across a reconnect

DAEMON §7.1's walk — dial candidates in rank order, first that answers is the broker, retry with backoff, publishes drop meanwhile — is unchanged on a leaf, except that §7.1 already says a leaf carries a single address rather than a ranked list because it is never a candidate. What §8.1's choice adds is that **`minimq` imposes no reconnect policy of its own**, which is why the walk stays the caller's:

- **The session outlives the connection.** `Session::connect(io)` takes a fresh byte stream each time and resets the packet reader, the transport state and the replay arming at the start of every call, so it does not matter how the previous connection ended — dropped, forgotten or errored. A leaf's loop is: open a socket, `connect`, drive, drop, repeat.
- **The broker tells the leaf whether to resubscribe.** `ConnectEvent::Connected` versus `Reconnected` distinguishes a fresh session from one the broker resumed, which is exactly the condition under which a leaf must re-issue its `subscriber` instances' `SUBSCRIBE`.
- **There is nothing to replay, and that is SCOPE §3.4 rather than a limitation.** QoS 0 publishes never enter retained outbound state, so a disconnect loses in-flight batches and accumulates nothing — the same loss DAEMON §7.1 already permits, counted at the bridge like every other discard (DAEMON §6.2).
- **Keepalive is the caller's obligation too**: `poll()`/`recv()` must be driven often enough to honour the interval advertised in CONNECT. On a leaf that is the same loop that delivers signals, so the constraint is that a leaf's idle path must not be idle for longer than the keepalive — which is a §11 bring-up number, not one this section can pick without a board.

**All four are read from `minimq` 0.13's source and its own documentation, not run.** A flaky-link characterisation — how long a dial takes to fail, what a half-open socket costs, whether the keepalive fires before the TCP stack notices — needs the board §6.2 says nobody has, and is §11's.

## 9. Conformance

**A leaf MUST pass what a daemon passes, and divergence is a conformance bug by definition** (ABI §13). Not "as far as is practical on an MCU": the suites are the contract, and a leaf that cannot pass one has found either a bug in itself or a rule the platform should not have.

Three suites, all of which already exist. **Suites 1 and 3 are per-engine and MUST be run against every engine the image may link** (§3.2); suite 2 is not — it drives no engine at all, so running it "on WAMR" and "on wasm3" would be the same run twice.

1. **The ABI §13 scenario suite**, driven through `host-core`'s `Engine` trait exactly as the daemon and the reference harness drive it. A capability a leaf does not implement is reported **skipped by name**, never passed over.

   **A leaf's skips are the leaf's, not its engine's**, and the count makes that visible: `crates/leaf` reaches **29 of 33 scenarios on each of its two engines**, with the same four skipped by the same four names — the budget scenario (§4: neither interpreter has a usable counter, so the watchdog is what closes it) and `eio:gpio`/`eio:i2c`/`eio:http`, which this crate has no host functions for. `crates/conformance/tests/wamr.rs` reaches 32 on the *same engine* because the reference harness supplies its own capability stand-ins; the difference between 29 and 32 is three namespaces, not three engine behaviours.
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
- **A successful flash changes nothing but the image.** The steps it emits write the image's regions and not the state region (§5.3), so a deploy never costs a block the state it wrote — and the two cases where that is not automatic, a state region being moved or resized and a graph that dropped an instance, are things the steps say rather than things a flash discovers.

The build pipeline's own mechanics — how the toolchain is pinned, where AOT artifacts are cached, how a build is reproduced — are **§11 expansion items**, not settled here.

## 11. Expansion list (for the in-depth pass)

Needed before implementation, and deliberately not guessed at in this draft:

- ~~**The target list.**~~ — **resolved** (eieio-x7g.2.2): §6.2 names it. v1 is one target, `riscv32imc-unknown-none-elf` (ESP32-C3 class); `thumbv7em-none-eabihf` stays a `check-nostd` gate target and is not a leaf target; `riscv32imac-unknown-none-elf` (ESP32-C6/H2) is the named next candidate; Xtensa is deferred for the esp-rs toolchain fork, measured rather than assumed. §6.2.1 fixes how an `aot` entry is spelled and §6.2.2 what adding a target costs.
- **A per-instance growth bound for wasm3.** §4.2 bounds `memory.grow` at the engine for a module that declares no maximum, which is every block the SDK builds — and only WAMR can be told. wasm3's only linear-memory ceiling is `d_m3MaxLinearMemoryPages`, a compile-time define of the published `wasm3x-sys` crate, so a host build linking that crate cannot set it and a firmware build compiling wasm3's sources can. A global compile-time constant is the right shape for a leaf, which has one number; what this item owes is the build plumbing that sets it and the measurement that it holds. `crates/leaf/tests/memory_growth.rs` asserts the gap as it stands, so closing it is a test that starts failing rather than a thing to remember.
- **Firmware build pipeline mechanics** — toolchain pinning, AOT artifact caching, reproducibility, and how `cargo eio aot` and `eio-leaf-gen` (§6.4.5) are invoked from a Designer deploy. It also owns where a build gets the two things §6.4.5 requires it to state and cannot invent: the node id between builds (§6.4.3) and the bus configuration. **And it owns the residue §6.1.1 hands it** (eieio-x7g.2.18): the WAMR/LLVM pair is recorded in the artifact by `wamrc` and nowhere in this platform, the device never reads it, and what is left is a build-host obligation — when the build reads the header WAMR would read at load, and how an artifact cache is keyed so a stale entry cannot outlive a version bump. It is concrete rather than hypothetical today: the engine's version is whatever `wamrx-sys` vendors (WAMR 2.4.3) and nothing in this repository chooses it, while the `wamrc` anyone would build here comes from a separate tree.
- ~~**The generated `main`**~~ — **resolved** (eieio-x7g.2.3): §6.4 gives the baked graph as one `static` of hand-written types, emitted as generated Rust source with no `fn` and no control flow in it, and §6.4.1 fixes the rule that keeps it from becoming a second router or a second property-resolution rule — a generator serialises what `host-core` computed on the build host and computes nothing itself. The `main` is not generated at all: it is hand-written per target and hands `GRAPH` to `spawn_graph`. §6.3 settles the ambiguity underneath it — a block's artifact is linked into the image, never loaded from a flash region. **The generator is built** (eieio-x7g.2.4): `crates/leaf-gen`, whose placement and build inputs §6.4.5 records, with §6.4.4's two rules as a suite over `examples/services/` on both engines. What remains of this item is the *invocation* — the pipeline row below — and nothing about the representation.
- ~~**Memory budget**: heap sizing per target, and what a leaf does when a batch will not fit.~~ — **resolved** (eieio-x7g.2.7): §4.2 sizes it and §4.3 answers it. The allocator is `embedded-alloc`'s TLSF heap, one heap shared with the engine; the heap is the linker's remainder against a **192 KiB floor** derived for the v1 target's real 313 KiB of SRAM, which sizes an image for **one** block instance. `max_payload` is 4 096 (EXPR §9's `MAX_VALUE_BYTES` floor) and `max_batch` is 8. §4.1's decode bound is **resolved as the daemon's constant, defended against a measured stack**: `Value::decode_at` is a 160-byte frame on `riscv32imc-unknown-none-elf`, so host parity costs 15 KiB more stack than the floor would, against a 32 KiB reserve. Two measurements do the work and both are recorded in place: a batch's decoded footprint expands by between 1.19× and **22.1×**, so `max_payload` does not bound host memory and a leaf reserves before it decodes; and every golden block declares **17 pages** of linear memory today, three and a half times the whole chip, which a link flag fixes and which a leaf refuses at firmware build time until it is fixed.

  **Amended** (eieio-x7g.2.27): the guest linear-memory row was one page per instance and is **two**, measured over §9's suite 1 rather than read off a memory section — `wasm-ld` puts a block's statics and shadow stack in the declared minimum and nothing else, while `dlmalloc` takes its whole heap from `memory.grow`, so the first `eio_alloc` a block serves leaves the page it declared. The floor stays 192 KiB and what it buys drops from two instances to one; two would want 320 KiB, more than the part has. The same bead bounded growth, which nothing on a leaf had done: the generator refuses a declared *maximum* over the reserve and the WAMR binding caps a module that declares none, both per ABI §4.1, and a guest sees `memory.grow` answer −1 and nothing else. **What is left here is wasm3**, which has no per-instance growth bound a host build can set — see the row below.
- ~~**The transport client** (§8), once one has been measured.~~ — **resolved** (eieio-x7g.2.10): §8.1 names **`minimq` 0.13** and gives the measurement. All three `no_std` MQTT clients that build for §6.2's target were linked for it and sized against a do-nothing baseline: `mountain-mqtt` 0.2.0 at 17 673 B of flash, `minimq` 0.13.0 at 21 766 B, `rust-mqtt` 0.5.1 at 36 290 B, and **none of the three carries any static RAM of its own** — every one reported `.bss` exactly equal to the caller-declared buffers, so a leaf's transport RAM is a number a leaf picks. `mountain-mqtt` is the smallest and is **disqualified for a reason read from its source**: `ConnectionSettings` exposes only `unauthenticated(client_id)` and no way to set a username or password, so it cannot present SCOPE §3.11's bus key — the credential this tier's whole security posture rests on. `rust-mqtt` costs 14.2 KiB more flash for QoS 1/2 retransmission and session recovery that at-most-once never uses, and needs a `BufferProvider` that would be a second allocator beside §4.2's one heap. §8.2 prices the choice at **≈ 20.6 KiB of RAM and ≈ 52 KiB of flash** including `embassy-net`/`smoltcp` underneath it (31.1 KiB, measured on the same target), and records the finding §4.2 should hear: **`esp-wifi` 0.15.1's own `esp_config.yml` defaults to ten ~1.6 KB static RX buffers allocated at init plus up to 32 dynamic RX and 32 dynamic TX**, against a heap §4.2 floored at 192 KiB with nothing reserved for a radio. §8.3 gives the wire-compatibility argument and the place it fails: the payload, topic, guarantee and credential are identical by construction, but **the daemon is MQTT 3.1.1 and every `no_std` client is v5 only**, and `rumqttd` 0.20 binds the two on separate listeners rather than negotiating — so one bus is one version, the fix is the daemon's (`rumqttc` ships `pub mod v5` unconditionally), and until it lands a leaf cannot talk to a daemon. §8.4 characterises reconnect from source: `minimq` imposes no reconnect policy, the session outlives the connection, `ConnectEvent` says whether to resubscribe, and QoS 0 accumulates nothing to replay. **Nothing ran on hardware** — §6.2 already records that no board exists here — so the timing half of reconnect and the radio's real buffer sizing stay §11's bring-up. The *configuration* it reads is already baked — §6.4.2's `BakedTransport`, filled in from the fields §6.4.5 makes build inputs — so what remains is where a firmware build gets those fields, which is the pipeline row above.
- ~~**Watchdog mechanics** (§4): which timer, what granularity, and how a killed instance is reported when there is no log stream to report it on.~~ — **resolved** (eieio-x7g.2.8): §4.4 fixes the mechanism, §4.5 the requirement it places on an engine binding, §4.6 the reporting. The timer is TIMG0's digital watchdog **MWDT0 in two stages** — stage 0 interrupts at a 250 ms callback deadline and kills one instance exactly as a daemon would, stage 1 resets the node at 1 s — armed on entry to a guest callback and disarmed on return, and fed across host calls whose duration is the host's rather than the guest's. Granularity is the engine's checking interval and not the timer's resolution, which is why "at least once per loop back-edge and once per call" is a **requirement on every binding** — now ABI §10's, with the trap-not-a-status-code obligation beside it, since both bind the daemon's epoch interruption just as they bind a watchdog — leaving §4.5 with what is genuinely a watchdog's: a termination entry point callable from outside the running call, and ISR-safety or a documented deferral. Neither of §3.2's engines meets it — measured, not read: `wasm3x` 0.1.0 exposes no interruption, abort or termination entry point at all, and WAMR's `wasm_runtime_set_instruction_count_limit` is compiled out behind `WASM_ENABLE_INSTRUCTION_METERING` with no `wamrx-sys` toggle to set it (confirmed by a linker error). So `crates/leaf` keeps `enforces_budgets = false` on both bindings and takes the named skip until one qualifies — it is the one skip a second engine did *not* close; that is eieio-x7g.2.13, and eieio-x7g.2.5's WAMR interpreter binding is written against §4.5's list. §4.6 states the divergence stage 1 buys and argues for it rather than arriving at it.
- ~~**Observability without an API** (§7): what a leaf publishes about itself, and on which topic.~~ — **resolved** (eieio-x7g.2.10): §7.1 answers both, and agrees with §4.6 field for field. **What a leaf publishes is a Signal** — one `Batch` in ABI §6.3.1's canonical CBOR, published the way `publisher` publishes and decoded the way `subscriber` decodes — which is how a leaf gets observability by having none of its own: no second schema, no second parser, no second version, and a `subscriber` anywhere on the bus carries the report into DAEMON §9's surface unchanged. **The topic is `eieio/<bus>/<node-id>`**, DAEMON §7's rule with the node's own id as the topic, and **nothing is reserved for it on purpose**: a `$`-prefixed or multi-segment topic would be unclaimable by a `publisher` but equally unreachable by a `subscriber`, which would put a leaf's diagnosis outside everything SCOPE §4 guarantees an agent. Three record kinds and no more — `boot`, `kill`, `gone` — with no heartbeat, no metrics and no log stream. **§4.6's minimum record is carried verbatim**: `instance`, `callback`, `deadline_ms`, `stage`, plus a `node` on every record, which DAEMON §7's boundary forces rather than duplicates since a topic never reaches anything above the bridge. §7.1.2 makes it *recorded* rather than emitted, as §4.6 requires — a ring of **`max_batch` − 1** records in RTC-retained memory, sized so that one `boot` plus a full ring drains as **exactly one Signal**, with the `boot` record outside the ring so the record that explains a reset is never the one evicted, and evictions counted into `dropped` the way DAEMON §6.2 counts every other discard. The drain is at-most-once like everything else on this bus and is not retried, because the record closes the gap between a kill and the next connect and does not turn a report into the one message with a delivery promise. §7.1.3 answers liveness with an MQTT will set at the bridge, and states the asymmetry it leaves against a daemon rather than hiding it.
- ~~**A class-aware CLI**~~ — **resolved** (eieio-x7g.5): it learns the class. A node entry in `nodes.toml` carries an optional `class`, `"daemon"` or `"leaf"`, absent meaning `"daemon"`, and `eio` refuses a leaf by naming the class rather than reporting a failed request. SCOPE §3.7 records the decision and why the two alternatives — requiring the key, or inferring the class from a refused connection — are each worse.
- ~~**Flash layout**~~ — **resolved** (eieio-x7g.2.9): §5.2 gives the two regions and how the image finds the second. The state region is one `data` partition named `eio_state`, located **by name** through the target's own partition table and never by an offset compiled into the image, at least 16 KB, holding `eio:state`'s keys and nothing else; identity and configuration are in the image (§6, §6.4.3), so there is no third region and nothing writable holds anything a build decided. §5.3 answers what an update does: a flash replaces the image and nothing else, so state survives one; state under a `(service, instance)` key the new graph has no instance for is left exactly where it is, and only an explicit erase removes it — the flash tool standing in for the `DELETE /state/orphans/{namespace}` a leaf has no API to offer, coarser by one namespace and stated as a cost; and an instance that survives keeps its namespace through a changed block and through changed properties, with the encoding-migration obligation landing on the block author where ABI §7.2 already put it. §5.1 names the constant service component — the service file's `name` — which is what makes the daemon key-layout parity §5 claims checkable rather than merely shaped. Deliberately still not settled: the **wear budget policy** behind a refusal, which stays **OPEN** in SCOPE §3.7, and the region's own record format, which is eieio-x7g.2.14's to write under it.

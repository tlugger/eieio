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

- **Adds:** an engine binding (§3), a `StateStore` against flash (§5), a `Timers` implementation against a hardware timer, a transport client (§8), and a generated `main` that constructs the baked graph (§6).
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

So a leaf's budget is a **watchdog**, not fuel: a hardware timer armed before entering a guest callback and disarmed on return, whose expiry kills the instance exactly as ABI §8 requires of a deadline violation. This is the leaf runtime's to add rather than the interpreter's to provide, which is why the conformance harness lets a host answer `enforces_budgets = false` and have `07_budget_exhausted` skipped by name — a binding without a watchdog is honest about it rather than hanging. **§4.4 fixes which timer and how long, §4.5 what an engine binding must expose for any of it to work, and §4.6 how a kill is reported on a node with no log stream.**

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
|Guest linear memory|2 × 64 KiB|One WASM page is 64 KiB and a module cannot declare less. This is the dominant per-instance cost and there is no lever on it below one page.|
|Engine execution stack|2 × 8 KiB|The stack WAMR is instantiated with, per instance. 8 KiB is the engine's own configuration and the figure §11's binding confirms.|
|Signal working set|48 KiB|One decoded batch in flight, the bounded emission queue, and one mailbox slot per connection (DAEMON §6.2). **Shared, not per-instance**: a leaf runs one callback at a time, so only the running instance's batch is live.|
|**Total**|**192 KiB**||

Outside the heap and additional to it: §4.1's **32 KiB native stack reserve**, and whatever `.data`/`.bss` the image and WAMR's runtime globals need, which is measured from the linked image rather than chosen. 192 + 32 = 224 KiB of 313 KiB, leaving ≈ 89 KiB for statics and the loaded modules.

**So a v1 leaf image sizes for two block instances, and that is the headline rather than a footnote.** Three fit only if the linked image leaves the heap more than the floor, which the build can compute and this document cannot. The lever, if two is not enough, is the 64 KiB page — not the working set.

**The measurement that should worry an implementer most: every golden block declares 17 pages.** Built as `examples/blocks/` builds today, all five of ABI §13.2's golden blocks declare a minimum linear memory of **17 pages, 1088 KiB** — three and a half times the whole chip. The cause is not the blocks: `wasm-ld` defaults the shadow stack to 1 MiB, and `RUSTFLAGS="-C link-arg=-zstack-size=16384"` brings all five to **1 page, 64 KiB** with no source change and no measurable size difference. Two consequences:

- **A leaf MUST refuse, at firmware build time, a module whose declared minimum linear memory exceeds its per-instance page budget** — one page for v1. This is the same class of check as ABI §4.3's load-time cross-check and belongs in the same place, where a refusal costs a build rather than a field failure.
- **Making the golden blocks pass that check is a change to how `cargo eio build` links, not to any block.** SDK §5.2 owns the default; it is filed rather than made here, because a link flag that changes every published module is a decision that belongs to the SDK's spec and not to a memory budget.

**`max_payload` is 4096 and `max_batch` is 8** for the v1 target. ABI §9.7 makes both host configuration with no floor and SCOPE §3 keeps the *question of a floor* OPEN; supplying values is what §4 already said a leaf does, and this is that.

- **4096 is EXPR §9's `MAX_VALUE_BYTES` floor**, and choosing it there is the whole argument: a conforming expression may build a value whose canonical encoding is 4 096 bytes, and a leaf whose `max_payload` were smaller would make a value the language guarantees can be *built* impossible to *emit* — the §4.1 shape of divergence, in a third place. Framing means a batch of exactly one maximal value does not fit, which is the honest cost of not sizing above the floor.
- **8 delivered signals** is a delivery bound only (ABI §9.7 rule 8): a leaf block may still emit a larger batch and the leaf routes it. It is deliberately small because §4.4's deadline is derived from it — `max_batch` is the one number that appears in both budgets, and a leaf that raises it pays in wall-clock time as well as in RAM.

**What §11's bring-up must report back:** the linked image's `.data`/`.bss` and WAMR's runtime globals, which decide whether 89 KiB of headroom is real; the engine execution stack a golden block actually needs, against the 8 KiB assumed; and the expansion factor §4.3 defines, which is measured there on a 64-bit host and is the least certain number in either section.

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
|`emit` whose reservation the heap cannot meet, or which would exceed the callback's emission budget|`ERR_LIMIT` to the emitter.|ABI §6.2's "queue full … is a status code to the emitter, policy is host-defined"; the policy is here|
|`emit` of bytes that are not a canonical batch, or on an undeclared port|`ERR_INVALID_ARG`.|ABI §6.2, rows 1–2|
|A guest pointer that is misaligned, zero-but-nonzero-length, or outside linear memory|The instance is discarded. Not a memory-pressure case at all: the guest has said something untrue about itself.|ABI §9.6|

**So: refuse outbound, drop inbound, never truncate, and never die.** Truncation is the one of the four that is rejected on principle rather than on arithmetic. A Signal is a *batch* (SCOPE §5), and half a batch is a value nobody wrote: a block that emitted eight readings and had five delivered has been told a lie about its own output, in a platform whose entire conformance argument is that two hosts do the same thing. Dying is rejected because ABI §8 reserves death for traps, fuel and deadlines, and running out of room to hold a signal is none of the three.

**The emission budget, because `emit` enqueues.** ABI §6.2 routes after the callback returns, so every batch a callback emits is held — `host-core` holds it *decoded*, as `Emission { port, batch }` — until the callback is over. On a daemon that queue is a `Vec` and grows; on a leaf an unbounded `Vec` inside one callback is the leak this section exists to close. **A leaf bounds the bytes it accepts from `emit` within one callback at `max_payload`**, 4 096 for v1: one payload's worth out for one payload's worth in. Past it, `emit` answers `ERR_LIMIT`, which is a status code and therefore life (ABI §8) — the block sees the refusal, and ABI §10's own advice for a block with more to say is already "long work is chunked via timers".

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

**Granularity is not the timer's resolution, and saying so avoids a false precision.** The systimer behind MWDT counts in microseconds, so the deadline is exact to far better than it needs to be. The real granularity is **how often the engine looks**: an interpreter can only abandon a call at a point where it checks, and §4.5 makes "at least once per loop back-edge and once per call" a requirement on the binding rather than a hope about one. With that requirement met, the overrun past the deadline is bounded by one straight-line instruction sequence, which WASM makes finite by construction — there is no unbounded straight-line code in a module the loader accepted. Without it, stage 1 is the only bound, and the answer is a reset.

### 4.5 What an engine binding MUST expose for the watchdog to work

This is a requirement on **every** engine binding, present and future — including the WAMR interpreter binding of eieio-x7g.2.5 — because §4.4's stage 0 is unimplementable without it, and a binding that cannot meet it makes a leaf's only budget a reset.

1. **A termination entry point callable from outside the running call**, taking the instance or its execution environment and asking the in-progress guest call to stop. This is the one `wasm3x` 0.1.0 does not have — measured before the bring-up was written, and the reason `crates/leaf` answers `enforces_budgets = false` and has `07_budget_exhausted` skipped by name.
2. **It MUST be callable from an interrupt context, or the binding MUST document a safe deferral.** This is the requirement that decides whether the watchdog can be a hardware alarm at all. A termination that takes a lock, allocates, or is otherwise not ISR-safe forces stage 0's ISR to do nothing but set a flag someone else must poll — which is a fine answer, but it has to be *stated*, because the polling interval then becomes the granularity §4.4 attributes to the engine.
3. **The terminated call MUST return to the host as a trap, not as a status code.** ABI §8 is explicit that a deadline violation kills the instance and that a non-zero callback return does not; a binding that unwound a terminated call as an ordinary return would turn a deadline into life and would make `07_budget_exhausted` pass while proving the opposite of what it tests. The host must be able to tell a terminated call apart from a block-level error, and the ABI already has exactly one place for it.
4. **The gap between the request and the return MUST be bounded**, by checking the request at least once per loop back-edge and once per call. §4.4's granularity claim is this requirement and nothing else.
5. **A binding that cannot do 1–4 answers `enforces_budgets = false`** and has the budget scenario skipped by name, rather than hanging or pretending. This is §4's honest-binding rule and it is not a concession: a suite that reports a skip is a suite you can read, and one that reports a pass on an unenforced budget is worse than one that reports nothing.

**Nothing here is WAMR-specific on purpose.** The list is what a *watchdog* needs from an interpreter, so it is the acceptance criterion the interpreter binding is written against rather than a description of what one interpreter happens to offer, and it is what `07_budget_exhausted` stops being skipped on (eieio-x7g.2.13).

### 4.6 Reporting a kill, when there is no log stream

§7 removes the whole of DAEMON §9, and it does so deliberately, so a leaf that kills an instance has nowhere obvious to say so. **The mechanism belongs to §8 and to §11's observability item; what this section fixes is the requirement that mechanism has to meet.** Stating it here rather than designing a topic here is the point: the transport decision (eieio-x7g.2.10) and this one have to agree, and the way to make them agree is for the watchdog to name what it needs rather than to invent a channel for itself.

Three obligations, and the second is the one that is easy to miss:

1. **A kill MUST be recorded, not merely emitted.** The normal condition of a leaf is that nobody is listening — no operator, possibly no broker reachable. A report that exists only as a message sent at the moment of the kill is a report that is usually lost. The record is state that outlives the callback and is readable by whatever §8 publishes, so the fact is still there at the next connect.
2. **A kill MUST survive the reset a kill can cause.** §4.4's stage 1 resets the node, and a fact that the reset erases cannot explain the reset — which is the failure mode most worth avoiding, because a leaf that reboots for an unrecorded reason is indistinguishable from a leaf with a hardware fault. This is why §4.4 chose MWDT over the RTC watchdog: the record goes in RTC-retained memory, which the digital watchdog's system reset does not clear. The chip's own reset-reason register is the backstop for the backstop — even with the record lost, "this node rebooted because of a watchdog" is recoverable from the silicon.
3. **The minimum record**, which is what §11's observability item must carry and what a firmware build must leave room for: the instance id, the callback that overran (ABI §5.1's step, or the export name), the deadline in force, and which stage fired. Which stage is not a detail: stage 0 means one instance died and the graph kept running, and stage 1 means the graph did not.

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
- Observability is the wire protocol's (§8), not an endpoint's. **§4.6 is the first concrete demand on that**: a leaf that kills an instance still has to be diagnosable, so a kill is recorded rather than merely emitted, and it survives the reset §4.4's stage 1 causes. The channel is §11's observability item; the requirement is §4.6's.

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
- ~~**Memory budget**: heap sizing per target, and what a leaf does when a batch will not fit.~~ — **resolved** (eieio-x7g.2.7): §4.2 sizes it and §4.3 answers it. The allocator is `embedded-alloc`'s TLSF heap, one heap shared with the engine; the heap is the linker's remainder against a **192 KiB floor** derived for the v1 target's real 313 KiB of SRAM, which sizes an image for two block instances. `max_payload` is 4 096 (EXPR §9's `MAX_VALUE_BYTES` floor) and `max_batch` is 8. §4.1's decode bound is **resolved as the daemon's constant, defended against a measured stack**: `Value::decode_at` is a 160-byte frame on `riscv32imc-unknown-none-elf`, so host parity costs 15 KiB more stack than the floor would, against a 32 KiB reserve. Two measurements do the work and both are recorded in place: a batch's decoded footprint expands by between 1.19× and **22.1×**, so `max_payload` does not bound host memory and a leaf reserves before it decodes; and every golden block declares **17 pages** of linear memory today, three and a half times the whole chip, which a link flag fixes and which a leaf refuses at firmware build time until it is fixed.
- **The transport client** (§8), once one has been measured.
- ~~**Watchdog mechanics** (§4): which timer, what granularity, and how a killed instance is reported when there is no log stream to report it on.~~ — **resolved** (eieio-x7g.2.8): §4.4 fixes the mechanism, §4.5 the requirement it places on an engine binding, §4.6 the reporting. The timer is TIMG0's digital watchdog **MWDT0 in two stages** — stage 0 interrupts at a 250 ms callback deadline and kills one instance exactly as a daemon would, stage 1 resets the node at 1 s — armed on entry to a guest callback and disarmed on return, and fed across host calls whose duration is the host's rather than the guest's. Granularity is the engine's checking interval and not the timer's resolution, which is why §4.5 makes "at least once per loop back-edge and once per call" a **requirement on every binding**, alongside a termination entry point callable from outside the running call, ISR-safety or a documented deferral, and a return that reaches the host as a trap rather than a status code (ABI §8). `wasm3x` 0.1.0 meets none of it — measured — so `crates/leaf` keeps `enforces_budgets = false` and the skip until a binding does; that is eieio-x7g.2.13, and eieio-x7g.2.5's WAMR interpreter binding is written against §4.5's list. §4.6 states the divergence stage 1 buys and argues for it rather than arriving at it.
- **Observability without an API** (§7): what a leaf publishes about itself, and on which topic. **§4.6 is a requirement on this item and the two answers must agree**: a watchdog kill must be *recorded* rather than merely emitted, because the normal condition of a leaf is that nobody is listening; it must survive the reset that §4.4's stage 1 causes, which is why the record sits in RTC-retained memory and why §4.4 chose MWDT over the RTC watchdog; and it carries the instance id, the callback that overran, the deadline in force and which stage fired. The topic and the encoding are this item's, not §4's.
- ~~**A class-aware CLI**~~ — **resolved** (eieio-x7g.5): it learns the class. A node entry in `nodes.toml` carries an optional `class`, `"daemon"` or `"leaf"`, absent meaning `"daemon"`, and `eio` refuses a leaf by naming the class rather than reporting a failed request. SCOPE §3.7 records the decision and why the two alternatives — requiring the key, or inferring the class from a refused connection — are each worse.
- **Flash layout**: where AOT artifacts, state and configuration sit, and how a firmware update treats existing state.

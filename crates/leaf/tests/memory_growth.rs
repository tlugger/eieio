//! LEAF §4.2's per-instance linear-memory reserve: bounded, and measured (eieio-x7g.2.27).
//!
//! ABI §4.1 says a host's page ceiling bounds **admission, not growth**: `memory.grow` is core
//! WASM, and what bounds growth is the module's declared *maximum* and the engine enforcing it.
//! LEAF §4.2 reserves guest linear memory out of a fixed heap floor, so a leaf is a host that
//! must bound growth — and until this file existed nothing did. A guest could reach 65 536
//! pages as far as either engine was concerned, on a device with no OS to absorb it.
//!
//! §4.1 settles it in two halves, and the halves cover **disjoint** modules rather than being
//! alternatives:
//!
//! 1. a module whose declared minimum or declared **maximum** exceeds the reserve is refused at
//!    firmware build time (`crates/leaf-gen`, measured in `crates/leaf-gen/tests/parity.rs`).
//!    Capping such a module instead would grant it less than it declared, which §4.1 refuses in
//!    as many words — and [`a_declared_maximum_is_silently_shrunk_which_is_why_the_generator_refuses_it`]
//!    measures exactly that happening when an engine is left to do it;
//! 2. a module declaring **no maximum** has said nothing for a loader to refuse, and its growth
//!    is bounded at the engine. That is the case that actually occurs: all five of ABI §13.2's
//!    golden blocks declare `(memory 1)` with nothing on the right, because `wasm-ld` emits no
//!    maximum unless asked and SDK §5.2 deliberately does not ask.
//!
//! # The number, and why it is measured rather than read off a memory section
//!
//! [`the_page_reserve_is_what_the_suite_measures`] is the measurement, and
//! `eio_leaf::V1_MEMORY_PAGES` is **one** page. That is the same number a reading of the golden
//! blocks' memory sections would suggest, and it is emphatically not read from there: a Rust
//! guest's declared minimum is its statics and its shadow stack, and its heap need not be in
//! there at all. This constant read **two** until SDK §4.1 landed, because `dlmalloc` at its
//! default 64 KiB granularity declined the ~38 KiB the linker leaves inside the declared page —
//! its donation of the `__heap_base`..`__heap_end` span fires only when the span is at least one
//! granule — and took its whole heap from `memory.grow` instead. At a one-page bound `counter`
//! failed `eio_configure` with `ERR_LIMIT` before a signal was ever routed. A 4 096-byte
//! granularity is what made the declaration and the footprint agree again, and this file is what
//! would notice if they stopped agreeing.
//!
//! # What a guest observes, in ABI §8's vocabulary: nothing new
//!
//! `memory.grow` answers `-1`. That is core WASM's own result for a growth the engine will not
//! perform — neither a trap nor a status code, and nothing this platform defines. A guest
//! allocator reads it as a failed allocation, and it reaches ABI §9 only where an allocation
//! failure always did: `eio_alloc` returning `0`, which §9.5 already makes `ERR_LIMIT`. §8's
//! death kinds are a closed set and this adds nothing to it. The tests below assert the `-1`
//! specifically, because a trap there would be that fourth death kind.
//!
//! # The engine gap this file also measures
//!
//! **wasm3 has no growth bound, and this asserts that it does not** — the shape LEAF §4 already
//! uses for WAMR's compiled-out instruction counter: a gap recorded as a passing assertion
//! about what the engine does, so that the day it changes the suite says so. See
//! [`wasm3_has_no_growth_bound_and_that_is_the_gap`] for where the fix belongs.

use std::sync::atomic::{AtomicU32, Ordering};

use eio_conformance::{Budget, Host, HostError, Outcome, suite};
use eio_host_core::Engine;
use eio_leaf::{V1_MEMORY_PAGES, wamr};
use eio_manifest::Capability;

// ── the bound, on one module, both ways ──────────────────────────────────────

/// A module that declares one page, no maximum, and reports what `memory.grow` said.
///
/// The shape of every block `cargo eio build` produces, reduced to the two facts this file is
/// about. `grow_one` answers `memory.grow`'s own result — the previous page count on success,
/// `-1` on refusal — and `pages` answers `memory.size` afterwards, so a bound enforced by lying
/// about the size rather than by refusing the growth would fail here too.
const NO_MAXIMUM: &str = r#"(module
  (memory (export "memory") 1)
  (func (export "grow_one") (result i32) (memory.grow (i32.const 1)))
  (func (export "pages") (result i32) (memory.size))
)"#;

/// The same module, declaring a maximum of four pages — over the reserve, so
/// `crates/leaf-gen` refuses it. Here to measure *why* it is refused rather than capped.
const MAXIMUM_OF_FOUR: &str = r#"(module
  (memory (export "memory") 1 4)
  (func (export "grow_one") (result i32) (memory.grow (i32.const 1)))
  (func (export "pages") (result i32) (memory.size))
)"#;

fn assemble(wat: &str) -> Vec<u8> {
    wat::parse_str(wat).expect("the fixture assembles")
}

/// Calls a zero-argument `i32` export, failing the test on a trap.
///
/// A trap here would be the interesting result rather than a plumbing failure: ABI §8's death
/// kinds are closed and "grew too far" is not one of them, so a bound that trapped would be a
/// conformance bug. Hence the message.
fn call<E: Engine>(guest: &mut E, export: &str) -> i32 {
    guest.call(export, &[]).unwrap_or_else(|trap| {
        panic!("{export} trapped: {trap} — ABI §8 has no death kind for a refused `memory.grow`")
    })
}

/// Grows `guest` a page at a time until it is refused, answering the page count it reached.
fn grow_until_refused<E: Engine>(guest: &mut E) -> i32 {
    for _ in 0..64 {
        if call(guest, "grow_one") == -1 {
            return call(guest, "pages");
        }
    }
    panic!("nothing refused the growth in 64 pages, 4 MiB — the bound is not in force");
}

/// Without a bound, a module declaring one page walks away from it. That is the defect.
///
/// The control for every other test here: if this stops holding, the bound below is measuring
/// nothing. 64 pages is not the engine's limit, only this test's patience — WAMR's is 65 536.
#[test]
fn without_a_bound_a_one_page_module_grows_far_past_its_page() {
    let wasm = assemble(NO_MAXIMUM);
    let mut guest = wamr::instantiate_with(&wasm, wamr::EXEC_STACK_SIZE, None)
        .expect("the fixture instantiates");

    for expected in 1..=8 {
        assert_eq!(
            call(&mut guest, "grow_one"),
            expected,
            "an unbounded `memory.grow` answers the previous page count and performs the growth"
        );
    }
    assert_eq!(
        call(&mut guest, "pages"),
        9,
        "nine pages, 576 KiB — nearly twice LEAF §4.2's whole target chip, from a module that \
         declared one page"
    );
}

/// With LEAF §4.2's reserve, the same module stops at the reserve and is told so.
///
/// The whole of half 2. Note what is asserted: `-1`, not a trap, and a page count that stopped
/// exactly where the reserve is — a bound that reported success and then lied about the size
/// would be worse than none.
#[test]
fn the_leafs_reserve_refuses_the_growth_and_says_so_in_core_wasms_own_words() {
    let wasm = assemble(NO_MAXIMUM);
    let mut guest = wamr::instantiate_with(&wasm, wamr::EXEC_STACK_SIZE, Some(V1_MEMORY_PAGES))
        .expect("the fixture instantiates");

    assert_eq!(
        grow_until_refused(&mut guest) as u32,
        V1_MEMORY_PAGES,
        "the instance stops at the pages LEAF §4.2 reserved it, and `memory.grow` answers -1 \
         rather than trapping — core WASM's own result, which reaches ABI §9 only as \
         `eio_alloc` returning 0 and §9.5's ERR_LIMIT"
    );
}

/// The leaf's production entry point carries the bound, not just the measurement seam.
///
/// `crates/leaf/src/lib.rs`'s `spawn` calls `wamr::instantiate` and nothing else, so this is
/// the assertion that a real graph gets it. Written separately from the test above because a
/// seam that holds while the default does not is exactly the defect eieio-x7g.2.24 found one
/// field over, in the execution stack.
#[test]
fn the_default_instantiate_is_the_bounded_one() {
    let wasm = assemble(NO_MAXIMUM);
    let mut guest = wamr::instantiate(&wasm).expect("the fixture instantiates");

    assert_eq!(grow_until_refused(&mut guest) as u32, V1_MEMORY_PAGES);
}

/// A module that declares a maximum over the reserve is *silently shrunk* by the engine —
/// which is precisely why `crates/leaf-gen` refuses it at build time instead.
///
/// This is half 1's justification, measured. WAMR's `wasm_runtime_get_max_mem` takes the
/// smaller of the host's number and the module's own, so a module that declared it may grow to
/// four pages is given the reserve and finds out at whatever allocation first crosses a line it was
/// never told about. ABI §4.1 names that outcome and forbids it: a host MUST NOT grant less
/// than the module declared. So the engine bound cannot be the whole answer, and the generator
/// refusal is not belt-and-braces — it is the only place this module can be dealt with
/// honestly, and the only one that produces a message a deployer can act on.
#[test]
fn a_declared_maximum_is_silently_shrunk_which_is_why_the_generator_refuses_it() {
    let wasm = assemble(MAXIMUM_OF_FOUR);
    let mut guest = wamr::instantiate(&wasm).expect("the fixture instantiates");

    assert_eq!(
        grow_until_refused(&mut guest) as u32,
        V1_MEMORY_PAGES,
        "the engine capped a module that declared four pages at the host's reserve, with no \
         diagnostic anywhere — the refusal a deployer can act on is `eio_leaf_gen`'s, at the \
         firmware build (LEAF §4.2)"
    );
}

/// A bound never grants an instance less than its declared **minimum**.
///
/// The other half of §4.1's second bullet, and the engine enforces it rather than this crate:
/// `wasm_runtime_get_max_mem` refuses to override below a module's initial page count. A module
/// declaring more than the reserve is refused by the loader long before it reaches an engine
/// (`34_memory_ceiling`), so this measures the belt behind that braces — if the loader check
/// were ever lost, the engine would still not hand out a crippled instance.
#[test]
fn a_bound_below_a_modules_declared_minimum_is_not_applied() {
    let wasm = assemble(
        r#"(module
             (memory (export "memory") 3)
             (func (export "pages") (result i32) (memory.size))
           )"#,
    );
    let mut guest = wamr::instantiate(&wasm).expect("the fixture instantiates");

    assert_eq!(
        call(&mut guest, "pages"),
        3,
        "the instance got what it declared; a host that granted less would fail it at an \
         allocation the guest was never told about (ABI §4.1)"
    );
}

/// Zero pages is refused rather than read as WAMR's "no bound at all".
///
/// The same defect as `InstantiateError::ZeroStack` one field over: WAMR reads a zero
/// `max_memory_pages` as "the caller is not overriding anything", the exact opposite of what a
/// caller writing `Some(0)` means. A silently unbounded leaf is the failure this whole file
/// exists to prevent, so it is refused loudly.
#[test]
fn a_bound_of_zero_pages_is_refused_rather_than_read_as_no_bound() {
    let wasm = assemble(NO_MAXIMUM);
    let Err(error) = wamr::instantiate_with(&wasm, wamr::EXEC_STACK_SIZE, Some(0)) else {
        panic!("0 pages is not a bound, and instantiating under one has to say so");
    };

    assert!(
        error.contains("no bound at all"),
        "the refusal has to say why zero is not the number the caller meant: {error}"
    );
}

/// **wasm3 has no growth bound, and this records the gap rather than hiding it.**
///
/// Measured, not assumed: wasm3's only linear-memory ceiling is `d_m3MaxLinearMemoryPages`, a
/// compile-time define of the published `wasm3x-sys` crate (65 536, `source/m3_config.h`), and
/// its only per-runtime knob is `M3Runtime::memoryLimit` — internal to wasm3, exposed by
/// neither `wasm3.h` nor `wasm3x`, and in any case a clamp on *bytes* that leaves the page
/// count alone, which would be worse than no bound at all.
///
/// So a wasm3 leaf is bounded only by half 1 (the generator) and by the heap itself: growth
/// past what the one heap can supply fails the engine's `realloc`, and wasm3 answers that the
/// same way it answers a declared maximum — `ResizeMemory` returns `m3Err_wasmMemoryOverflow`
/// and `op_MemGrow` pushes `-1` (`source/m3_exec.h`). Not a trap, so the guest-visible
/// behaviour is identical; what is missing is the *isolation*, which is one instance's ability
/// to eat the reserve every other instance was budgeted out of.
///
/// **Where the fix has to happen is a firmware build, not this one.** LEAF §3.2 makes the
/// engine a per-image choice and a firmware build compiles wasm3's C sources itself, so it can
/// define `d_m3MaxLinearMemoryPages` as §4.2's reserve — a global compile-time constant is
/// exactly the right shape for a leaf, which has one number. This host build links a published
/// crate and cannot. LEAF §4.2 and §11 record it; this test is the notification if `wasm3x`
/// ever grows the knob.
#[test]
fn wasm3_has_no_growth_bound_and_that_is_the_gap() {
    let wasm = assemble(NO_MAXIMUM);
    let mut guest = eio_leaf::wasm3::instantiate(&wasm).expect("the fixture instantiates");

    for expected in 1..=8 {
        assert_eq!(
            call(&mut guest, "grow_one"),
            expected,
            "wasm3 grew past LEAF §4.2's reserve. If this ever fails, wasm3x has acquired a \
             memory limit and §4.2's engine gap can be closed — say so there rather than here"
        );
    }
}

// ── the measurement the reserve is derived from ──────────────────────────────

/// The page bound [`BisectHost`] instantiates under, for one scenario at a time.
///
/// A static for `Host::instantiate`'s reason in `tests/exec_stack.rs`: the trait method takes
/// the module and the budget, and a per-run number has nowhere else to travel.
static CANDIDATE: AtomicU32 = AtomicU32::new(V1_MEMORY_PAGES);

/// The most pages this measurement will try before declaring a scenario unbounded.
///
/// Small on purpose. A page is 64 KiB and LEAF §4.2's whole heap floor is 192; a scenario
/// needing eight would not be a number to record, it would be a finding that the reserve
/// cannot be met at all.
const MAX_PROBE: u32 = 8;

/// The conformance suite as a growth-bound measurement rig — **deliberately not
/// `tests/conformance.rs`'s `LeafHost`**, and for `tests/exec_stack.rs`'s reason.
///
/// It differs in [`Host::capabilities`], which answers all five namespaces rather than the two
/// this crate has host functions for. How much linear memory a block needs is a property of the
/// *block and its allocator*, not of which host functions someone has written yet, so declaring
/// all five measures the two golden blocks and the hand-written harnesses a leaf cannot host
/// today. Measuring what a leaf will need before it needs it is the whole reason a budget is a
/// reserve.
struct BisectHost;

impl Host for BisectHost {
    type Guest = wamr::Guest;

    fn name(&self) -> &str {
        "eio-leaf (wamr, measuring the linear-memory reserve)"
    }

    fn capabilities(&self) -> &[Capability] {
        &[
            Capability::State,
            Capability::Timer,
            Capability::Gpio,
            Capability::I2c,
            Capability::Http,
        ]
    }

    fn enforces_budgets(&self) -> bool {
        false
    }

    fn names_refusals(&self) -> bool {
        false
    }

    fn instantiate(&mut self, wasm: &[u8], _budget: Budget) -> Result<wamr::Guest, HostError> {
        wamr::instantiate_with(
            wasm,
            wamr::EXEC_STACK_SIZE,
            Some(CANDIDATE.load(Ordering::Relaxed)),
        )
        .map_err(HostError::Refused)
    }
}

/// Every scenario in the suite, in the order `suite::run_dir` would run them.
fn scenarios() -> Vec<std::path::PathBuf> {
    eio_conformance::golden::build();
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(suite::scenarios_dir())
        .expect("the scenario directory is readable")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "the suite holds no scenarios");
    paths
}

/// The smallest page bound at which `path` still holds, or `None` if this host cannot reach it.
///
/// Scanned upwards rather than bisected: the answer is a small integer and the honest way to
/// find the smallest one is to try them in order. [`Outcome::Skipped`] counts as unreachable,
/// the same way `tests/exec_stack.rs` treats it — a scenario this host cannot run says nothing
/// about how much memory it would have needed.
fn minimum_for(path: &std::path::Path) -> Option<u32> {
    for pages in 1..=MAX_PROBE {
        CANDIDATE.store(pages, Ordering::Relaxed);
        let loaded = suite::load(path).expect("the scenario loads");
        let report = eio_conformance::run(&loaded, &mut BisectHost);
        if matches!(report.outcome, Outcome::Skipped(_)) {
            return None;
        }
        if report.ok() {
            return Some(pages);
        }
    }
    panic!(
        "{} does not hold at {MAX_PROBE} pages, {} KiB — that is not a reserve to record, it is \
         a block LEAF §4.2's heap floor cannot host",
        path.display(),
        MAX_PROBE * 64
    );
}

/// The headline: what every scenario actually needs, against §4.2's reserve.
///
/// **This is where `V1_MEMORY_PAGES` comes from**, and the reason it is taken here rather than
/// read off a golden block's memory section is that the two once disagreed. `wasm-ld` puts a
/// block's statics and shadow stack in the declared minimum and nothing else, and an allocator
/// that will not use the remainder inside that page takes its whole heap from `memory.grow` —
/// which is what `dlmalloc` did at its default granularity, and why this table read two for
/// every SDK-built block. SDK §4.1's 4 096-byte granularity is what brought it back to one. The
/// table below is the current state of that, per scenario.
///
/// Two assertions, and the second is the one that keeps the number honest:
///
/// - nothing needs **more** than the reserve, or a leaf built to §4.2 cannot run the suite;
/// - something needs **exactly** it, or the reserve is slack nobody measured and the next
///   person to read §4.2 will not know how much of it is real.
///
/// Printed always. The numbers are the finding, and a run that only said "ok" would leave the
/// next person to re-take the measurement to learn anything from it. Unlike
/// `tests/exec_stack.rs`'s stack bisection, these numbers are **not** a property of this host:
/// linear memory is the guest's own address space, so a 32-bit target sees the same pages,
/// which makes this one of the few rows in §4.2 the MCU bring-up need not re-take.
#[test]
fn the_page_reserve_is_what_the_suite_measures() {
    let mut worst = 0;
    let mut reached = 0;

    println!("pages  scenario");
    for path in scenarios() {
        let name = path.file_name().expect("a file name").to_string_lossy();
        match minimum_for(&path) {
            Some(pages) => {
                println!("{pages:>5}  {name}");
                worst = worst.max(pages);
                reached += 1;
            }
            None => println!("    -  {name} (skipped: this host cannot reach it)"),
        }
    }
    println!(
        "worst {worst} page(s), {} KiB, over {reached} scenario(s); LEAF §4.2 reserves \
         {V1_MEMORY_PAGES}",
        worst * 64
    );

    assert!(
        worst <= V1_MEMORY_PAGES,
        "a scenario needs {worst} page(s) and LEAF §4.2 reserves {V1_MEMORY_PAGES} — a leaf \
         built to that reserve cannot run its own conformance suite"
    );
    assert_eq!(
        worst, V1_MEMORY_PAGES,
        "no scenario needs the whole reserve, so {V1_MEMORY_PAGES} page(s) is slack nobody \
         measured. Lower it in `crates/leaf` and say in LEAF §4.2 what changed"
    );

    // Restored so a `--test-threads=1` run of this file leaves the static where the rest of it
    // expects it, and so the constant is what a reader of the table above sees in force.
    CANDIDATE.store(V1_MEMORY_PAGES, Ordering::Relaxed);
}

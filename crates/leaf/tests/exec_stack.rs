//! **The measurement behind `eio_leaf::wamr::EXEC_STACK_SIZE`** — LEAF-SPEC §4.2's per-instance
//! engine execution stack, bisected against the suite that has to keep running on it
//! (eieio-x7g.2.24).
//!
//! # Why a test and not a number in a comment
//!
//! §4.2 budgets the v1 target's 192 KiB heap floor as 2 × (64 KiB linear memory + **8 KiB
//! engine execution stack**) + a 48 KiB shared working set, and lists "the engine execution
//! stack a golden block actually needs, against the 8 KiB assumed" among the things §11's MCU
//! bring-up has to report back. Until that hardware exists, the honest way to answer it is to
//! ask the interpreter: run every ABI §13 scenario WAMR can execute at all, shrink the stack
//! until each stops passing, and report where that happened. That is what this file does, on
//! every `just ci`, so the answer cannot quietly stop being true when a golden block, the SDK's
//! link flags or WAMR's version changes — which is exactly how the 8 MiB this replaced
//! survived: it was a desktop harness's default, copied once, never re-asked.
//!
//! # What is actually being measured, and what it is not
//!
//! Not "the frame depth" in the sense of a count of frames. WAMR's
//! `wasm_exec_env_alloc_wasm_frame` (`core/iwasm/common/wasm_exec_env.h`) bumps a pointer by a
//! per-function frame size — locals plus operand stack cells, computed at load time — and
//! refuses when `size * 2` exceeds what is left, so what a callback needs is the deepest sum
//! of frame sizes along any path it takes, plus that doubling of the last frame as headroom.
//! Bytes are therefore the only unit in which the question has one answer, and bytes are the
//! unit `wasm_runtime_create_exec_env` takes. A number here is a *whole call chain*: ABI §5.1's
//! export, everything the block calls, and — since `emit`, `prop` and `state_get` return into
//! the guest — everything it calls after a host function returns.
//!
//! Nor is it a per-instance total for a graph: this is one instance's stack, and a leaf running
//! two instances pays it twice, which is precisely how §4.2 counts it.
//!
//! # The caveat this test cannot remove
//!
//! A handful of scenarios expect a *refusal* or a *trap*, and a stack too small to run anything
//! is a way to get one. So the bisection's lower edge could in principle be one scenario
//! passing for the wrong reason, which would make a measured minimum look smaller than it is.
//! That is harmless here and worth saying why: nothing ships at the measured minimum.
//! `EXEC_STACK_SIZE` is §4.2's 8 KiB, the spec's number, and the measurement's job is to show
//! the margin is real — an error in the direction of "the true minimum is larger than measured"
//! is an error in the direction the margin already covers.

use std::sync::atomic::{AtomicU32, Ordering};

use eio_conformance::{Budget, Host, HostError, Outcome, suite};
use eio_leaf::wamr;
use eio_manifest::Capability;

/// Frames are a multiple of four bytes, so there is nothing finer for the bisection to find.
const STEP: u32 = 4;

/// The stack size [`BisectHost::instantiate`] will ask for next.
///
/// A `static` rather than a field because `Host` is driven by the harness, not by this file: it
/// calls `instantiate` itself, once per scenario, so the size has to reach it out of band. The
/// bisection below is single-threaded within one `#[test]`, and every run it drives takes
/// `wamr`'s own process-global lock anyway.
static CANDIDATE: AtomicU32 = AtomicU32::new(0);

/// A WAMR host that reads its execution stack from [`CANDIDATE`] — **a measurement rig, and
/// deliberately not `tests/conformance.rs`'s `LeafHost`.**
///
/// It differs in one answer, [`Host::capabilities`], and the difference is the point. The leaf
/// declares only `state` and `timer` because those are the two it has host functions for, so
/// three scenarios — `gpio`, `i2c`, `http` — are skipped there and one of ABI §13.2's five
/// golden blocks (`gpio-echo`) never runs. But how much engine stack a block needs is a
/// property of the *block and the interpreter*, not of which host functions someone has
/// written: the harness answers all five namespaces with its own generic implementation
/// (`crates/conformance/src/capability.rs`), so declaring them here measures the two golden
/// blocks and the two hand-written harnesses that the leaf cannot yet host. Measuring what a
/// leaf will need before it needs it is the whole reason a budget is a reserve.
///
/// Everything else matches `LeafHost`, and `enforces_budgets` in particular: no engine here has
/// a usable fuel counter, so `07_budget_exhausted` is skipped in both places for the same
/// reason.
struct BisectHost;

impl Host for BisectHost {
    type Guest = wamr::Guest;

    fn name(&self) -> &str {
        "eio-leaf (wamr, bisecting the engine execution stack)"
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
        wamr::instantiate_with_stack(wasm, CANDIDATE.load(Ordering::Relaxed))
            .map_err(HostError::Refused)
    }
}

/// Every scenario in this repository's suite, in the order `suite::run_dir` would run them.
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

/// Runs one scenario at `stack` bytes and says whether it held.
///
/// [`Outcome::Skipped`] counts as held, the same way `Report::ok` does: a scenario this host
/// cannot reach says nothing about how much stack it would have needed.
fn holds_at(path: &std::path::Path, stack: u32) -> bool {
    CANDIDATE.store(stack, Ordering::Relaxed);
    let loaded = suite::load(path).expect("the scenario loads");
    eio_conformance::run(&loaded, &mut BisectHost).ok()
}

/// Whether a scenario reaches this host at all, at a stack size that certainly suffices.
fn reached(path: &std::path::Path) -> bool {
    CANDIDATE.store(wamr::EXEC_STACK_SIZE, Ordering::Relaxed);
    let loaded = suite::load(path).expect("the scenario loads");
    !matches!(
        eio_conformance::run(&loaded, &mut BisectHost).outcome,
        Outcome::Skipped(_)
    )
}

/// The smallest stack, to [`STEP`] bytes, at which `path` still holds.
///
/// Bisection assumes monotonicity — that a scenario passing at *n* bytes passes at *n + 4* —
/// which is a property of how `wasm_exec_env_alloc_wasm_frame` fails: it refuses a frame when
/// there is not room for it, never because there is too much room.
fn minimum_for(path: &std::path::Path) -> u32 {
    assert!(
        holds_at(path, wamr::EXEC_STACK_SIZE),
        "{} does not hold at EXEC_STACK_SIZE ({} bytes) — bisecting below it is meaningless",
        path.display(),
        wamr::EXEC_STACK_SIZE
    );
    // `instantiate_with_stack` refuses zero (WAMR reads it as its own default), so the
    // known-bad edge starts at the smallest size it accepts.
    let mut bad = STEP;
    let mut good = wamr::EXEC_STACK_SIZE;
    if holds_at(path, bad) {
        return bad;
    }
    while good - bad > STEP {
        let mid = (bad + (good - bad) / 2) / STEP * STEP;
        let mid = if mid <= bad { bad + STEP } else { mid };
        if holds_at(path, mid) {
            good = mid;
        } else {
            bad = mid;
        }
    }
    good
}

/// The headline: every scenario's own minimum, and the suite's, against §4.2's reserve.
///
/// Prints the table always. The numbers are the finding — a run that only said "ok" would leave
/// the next person to re-derive them, which is how this constant went wrong the first time.
#[test]
fn the_engine_execution_stack_leaf_spec_4_2_reserves_is_measured_not_assumed() {
    let mut worst: Option<(String, std::path::PathBuf, u32)> = None;
    for path in scenarios() {
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("?")
            .to_string();
        if !reached(&path) {
            println!("{name:<40} skipped on this host");
            continue;
        }
        let minimum = minimum_for(&path);
        println!(
            "{name:<40} {minimum:>6} bytes ({:.1}% of LEAF §4.2's {} B reserve)",
            100.0 * f64::from(minimum) / f64::from(wamr::EXEC_STACK_SIZE),
            wamr::EXEC_STACK_SIZE
        );
        if worst.as_ref().is_none_or(|(_, _, bytes)| minimum > *bytes) {
            worst = Some((name, path, minimum));
        }
    }

    let (scenario, path, measured) = worst.expect("at least one scenario reaches this host");
    println!(
        "\nsuite minimum: {measured} bytes, set by {scenario}; EXEC_STACK_SIZE is {} bytes, a \
         margin of {:.1}x",
        wamr::EXEC_STACK_SIZE,
        f64::from(wamr::EXEC_STACK_SIZE) / f64::from(measured)
    );

    assert_eq!(
        wamr::EXEC_STACK_SIZE,
        8 * 1024,
        "LEAF §4.2's memory-budget table reserves 8 KiB of engine execution stack per \
         instance. Changing this constant is changing that table, so change the table in the \
         same commit — and say what was measured, as this test does."
    );
    assert!(
        measured < wamr::EXEC_STACK_SIZE,
        "the suite needs {measured} bytes of engine stack and LEAF §4.2 reserves {} — the \
         reserve is no longer a reserve. That is §4.2's own 'what §11's bring-up must report \
         back' item coming due: amend the table with this number rather than raising the \
         constant past a spec that still says 8 KiB.",
        wamr::EXEC_STACK_SIZE
    );

    // **The bisection found a real edge, not the bottom of its own search range.** Without
    // this, a suite that passed on four bytes of stack — because every scenario in it had
    // quietly become one that expects a refusal, say — would report a minimum of four and a
    // magnificent margin. Asserting that one step *below* the worst scenario's minimum really
    // does fail is what makes the number above a measurement.
    assert!(
        measured > STEP,
        "every scenario held on {STEP} bytes of engine execution stack, which is not credible \
         — this is measuring something other than what it runs"
    );
    assert!(
        !holds_at(&path, measured - STEP),
        "{} held on {} bytes but was bisected to {measured} — the bisection is not finding an \
         edge",
        path.display(),
        measured - STEP
    );
}

/// None of ABI §13.2's golden blocks exports a post-instantiate function.
///
/// `wasm_runtime_instantiate`'s second argument is WAMR's `default_wasm_stack_size`, and the
/// one place this build reads it is `execute_post_instantiate_functions`, which creates a
/// temporary `exec_env` of that size for a module's start section or its
/// `__post_instantiate`/`__wasm_call_ctors`/`_initialize` export. `instantiate_with_stack`
/// passes the same size there as it does to the instance's own `exec_env`, so nothing depends
/// on this — but the claim in that call site's comment, that today's fixtures never take the
/// path, should be checked rather than asserted, and this is one line per block.
#[test]
fn no_golden_block_takes_wamrs_post_instantiate_path() {
    for block in ["transform", "filter", "counter", "emitter", "gpio-echo"] {
        let wasm = eio_leaf::fixtures::wasm(block);
        let module = eio_manifest::Module::read(&wasm).expect("a golden block is a valid module");
        for name in [
            "__post_instantiate",
            "__wasm_call_ctors",
            "_initialize",
            "_start",
        ] {
            assert!(
                module.export(name).is_none(),
                "{block} exports {name}, so WAMR runs it at instantiate time on an `exec_env` \
                 of its own — see `instantiate_with_stack`'s comment on that argument"
            );
        }
    }
}

//! The wasmtime binding (DAEMON-SPEC §5.1).
//!
//! Everything in this file exists so that nothing outside it has to know wasmtime is the
//! engine. `eio_host_core` drives a guest through the [`Engine`] trait, and this module is
//! the only implementation of that trait the daemon has; a daemon that reached past it
//! would be writing a second host, which is the divergence with the leaf runtime that the
//! shared crate exists to prevent (DAEMON §1).
//!
//! # The dispatch table, and why it is one
//!
//! wasmtime's [`Linker`] wants host functions *before* instantiation, and it wants each of
//! them to be `Send + Sync + 'static`. `eio_host_core`'s [`HostFn`] is neither: it is a
//! boxed `FnMut` closing over `Rc`-shared state, because ABI §1.2 gives an instance one
//! caller at a time and nothing here needs atomics. The two are reconciled by never putting
//! a `HostFn` in the linker at all:
//!
//! - The linker defines all seven `eio:core` functions and all three `eio:state` ones once,
//!   with ABI §7.0's and §7.2's exact signatures. Each definition is a closure capturing an
//!   [`Import`] — a plain enum — so it is trivially `Send + Sync`.
//! - The real [`HostFn`]s live in the store's data, and `register` puts them there. The
//!   store is per-instance and never leaves its thread, so nothing about it needs to be
//!   `Send`.
//!
//! The useful consequence is that [`Engine::register`] works *after* instantiation, which
//! is the order `eio_host_core` expects: a caller builds an instance, registers the
//! functions its capabilities call for, and hands the whole thing to
//! [`Configured::configure`](eio_host_core::Configured::configure).
//!
//! Import signatures are therefore checked by the engine at link time, which is exactly
//! where ABI §4.3 puts them: the `manifest` cross-check is a superset "in namespaces and
//! names only", and a module importing `eio:core` `log` with the wrong arity fails to
//! instantiate.
//!
//! # Both budgets of ABI §10, armed on every guest entry
//!
//! A callback's budget is refreshed inside [`Engine::call`] rather than by the lifecycle
//! driver, for the reason the dispatch table exists: the driver is `eio_host_core`'s and
//! knows nothing about fuel. `call` is the one place every guest entry passes through, so
//! arming it there is exhaustive by construction — including `eio_alloc`, which is a guest
//! call like any other and is just as capable of spinning.
//!
//! Both budgets, not one, because they measure different things and DAEMON §5.1's trap
//! table already names both: **fuel** bounds *work* and is deterministic, so the same block
//! given the same batch dies at the same instruction on every run rather than only on a busy
//! machine; **epoch interruption** bounds *wall-clock time*, which is what an operator
//! actually promised. A guest blocked in a host function burns no fuel at all, so fuel alone
//! would leave that case unbounded.
//!
//! Epoch interruption needs someone to advance the epoch, so a [`Runtime`] owns a ticker
//! thread that does — one per engine, not one per instance. It holds a *weak* handle, so the
//! last [`Runtime`] dropping is what ends it; nothing has to remember to shut it down.
//!
//! # Core WASM MVP, and nothing past it
//!
//! ABI §4.3 puts MVP conformance here and nowhere else — `eio_manifest` deliberately does no
//! feature gating — so [`MVP`] is the *only* thing standing between a block that uses a
//! post-MVP proposal and a leaf runtime that will refuse it at flash time.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;

use eio_host_core::exports::{core_fn, namespace, state_fn};
use eio_host_core::{
    Arg, Engine, EngineError, ExprBudgets, HostCall, HostFn, Memory, Ret, Trap, TrapKind,
    memory_range,
};
use eio_manifest::MEMORY_EXPORT;
use wasmtime::{Caller, Config, Extern, Func, Linker, Module, Store, Val, WasmFeatures};

/// What a host function with no handler answers.
///
/// A missing or wrongly-shaped handler is a *host* bug — registration happens before the
/// guest runs — so the guest is told the truth about this host rather than being given a
/// plausible number: ABI §8's `ERR_UNSUPPORTED` is "a valid call, unimplemented on this
/// host", which is precisely the situation.
const UNIMPLEMENTED: i32 = eio_host_core::ErrorCode::Unsupported.as_i32();

/// The WebAssembly this host accepts (ABI §1.1, §4.3).
///
/// wasmparser's `MVP` set, less `GC_TYPES`, plus the six proposals the guest toolchain emits
/// by default for `wasm32-unknown-unknown` — every one of which wasm3 executes.
///
/// This was strict MVP until it was measured. ABI §1.1 restricted blocks to core MVP on the
/// grounds that the leaf interpreter admits nothing else; `crates/conformance/tests/wasm3.rs`
/// runs each of these instructions on wasm3 and checks the value it produces, and runs a
/// stock-built Rust block through ABI §5.1's whole lifecycle there. The restriction was
/// protecting a constraint the protected engine does not have, while making a loadable Rust
/// block impossible — `alloc::string::String::clone` in the precompiled `rust-std` contains a
/// `memory.copy`, and no `RUSTFLAGS` rebuilds that.
///
/// `GC_TYPES` still goes: it gates the `externref`/`anyref` *types* rather than a proposal, and
/// a wasmtime built without the `gc` cargo feature refuses to build an engine while it is set.
/// `REFERENCE_TYPES` here is the `call_indirect` *encoding* rustc emits, not a guest using
/// `externref` — which the type flag being absent is what guarantees.
const ACCEPTED: WasmFeatures = WasmFeatures::MVP
    .difference(WasmFeatures::GC_TYPES)
    .union(WasmFeatures::BULK_MEMORY)
    .union(WasmFeatures::REFERENCE_TYPES)
    .union(WasmFeatures::SIGN_EXTENSION)
    .union(WasmFeatures::MULTI_VALUE)
    .union(WasmFeatures::SATURATING_FLOAT_TO_INT)
    .union(WasmFeatures::MUTABLE_GLOBAL);

/// How often the epoch ticker advances the engine's epoch.
///
/// The resolution of every wall-clock deadline. A deadline is rounded *up* to whole ticks,
/// and the ticker's phase is unrelated to when a guest was entered, so a 50 ms deadline
/// fires somewhere between 49 ms and 50 ms in — within one tick of what was asked for,
/// either side. That is imprecision an operator can ignore at any budget worth setting, and
/// one sleeping thread per process is a cost that does not scale with the instance count.
const EPOCH_TICK: Duration = Duration::from_millis(1);

/// The most arguments any guest export takes.
///
/// `eio_on_http(req_id, status, ptr, len)` (ABI §4.2). Every other export in §4.1 and §4.2
/// is narrower, so an argument buffer this wide never has to grow — which is what keeps a
/// guest call off the heap.
const MAX_ARITY: usize = 4;

/// The seven `eio:core` functions (ABI §7.0), as slots in the dispatch table.
///
/// An enum rather than an index into [`eio_manifest::CORE_FUNCTIONS`] so that the linker
/// closures can name their slot as a constant. `core_fn_names_match_the_shared_table` is
/// what keeps this list and the two shared ones from drifting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoreFn {
    Log,
    Emit,
    Prop,
    Error,
    TimeUnixMs,
    TimeMonoMs,
    Rand,
}

impl CoreFn {
    /// Every one, in ABI §7.0's table order.
    const ALL: [CoreFn; 7] = [
        CoreFn::Log,
        CoreFn::Emit,
        CoreFn::Prop,
        CoreFn::Error,
        CoreFn::TimeUnixMs,
        CoreFn::TimeMonoMs,
        CoreFn::Rand,
    ];

    /// The name the guest imports it as.
    const fn name(self) -> &'static str {
        match self {
            CoreFn::Log => core_fn::LOG,
            CoreFn::Emit => core_fn::EMIT,
            CoreFn::Prop => core_fn::PROP,
            CoreFn::Error => core_fn::ERROR,
            CoreFn::TimeUnixMs => core_fn::TIME_UNIX_MS,
            CoreFn::TimeMonoMs => core_fn::TIME_MONO_MS,
            CoreFn::Rand => core_fn::RAND,
        }
    }

    /// The function `name` denotes, if `eio:core` has one.
    fn from_name(name: &str) -> Option<CoreFn> {
        CoreFn::ALL.into_iter().find(|f| f.name() == name)
    }
}

/// The three `eio:state` functions (ABI §7.2), as slots in the dispatch table.
///
/// A second enum beside [`CoreFn`] rather than one flat list, because the two namespaces are
/// not the same kind of thing: §7.0 is always available and §7.2 is a capability a block has
/// to declare (§4.3). What they share is the slot table, through [`Import`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StateFn {
    Get,
    Put,
    Del,
}

impl StateFn {
    /// Every one, in ABI §7.2's table order.
    const ALL: [StateFn; 3] = [StateFn::Get, StateFn::Put, StateFn::Del];

    /// The name the guest imports it as.
    const fn name(self) -> &'static str {
        match self {
            StateFn::Get => state_fn::GET,
            StateFn::Put => state_fn::PUT,
            StateFn::Del => state_fn::DEL,
        }
    }

    /// The function `name` denotes, if `eio:state` has one.
    fn from_name(name: &str) -> Option<StateFn> {
        StateFn::ALL.into_iter().find(|f| f.name() == name)
    }
}

/// One host function this binding can dispatch to (ABI §7.0, §7.2).
///
/// The slot table is flat, and this is what indexes it: a linker closure names its import as
/// a constant, and [`Engine::register`] resolves a `(namespace, name)` pair to the same slot.
/// A namespace the daemon has no functions in resolves to nothing at all, which is what makes
/// "this host implements `eio:core` and `eio:state`" a single statement rather than one per
/// call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Import {
    Core(CoreFn),
    State(StateFn),
}

impl Import {
    /// How many slots the table has.
    const COUNT: usize = CoreFn::ALL.len() + StateFn::ALL.len();

    /// Its slot in [`State::slots`].
    const fn slot(self) -> usize {
        match self {
            Import::Core(function) => function as usize,
            Import::State(function) => CoreFn::ALL.len() + function as usize,
        }
    }

    /// The import `namespace`.`name` denotes, if this host implements it.
    fn from_name(namespace: &str, name: &str) -> Option<Import> {
        match namespace {
            namespace::CORE => CoreFn::from_name(name).map(Import::Core),
            namespace::STATE => StateFn::from_name(name).map(Import::State),
            _ => None,
        }
    }
}

impl From<CoreFn> for Import {
    fn from(function: CoreFn) -> Import {
        Import::Core(function)
    }
}

impl From<StateFn> for Import {
    fn from(function: StateFn) -> Import {
        Import::State(function)
    }
}

/// One instance's host-side state, as wasmtime's store carries it.
///
/// Not `Send`, and it does not need to be: DAEMON §5 gives each instance its own task, and
/// ABI §1.2 gives it one caller at a time.
struct State {
    /// The guest's linear memory. Set once, immediately after instantiation.
    ///
    /// [`Option`] only because the store must exist before the instance does. Every path
    /// that can reach a host function goes through [`Runtime::instantiate`], which fails
    /// unless the module exports `memory` (ABI §4.1).
    memory: Option<wasmtime::Memory>,
    /// The registered handlers, indexed by [`Import::slot`].
    slots: [Option<HostFn>; Import::COUNT],
}

/// What one guest entry is allowed to consume (ABI §10), and what one expression is (EXPR §9).
///
/// Host configuration, not ABI constants — §10 says so plainly, and leaf hosts will be
/// tighter. Every number here is therefore stated by whoever builds a [`Runtime`] rather than
/// defaulted silently anywhere below this type, and `node.toml`'s `[budgets]` is where a
/// daemon-class node states them (DAEMON §2.1).
///
/// The expression budgets travel with the guest ones because they are one operator decision
/// about one node: a callback's fuel bounds what the *guest* runs, and EXPR §9's bounds what
/// the host runs on the guest's behalf when it evaluates a property (ABI §7.1). Splitting them
/// across two types would give a caller two chances to configure half a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budgets {
    /// Fuel per guest entry. wasmtime's unit: roughly one per WASM instruction executed.
    pub fuel: u64,
    /// Wall-clock time per guest entry, rounded up to [`EPOCH_TICK`].
    pub deadline: Duration,
    /// What one property expression may consume, and the decode bound that travels with it.
    pub expr: ExprBudgets,
}

impl Budgets {
    /// Enough fuel for a callback doing real work, and far too little for a spin.
    ///
    /// A number with no ABI meaning (§10: "budgets are host configuration, not ABI
    /// constants"), stated here so that a daemon started with no configuration still
    /// enforces *something* — an unbudgeted callback is the one thing §10 does not allow.
    /// `node.toml` will supply the real one (DAEMON §2).
    pub const DEFAULT_FUEL: u64 = 100_000_000;

    /// The wall-clock companion to [`Budgets::DEFAULT_FUEL`], chosen the same way.
    ///
    /// Generous on purpose: it is the backstop for a callback blocked in a host function,
    /// which fuel cannot see, rather than the primary limit.
    pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(1);

    /// The deadline in whole [`EPOCH_TICK`]s, rounded up and never zero.
    ///
    /// Zero would mean "already expired", which would kill every instance on its first call
    /// — so a deadline shorter than one tick is one tick, and the log line an operator gets
    /// is a deadline trap rather than a mystery.
    fn epoch_ticks(self) -> u64 {
        let tick = EPOCH_TICK.as_nanos();
        let ticks = self.deadline.as_nanos().div_ceil(tick);
        u64::try_from(ticks).unwrap_or(u64::MAX).max(1)
    }
}

impl Default for Budgets {
    fn default() -> Budgets {
        Budgets {
            fuel: Budgets::DEFAULT_FUEL,
            deadline: Budgets::DEFAULT_DEADLINE,
            expr: ExprBudgets::DEFAULT,
        }
    }
}

/// Writes `module`'s compiled form to `path`, atomically (DAEMON §4.3).
///
/// Temporary-then-rename so that a daemon killed mid-write leaves either the previous artifact
/// or none — never a half of one, which the loader would have to be able to tell apart from a
/// whole one. The temporary is named for the process, so two daemons on one data directory do
/// not overwrite each other's partial writes.
fn store_artifact(path: &Path, module: &Module) -> anyhow::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let staging = dir.join(format!(".{}.{}.tmp", std::process::id(), file_name(path)));
    std::fs::write(&staging, module.serialize()?)?;
    std::fs::rename(&staging, path)?;
    Ok(())
}

/// A path's file name, for building a sibling temporary out of.
fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Feeds a [`Hash`] into sha256, so that a hash of one is stable across processes.
struct Sha256Hasher(sha2::Sha256);

impl Hasher for Sha256Hasher {
    fn write(&mut self, bytes: &[u8]) {
        sha2::Digest::update(&mut self.0, bytes);
    }

    /// The first eight bytes of the digest so far.
    ///
    /// Nothing here calls it — [`engine_key`](Runtime::engine_key) wants the whole digest in
    /// hex — but a trait method that panicked would be a trap for whoever hashes something
    /// else with this later, and answering honestly costs a clone of the digest state.
    fn finish(&self) -> u64 {
        let digest = sha2::Digest::finalize(self.0.clone());
        u64::from_be_bytes(digest[..8].try_into().expect("sha256 is 32 bytes"))
    }
}

/// The wasmtime engine, shared by every instance this daemon runs.
///
/// Compilation artifacts are cached per engine, so there is one of these per process rather
/// than one per block. `Send + Sync`, unlike everything downstream of it, which is what lets
/// one engine serve an instance on every thread (DAEMON §5).
pub struct Runtime {
    engine: wasmtime::Engine,
    budgets: Budgets,
    /// Where compiled modules are kept between boots, if this runtime has a node around it
    /// (DAEMON §4.3). `None` for `dev run-block` and the tests, which compile once and exit.
    precompiled: Option<PathBuf>,
}

impl Runtime {
    /// Builds the engine and starts its epoch ticker.
    ///
    /// The configuration is ABI §1.1's "core WASM only" plus the two budget mechanisms of
    /// §10. Narrower still is the *feature* set (workspace `Cargo.toml`): threads, the
    /// component model and GC are compiled out, so no configuration can turn them back on.
    pub fn new(budgets: Budgets) -> anyhow::Result<Runtime> {
        Runtime::build(budgets, None)
    }

    /// A runtime that keeps its compiled modules in `precompiled` (DAEMON §4.3).
    ///
    /// A node's, as against [`new`](Runtime::new)'s: cold start is what the directory buys,
    /// and a process that compiles one block and exits has no cold start to improve.
    pub fn caching(budgets: Budgets, precompiled: PathBuf) -> anyhow::Result<Runtime> {
        Runtime::build(budgets, Some(precompiled))
    }

    /// See [`new`](Runtime::new).
    fn build(budgets: Budgets, precompiled: Option<PathBuf>) -> anyhow::Result<Runtime> {
        let mut config = Config::new();
        // Every proposal off, then exactly [`MVP`] back on — not a list of `wasm_*(false)`
        // calls. The difference is what happens to the proposal wasmtime enables by default
        // in some later release: a list admits it silently on the next `cargo update`, and
        // blocks using it would run here and be refused by wasm3 at flash time, which is the
        // two-hosts divergence the shared crates exist to prevent (DAEMON §1). Subtracting
        // from `all()` refuses it instead, on a host nobody has touched.
        config.wasm_features(WasmFeatures::all(), false);
        config.wasm_features(ACCEPTED, true);
        config.consume_fuel(true);
        config.epoch_interruption(true);
        let engine = wasmtime::Engine::new(&config)?;
        spawn_epoch_ticker(&engine)?;
        Ok(Runtime {
            engine,
            budgets,
            precompiled,
        })
    }

    /// What one property expression may consume on this node (EXPR §9, DAEMON §2.1).
    ///
    /// Read off the runtime rather than passed alongside it, so that an instance cannot be
    /// built with expression budgets other than the node's — the same reason ABI §10's two
    /// numbers are armed from here rather than by each caller.
    pub fn expr(&self) -> ExprBudgets {
        self.budgets.expr
    }

    /// Compiles `wasm` to a module this runtime can instantiate.
    ///
    /// Separate from [`instantiate`](Runtime::instantiate) because the two have different
    /// lifetimes: a module is compiled once and instantiated once per *life* of the block
    /// instance, and DAEMON §8's restart is a second life. Keeping the [`Module`] is what
    /// lets a supervisor re-instantiate without either recompiling or holding the block's
    /// bytes resident — the module's compiled code is already alive for as long as any
    /// instance of it is, so a retained handle costs a refcount.
    ///
    /// No guest code runs here. ABI §5.1 step 1 is *instantiation*, which is where module
    /// initialisation executes and where a budget therefore has to be armed; compilation is
    /// the host reading a file.
    /// A cache hit skips cranelift entirely, which is what DAEMON §4.3 is for: on a Pi,
    /// compiling a block costs more than everything else about starting it put together.
    pub fn compile(&self, wasm: &[u8]) -> anyhow::Result<Module> {
        let Some(path) = self.artifact(wasm) else {
            return Ok(Module::new(&self.engine, wasm)?);
        };
        if let Some(module) = self.load_artifact(&path) {
            return Ok(module);
        }

        let module = Module::new(&self.engine, wasm)?;
        // A cache that cannot be written is a slow node, not a broken one, so this is logged
        // and not propagated — the module in hand is the same module either way (§4.3).
        if let Err(error) = store_artifact(&path, &module) {
            tracing::debug!(path = %path.display(), %error, "the compiled block was not cached");
        }
        Ok(module)
    }

    /// Where this engine keeps `wasm` compiled, if it keeps it anywhere (DAEMON §4.3).
    fn artifact(&self, wasm: &[u8]) -> Option<PathBuf> {
        let dir = self.precompiled.as_ref()?;
        Some(dir.join(format!(
            "{}.{}.cwasm",
            crate::blocks::sha256_hex(wasm),
            self.engine_key()
        )))
    }

    /// A short hash of everything about this engine that changes what it compiles to.
    ///
    /// wasmtime's own compatibility hash, run through sha256 rather than through
    /// [`DefaultHasher`](std::collections::hash_map::DefaultHasher): that one is explicitly
    /// not stable between Rust releases, and a key that changed on a toolchain upgrade would
    /// silently orphan every artifact on the node.
    fn engine_key(&self) -> String {
        let mut hasher = Sha256Hasher(<sha2::Sha256 as sha2::Digest>::new());
        self.engine
            .precompile_compatibility_hash()
            .hash(&mut hasher);
        String::from(&crate::blocks::hex(&sha2::Digest::finalize(hasher.0))[..16])
    }

    /// Loads a cached artifact, or answers `None` for every way that can fail.
    ///
    /// A `.cwasm` that will not load is a **miss**, never an error: it is derived, and a node
    /// that refused to boot over a truncated cache file is a node any interrupted write can
    /// take down (DAEMON §4.3).
    fn load_artifact(&self, path: &Path) -> Option<Module> {
        // SAFETY: DAEMON §4.3. The file is inside the node's own data directory and was
        // written there by this daemon from bytes it had already verified (§4.1); wasmtime
        // independently refuses an artifact produced by an incompatible engine build, and the
        // filename pins the compilation configuration besides. A node whose data directory an
        // untrusted party can write to has already lost — that directory holds the service
        // files saying what to run.
        let loaded = unsafe { Module::deserialize_file(&self.engine, path) };
        match loaded {
            Ok(module) => Some(module),
            Err(error) => {
                if path.exists() {
                    tracing::debug!(path = %path.display(), %error, "recompiling: the cached artifact did not load");
                }
                None
            }
        }
    }

    /// Instantiates `module`, with `eio:core` linked but not yet implemented.
    ///
    /// ABI §5.1 step 1. Validation of the module against the ABI — exports, imports,
    /// signatures, manifest agreement — is `eio_manifest`'s and happens before this; what
    /// is left here is what only an engine can do: link its imports and give back something
    /// callable.
    ///
    /// The returned guest answers every `eio:core` import with `ERR_UNSUPPORTED` until
    /// [`Engine::register`] supplies a handler.
    pub fn instantiate(&self, module: &Module) -> anyhow::Result<Guest> {
        let mut linker = Linker::new(&self.engine);
        link_core(&mut linker)?;
        link_state(&mut linker)?;

        let mut store = Store::new(
            &self.engine,
            State {
                memory: None,
                slots: [const { None }; Import::COUNT],
            },
        );
        // Before instantiation, not just before the callbacks: a store with fuel metering on
        // starts with none, and instantiation runs the module's own initialisation (ABI §5.1
        // step 1). Unarmed, every block would die on the way in.
        arm(&mut store, self.budgets)?;
        let instance = linker.instantiate(&mut store, module)?;

        let memory = instance
            .get_memory(&mut store, MEMORY_EXPORT)
            .ok_or_else(|| anyhow::anyhow!("module does not export {MEMORY_EXPORT:?}"))?;
        store.data_mut().memory = Some(memory);

        // Resolved once, here, rather than per call: `Engine::has_export` takes `&self`
        // while wasmtime's export lookup needs `&mut Store`, and the answer cannot change
        // for the life of an instance anyway.
        let mut funcs = BTreeMap::new();
        for export in module.exports() {
            let name = export.name().to_string();
            if let Some(Extern::Func(func)) = instance.get_export(&mut store, &name) {
                let results = func.ty(&store).results().len();
                funcs.insert(name, Exported { func, results });
            }
        }

        Ok(Guest {
            store,
            memory,
            funcs,
            budgets: self.budgets,
        })
    }
}

/// Gives `store` a full budget for one guest entry (ABI §10).
fn arm(store: &mut Store<State>, budgets: Budgets) -> anyhow::Result<()> {
    store
        .set_fuel(budgets.fuel)
        .map_err(|error| anyhow::anyhow!("this engine does not meter fuel: {error}"))?;
    store.set_epoch_deadline(budgets.epoch_ticks());
    Ok(())
}

/// Starts the thread that advances `engine`'s epoch, and ends when the engine is gone.
///
/// A *weak* handle rather than a clone, so that this thread is not what keeps the engine
/// alive: when the last [`Runtime`] is dropped the upgrade fails and the loop returns. A
/// strong clone here would make the ticker immortal and the engine unfreeable, which in a
/// test binary means one leaked thread per test.
fn spawn_epoch_ticker(engine: &wasmtime::Engine) -> anyhow::Result<()> {
    let weak = engine.weak();
    std::thread::Builder::new()
        .name(String::from("eio-epoch"))
        .spawn(move || {
            loop {
                std::thread::sleep(EPOCH_TICK);
                match weak.upgrade() {
                    Some(engine) => engine.increment_epoch(),
                    None => return,
                }
            }
        })?;
    Ok(())
}

/// An exported function, with the arity its results buffer needs.
#[derive(Clone, Copy)]
struct Exported {
    func: Func,
    /// How many results it returns — one for every ABI §4.1 export but `eio_free`.
    results: usize,
}

/// A live guest instance, as `eio_host_core` drives it.
pub struct Guest {
    store: Store<State>,
    memory: wasmtime::Memory,
    funcs: BTreeMap<String, Exported>,
    /// Refreshed on every entry through [`Engine::call`] (ABI §10).
    budgets: Budgets,
}

impl Engine for Guest {
    fn call(&mut self, export: &str, args: &[i32]) -> Result<i32, Trap> {
        let Some(exported) = self.funcs.get(export).copied() else {
            return Err(Trap::with_detail(
                TrapKind::Engine,
                format!("the guest does not export {export:?}"),
            ));
        };
        // On the stack, not the heap: this is every callback of every instance, and ABI §4
        // bounds both ends of it — no export takes more than [`MAX_ARITY`] arguments and
        // none returns more than one value.
        if args.len() > MAX_ARITY {
            return Err(Trap::with_detail(
                TrapKind::Engine,
                format!("{export:?} was called with {} arguments", args.len()),
            ));
        }
        let mut params = [Val::I32(0); MAX_ARITY];
        for (slot, arg) in params.iter_mut().zip(args) {
            *slot = Val::I32(*arg);
        }
        // ABI §10, for this entry and no further. Both budgets are set from scratch rather
        // than topped up, because §10 budgets a *callback*: what the previous one spent is
        // not this one's business, and a guest cannot bank an unspent allowance.
        arm(&mut self.store, self.budgets).map_err(|error| {
            // Unreachable: `Runtime::new` is the only way to a `Guest`, and it enables fuel.
            Trap::with_detail(TrapKind::Engine, format!("{error}"))
        })?;
        let mut results = [Val::I32(0)];
        exported
            .func
            .call(
                &mut self.store,
                &params[..args.len()],
                &mut results[..exported.results],
            )
            .map_err(trap_of)?;
        match results[..exported.results] {
            [Val::I32(value)] => Ok(value),
            // Unreachable for a validated module: ABI §4.1 and §4.2 give every callable
            // export a single `i32` result, and `eio_manifest` refused anything else.
            _ => Err(Trap::with_detail(
                TrapKind::Engine,
                format!("{export:?} did not return a single i32"),
            )),
        }
    }

    fn has_export(&self, export: &str) -> bool {
        // `memory` is the one ABI §4.1 export that is not a function, and instantiation
        // failed without it, so its presence is already established.
        export == MEMORY_EXPORT || self.funcs.contains_key(export)
    }

    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
        let data = self.memory.data(&self.store);
        memory_range(data.len(), ptr, len).map(|r| data[r].to_vec())
    }

    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
        let data = self.memory.data_mut(&mut self.store);
        let range = memory_range(data.len(), ptr, bytes.len() as u64)?;
        data[range].copy_from_slice(bytes);
        Ok(())
    }

    fn register(&mut self, ns: &str, name: &str, f: HostFn) -> Result<(), EngineError> {
        // `eio:core` (ABI §7.0) and `eio:state` (§7.2) are what this host implements. A block
        // needing anything else is refused at load time with the capability named (DAEMON
        // §12); reaching here with another namespace means a caller registered for one the
        // linker never defined, and the guest could not have imported it.
        let Some(import) = Import::from_name(ns, name) else {
            return Err(EngineError::Engine(format!(
                "this host implements no host function {ns} {name:?} (ABI §7.0, §7.2)"
            )));
        };
        let slot = &mut self.store.data_mut().slots[import.slot()];
        if slot.is_some() {
            return Err(EngineError::DuplicateImport {
                namespace: ns.to_string(),
                name: name.to_string(),
            });
        }
        *slot = Some(f);
        Ok(())
    }
}

/// Defines every `eio:core` function on `linker`, with ABI §7.0's signatures.
///
/// The signatures live in these closures rather than in a table, because wasmtime encodes
/// them in the closure's Rust type — a table would be a second statement of the same thing,
/// and only one of the two would be checked by the compiler.
fn link_core(linker: &mut Linker<State>) -> anyhow::Result<()> {
    let ns = namespace::CORE;
    linker.func_wrap(
        ns,
        CoreFn::Log.name(),
        |mut caller: Caller<'_, State>, level: i32, ptr: i32, len: i32| {
            void(dispatch(
                &mut caller,
                CoreFn::Log,
                &[Arg::I32(level), Arg::I32(ptr), Arg::I32(len)],
            ));
        },
    )?;
    linker.func_wrap(
        ns,
        CoreFn::Emit.name(),
        |mut caller: Caller<'_, State>, port: i32, ptr: i32, len: i32| -> i32 {
            i32_of(dispatch(
                &mut caller,
                CoreFn::Emit,
                &[Arg::I32(port), Arg::I32(ptr), Arg::I32(len)],
            ))
        },
    )?;
    linker.func_wrap(
        ns,
        CoreFn::Prop.name(),
        |mut caller: Caller<'_, State>, prop_id: i32, signal_idx: i32, buf: i32, cap: i32| -> i32 {
            i32_of(dispatch(
                &mut caller,
                CoreFn::Prop,
                &[
                    Arg::I32(prop_id),
                    Arg::I32(signal_idx),
                    Arg::I32(buf),
                    Arg::I32(cap),
                ],
            ))
        },
    )?;
    linker.func_wrap(
        ns,
        CoreFn::Error.name(),
        |mut caller: Caller<'_, State>, code: i32, ptr: i32, len: i32| {
            void(dispatch(
                &mut caller,
                CoreFn::Error,
                &[Arg::I32(code), Arg::I32(ptr), Arg::I32(len)],
            ));
        },
    )?;
    linker.func_wrap(
        ns,
        CoreFn::TimeUnixMs.name(),
        |mut caller: Caller<'_, State>| -> i64 {
            i64_of(dispatch(&mut caller, CoreFn::TimeUnixMs, &[]))
        },
    )?;
    linker.func_wrap(
        ns,
        CoreFn::TimeMonoMs.name(),
        |mut caller: Caller<'_, State>| -> i64 {
            i64_of(dispatch(&mut caller, CoreFn::TimeMonoMs, &[]))
        },
    )?;
    linker.func_wrap(
        ns,
        CoreFn::Rand.name(),
        |mut caller: Caller<'_, State>, buf: i32, len: i32| -> i32 {
            i32_of(dispatch(
                &mut caller,
                CoreFn::Rand,
                &[Arg::I32(buf), Arg::I32(len)],
            ))
        },
    )?;
    Ok(())
}

/// Defines `eio:state`'s three functions on `linker`, with ABI §7.2's signatures.
///
/// Defined whatever the module imports, like `eio:core`: a linker definition nothing imports
/// costs nothing, and choosing which to define from the manifest would put ABI §4.3's
/// capability question in the engine, where §4.3 explicitly does not want it. What decides
/// whether a *guest* can use them is the manifest — a module importing `eio:state` without
/// declaring the capability is refused before it reaches here (ABI §4.3) — and whether a
/// handler was registered at all, since an unregistered slot answers `ERR_UNSUPPORTED`.
fn link_state(linker: &mut Linker<State>) -> anyhow::Result<()> {
    let ns = namespace::STATE;
    linker.func_wrap(
        ns,
        StateFn::Get.name(),
        |mut caller: Caller<'_, State>, key: i32, key_len: i32, buf: i32, cap: i32| -> i32 {
            i32_of(dispatch(
                &mut caller,
                StateFn::Get,
                &[
                    Arg::I32(key),
                    Arg::I32(key_len),
                    Arg::I32(buf),
                    Arg::I32(cap),
                ],
            ))
        },
    )?;
    linker.func_wrap(
        ns,
        StateFn::Put.name(),
        |mut caller: Caller<'_, State>, key: i32, key_len: i32, val: i32, val_len: i32| -> i32 {
            i32_of(dispatch(
                &mut caller,
                StateFn::Put,
                &[
                    Arg::I32(key),
                    Arg::I32(key_len),
                    Arg::I32(val),
                    Arg::I32(val_len),
                ],
            ))
        },
    )?;
    linker.func_wrap(
        ns,
        StateFn::Del.name(),
        |mut caller: Caller<'_, State>, key: i32, key_len: i32| -> i32 {
            i32_of(dispatch(
                &mut caller,
                StateFn::Del,
                &[Arg::I32(key), Arg::I32(key_len)],
            ))
        },
    )?;
    Ok(())
}

/// Runs the handler registered in `function`'s slot.
///
/// The one place a guest→host call crosses into `eio_host_core`, and the reason the
/// crossing is safe: [`wasmtime::Memory::data_and_store_mut`] hands back the guest's bytes
/// and the store's data from a single disjoint borrow, so a handler gets `&mut dyn Memory`
/// and its own `&mut HostFn` without either being reconstructed from the other. The memory
/// borrow ends with this function, which is ABI §9.3 — "host MUST NOT retain guest pointers
/// past the call" — as a lifetime rather than as a rule.
fn dispatch(caller: &mut Caller<'_, State>, import: impl Into<Import>, args: &[Arg]) -> Ret {
    let Some(memory) = caller.data().memory else {
        // Unreachable: `Runtime::instantiate` sets this before returning a `Guest`, and
        // there is no other way to reach a host function.
        return Ret::None;
    };
    let (bytes, state) = memory.data_and_store_mut(caller);
    let Some(handler) = state.slots[import.into().slot()].as_mut() else {
        return Ret::None;
    };
    let mut view = View(bytes);
    handler(HostCall {
        args,
        memory: &mut view,
    })
}

/// A [`Ret`] for an `-> i32` import (ABI §7.0).
fn i32_of(ret: Ret) -> i32 {
    match ret {
        Ret::I32(value) => value,
        _ => {
            tracing::error!("an eio:* i32 host function is unimplemented on this host");
            UNIMPLEMENTED
        }
    }
}

/// A [`Ret`] for an `-> i64` import — the two clocks of ABI §7.0.
fn i64_of(ret: Ret) -> i64 {
    match ret {
        Ret::I64(value) => value,
        _ => {
            // There is no error code in an `i64` return: the clocks have no status
            // convention (ABI §7.0), so an unimplemented one can only answer with a number.
            tracing::error!("an eio:core clock is unimplemented on this host");
            0
        }
    }
}

/// A [`Ret`] for a `-> ()` import — `log` and `error` (ABI §7.0).
fn void(ret: Ret) {
    if ret != Ret::None {
        tracing::error!("an eio:core void function answered with a value");
    }
}

/// Guest memory for the duration of one host call.
///
/// Deliberately holds a slice and not the store: [`eio_host_core::Memory`] has no `call`,
/// so a handler cannot re-enter the guest (ABI §1.2), and a `&mut [u8]` cannot be used to
/// try.
struct View<'a>(&'a mut [u8]);

impl Memory for View<'_> {
    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
        memory_range(self.0.len(), ptr, len).map(|r| self.0[r].to_vec())
    }

    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
        let range = memory_range(self.0.len(), ptr, bytes.len() as u64)?;
        self.0[range].copy_from_slice(bytes);
        Ok(())
    }
}

/// Classifies an engine failure as one of ABI §5.1's deaths.
///
/// Every arm is a discarded instance — §5.1 has no state to return to that is not "discard
/// it" — but *which* death it was is what supervision and the operator's log need: a guest
/// that overran its budget is a sizing problem, and a guest that trapped is a bug.
fn trap_of(error: wasmtime::Error) -> Trap {
    let kind = match error.downcast_ref::<wasmtime::Trap>() {
        // ABI §10: the callback's execution budget. wasmtime calls it fuel.
        Some(wasmtime::Trap::OutOfFuel) => TrapKind::Fuel,
        // ABI §10: the wall-clock deadline. wasmtime calls it epoch interruption.
        Some(wasmtime::Trap::Interrupt) => TrapKind::Deadline,
        // Any other WASM trap: unreachable, out of bounds, division by zero (ABI §8).
        Some(_) => TrapKind::Trap,
        // Not a trap at all — a host function that panicked, or an engine-internal
        // failure. Death all the same (ABI §5.1 step 6).
        None => TrapKind::Engine,
    };
    // `{:?}` rather than `{}`: wasmtime attaches the guest backtrace as error context, and
    // a trap's log line is all anyone gets.
    Trap::with_detail(kind, format!("{error:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::scratch::scratch;

    #[test]
    fn core_fn_names_match_the_shared_tables() {
        let names: Vec<&str> = CoreFn::ALL.into_iter().map(CoreFn::name).collect();
        assert_eq!(
            names,
            eio_manifest::CORE_FUNCTIONS,
            "the linker defines exactly the functions the load-time check admits (ABI §7.0)"
        );
        assert_eq!(names, eio_host_core::exports::core_fn::ALL);
    }

    #[test]
    fn state_fn_names_match_the_shared_tables() {
        let names: Vec<&str> = StateFn::ALL.into_iter().map(StateFn::name).collect();
        assert_eq!(
            names,
            eio_manifest::Capability::State.functions(),
            "the linker defines exactly the functions ABI §7.2 gives the capability"
        );
        assert_eq!(names, eio_host_core::exports::state_fn::ALL);
    }

    #[test]
    fn every_import_has_its_own_slot_and_resolves_from_its_name() {
        // The dispatch table is flat and indexed by these numbers, so two imports sharing a
        // slot would be one handler answering the other's calls — `state_get` reached through
        // `log`, and nothing failing to compile.
        let imports: Vec<Import> = CoreFn::ALL
            .into_iter()
            .map(Import::Core)
            .chain(StateFn::ALL.into_iter().map(Import::State))
            .collect();
        assert_eq!(imports.len(), Import::COUNT);
        for (index, import) in imports.into_iter().enumerate() {
            assert_eq!(import.slot(), index, "{import:?}");
            let (namespace, name) = match import {
                Import::Core(function) => (namespace::CORE, function.name()),
                Import::State(function) => (namespace::STATE, function.name()),
            };
            assert_eq!(Import::from_name(namespace, name), Some(import));
        }

        assert_eq!(Import::from_name(namespace::CORE, "frobnicate"), None);
        // A function of the right name in the wrong namespace is nothing: `eio:state`'s three
        // are not reachable as `eio:core`'s, and a namespace this host has no functions in has
        // no slots at all.
        assert_eq!(Import::from_name(namespace::CORE, state_fn::GET), None);
        assert_eq!(Import::from_name(namespace::STATE, core_fn::LOG), None);
        assert_eq!(Import::from_name(namespace::GPIO, "gpio_read"), None);
    }

    /// Compiles `wat` on a real [`Runtime`], through the same call production code uses.
    ///
    /// [`Runtime::compile`] rather than the whole of [`Runtime::instantiate`], because a
    /// post-MVP module is refused while it is being *validated* — before there is anything
    /// to link, and long before the `eio:core` imports or the `memory` export these snippets
    /// deliberately lack would be looked for. A fixture carrying the full ABI surface would
    /// test the same rejection while hiding which of several reasons produced it.
    fn compile(runtime: &Runtime, wat: &str) -> anyhow::Result<()> {
        let wasm = wat::parse_str(wat).expect("the snippet assembles");
        runtime.compile(&wasm).map(drop)
    }

    /// The smallest module that compiles, for the artifact tests.
    fn a_module() -> Vec<u8> {
        wat::parse_str(r#"(module (func (export "f")))"#).expect("the snippet assembles")
    }

    #[test]
    fn a_compiled_block_is_kept_under_its_content_and_engine() {
        // DAEMON §4.3's key, both halves of it, read off the filename the compile wrote.
        let dir = scratch("precompiled-key");
        let runtime = Runtime::caching(Budgets::default(), dir.clone()).expect("an engine");
        let wasm = a_module();

        runtime.compile(&wasm).expect("a first compile");
        let artifacts: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("the directory")
            .map(|entry| entry.expect("an entry").path())
            .collect();
        assert_eq!(
            artifacts.len(),
            1,
            "one artifact for one block: {artifacts:?}"
        );
        assert_eq!(
            artifacts[0],
            runtime.artifact(&wasm).expect("a caching runtime"),
            "and it is where the next boot will look for it"
        );
        assert_eq!(
            file_name(&artifacts[0]),
            format!(
                "{}.{}.cwasm",
                crate::blocks::sha256_hex(&wasm),
                runtime.engine_key()
            )
        );
    }

    #[test]
    fn a_cached_artifact_is_read_rather_than_recompiled() {
        // The hit itself, and it needs proving rather than asserting: a cache that was
        // written and then silently ignored passes every "it still compiles" test there is.
        // So one block's artifact is filed under *another* block's key, and the compile is
        // asked for the second — if the answer has the first one's exports, the file was
        // read, and nothing else explains it.
        let dir = scratch("precompiled-hit");
        let runtime = Runtime::caching(Budgets::default(), dir).expect("an engine");
        let cached = wat::parse_str(r#"(module (func (export "cached")))"#).expect("assembles");
        let asked = wat::parse_str(r#"(module (func (export "asked")))"#).expect("assembles");

        runtime.compile(&cached).expect("a first compile");
        std::fs::rename(
            runtime.artifact(&cached).expect("a caching runtime"),
            runtime.artifact(&asked).expect("a caching runtime"),
        )
        .expect("filing it under the other key");

        let module = runtime.compile(&asked).expect("a compile");
        let exports: Vec<String> = module
            .exports()
            .map(|export| String::from(export.name()))
            .collect();
        assert_eq!(exports, ["cached"], "the artifact was read, not the bytes");
    }

    #[test]
    fn a_corrupt_artifact_is_a_miss_and_never_a_failure() {
        // §4.3: the cache is derived, and a node that refused to boot over a truncated file
        // is a node any interrupted write can take down.
        let dir = scratch("precompiled-corrupt");
        let runtime = Runtime::caching(Budgets::default(), dir).expect("an engine");
        let wasm = a_module();
        let artifact = runtime.artifact(&wasm).expect("a caching runtime");

        std::fs::create_dir_all(artifact.parent().expect("the directory")).expect("the directory");
        std::fs::write(&artifact, b"not a compiled module").expect("poisoning the artifact");
        runtime.compile(&wasm).expect("a miss, not an error");
        assert_ne!(
            std::fs::read(&artifact).expect("the artifact"),
            b"not a compiled module",
            "and the miss rewrote it"
        );
    }

    #[test]
    fn an_engine_configured_differently_does_not_reuse_the_artifact() {
        // The other half of §4.3's key. Two runtimes over one directory: the compiled form is
        // not portable between engine configurations, so the filenames must differ before
        // wasmtime is asked to tell them apart.
        let dir = scratch("precompiled-engine-key");
        let same = Runtime::caching(Budgets::default(), dir.clone()).expect("an engine");
        assert_eq!(
            same.engine_key(),
            Runtime::caching(Budgets::default(), dir.clone())
                .expect("an engine")
                .engine_key(),
            "the same configuration keys the same artifacts across processes"
        );

        let mut config = Config::new();
        config.wasm_features(WasmFeatures::all(), false);
        config.wasm_features(ACCEPTED, true);
        config.consume_fuel(true);
        config.epoch_interruption(true);
        // One knob, and one that changes generated code rather than a name.
        config.cranelift_opt_level(wasmtime::OptLevel::None);
        let other = Runtime {
            engine: wasmtime::Engine::new(&config).expect("an engine"),
            budgets: Budgets::default(),
            precompiled: Some(dir.clone()),
        };
        assert_ne!(same.engine_key(), other.engine_key());

        let wasm = a_module();
        same.compile(&wasm).expect("a compile");
        other.compile(&wasm).expect("a compile");
        assert_eq!(
            std::fs::read_dir(&dir).expect("the directory").count(),
            2,
            "one artifact each, not one overwritten twice"
        );
    }

    #[test]
    fn a_runtime_with_no_directory_writes_nothing() {
        // `dev run-block` and every test above: no node, no second boot, nothing to cache.
        let runtime = Runtime::new(Budgets::default()).expect("an engine");
        assert_eq!(runtime.artifact(&a_module()), None);
        runtime.compile(&a_module()).expect("a compile");
    }

    #[test]
    fn a_core_mvp_module_is_accepted() {
        // The control for every rejection below: this config refuses *some* proposals rather
        // than refusing WebAssembly. Every instruction here is in the 2017 MVP.
        compile(
            &Runtime::new(Budgets::default()).expect("an engine"),
            r#"(module
                 (memory (export "memory") 1)
                 (global $g (mut i32) (i32.const 0))
                 (func (export "f") (param i32) (result i32)
                   (global.set $g (local.get 0))
                   (i32.add (global.get $g) (i32.load (i32.const 0)))))"#,
        )
        .expect("core WASM is what this host runs");
    }

    #[test]
    fn a_module_past_the_accepted_set_is_refused_by_the_engine() {
        // One engine for the whole table. Each case is a fresh `Module::new`, which is where
        // the refusal happens; rebuilding the engine per case would re-test `Runtime::new`
        // eight times and spawn an epoch ticker for each.
        let runtime = Runtime::new(Budgets::default()).expect("an engine");
        // ABI §4.3: feature conformance is enforced here and nowhere else — `eio_manifest`
        // accepts every one of these. Each is the smallest module that needs its proposal,
        // paired with the words its rejection has to contain.
        //
        // These are the proposals still *outside* [`ACCEPTED`]. The six the guest toolchain
        // emits have their own test below; what is left is what neither rustc emits nor wasm3
        // implements, and admitting one of those would be the two-hosts divergence with
        // nothing to catch it.
        //
        // Matching the message is the point rather than an incidental strictness: ABI §4.3
        // requires the rejection to *name* the proposal, because a deployer holding a valid
        // manifest and a refused block has nothing else to act on. Each expectation is the
        // distinctive noun and nothing around it, so wasmtime is free to rephrase the
        // sentence without failing this — but not free to stop saying what was wrong.
        for (proposal, needle, wat) in [
            (
                "simd",
                "simd",
                r#"(module (func (export "f") (result i32)
                     (i32x4.extract_lane 0 (v128.const i32x4 1 2 3 4))))"#,
            ),
            (
                "tail call",
                "tail call",
                r#"(module (func $g (result i32) (i32.const 1))
                     (func (export "f") (result i32) (return_call $g)))"#,
            ),
            // A second linear memory needs no instruction to be past MVP: declaring it is
            // already the proposal.
            (
                "multi-memory",
                "memories",
                r#"(module (memory 1) (memory 1))"#,
            ),
        ] {
            let error = compile(&runtime, wat)
                .expect_err(&format!("{proposal} is outside the accepted set"));
            // `{:?}`, because the sentence naming the proposal is a *cause* — the top line
            // says only which function failed to compile. This is the same rendering the
            // deployer gets, since the daemon returns the error out of `main` (ABI §4.3).
            let message = format!("{error:?}").to_lowercase();
            assert!(
                message.contains(needle),
                "refusing {proposal} has to say so, and said: {message}"
            );
        }
    }

    #[test]
    fn every_proposal_the_guest_toolchain_emits_is_accepted() {
        // The other half of [`ACCEPTED`], and the half that used to be false. rustc enables
        // all six of these by default for `wasm32-unknown-unknown`, so a host refusing any one
        // of them refuses ordinary Rust blocks — which is what it did, until measured.
        //
        // `crates/conformance/tests/wasm3.rs` runs the same six on wasm3 and checks the value
        // each produces. That pairing is the whole argument: this test says the daemon accepts
        // them, that one says the leaf engine executes them correctly, and neither alone would
        // justify the set.
        let runtime = Runtime::new(Budgets::default()).expect("an engine");
        for (proposal, wat) in [
            (
                "bulk memory",
                r#"(module (memory 1) (func (export "f")
                     (memory.copy (i32.const 0) (i32.const 8) (i32.const 8))))"#,
            ),
            (
                "sign extension",
                r#"(module (func (export "f") (result i32)
                     (i32.extend8_s (i32.const 1))))"#,
            ),
            (
                "multi-value",
                r#"(module (func (export "f") (result i32 i32)
                     (i32.const 1) (i32.const 2)))"#,
            ),
            (
                "saturating float-to-int",
                r#"(module (func (export "f") (result i32)
                     (i32.trunc_sat_f32_s (f32.const 1))))"#,
            ),
            (
                "reference types (the call_indirect encoding)",
                r#"(module (table 1 funcref) (elem (i32.const 0) $g)
                     (func $g (result i32) (i32.const 5))
                     (type $t (func (result i32)))
                     (func (export "f") (result i32)
                       (call_indirect (type $t) (i32.const 0))))"#,
            ),
            (
                "mutable globals",
                r#"(module (global (export "g") (mut i32) (i32.const 0)))"#,
            ),
        ] {
            compile(&runtime, wat)
                .unwrap_or_else(|e| panic!("the guest toolchain emits {proposal}: {e:?}"));
        }
    }

    #[test]
    fn what_the_engine_cannot_refuse_the_loader_does() {
        // ABI §4.3's carve-out, from the daemon's side. Two of the six proposals are
        // accepted only in part, and this engine accepts them whole — not by oversight but
        // because a `Config` has one switch per proposal, and turning bulk memory off to
        // refuse `table.copy` would refuse the `memory.copy` in every Rust block with it.
        //
        // So the engine compiles these and `eio_manifest::validate` refuses them, and both
        // halves are asserted here rather than only the second: a test that checked the
        // loader alone would still pass on the day someone "fixed" this config, leaving
        // one refusal where §4.3 requires two layers. `crates/manifest/tests/portable.rs`
        // carries the full carve-out; three cases are enough to pin the pairing.
        let runtime = Runtime::new(Budgets::default()).expect("an engine");
        for (feature, wat) in [
            (
                "table.copy",
                r#"(module (table 4 funcref)
                     (func (export "f") (table.copy (i32.const 2) (i32.const 0) (i32.const 1))))"#,
            ),
            (
                "table.get",
                r#"(module (table 4 funcref)
                     (func (export "f") (drop (table.get (i32.const 0)))))"#,
            ),
            (
                "data.drop",
                r#"(module (data $d "\07") (func (export "f") (data.drop $d)))"#,
            ),
        ] {
            compile(&runtime, wat)
                .unwrap_or_else(|e| panic!("the engine has no switch that refuses {feature}: {e}"));

            // No manifest section, so validation would fail for that reason too — which is
            // exactly why the variant is matched rather than the fact of an error.
            let wasm = wat::parse_str(wat).expect("the snippet assembles");
            let error = eio_manifest::validate(&wasm, None)
                .err()
                .unwrap_or_else(|| panic!("{feature} is carved out of the accepted set"));
            assert!(
                matches!(error, eio_manifest::ModuleError::Unportable { .. }),
                "{feature} must be refused as unportable, and was: {error}"
            );
            let message = error.to_string();
            assert!(
                message.contains(feature),
                "the refusal of {feature} has to name it, and said: {message}"
            );
        }
    }

    #[test]
    fn wasmparsers_mvp_set_is_not_one_this_build_can_ask_for() {
        // Why [`MVP`] subtracts `GC_TYPES` rather than being wasmparser's set as-is: with
        // the `gc` cargo feature compiled out, asking for that flag does not merely admit
        // more WebAssembly, it fails `Engine::new` outright and the daemon does not start.
        //
        // Asserting on the unsubtracted set rather than on `MVP`'s own bits, because the
        // latter would only re-run wasmtime's `difference`. This fails if a wasmtime upgrade
        // drops `GC_TYPES` from `MVP`, or if the cargo feature comes back — either of which
        // should be a decision someone makes, not a startup failure someone debugs.
        let mut config = Config::new();
        config.wasm_features(WasmFeatures::MVP, true);
        assert!(
            wasmtime::Engine::new(&config).is_err(),
            "wasmparser's MVP set includes GC_TYPES, which this build refuses"
        );
    }
}

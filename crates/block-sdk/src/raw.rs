//! The `eio:core` host interface (ABI §7.0), and the only place `(ptr, len)` exists.
//!
//! Every function here takes and returns *slices*. The ABI carries `(ptr, len)` pairs
//! (ABI §3), and converting between the two is the boundary this module is: above it,
//! [`Ctx`](crate::Ctx) and the rest of the SDK never see an address; below it, on
//! `wasm32`, the pairs are handed straight to the imports.
//!
//! Return values are left as the raw `i32` the ABI defines, because ABI §8's three
//! conventions are not interchangeable and choosing between them is the caller's business:
//! `emit` is a status, `prop` is a size, and `rand` is a status over a `len` despite
//! writing data. Decoding them is [`Ctx`](crate::Ctx)'s job, through `eio_abi`'s decoders.
//!
//! # Three builds
//!
//! - **The guest** (`wasm32-unknown-unknown`) calls real WASM imports from the `eio:core`
//!   module. This is the only one that ships.
//! - **A hosted target** (the `cargo test` build) gets a stub that *records* the call,
//!   which is what lets the SDK's own tests exercise [`Ctx`](crate::Ctx) natively — a
//!   `cargo test` that had to boot a WASM engine would not be the fast inner loop SDK §6.1
//!   asks for.
//! - **A bare-metal target** (`target_os = "none"`, the two legs of `just check-nostd`)
//!   gets an inert stub. Nothing there ever calls it: those builds exist to prove the crate
//!   has no `std`, and the recording stub needs `std` for its thread-local. Compiling the
//!   whole crate against them is what makes that proof cover `Ctx`, `Descriptor` and the
//!   error types rather than just the parts with no dependencies.
//!
//! **The slice-based shape is what makes that possible.** An earlier version passed `i32`
//! addresses through to the stub and segfaulted on the first call: a host pointer is 64
//! bits and ABI §3's `i32` cannot hold one. The truncation is invisible on `wasm32`, where
//! pointers *are* 32 bits — which is precisely the kind of divergence between the two
//! builds that would have made every native test worthless. Slices cross unchanged, so the
//! two builds differ only in what sits at the bottom.
//!
//! The stub is **not** a mock host. It answers the shape of a call, not its meaning: there
//! is no router behind `emit` and no expression interpreter behind `prop`. `TestHost`
//! (SDK §6.1, eieio-7d8.4) is the real thing and evaluates properties with the real `expr`
//! interpreter. Anything here that started deciding what a call *means* would be a second,
//! worse host, and the two would drift.

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod wasm {
    use eio_abi::Level;

    // All imports are from `eio:*` namespaces, and a module importing anything else is
    // rejected at load time (ABI §4.3). This is the only `wasm_import_module` in the
    // crate, and these seven names are exactly ABI §7.0's table, in its order.
    //
    // SAFETY: declaring these is what makes the block `unsafe extern`, and ABI §7.0 and
    // §8 are what discharge it. Each function is individually `safe` to call because the
    // host side of every one is specified *total* over its arguments: a bad port, a bad
    // index, a bad pointer and an oversized length are all status codes (ABI §6.2, §7.1,
    // §8), never undefined behaviour. The signatures are `i32`/`i64` only, so there is no
    // type for a caller to get wrong, and every pointer reaching them is derived from a
    // live slice by the wrappers below rather than by a caller. That is also what keeps
    // the unsafe budget (SDK §4) to this declaration plus the allocator.
    #[link(wasm_import_module = "eio:core")]
    unsafe extern "C" {
        safe fn log(level: i32, ptr: i32, len: i32);
        safe fn emit(port: i32, ptr: i32, len: i32) -> i32;
        safe fn prop(prop_id: i32, signal_idx: i32, buf: i32, cap: i32) -> i32;
        safe fn error(code: i32, ptr: i32, len: i32);
        safe fn time_unix_ms() -> i64;
        safe fn time_mono_ms() -> i64;
        safe fn rand(buf: i32, len: i32) -> i32;
    }

    /// The `(ptr, len)` ABI §3 carries for a slice the host will *read* (ABI §9.3).
    ///
    /// No `unsafe` is needed: taking an address is safe, and the host's obligation not to
    /// retain it past the call is ABI §9.3's rather than something a guest could enforce.
    /// The bytes stay alive across the call because the slice is a live borrow held over
    /// it.
    fn out(bytes: &[u8]) -> (i32, i32) {
        (bytes.as_ptr() as usize as i32, bytes.len() as i32)
    }

    /// The same, for a buffer the host will *write* (ABI §9.4).
    ///
    /// Derived from a `&mut` so the pointer's provenance permits the host's write.
    fn out_mut(bytes: &mut [u8]) -> (i32, i32) {
        (bytes.as_mut_ptr() as usize as i32, bytes.len() as i32)
    }

    /// `log` (ABI §7.0).
    pub fn host_log(level: Level, message: &str) {
        let (ptr, len) = out(message.as_bytes());
        log(level.as_i32(), ptr, len);
    }

    /// `emit` (ABI §6.2). Status convention.
    pub fn host_emit(port: i32, batch: &[u8]) -> i32 {
        let (ptr, len) = out(batch);
        emit(port, ptr, len)
    }

    /// `prop` (ABI §7.1). Size convention.
    pub fn host_prop(prop_id: i32, signal_idx: i32, buffer: &mut [u8]) -> i32 {
        let (buf, cap) = out_mut(buffer);
        prop(prop_id, signal_idx, buf, cap)
    }

    /// `error` (ABI §8).
    pub fn host_error(code: i32, detail: &str) {
        let (ptr, len) = out(detail.as_bytes());
        error(code, ptr, len);
    }

    /// `time_unix_ms` (ABI §7.0).
    pub fn host_time_unix_ms() -> i64 {
        time_unix_ms()
    }

    /// `time_mono_ms` (ABI §7.0).
    pub fn host_time_mono_ms() -> i64 {
        time_mono_ms()
    }

    /// `rand` (ABI §7.0). Status convention over a `len`.
    pub fn host_rand(buffer: &mut [u8]) -> i32 {
        let (buf, len) = out_mut(buffer);
        rand(buf, len)
    }
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use wasm::{
    host_emit as emit, host_error as error, host_log as log, host_prop as prop, host_rand as rand,
    host_time_mono_ms as time_mono_ms, host_time_unix_ms as time_unix_ms,
};

#[cfg(all(
    not(all(target_arch = "wasm32", target_os = "unknown")),
    not(target_os = "none")
))]
pub use stub::{
    Call, Recorder, emit, error, log, prop, rand, recorded, time_mono_ms, time_unix_ms,
};

#[cfg(target_os = "none")]
pub use inert::{emit, error, log, prop, rand, time_mono_ms, time_unix_ms};

#[cfg(all(
    not(all(target_arch = "wasm32", target_os = "unknown")),
    not(target_os = "none")
))]
mod stub {
    extern crate std;

    use eio_abi::Level;

    use alloc::collections::VecDeque;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use std::thread_local;

    /// One recorded call into the host, with any payload already copied.
    ///
    /// Copied for the same reason the real host copies it (ABI §9.3): the guest owns that
    /// memory and may free it the moment the call returns.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Call {
        /// `log` — the level and the message.
        Log(Level, String),
        /// `emit` — the port and the CBOR payload.
        Emit(i32, Vec<u8>),
        /// `prop` — the `prop_id` and the `signal_idx`. The answer comes from whatever the
        /// test queued.
        Prop(i32, i32),
        /// `error` — the code and the detail.
        Error(i32, String),
        /// `time_unix_ms`.
        TimeUnixMs,
        /// `time_mono_ms`.
        TimeMonoMs,
        /// `rand` — the number of bytes asked for.
        Rand(usize),
    }

    #[derive(Default)]
    struct State {
        calls: Vec<Call>,
        /// Answers `prop` hands out, in order. An exhausted queue is `ERR_NOT_FOUND`,
        /// which is ABI §7.1's answer for a property with no value at all — the one a test
        /// that queued nothing is actually asking about.
        prop_answers: VecDeque<Vec<u8>>,
        unix_ms: i64,
        mono_ms: i64,
        /// The byte `rand` fills with, so a test can tell host bytes from untouched ones
        /// without needing an RNG in the stub.
        rand_fill: u8,
    }

    thread_local! {
        static STATE: RefCell<State> = RefCell::new(State::default());
    }

    /// A scoped handle over the recording stub, one per test.
    ///
    /// The state is thread-local and `cargo test` runs tests on many threads, so each test
    /// gets its own without coordinating. Creating one clears whatever the thread was
    /// holding, which matters because the harness reuses threads across tests.
    #[derive(Debug)]
    pub struct Recorder {
        _private: (),
    }

    impl Default for Recorder {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Recorder {
        /// Starts recording on this thread, discarding anything a previous test left.
        pub fn new() -> Recorder {
            STATE.with(|state| *state.borrow_mut() = State::default());
            Recorder { _private: () }
        }

        /// Queues the bytes the next `prop` call answers with.
        pub fn queue_prop(&self, answer: &[u8]) -> &Self {
            STATE.with(|state| state.borrow_mut().prop_answers.push_back(answer.to_vec()));
            self
        }

        /// Sets what `time_unix_ms` and `time_mono_ms` return.
        pub fn set_clocks(&self, unix_ms: i64, mono_ms: i64) -> &Self {
            STATE.with(|state| {
                let mut state = state.borrow_mut();
                state.unix_ms = unix_ms;
                state.mono_ms = mono_ms;
            });
            self
        }

        /// Sets the byte `rand` fills buffers with.
        pub fn set_rand_fill(&self, byte: u8) -> &Self {
            STATE.with(|state| state.borrow_mut().rand_fill = byte);
            self
        }

        /// Every call recorded on this thread, in order.
        pub fn calls(&self) -> Vec<Call> {
            recorded()
        }
    }

    /// Every call recorded on this thread, in order.
    pub fn recorded() -> Vec<Call> {
        STATE.with(|state| state.borrow().calls.clone())
    }

    fn record(call: Call) {
        STATE.with(|state| state.borrow_mut().calls.push(call));
    }

    /// Records the message.
    pub fn log(level: Level, message: &str) {
        record(Call::Log(level, message.to_string()));
    }

    /// Records the emission and accepts it. There is no router behind this and no port
    /// validation: `TestHost` (eieio-7d8.4) is where a refusal comes from.
    pub fn emit(port: i32, batch: &[u8]) -> i32 {
        record(Call::Emit(port, batch.to_vec()));
        0
    }

    /// Answers from the queued list, honouring ABI §8's size convention so that `Ctx`'s
    /// grow-and-retry loop is exercised rather than merely compiled.
    pub fn prop(prop_id: i32, signal_idx: i32, buffer: &mut [u8]) -> i32 {
        record(Call::Prop(prop_id, signal_idx));
        let answer = STATE.with(|state| state.borrow_mut().prop_answers.pop_front());
        let Some(answer) = answer else {
            // ABI §7.1: no value at all.
            return eio_abi::ErrorCode::NotFound.as_i32();
        };
        if answer.len() > buffer.len() {
            // ABI §8: nothing written, this much required. The answer goes back so the
            // caller's retry — which is the thing under test — finds it.
            let required = answer.len() as i32;
            STATE.with(|state| state.borrow_mut().prop_answers.push_front(answer));
            return required;
        }
        buffer[..answer.len()].copy_from_slice(&answer);
        answer.len() as i32
    }

    /// Records the structured detail accompanying a non-zero callback return.
    pub fn error(code: i32, detail: &str) {
        record(Call::Error(code, detail.to_string()));
    }

    /// Returns whatever [`Recorder::set_clocks`] was given.
    pub fn time_unix_ms() -> i64 {
        record(Call::TimeUnixMs);
        STATE.with(|state| state.borrow().unix_ms)
    }

    /// Returns whatever [`Recorder::set_clocks`] was given.
    pub fn time_mono_ms() -> i64 {
        record(Call::TimeMonoMs);
        STATE.with(|state| state.borrow().mono_ms)
    }

    /// Fills the buffer with [`Recorder::set_rand_fill`]'s byte. Deterministic on purpose:
    /// a test asserting on randomness would assert on nothing.
    pub fn rand(buffer: &mut [u8]) -> i32 {
        record(Call::Rand(buffer.len()));
        let fill = STATE.with(|state| state.borrow().rand_fill);
        buffer.fill(fill);
        0
    }
}

/// The `eio:core` surface on a target with no host behind it (`target_os = "none"`).
///
/// These builds exist to prove the crate is `no_std` — a block runs on `wasm32`, and the
/// leaf runtime is a *host*, not a guest, so nothing here is ever called. The functions
/// exist so that `Ctx` and everything above it is compiled on those targets too, which is
/// what makes the proof worth running.
///
/// `ERR_UNSUPPORTED` rather than a panic or a plausible-looking success: ABI §8 defines it
/// as "a valid call, unimplemented on this host", and that is exactly true here. A stub
/// that returned `0` would let a future caller believe it had emitted something.
#[cfg(target_os = "none")]
mod inert {
    use eio_abi::{ErrorCode, Level};

    /// Discards the message.
    pub fn log(_level: Level, _message: &str) {}

    /// `ERR_UNSUPPORTED`: there is no router here.
    pub fn emit(_port: i32, _batch: &[u8]) -> i32 {
        ErrorCode::Unsupported.as_i32()
    }

    /// `ERR_UNSUPPORTED`: there is no expression interpreter here.
    pub fn prop(_prop_id: i32, _signal_idx: i32, _buffer: &mut [u8]) -> i32 {
        ErrorCode::Unsupported.as_i32()
    }

    /// Discards the detail.
    pub fn error(_code: i32, _detail: &str) {}

    /// Zero: there is no host clock here.
    pub fn time_unix_ms() -> i64 {
        0
    }

    /// Zero: there is no host clock here.
    pub fn time_mono_ms() -> i64 {
        0
    }

    /// `ERR_UNSUPPORTED`: there is no host entropy here.
    pub fn rand(_buffer: &mut [u8]) -> i32 {
        ErrorCode::Unsupported.as_i32()
    }
}

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

    // The capability namespaces (ABI §7.2–§7.6). Declared unconditionally, and that costs
    // nothing: WASM emits an import only for a function something *references*, so a block
    // that never calls a timer imports no `eio:timer` — which is exactly what ABI §4.3
    // wants, since a module's imports must not exceed its declared capabilities. Measured,
    // not assumed: `eio:core` declares seven and a real block imported the four it used.
    //
    // SAFETY: ABI §7.2 and §8, and the same reasoning as `eio:core` above — every one of
    // these is specified total over its arguments, answering under §8's status, size or id
    // convention, so a bad key or an undersized buffer is a code and never undefined
    // behaviour. Each is `safe fn` for that reason, and every pointer reaching one is
    // derived from a live slice by the wrappers in `capability`.
    #[link(wasm_import_module = "eio:state")]
    unsafe extern "C" {
        safe fn state_get(key: i32, key_len: i32, buf: i32, cap: i32) -> i32;
        safe fn state_put(key: i32, key_len: i32, val: i32, val_len: i32) -> i32;
        safe fn state_del(key: i32, key_len: i32) -> i32;
    }

    // SAFETY: ABI §7.3 and §8, as for `eio:state` above — every function is
    // specified total over its arguments and answers under §8's conventions, so
    // a bad delay or timer id is a status code and never undefined behaviour. That is why each is
    // `safe fn`, and every pointer reaching one is derived from a live slice by the
    // wrappers below.
    #[link(wasm_import_module = "eio:timer")]
    unsafe extern "C" {
        safe fn timer_set(delay_ms: i64, repeat: i32) -> i32;
        safe fn timer_cancel(timer_id: i32) -> i32;
    }

    // SAFETY: ABI §7.4 and §8, as for `eio:state` above — every function is
    // specified total over its arguments and answers under §8's conventions, so
    // a bad pin, mode, edge or watch id is a status code and never undefined behaviour. That is why each is
    // `safe fn`, and every pointer reaching one is derived from a live slice by the
    // wrappers below.
    #[link(wasm_import_module = "eio:gpio")]
    unsafe extern "C" {
        safe fn gpio_mode(pin: i32, mode: i32) -> i32;
        safe fn gpio_read(pin: i32) -> i32;
        safe fn gpio_write(pin: i32, value: i32) -> i32;
        safe fn gpio_watch(pin: i32, edge: i32) -> i32;
        safe fn gpio_unwatch(watch_id: i32) -> i32;
    }

    // SAFETY: ABI §7.5 and §8, as for `eio:state` above — every function is
    // specified total over its arguments and answers under §8's conventions, so
    // a bad bus, address or buffer is a status code and never undefined behaviour. That is why each is
    // `safe fn`, and every pointer reaching one is derived from a live slice by the
    // wrappers below.
    #[link(wasm_import_module = "eio:i2c")]
    unsafe extern "C" {
        safe fn i2c_write(bus: i32, addr: i32, ptr: i32, len: i32) -> i32;
        safe fn i2c_read(bus: i32, addr: i32, buf: i32, cap: i32) -> i32;
        safe fn i2c_write_read(
            bus: i32,
            addr: i32,
            wptr: i32,
            wlen: i32,
            buf: i32,
            cap: i32,
        ) -> i32;
    }

    // SAFETY: ABI §7.6 and §8, as for `eio:state` above — every function is
    // specified total over its arguments and answers under §8's conventions, so
    // a malformed request map is a status code and never undefined behaviour. That is why each is
    // `safe fn`, and every pointer reaching one is derived from a live slice by the
    // wrappers below.
    #[link(wasm_import_module = "eio:http")]
    unsafe extern "C" {
        safe fn http_request(ptr: i32, len: i32) -> i32;
    }

    /// `state_get` (ABI §7.2). Size convention.
    pub fn host_state_get(key: &str, buffer: &mut [u8]) -> i32 {
        let (key_ptr, key_len) = out(key.as_bytes());
        let (buf, cap) = out_mut(buffer);
        state_get(key_ptr, key_len, buf, cap)
    }

    /// `state_put` (ABI §7.2).
    pub fn host_state_put(key: &str, value: &[u8]) -> i32 {
        let (key_ptr, key_len) = out(key.as_bytes());
        let (val, val_len) = out(value);
        state_put(key_ptr, key_len, val, val_len)
    }

    /// `state_del` (ABI §7.2).
    pub fn host_state_del(key: &str) -> i32 {
        let (key_ptr, key_len) = out(key.as_bytes());
        state_del(key_ptr, key_len)
    }

    /// `timer_set` (ABI §7.3). Id convention.
    pub fn host_timer_set(delay_ms: i64, repeat: bool) -> i32 {
        timer_set(delay_ms, i32::from(repeat))
    }

    /// `timer_cancel` (ABI §7.3).
    pub fn host_timer_cancel(timer_id: u32) -> i32 {
        timer_cancel(timer_id as i32)
    }

    /// `gpio_mode` (ABI §7.4).
    pub fn host_gpio_mode(pin: u32, mode: i32) -> i32 {
        gpio_mode(pin as i32, mode)
    }

    /// `gpio_read` (ABI §7.4): `0`/`1`, or an error.
    pub fn host_gpio_read(pin: u32) -> i32 {
        gpio_read(pin as i32)
    }

    /// `gpio_write` (ABI §7.4).
    pub fn host_gpio_write(pin: u32, value: i32) -> i32 {
        gpio_write(pin as i32, value)
    }

    /// `gpio_watch` (ABI §7.4). Id convention.
    pub fn host_gpio_watch(pin: u32, edge: i32) -> i32 {
        gpio_watch(pin as i32, edge)
    }

    /// `gpio_unwatch` (ABI §7.4).
    pub fn host_gpio_unwatch(watch_id: u32) -> i32 {
        gpio_unwatch(watch_id as i32)
    }

    /// `i2c_write` (ABI §7.5).
    pub fn host_i2c_write(bus: u32, addr: u32, bytes: &[u8]) -> i32 {
        let (ptr, len) = out(bytes);
        i2c_write(bus as i32, addr as i32, ptr, len)
    }

    /// `i2c_read` (ABI §7.5). Size convention.
    pub fn host_i2c_read(bus: u32, addr: u32, buffer: &mut [u8]) -> i32 {
        let (buf, cap) = out_mut(buffer);
        i2c_read(bus as i32, addr as i32, buf, cap)
    }

    /// `i2c_write_read` (ABI §7.5). Size convention.
    pub fn host_i2c_write_read(bus: u32, addr: u32, write: &[u8], buffer: &mut [u8]) -> i32 {
        let (wptr, wlen) = out(write);
        let (buf, cap) = out_mut(buffer);
        i2c_write_read(bus as i32, addr as i32, wptr, wlen, buf, cap)
    }

    /// `http_request` (ABI §7.6). Id convention.
    pub fn host_http_request(request: &[u8]) -> i32 {
        let (ptr, len) = out(request);
        http_request(ptr, len)
    }
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use wasm::{
    host_emit as emit, host_error as error, host_gpio_mode as gpio_mode,
    host_gpio_read as gpio_read, host_gpio_unwatch as gpio_unwatch, host_gpio_watch as gpio_watch,
    host_gpio_write as gpio_write, host_http_request as http_request, host_i2c_read as i2c_read,
    host_i2c_write as i2c_write, host_i2c_write_read as i2c_write_read, host_log as log,
    host_prop as prop, host_rand as rand, host_state_del as state_del, host_state_get as state_get,
    host_state_put as state_put, host_time_mono_ms as time_mono_ms,
    host_time_unix_ms as time_unix_ms, host_timer_cancel as timer_cancel,
    host_timer_set as timer_set,
};

#[cfg(all(
    not(all(target_arch = "wasm32", target_os = "unknown")),
    not(target_os = "none")
))]
pub use stub::{
    Call, Recorder, StateAnswerer, emit, error, gpio_mode, gpio_read, gpio_unwatch, gpio_watch,
    gpio_write, http_request, i2c_read, i2c_write, i2c_write_read, log, prop, rand, recorded,
    set_prop_answerer, set_state_answerer, state_del, state_get, state_put, take_calls,
    time_mono_ms, time_unix_ms, timer_cancel, timer_set,
};

#[cfg(target_os = "none")]
pub use inert::{
    emit, error, gpio_mode, gpio_read, gpio_unwatch, gpio_watch, gpio_write, http_request,
    i2c_read, i2c_write, i2c_write_read, log, prop, rand, state_del, state_get, state_put,
    time_mono_ms, time_unix_ms, timer_cancel, timer_set,
};

#[cfg(all(
    not(all(target_arch = "wasm32", target_os = "unknown")),
    not(target_os = "none")
))]
mod stub {
    extern crate std;

    use eio_abi::Level;

    use alloc::collections::VecDeque;
    use alloc::rc::Rc;
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
        /// `state_get` — the key.
        StateGet(String),
        /// `state_put` — the key and the value.
        StatePut(String, Vec<u8>),
        /// `state_del` — the key.
        StateDel(String),
        /// `timer_set` — the delay and whether it repeats.
        TimerSet(i64, bool),
        /// `timer_cancel` — the timer id.
        TimerCancel(u32),
        /// `gpio_mode` — the pin and the ABI §7.4 mode number.
        GpioMode(u32, i32),
        /// `gpio_read` — the pin.
        GpioRead(u32),
        /// `gpio_write` — the pin and the level.
        GpioWrite(u32, i32),
        /// `gpio_watch` — the pin and the ABI §7.4 edge number.
        GpioWatch(u32, i32),
        /// `gpio_unwatch` — the watch id.
        GpioUnwatch(u32),
        /// `i2c_write` — bus, address, bytes.
        I2cWrite(u32, u32, Vec<u8>),
        /// `i2c_read` — bus and address.
        I2cRead(u32, u32),
        /// `i2c_write_read` — bus, address, the bytes written.
        I2cWriteRead(u32, u32, Vec<u8>),
        /// `http_request` — the encoded request map.
        HttpRequest(Vec<u8>),
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
        /// What the size-convention capability reads answer with, in order. Shared by
        /// `state_get` and `i2c_read`: both follow ABI §8's size convention, and a test
        /// exercising the grow-and-retry loop is exercising the same loop either way.
        reads: VecDeque<Vec<u8>>,
        /// Ids handed out by `timer_set`, `gpio_watch` and `http_request`, in order.
        ids: VecDeque<i32>,
        /// What `gpio_read` returns, in order.
        levels: VecDeque<i32>,
        /// A status every capability call returns instead of succeeding, if set — how a
        /// test reaches `ERR_THROTTLED` (ABI §7.2) without a real flash budget.
        refuse_with: Option<i32>,
        /// What answers `prop`, when something richer than a queue is driving.
        ///
        /// A seam in this stub, and it exists because `prop` is a call whose answer
        /// depends on *evaluating* something: ABI §7.1 makes a property an expression
        /// resolved per signal, so a queue can only replay answers a test worked out in
        /// advance, in the order the block happens to ask. `eio-test-host` installs the
        /// real protocol here — `host-core`'s `PropContext`, the same implementation a
        /// daemon runs. Checked *before* `prop_answers`, because once a real evaluator is
        /// installed there is no meaningful scripted property value to prefer over it.
        ///
        /// Emissions are recorded and read back, and timers, GPIO edges and HTTP
        /// completions are *callbacks* — a host drives those by calling the block, not by
        /// answering it — so neither needs a seam. `eio:state` does, for the reason given
        /// at [`State::state_answerer`], but with the opposite precedence.
        #[allow(clippy::type_complexity)]
        prop_answerer: Option<Rc<dyn Fn(i32, i32, &mut [u8]) -> i32>>,
        /// What answers `state_get`/`state_put`/`state_del`, when neither a refusal nor
        /// the scripted queue answered first (ABI §7.2).
        ///
        /// `TestHost` (eieio-7d8.23) installs a real key-value store here, the same way
        /// it installs `host-core`'s `PropContext` above — it runs a block as native
        /// Rust, with no wasm import table to register a handler on the way the
        /// reference conformance harness does (`eio_host_core::state::register`), so this
        /// stub is the only place such a seam can live.
        ///
        /// **Checked last, unlike `prop_answerer`.** A property's scripted queue is a
        /// fallback for when no real evaluator is installed; `eio:state`'s scripted queue
        /// is a first-class fault-injection surface a test relies on even when a real
        /// store *is* installed (a refusal, an oversized answer, SDK §6.1) — so the queue
        /// keeps priority and the store is what an empty queue falls through to.
        state_answerer: Option<StateAnswerer>,
    }

    /// What backs `eio:state` once neither a refusal nor the scripted queue answers
    /// first (see [`State::state_answerer`]).
    ///
    /// Three closures and not one: unlike `prop`, which is a single read, `eio:state` has
    /// three distinct shapes — a lookup that hands back bytes, a write, and a removal —
    /// bundled into one type so a caller installs and clears all three together, the same
    /// way ABI §4.3 grants a capability whole rather than function by function.
    #[derive(Clone)]
    pub struct StateAnswerer {
        /// Answers `state_get`. `None` becomes ABI §8's `ERR_NOT_FOUND` — a store with
        /// nothing under `key` is the ordinary case, not a failure (ABI §7.2).
        #[allow(clippy::type_complexity)]
        pub get: Rc<dyn Fn(&str) -> Option<Vec<u8>>>,
        /// Answers `state_put`.
        #[allow(clippy::type_complexity)]
        pub put: Rc<dyn Fn(&str, &[u8])>,
        /// Answers `state_del`. What this returns for a key that was never there is
        /// still open (eieio-7d8.16); this stub's callers return `0` either way, matching
        /// `host-core`'s reference implementation, and this closure is not asked to say
        /// which case it was in.
        pub del: Rc<dyn Fn(&str)>,
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

        /// A handle on whatever this thread is already recording, changing nothing.
        ///
        /// [`Recorder::new`] clears the state, which is right when a test *is* the thing
        /// driving. It is wrong for a caller layered above one — `eio-test-host` drains
        /// after every callback and scripts answers between them, so a handle that reset
        /// would throw away the answers it was about to queue.
        pub fn attach() -> Recorder {
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

        /// Queues bytes for the next size-convention read (`state_get`, `i2c_read`).
        pub fn queue_read(&self, answer: &[u8]) -> &Self {
            STATE.with(|state| state.borrow_mut().reads.push_back(answer.to_vec()));
            self
        }

        /// Queues an id for the next `timer_set`, `gpio_watch` or `http_request`.
        pub fn queue_id(&self, id: i32) -> &Self {
            STATE.with(|state| state.borrow_mut().ids.push_back(id));
            self
        }

        /// Queues a level for the next `gpio_read`.
        pub fn queue_level(&self, level: i32) -> &Self {
            STATE.with(|state| state.borrow_mut().levels.push_back(level));
            self
        }

        /// Makes every capability call refuse with `code` (ABI §8).
        pub fn refuse_with(&self, code: eio_abi::ErrorCode) -> &Self {
            STATE.with(|state| state.borrow_mut().refuse_with = Some(code.as_i32()));
            self
        }

        /// Every call recorded on this thread, in order.
        pub fn calls(&self) -> Vec<Call> {
            recorded()
        }
    }

    /// The refusal a test asked for, if any.
    fn refusal() -> Option<i32> {
        STATE.with(|state| state.borrow().refuse_with)
    }

    /// ABI §8's size convention over the queued reads, shared by `state_get` and
    /// `i2c_read` — the two capability calls that use it.
    fn sized_read(buffer: &mut [u8]) -> i32 {
        if let Some(code) = refusal() {
            return code;
        }
        let answer = STATE.with(|state| state.borrow_mut().reads.pop_front());
        let Some(answer) = answer else {
            return eio_abi::ErrorCode::NotFound.as_i32();
        };
        if answer.len() > buffer.len() {
            STATE.with(|state| state.borrow_mut().reads.push_front(answer.clone()));
            return answer.len() as i32;
        }
        buffer[..answer.len()].copy_from_slice(&answer);
        answer.len() as i32
    }

    /// An id-convention answer (ABI §8), or the refusal a test asked for.
    fn next_id() -> i32 {
        if let Some(code) = refusal() {
            return code;
        }
        STATE
            .with(|state| state.borrow_mut().ids.pop_front())
            .unwrap_or(0)
    }

    /// A status-convention answer, or the refusal a test asked for.
    fn ok_or_refusal() -> i32 {
        refusal().unwrap_or(0)
    }

    /// Installs what answers `prop` on this thread (see [`State::prop_answerer`]).
    ///
    /// Returns the previous answerer, so a caller can restore it. `Recorder::new` clears
    /// it, which is what keeps a test that installed one from leaking into the next.
    #[allow(clippy::type_complexity)]
    pub fn set_prop_answerer(
        answerer: Option<Rc<dyn Fn(i32, i32, &mut [u8]) -> i32>>,
    ) -> Option<Rc<dyn Fn(i32, i32, &mut [u8]) -> i32>> {
        STATE.with(|state| core::mem::replace(&mut state.borrow_mut().prop_answerer, answerer))
    }

    /// Installs what answers `eio:state`'s three calls once a refusal and the scripted
    /// queue have both had first refusal (see [`State::state_answerer`]).
    ///
    /// Returns the previous answerer, mirroring [`set_prop_answerer`] so a caller can
    /// restore it the same way. `Recorder::new` clears it along with everything else,
    /// which is what keeps one test's store from leaking into the next.
    pub fn set_state_answerer(answerer: Option<StateAnswerer>) -> Option<StateAnswerer> {
        STATE.with(|state| core::mem::replace(&mut state.borrow_mut().state_answerer, answerer))
    }

    /// Takes the calls recorded so far, leaving everything else in place.
    ///
    /// What a caller draining between callbacks needs, and distinct from
    /// [`Recorder::new`] in exactly the way that matters: `new` resets the whole stub,
    /// including queued answers and any installed `prop` answerer. Draining with it
    /// would discard the answers a test queued for the *next* callback, and unhook the
    /// host that was driving.
    pub fn take_calls() -> Vec<Call> {
        STATE.with(|state| core::mem::take(&mut state.borrow_mut().calls))
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
        // Taken out of the cell before calling, not called through the borrow: the
        // answerer reaches back into `host-core`, which is free to do anything, and a
        // `RefCell` held across that is a panic waiting for the first re-entrant read.
        let installed = STATE.with(|state| state.borrow().prop_answerer.clone());
        if let Some(answer) = installed {
            return answer(prop_id, signal_idx, buffer);
        }
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

    /// `state_get` (ABI §7.2), size convention.
    ///
    /// Precedence (see [`State::state_answerer`]): a refusal first, then the scripted
    /// queue if a test put something in it, then the installed store, then `ERR_NOT_FOUND`
    /// — a store with nothing under `key` is the same answer either way.
    pub fn state_get(key: &str, buffer: &mut [u8]) -> i32 {
        record(Call::StateGet(key.to_string()));
        if let Some(code) = refusal() {
            return code;
        }
        let queued = STATE.with(|state| state.borrow_mut().reads.pop_front());
        if let Some(answer) = queued {
            if answer.len() > buffer.len() {
                // ABI §8: nothing written, this much required. Put back so the retry —
                // which is the thing under test — finds it, same as `sized_read`.
                STATE.with(|state| state.borrow_mut().reads.push_front(answer.clone()));
                return answer.len() as i32;
            }
            buffer[..answer.len()].copy_from_slice(&answer);
            return answer.len() as i32;
        }
        let answerer = STATE.with(|state| state.borrow().state_answerer.clone());
        let Some(bytes) = answerer.and_then(|answerer| (answerer.get)(key)) else {
            return eio_abi::ErrorCode::NotFound.as_i32();
        };
        if bytes.len() > buffer.len() {
            // No push-back needed: the store answers the same key again for free, unlike
            // a queue that would otherwise have consumed its only copy.
            return bytes.len() as i32;
        }
        buffer[..bytes.len()].copy_from_slice(&bytes);
        bytes.len() as i32
    }

    /// `state_put` (ABI §7.2). See [`state_get`] for the precedence.
    pub fn state_put(key: &str, value: &[u8]) -> i32 {
        record(Call::StatePut(key.to_string(), value.to_vec()));
        if let Some(code) = refusal() {
            return code;
        }
        let answerer = STATE.with(|state| state.borrow().state_answerer.clone());
        if let Some(answerer) = answerer {
            (answerer.put)(key, value);
        }
        0
    }

    /// `state_del` (ABI §7.2). See [`state_get`] for the precedence.
    ///
    /// `0` whether or not the key was there, matching `host-core`'s reference
    /// implementation — ABI §7.2 does not say which it is and §8's `ERR_NOT_FOUND` is a
    /// plausible other reading, so the question is open (eieio-7d8.16) and this does not
    /// settle it.
    pub fn state_del(key: &str) -> i32 {
        record(Call::StateDel(key.to_string()));
        if let Some(code) = refusal() {
            return code;
        }
        let answerer = STATE.with(|state| state.borrow().state_answerer.clone());
        if let Some(answerer) = answerer {
            (answerer.del)(key);
        }
        0
    }

    /// `timer_set` (ABI §7.3), id convention.
    pub fn timer_set(delay_ms: i64, repeat: bool) -> i32 {
        record(Call::TimerSet(delay_ms, repeat));
        next_id()
    }

    /// `timer_cancel` (ABI §7.3).
    pub fn timer_cancel(timer_id: u32) -> i32 {
        record(Call::TimerCancel(timer_id));
        ok_or_refusal()
    }

    /// `gpio_mode` (ABI §7.4).
    pub fn gpio_mode(pin: u32, mode: i32) -> i32 {
        record(Call::GpioMode(pin, mode));
        ok_or_refusal()
    }

    /// `gpio_read` (ABI §7.4): `0`/`1`, or an error.
    pub fn gpio_read(pin: u32) -> i32 {
        record(Call::GpioRead(pin));
        if let Some(code) = refusal() {
            return code;
        }
        STATE
            .with(|state| state.borrow_mut().levels.pop_front())
            .unwrap_or(0)
    }

    /// `gpio_write` (ABI §7.4).
    pub fn gpio_write(pin: u32, value: i32) -> i32 {
        record(Call::GpioWrite(pin, value));
        ok_or_refusal()
    }

    /// `gpio_watch` (ABI §7.4), id convention.
    pub fn gpio_watch(pin: u32, edge: i32) -> i32 {
        record(Call::GpioWatch(pin, edge));
        next_id()
    }

    /// `gpio_unwatch` (ABI §7.4).
    pub fn gpio_unwatch(watch_id: u32) -> i32 {
        record(Call::GpioUnwatch(watch_id));
        ok_or_refusal()
    }

    /// `i2c_write` (ABI §7.5).
    pub fn i2c_write(bus: u32, addr: u32, bytes: &[u8]) -> i32 {
        record(Call::I2cWrite(bus, addr, bytes.to_vec()));
        ok_or_refusal()
    }

    /// `i2c_read` (ABI §7.5), size convention.
    pub fn i2c_read(bus: u32, addr: u32, buffer: &mut [u8]) -> i32 {
        record(Call::I2cRead(bus, addr));
        sized_read(buffer)
    }

    /// `i2c_write_read` (ABI §7.5), size convention.
    pub fn i2c_write_read(bus: u32, addr: u32, write: &[u8], buffer: &mut [u8]) -> i32 {
        record(Call::I2cWriteRead(bus, addr, write.to_vec()));
        sized_read(buffer)
    }

    /// `http_request` (ABI §7.6), id convention.
    pub fn http_request(request: &[u8]) -> i32 {
        record(Call::HttpRequest(request.to_vec()));
        next_id()
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

    /// `ERR_UNSUPPORTED`: there is no state store here.
    pub fn state_get(_key: &str, _buffer: &mut [u8]) -> i32 {
        ErrorCode::Unsupported.as_i32()
    }

    /// `ERR_UNSUPPORTED`: there is no state store here.
    pub fn state_put(_key: &str, _value: &[u8]) -> i32 {
        ErrorCode::Unsupported.as_i32()
    }

    /// `ERR_UNSUPPORTED`: there is no state store here.
    pub fn state_del(_key: &str) -> i32 {
        ErrorCode::Unsupported.as_i32()
    }

    /// `ERR_UNSUPPORTED`: there is no timer wheel here.
    pub fn timer_set(_delay_ms: i64, _repeat: bool) -> i32 {
        ErrorCode::Unsupported.as_i32()
    }

    /// `ERR_UNSUPPORTED`: there is no timer wheel here.
    pub fn timer_cancel(_timer_id: u32) -> i32 {
        ErrorCode::Unsupported.as_i32()
    }

    /// `ERR_UNSUPPORTED`: there are no pins here.
    pub fn gpio_mode(_pin: u32, _mode: i32) -> i32 {
        ErrorCode::Unsupported.as_i32()
    }

    /// `ERR_UNSUPPORTED`: there are no pins here.
    pub fn gpio_read(_pin: u32) -> i32 {
        ErrorCode::Unsupported.as_i32()
    }

    /// `ERR_UNSUPPORTED`: there are no pins here.
    pub fn gpio_write(_pin: u32, _value: i32) -> i32 {
        ErrorCode::Unsupported.as_i32()
    }

    /// `ERR_UNSUPPORTED`: there are no pins here.
    pub fn gpio_watch(_pin: u32, _edge: i32) -> i32 {
        ErrorCode::Unsupported.as_i32()
    }

    /// `ERR_UNSUPPORTED`: there are no pins here.
    pub fn gpio_unwatch(_watch_id: u32) -> i32 {
        ErrorCode::Unsupported.as_i32()
    }

    /// `ERR_UNSUPPORTED`: there is no bus here.
    pub fn i2c_write(_bus: u32, _addr: u32, _bytes: &[u8]) -> i32 {
        ErrorCode::Unsupported.as_i32()
    }

    /// `ERR_UNSUPPORTED`: there is no bus here.
    pub fn i2c_read(_bus: u32, _addr: u32, _buffer: &mut [u8]) -> i32 {
        ErrorCode::Unsupported.as_i32()
    }

    /// `ERR_UNSUPPORTED`: there is no bus here.
    pub fn i2c_write_read(_bus: u32, _addr: u32, _write: &[u8], _buffer: &mut [u8]) -> i32 {
        ErrorCode::Unsupported.as_i32()
    }

    /// `ERR_UNSUPPORTED`: there is no network here.
    pub fn http_request(_request: &[u8]) -> i32 {
        ErrorCode::Unsupported.as_i32()
    }
}

//! The export and import names of ABI-SPEC §4 and §7, in one place.
//!
//! Every one of these strings is a contract with the SDK's code generator (SDK §1) and
//! with hand-written blocks. Spelled once here so that a typo is a compile error in one
//! place rather than a block that mysteriously never receives a timer callback, and so
//! that `eio_` is the only prefix in the tree — `nio_*` is the defunct predecessor's, and
//! anything carrying it is a leftover (CLAUDE.md).

/// Required guest exports (ABI §4.1).
pub mod required {
    /// Packed ABI version, `(major << 16) | minor` (ABI §12).
    pub const ABI_VERSION: &str = "eio_abi_version";
    /// Guest allocator; returns a pointer or 0 (ABI §9.5).
    pub const ALLOC: &str = "eio_alloc";
    /// Releases an [`ALLOC`] allocation.
    pub const FREE: &str = "eio_free";
    /// Receives the instance descriptor (ABI §5.2).
    pub const CONFIGURE: &str = "eio_configure";
    /// Transition to running (ABI §5.1).
    pub const START: &str = "eio_start";
    /// Transition to stopped (ABI §5.1).
    pub const STOP: &str = "eio_stop";
    /// Delivers a batch on an input port (ABI §6.1).
    pub const PROCESS_SIGNALS: &str = "eio_process_signals";

    /// All of them, for a loader's presence check.
    pub const ALL: [&str; 7] = [
        ABI_VERSION,
        ALLOC,
        FREE,
        CONFIGURE,
        START,
        STOP,
        PROCESS_SIGNALS,
    ];
}

/// Optional guest exports — present only with the paired capability (ABI §4.2).
///
/// The pairing is required in both directions: importing `eio:timer` without exporting
/// `eio_on_timer` is a rejection, and so is the reverse, because such an export is a
/// callback the host can never invoke. `eio_manifest`'s module cross-check is what
/// enforces it at load time; this module is only the names.
pub mod optional {
    /// A timer fired. Paired with capability `timer`.
    pub const ON_TIMER: &str = "eio_on_timer";
    /// A watched GPIO line changed. Paired with capability `gpio`.
    pub const ON_GPIO: &str = "eio_on_gpio";
    /// An HTTP response arrived. Paired with capability `http`.
    pub const ON_HTTP: &str = "eio_on_http";

    /// All of them.
    pub const ALL: [&str; 3] = [ON_TIMER, ON_GPIO, ON_HTTP];
}

/// Import namespaces (ABI §7). The capability system *is* the import section (ABI §1.5).
pub mod namespace {
    /// Always available, requires no manifest capability (ABI §7.0).
    pub const CORE: &str = "eio:core";
    /// Capability `state` (ABI §7.2).
    pub const STATE: &str = "eio:state";
    /// Capability `timer` (ABI §7.3).
    pub const TIMER: &str = "eio:timer";
    /// Capability `gpio` (ABI §7.4).
    pub const GPIO: &str = "eio:gpio";
    /// Capability `i2c` (ABI §7.5).
    pub const I2C: &str = "eio:i2c";
    /// Capability `http` (ABI §7.6).
    pub const HTTP: &str = "eio:http";
}

/// The `eio:core` functions (ABI §7.0).
///
/// Named here because the driver has to know which names are always available in order to
/// answer the capability question; what they *mean* is not this crate's yet. `prop` lands
/// with the property protocol (eieio-35h.2), `emit` with the router (eieio-35h.5).
pub mod core_fn {
    /// `(level, ptr, len) -> ()` — UTF-8 message, levels 0=trace..4=error.
    pub const LOG: &str = "log";
    /// `(port, ptr, len) -> i32` — enqueue a batch (ABI §6.2).
    pub const EMIT: &str = "emit";
    /// `(prop_id, signal_idx, buf, cap) -> i32` — evaluate a property (ABI §7.1).
    pub const PROP: &str = "prop";
    /// `(code, ptr, len) -> ()` — detail accompanying a non-zero callback return.
    pub const ERROR: &str = "error";
    /// `() -> i64` — wall clock, host-mediated for determinism.
    pub const TIME_UNIX_MS: &str = "time_unix_ms";
    /// `() -> i64` — monotonic clock.
    pub const TIME_MONO_MS: &str = "time_mono_ms";
    /// `(buf, len) -> i32` — host RNG, same rationale as the clocks.
    pub const RAND: &str = "rand";

    /// All of them.
    pub const ALL: [&str; 7] = [LOG, EMIT, PROP, ERROR, TIME_UNIX_MS, TIME_MONO_MS, RAND];
}

/// The custom section a self-describing module carries its manifest in (ABI §4.4).
pub const MANIFEST_SECTION: &str = "eio:manifest";

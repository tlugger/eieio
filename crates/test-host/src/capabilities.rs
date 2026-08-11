//! Scripting what the capability stubs answer (SDK §6.1).
//!
//! The *answers* only. A timer firing, a GPIO edge and an HTTP completion are callbacks,
//! and a host drives those by calling the block — [`TestHost::fire_timer`],
//! [`TestHost::fire_gpio`], [`TestHost::complete_http`]. What is scripted here is the other
//! direction: what the host says when the block asks.
//!
//! [`TestHost::fire_timer`]: crate::TestHost::fire_timer
//! [`TestHost::fire_gpio`]: crate::TestHost::fire_gpio
//! [`TestHost::complete_http`]: crate::TestHost::complete_http

use eio_sdk::raw::Recorder;
use eio_sdk::{ErrorCode, Value};

/// A scripted refusal, so a test can reach a code without the condition that causes it
/// (SDK §6.1).
///
/// Two codes, not all of ABI §8's nine, because these are the two a block meets through a
/// *granted* capability. `ERR_CAPABILITY` and `ERR_UNSUPPORTED` are refusals of the
/// capability itself, which SCOPE §3.3 settles at deploy validation — a block holding a
/// wrapper has already been granted the namespace. More variants can be added when a block
/// needs one; guessing now would be shipping API on speculation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Throttle {
    /// `ERR_THROTTLED` — what a leaf host answers `state_put` with when a flash wear
    /// budget is exhausted (ABI §7.2). The reason this exists: the condition is a property
    /// of the *hardware*, so a block's back-off path is otherwise untestable.
    Throttled,
    /// `ERR_IO` — the device or transport failed (ABI §8).
    Io,
}

impl Throttle {
    const fn code(self) -> ErrorCode {
        match self {
            Throttle::Throttled => ErrorCode::Throttled,
            Throttle::Io => ErrorCode::Io,
        }
    }
}

/// What the capability stubs answer next (SDK §6.1).
///
/// Queued rather than set: a block that reads twice gets two answers, which is what lets a
/// test script a sensor that changes between polls.
#[derive(Debug)]
pub struct Scripted<'host> {
    recorder: Recorder,
    _host: core::marker::PhantomData<&'host mut ()>,
}

impl Scripted<'_> {
    pub(crate) fn new() -> Scripted<'static> {
        Scripted {
            // Attaches to the thread's existing recorder rather than clearing it: the host
            // drains after every callback, and a `Scripted` that reset the state would
            // discard answers queued before the delivery it was queued for.
            recorder: Recorder::attach(),
            _host: core::marker::PhantomData,
        }
    }

    /// The bytes the next `state_get` returns.
    pub fn state(&self, value: &Value) -> &Self {
        self.recorder.queue_read(&value.to_cbor());
        self
    }

    /// The raw bytes the next size-convention read returns — `state_get` or `i2c_read`.
    pub fn read(&self, bytes: &[u8]) -> &Self {
        self.recorder.queue_read(bytes);
        self
    }

    /// The id the next `timer_set`, `gpio_watch` or `http_request` is assigned.
    ///
    /// Worth scripting rather than counting from zero: ABI §8 makes `0` a *valid* id, and a
    /// block that treats it as a failure should be caught by a test that hands it one.
    pub fn id(&self, id: u32) -> &Self {
        self.recorder.queue_id(id as i32);
        self
    }

    /// The level the next `gpio_read` returns.
    pub fn level(&self, level: eio_sdk::PinLevel) -> &Self {
        self.recorder.queue_level(level.as_i32());
        self
    }

    /// A raw level, for the values ABI §7.4 does not define.
    ///
    /// A host answering `gpio_read` with anything but `0`, `1` or an error is
    /// non-conformant, and a block should not silently believe it. This is how a test
    /// checks that it does not.
    pub fn raw_level(&self, value: i32) -> &Self {
        self.recorder.queue_level(value);
        self
    }

    /// Makes every subsequent capability call refuse with this code.
    pub fn refuse(&self, refusal: Throttle) -> &Self {
        self.recorder.refuse_with(refusal.code());
        self
    }
}

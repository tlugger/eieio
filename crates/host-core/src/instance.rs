//! The lifecycle driver (ABI-SPEC §5.1).
//!
//! ```text
//! instantiate → CONFIGURED → RUNNING → STOPPED
//!                   ↑            |
//!                   └── (restart = new instance; no re-start of a stopped instance)
//! trap (any state) → DEAD
//! ```
//!
//! # The states are types
//!
//! [`Configured`], [`Running`] and [`Stopped`] are separate types, and every guest call
//! consumes the instance and hands back its next state. That is what makes ABI §5.1's
//! illegal transitions unwriteable rather than merely untested:
//!
//! - A stopped instance cannot be restarted, because [`Stopped`] has no `start`. "Restart =
//!   new instance" is not a rule to remember; it is the only thing the type system leaves
//!   available.
//! - A dead instance cannot be called, because death is reported as a [`Trap`] and the
//!   instance is *not* returned with it. There is nothing left to call.
//! - A batch cannot be delivered before `start`, because [`Configured`] has no
//!   `process_signals`.
//! - An instance cannot be used twice from one state, because the call moved it.
//!
//! # Traps are death, status codes are life
//!
//! ABI §8's rule is the shape of every return here. A non-zero callback status comes back
//! *with the live instance* ([`Outcome::Live`]) and is logged and counted; a trap, fuel
//! exhaustion or deadline violation comes back as [`Outcome::Dead`] and carries no
//! instance at all. A caller cannot treat a status as fatal without deliberately dropping a
//! live instance, and cannot treat death as recoverable, because there is nothing to
//! recover.
//!
//! `eio_configure` is the one exception, and the spec's, not this crate's: ABI §5.1 makes a
//! non-zero configure return a *configuration rejection* — "instance is discarded and the
//! error surfaced to the deployer" — so [`Configuring::Rejected`] withholds the instance
//! exactly as death does.
//!
//! # The illegal transitions, as compile errors
//!
//! These are the tests that cannot be written as tests: a test asserting that
//! `stopped.start()` returns an error would mean the method existed. Each of the four is
//! pinned by the error code it produces, so it stays a proof rather than becoming a
//! doctest that fails for some new reason.
//!
//! A stopped instance is never restarted (ABI §5.1 step 5):
//!
//! ```compile_fail,E0599
//! use eio_host_core::{Engine, Stopped};
//! fn restart<E: Engine>(stopped: Stopped<E>) {
//!     stopped.start();
//! }
//! ```
//!
//! Nor stopped twice, nor asked to process anything:
//!
//! ```compile_fail,E0599
//! use eio_host_core::{Engine, Stopped};
//! fn deliver_to_a_stopped_instance<E: Engine>(stopped: Stopped<E>) {
//!     stopped.process_signals(0, b"\x80");
//! }
//! ```
//!
//! A batch cannot arrive before `start` — delivery begins after `eio_start` returns zero
//! (ABI §5.1 step 3):
//!
//! ```compile_fail,E0599
//! use eio_host_core::{Configured, Engine};
//! fn deliver_before_start<E: Engine>(configured: Configured<E>) {
//!     configured.process_signals(0, b"\x80");
//! }
//! ```
//!
//! And an instance cannot be driven twice from one state, because the call moved it — which
//! is what makes "the instance is gone" mean something after a trap:
//!
//! ```compile_fail,E0382
//! use eio_host_core::{Engine, Running};
//! fn stop_twice<E: Engine>(running: Running<E>) {
//!     let _ = running.stop();
//!     let _ = running.stop();
//! }
//! ```

use alloc::string::String;
use alloc::vec::Vec;

use crate::descriptor::Descriptor;
use crate::engine::{Engine, Trap, TrapKind};
use crate::exports::{optional, required};
use crate::memory::{DeliveryFailure, Inbound};
use crate::status::{ErrorCode, Status};

/// What a guest call did, for a call that leaves the instance in state `T`.
///
/// The two arms are ABI §8's two halves. [`Outcome::Live`] carries the instance onward
/// whatever the status was; [`Outcome::Dead`] carries only the reason.
#[derive(Debug)]
pub enum Outcome<T> {
    /// The guest returned. The instance lives, in its next state, and said this.
    Live(T, Status),
    /// The instance died (ABI §5.1: any trap, fuel exhaustion, or deadline violation).
    Dead(Trap),
}

impl<T> Outcome<T> {
    /// The instance, if it survived — discarding what it said.
    ///
    /// For callers that have already dealt with the status, or that treat a block-level
    /// error as nothing to act on beyond the count the driver keeps.
    pub fn live(self) -> Option<T> {
        match self {
            Outcome::Live(state, _) => Some(state),
            Outcome::Dead(_) => None,
        }
    }

    /// The trap, if the instance died.
    pub fn dead(self) -> Option<Trap> {
        match self {
            Outcome::Live(..) => None,
            Outcome::Dead(trap) => Some(trap),
        }
    }
}

/// What `eio_configure` did (ABI §5.1 step 2).
///
/// Three outcomes rather than two, because a non-zero configure return is not a
/// block-level error to count and continue past — it is the block refusing this
/// configuration, and §5.1 says the instance is discarded and the error surfaced to the
/// deployer. [`Configuring::Rejected`] therefore carries no instance, so "discarded" is
/// what the type does rather than what a comment asks for.
#[derive(Debug)]
pub enum Configuring<E> {
    /// The guest accepted its configuration.
    Configured(Configured<E>),
    /// The guest refused it. The instance is gone; report the code to the deployer.
    Rejected(ErrorCode),
    /// The guest died while configuring.
    Dead(Trap),
}

/// What `eio_start` did (ABI §5.1 step 3).
///
/// A non-zero start return is a block-level error, so ABI §8 keeps the instance alive —
/// but §5.1 begins delivery only "after a zero return", so it is not RUNNING either. It
/// stays [`Configured`], and what a host does next (retry, or discard) is supervision
/// policy, which is an open question (SCOPE §3.13).
#[derive(Debug)]
pub enum Starting<E> {
    /// The guest started. Delivery may begin.
    Running(Running<E>),
    /// The guest refused to start. Still configured, not running, not dead.
    Refused(Configured<E>, ErrorCode),
    /// The guest died while starting.
    Dead(Trap),
}

/// The shared innards of an instance in any state.
///
/// Private, and reached only through the state types, so there is no way to hold one of
/// these and call whatever you like on it.
#[derive(Debug)]
struct Live<E> {
    engine: E,
    instance_id: String,
    /// Non-zero callback returns, counted (ABI §8). Saturating: a block failing four
    /// billion times has made its point.
    errors: u32,
}

impl<E: Engine> Live<E> {
    /// Calls `export` with no payload and decodes the status convention.
    fn call(&mut self, export: &str, args: &[i32]) -> Result<Status, Trap> {
        let status = Status::decode(self.engine.call(export, args)?);
        self.count(status);
        Ok(status)
    }

    /// Records a non-zero return (ABI §8: the host logs it, counts it, and continues).
    ///
    /// Counting is here; *logging* is not, because this crate has no logger and the leaf
    /// tier's log sink is nothing like the daemon's. A caller reads the status it was
    /// handed and logs it however it logs things.
    fn count(&mut self, status: Status) {
        if !status.is_ok() {
            self.errors = self.errors.saturating_add(1);
        }
    }
}

/// An instance that has accepted its configuration (ABI §5.1).
///
/// Cannot receive batches: delivery begins after `eio_start` returns zero, so
/// `process_signals` does not exist until [`Running`].
#[derive(Debug)]
pub struct Configured<E> {
    inner: Live<E>,
}

/// A running instance (ABI §5.1) — the only state that receives callbacks.
#[derive(Debug)]
pub struct Running<E> {
    inner: Live<E>,
}

/// A stopped instance (ABI §5.1 step 5).
///
/// Terminal by construction: there is no `start`, and no method that returns a
/// [`Running`]. "A stopped instance is never restarted; service restart creates fresh
/// instances" is therefore not enforceable-by-review, it is the only thing available.
/// [`Stopped::into_engine`] is the way out, for a caller that has teardown of its own to do.
#[derive(Debug)]
pub struct Stopped<E> {
    inner: Live<E>,
}

impl<E: Engine> Configured<E> {
    /// Instantiate → CONFIGURED: writes the descriptor and calls `eio_configure`
    /// (ABI §5.1 step 2, §5.2, §6.1).
    ///
    /// `engine` is an already-instantiated guest. Instantiation, and the export, import and
    /// ABI-version validation that goes with it (ABI §4, §12), belong to the caller: they
    /// happen before there is anything to drive, and they are where the engine's own API
    /// shows through most. [`check_required_exports`] and [`abi_version`] are here to help
    /// with the parts that only need the trait.
    ///
    /// The descriptor is delivered by ABI §6.1's convention — host allocates, host writes,
    /// guest frees — so a guest that keeps a pointer to it after returning has kept a
    /// pointer to memory it owns and freed.
    pub fn configure(mut engine: E, descriptor: &Descriptor) -> Configuring<E> {
        let bytes = descriptor.to_cbor();
        let payload = match Inbound::write(&mut engine, &bytes) {
            Ok(payload) => payload,
            // The guest could not take its own descriptor, so there is nothing configured
            // here and nothing to keep driving — the same shape as a refusal, reported with
            // the code that says why (ABI §9.5).
            Err(DeliveryFailure::Refused) => return Configuring::Rejected(ErrorCode::Limit),
            Err(DeliveryFailure::Dead(trap)) => return Configuring::Dead(trap),
        };
        match payload.call(&mut engine, required::CONFIGURE, &[]) {
            Ok(Status::Ok) => Configuring::Configured(Configured {
                inner: Live {
                    engine,
                    instance_id: descriptor.instance_id.clone(),
                    errors: 0,
                },
            }),
            // ABI §5.1: a non-zero configure return discards the instance. Dropping
            // `engine` here is that, and it is why this arm cannot hand one back.
            Ok(Status::Failed(code)) => Configuring::Rejected(code),
            Err(trap) => Configuring::Dead(trap),
        }
    }

    /// CONFIGURED → RUNNING: calls `eio_start` (ABI §5.1 step 3).
    ///
    /// The guest may arm timers, register watches and emit initial signals here. Delivery
    /// begins only on a zero return — see [`Starting`] for what a non-zero one means.
    pub fn start(mut self) -> Starting<E> {
        match self.inner.call(required::START, &[]) {
            Ok(Status::Ok) => Starting::Running(Running { inner: self.inner }),
            Ok(Status::Failed(code)) => Starting::Refused(self, code),
            Err(trap) => Starting::Dead(trap),
        }
    }

    /// The instance id from its descriptor.
    pub fn instance_id(&self) -> &str {
        &self.inner.instance_id
    }

    /// How many non-zero callback returns this instance has produced (ABI §8).
    pub fn errors(&self) -> u32 {
        self.inner.errors
    }
}

impl<E: Engine> Running<E> {
    /// Delivers a batch on an input port (ABI §6.1).
    ///
    /// `batch` is canonical CBOR (ABI §6.3.1) — encode it with `eio_signal`. The sequence
    /// is §6.1's: the host allocates in the guest, writes, calls, and the *guest* frees.
    /// This crate never frees a delivered payload, because from the moment the callback
    /// begins the guest owns it (ABI §9.2), and a host-side free would be a second owner.
    ///
    /// Enforcing `max_payload` is the caller's: the driver has no opinion about which
    /// limits apply to which port, and ABI §9.7 leaves the numbers to host configuration
    /// with no floor (SCOPE §3.4). The descriptor already told the guest what they are.
    pub fn process_signals(mut self, input_port: u32, batch: &[u8]) -> Outcome<Running<E>> {
        let payload = match Inbound::write(&mut self.inner.engine, batch) {
            Ok(payload) => payload,
            // ABI §9.5: the guest declined the allocation. The batch was not delivered, the
            // instance is untouched, and the caller hears `ERR_LIMIT` — which is what a
            // refused payload is.
            Err(DeliveryFailure::Refused) => return self.refused(),
            Err(DeliveryFailure::Dead(trap)) => return Outcome::Dead(trap),
        };
        match payload.call(
            &mut self.inner.engine,
            required::PROCESS_SIGNALS,
            &[input_port as i32],
        ) {
            Ok(status) => {
                self.inner.count(status);
                Outcome::Live(self, status)
            }
            Err(trap) => Outcome::Dead(trap),
        }
    }

    /// A timer fired: calls `eio_on_timer` (ABI §4.2, §7.3).
    ///
    /// Check [`Running::handles`] first. A block without the `timer` capability does not
    /// export this, and a host that armed a timer for such a block has a bug of its own —
    /// load-time validation (ABI §4.2's paired-export rule, which `eio_manifest` enforces)
    /// is what makes that unreachable in a correct host.
    pub fn on_timer(self, timer_id: u32) -> Outcome<Running<E>> {
        self.call_optional(optional::ON_TIMER, &[timer_id as i32])
    }

    /// A watched GPIO line changed: calls `eio_on_gpio` (ABI §4.2, §7.4).
    pub fn on_gpio(self, watch_id: u32, value: i32) -> Outcome<Running<E>> {
        self.call_optional(optional::ON_GPIO, &[watch_id as i32, value])
    }

    /// An HTTP response arrived: calls `eio_on_http` (ABI §4.2, §7.6).
    ///
    /// The body is delivered by §6.1's convention like any other inbound payload, so the
    /// guest frees it.
    pub fn on_http(mut self, req_id: u32, status_code: i32, body: &[u8]) -> Outcome<Running<E>> {
        if !self.inner.engine.has_export(optional::ON_HTTP) {
            return Outcome::Dead(missing_export(optional::ON_HTTP));
        }
        let payload = match Inbound::write(&mut self.inner.engine, body) {
            Ok(payload) => payload,
            Err(DeliveryFailure::Refused) => return self.refused(),
            Err(DeliveryFailure::Dead(trap)) => return Outcome::Dead(trap),
        };
        match payload.call(
            &mut self.inner.engine,
            optional::ON_HTTP,
            &[req_id as i32, status_code],
        ) {
            Ok(status) => {
                self.inner.count(status);
                Outcome::Live(self, status)
            }
            Err(trap) => Outcome::Dead(trap),
        }
    }

    /// RUNNING → STOPPED: calls `eio_stop` (ABI §5.1 step 5).
    ///
    /// The host cancels outstanding timers, watches and requests *after* stop returns, and
    /// the guest should flush state through `eio:state` before returning.
    ///
    /// A non-zero return is counted like any other callback error and does not prevent the
    /// transition: the instance is stopped either way, because §5.1 offers no state for
    /// "asked to stop and declined" and a stopped instance is never restarted regardless.
    pub fn stop(mut self) -> Outcome<Stopped<E>> {
        match self.inner.call(required::STOP, &[]) {
            Ok(status) => Outcome::Live(Stopped { inner: self.inner }, status),
            Err(trap) => Outcome::Dead(trap),
        }
    }

    /// Whether the guest exports `callback` — one of [`optional`]'s names.
    pub fn handles(&self, callback: &str) -> bool {
        self.inner.engine.has_export(callback)
    }

    /// The instance id from its descriptor.
    pub fn instance_id(&self) -> &str {
        &self.inner.instance_id
    }

    /// How many non-zero callback returns this instance has produced (ABI §8).
    pub fn errors(&self) -> u32 {
        self.inner.errors
    }

    /// A payload the guest refused: alive, counted, and reported as `ERR_LIMIT`
    /// (ABI §9.5).
    fn refused(mut self) -> Outcome<Running<E>> {
        let status = Status::Failed(ErrorCode::Limit);
        self.inner.count(status);
        Outcome::Live(self, status)
    }

    /// An optional callback that carries no payload.
    ///
    /// Takes `self` by value like every other call here, because the instance has to come
    /// back out in the [`Outcome`] and a `&mut self` helper could not hand it over.
    fn call_optional(mut self, export: &str, args: &[i32]) -> Outcome<Running<E>> {
        if !self.inner.engine.has_export(export) {
            return Outcome::Dead(missing_export(export));
        }
        match self.inner.call(export, args) {
            Ok(status) => Outcome::Live(self, status),
            Err(trap) => Outcome::Dead(trap),
        }
    }
}

impl<E: Engine> Stopped<E> {
    /// The instance id from its descriptor.
    pub fn instance_id(&self) -> &str {
        &self.inner.instance_id
    }

    /// How many non-zero callback returns this instance produced over its life (ABI §8).
    pub fn errors(&self) -> u32 {
        self.inner.errors
    }

    /// Unwraps the engine, for a caller with teardown of its own.
    ///
    /// The only way out of [`Stopped`], and deliberately not a way back in: what comes out
    /// is the engine, not an instance, so re-driving it means going through
    /// [`Configured::configure`] again — which is what ABI §5.1 means by "restart = new
    /// instance". A guest MUST NOT assume linear-memory continuity across lives, and a host
    /// that reconfigured this engine would be handing it exactly that.
    pub fn into_engine(self) -> E {
        self.inner.engine
    }
}

/// The trap a host bug becomes: a callback the guest does not export.
fn missing_export(export: &str) -> Trap {
    Trap::with_detail(
        TrapKind::Engine,
        alloc::format!("the guest does not export {export}"),
    )
}

/// Checks that every ABI §4.1 export is present.
///
/// Part of ABI §5.1 step 1, and the part of it that needs only the [`Engine`] trait.
/// Signature checking is the engine's, at link time (ABI §4.3), and the manifest
/// cross-check is `eio_manifest`'s.
pub fn check_required_exports<E: Engine>(engine: &E) -> Result<(), Vec<&'static str>> {
    let missing: Vec<&'static str> = required::ALL
        .into_iter()
        .filter(|export| !engine.has_export(export))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

/// Reads the guest's packed ABI version (ABI §12).
///
/// `(major << 16) | minor`. Comparing it against a host's own version is
/// `eio_manifest::Abi`'s job — one implementation of the compatibility rule, in the crate
/// that already owns the manifest's `abi` field.
pub fn abi_version<E: Engine>(engine: &mut E) -> Result<u32, Trap> {
    Ok(engine.call(required::ABI_VERSION, &[])? as u32)
}

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
//! # The property scope is the driver's, not the caller's
//!
//! ABI §7.1 answers `prop` "for the duration of the current callback", against "the batch of
//! the current `eio_process_signals` call". Both halves of that are the *driver's* to get
//! right, so the [`PropContext`] is taken once at [`Configured::configure`] and every guest
//! entry below opens its own scope on it. A caller cannot forget to, cannot open one that
//! outlives a callback, and — because `process_signals` takes the batch itself rather than
//! bytes alongside it — cannot hand the guest one batch and `prop` another. The scopes are
//! the reason `configure`, `on_http` and `process_signals` do their `Inbound::write` inside
//! a closure: `eio_alloc` is a guest call like any other and runs in the same scope as the
//! callback it is allocating for.
//!
//! [`abi_version`] is deliberately outside all of this. ABI §12 makes it readable before
//! `eio_configure`, when there is no configuration and so no context a scope could carry;
//! a guest calling `prop` from it is answered `ERR_INVALID_ARG`, which is the truth.
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
//! # extern crate alloc;
//! use alloc::rc::Rc;
//! use eio_host_core::{Engine, Stopped};
//! use eio_signal::Batch;
//! fn deliver_to_a_stopped_instance<E: Engine>(stopped: Stopped<E>) {
//!     stopped.process_signals(0, Rc::new(Batch::new()));
//! }
//! ```
//!
//! A batch cannot arrive before `start` — delivery begins after `eio_start` returns zero
//! (ABI §5.1 step 3):
//!
//! ```compile_fail,E0599
//! # extern crate alloc;
//! use alloc::rc::Rc;
//! use eio_host_core::{Configured, Engine};
//! use eio_signal::Batch;
//! fn deliver_before_start<E: Engine>(configured: Configured<E>) {
//!     configured.process_signals(0, Rc::new(Batch::new()));
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

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use eio_signal::Batch;

use crate::descriptor::{Descriptor, Limits};
use crate::engine::{Engine, Trap, TrapKind};
use crate::exports::{optional, required};
use crate::memory::{DeliveryFailure, Inbound};
use crate::prop::PropContext;
use eio_abi::{ErrorCode, Status};

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

/// What `eio_process_signals` did (ABI §5.1 step 4).
///
/// Three arms rather than [`Outcome`]'s two, because ABI §9.7 gives the host its own way to
/// say no. A batch the host declines is not a block-level error: the guest was never
/// called, so there is no status it returned and nothing to count against it (§8). Folding
/// that into `Live(_, Status::Failed(ERR_LIMIT))` would tell a caller the block reported an
/// error it never saw.
#[derive(Debug)]
pub enum Delivering<E> {
    /// The guest processed the batch and returned this.
    Delivered(Running<E>, Status),
    /// The host declined to deliver it (ABI §9.7). The guest was not called.
    Refused(Running<E>, Refusal),
    /// The instance died mid-callback.
    Dead(Trap),
}

/// Why a batch was not delivered (ABI §9.7).
///
/// ABI §9.7: the host "never delivers batches beyond" the limits it published in the
/// descriptor. A block that read them and sized its buffers accordingly is entitled to
/// that, so these are checked here rather than left to the guest's allocator to discover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The port index is outside the block's declared inputs (ABI §5.2, §8).
    UnknownPort {
        /// The index that was asked for.
        port: u32,
        /// How many input ports the block declares.
        inputs: usize,
    },
    /// More signals than `max_batch`.
    Batch {
        /// How many signals the batch holds.
        signals: usize,
        /// The instance's limit.
        max_batch: u32,
    },
    /// The canonical encoding is longer than `max_payload`.
    Payload {
        /// How many bytes the batch encodes to.
        bytes: usize,
        /// The instance's limit.
        max_payload: u32,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::UnknownPort { port, inputs } => write!(
                f,
                "input port {port} is outside the block's {inputs} input port(s)"
            ),
            Refusal::Batch {
                signals,
                max_batch: limit,
            } => write!(
                f,
                "the batch has {signals} signals, beyond this instance's max_batch of {limit}"
            ),
            Refusal::Payload {
                bytes,
                max_payload: limit,
            } => write!(
                f,
                "the batch encodes to {bytes} bytes, beyond this instance's max_payload of {limit}"
            ),
        }
    }
}

/// The shared innards of an instance in any state.
///
/// Private, and reached only through the state types, so there is no way to hold one of
/// these and call whatever you like on it.
#[derive(Debug)]
struct Live<E> {
    engine: E,
    /// The instance's identity, from its descriptor (ABI §5.2).
    instance_id: String,
    /// How many input ports the block declares (ABI §5.2) — what §9.7's port check is
    /// against. The names are the caller's business; only the count decides a delivery.
    inputs: usize,
    /// The limits this host published to the instance (ABI §5.2, §9.7).
    limits: Limits,
    /// This instance's property context (ABI §7.1). Held here, not by the caller, because
    /// every guest call below opens a scope on it.
    properties: PropContext,
    /// Non-zero callback returns, counted (ABI §8). Saturating: a block failing four
    /// billion times has made its point.
    errors: u32,
}

impl<E: Engine> Live<E> {
    /// Calls `export` with no payload, inside a property scope, and decodes the status
    /// convention.
    fn call(&mut self, export: &str, args: &[i32]) -> Result<Status, Trap> {
        let raw = self.in_scope(None, |engine| engine.call(export, args))?;
        let status = Status::decode(raw);
        self.count(status);
        Ok(status)
    }

    /// Runs `call` against the engine inside a property scope carrying `signals`.
    ///
    /// The choke point ABI §7.1 needs: `prop` answers only inside a scope, and every guest
    /// entry in this module goes through here, so "the scope matches the call" is not a
    /// pairing a caller can get wrong.
    fn in_scope<T>(&mut self, signals: Option<Rc<Batch>>, call: impl FnOnce(&mut E) -> T) -> T {
        self.properties.during(signals, || call(&mut self.engine))
    }

    /// The two checks a batch answers without being encoded (ABI §5.2, §9.7).
    ///
    /// Split from [`accept_payload`](Self::accept_payload) because the encoding is the
    /// expensive part: a batch refused for its port or its signal count never pays to be
    /// encoded, which is the same ordering ABI §6.2 fixes for `emit`.
    fn accept(&self, input_port: u32, batch: &Batch) -> Result<(), Refusal> {
        if input_port as usize >= self.inputs {
            return Err(Refusal::UnknownPort {
                port: input_port,
                inputs: self.inputs,
            });
        }
        if batch.len() > self.limits.max_batch as usize {
            return Err(Refusal::Batch {
                signals: batch.len(),
                max_batch: self.limits.max_batch,
            });
        }
        Ok(())
    }

    /// The check that needs the encoding (ABI §9.7), asked of the exact bytes the guest is
    /// about to be handed rather than of a predicted length.
    fn accept_payload(&self, bytes: &[u8]) -> Result<(), Refusal> {
        if bytes.len() > self.limits.max_payload as usize {
            return Err(Refusal::Payload {
                bytes: bytes.len(),
                max_payload: self.limits.max_payload,
            });
        }
        Ok(())
    }

    /// A payload the guest declined: counted, and reported as `ERR_LIMIT` (ABI §9.5).
    ///
    /// Counted, unlike a §9.7 refusal, because the guest is the one that said no.
    fn refused(&mut self) -> Status {
        let status = Status::Failed(ErrorCode::Limit);
        self.count(status);
        status
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
    ///
    /// `properties` is this instance's compiled property context (ABI §7.1), taken here
    /// because `eio_configure` is already a callback that may read properties under
    /// `SIGNAL_NONE` — and because taking it at the first guest call is what leaves no
    /// later call able to happen without it.
    pub fn configure(
        engine: E,
        descriptor: &Descriptor,
        properties: PropContext,
    ) -> Configuring<E> {
        let bytes = descriptor.to_cbor();
        let mut live = Live {
            engine,
            instance_id: descriptor.instance_id.clone(),
            inputs: descriptor.inputs.len(),
            limits: descriptor.limits,
            properties,
            errors: 0,
        };
        let configured = live.in_scope(None, |engine| {
            let payload = Inbound::write(engine, &bytes)?;
            payload
                .call(engine, required::CONFIGURE, &[])
                .map_err(DeliveryFailure::Dead)
        });
        match configured {
            Ok(Status::Ok) => Configuring::Configured(Configured { inner: live }),
            // ABI §5.1: a non-zero configure return discards the instance. Dropping `live`
            // here is that, and it is why this arm cannot hand one back.
            Ok(Status::Failed(code)) => Configuring::Rejected(code),
            // The guest could not take its own descriptor, so there is nothing configured
            // here and nothing to keep driving — the same shape as a refusal, reported with
            // the code that says why (ABI §9.5).
            Err(DeliveryFailure::Refused) => Configuring::Rejected(ErrorCode::Limit),
            Err(DeliveryFailure::Dead(trap)) => Configuring::Dead(trap),
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
    /// Delivers a batch on an input port (ABI §6.1, §7.1, §9.7).
    ///
    /// The batch arrives *decoded*, once, and is encoded here. That is not a convenience:
    /// the guest is handed canonical CBOR (§6.3.1) while `prop`'s `signal_idx` indexes the
    /// signals of this same call (§7.1), and a caller supplying those two by separate paths
    /// could supply two different batches. There is one batch, so they cannot disagree.
    ///
    /// ABI §9.7's inbound half is enforced here, before the encoding reaches the guest:
    /// the port must exist and the batch must be within the `max_batch` and `max_payload`
    /// this instance's descriptor published. A refusal never touches the guest, so it is
    /// [`Delivering::Refused`] rather than a status — the instance made no error and its
    /// §8 count does not move. This is the same rule as `emit`'s
    /// [`Outbound::accept`](crate::Outbound::accept), on the other side of the boundary.
    ///
    /// Past those checks the sequence is §6.1's: the host allocates in the guest, writes,
    /// calls, and the *guest* frees. This crate never frees a delivered payload, because
    /// from the moment the callback begins the guest owns it (ABI §9.2), and a host-side
    /// free would be a second owner.
    pub fn process_signals(mut self, input_port: u32, signals: Rc<Batch>) -> Delivering<E> {
        if let Err(refusal) = self.inner.accept(input_port, &signals) {
            return Delivering::Refused(self, refusal);
        }
        let bytes = signals.to_cbor();
        if let Err(refusal) = self.inner.accept_payload(&bytes) {
            return Delivering::Refused(self, refusal);
        }

        let delivered = self.inner.in_scope(Some(signals), |engine| {
            let payload = Inbound::write(engine, &bytes)?;
            payload
                .call(engine, required::PROCESS_SIGNALS, &[input_port as i32])
                .map_err(DeliveryFailure::Dead)
        });
        match delivered {
            Ok(status) => {
                self.inner.count(status);
                Delivering::Delivered(self, status)
            }
            // ABI §9.5: the guest declined the allocation. The batch was not delivered, the
            // instance is untouched, and the caller hears `ERR_LIMIT` — which is what a
            // refused payload is.
            Err(DeliveryFailure::Refused) => {
                let status = self.inner.refused();
                Delivering::Delivered(self, status)
            }
            Err(DeliveryFailure::Dead(trap)) => Delivering::Dead(trap),
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
    /// guest frees it. It is deliberately *not* held to `max_payload`: ABI §9.7 bounds what
    /// the host will accept from `emit` and the *batches* it delivers, and a response body is
    /// neither. Adding the check here would be implementing past the spec; if §7.6 wants a
    /// bound on a response body, that is a change to §7.6.
    pub fn on_http(mut self, req_id: u32, status_code: i32, body: &[u8]) -> Outcome<Running<E>> {
        if !self.inner.engine.has_export(optional::ON_HTTP) {
            return Outcome::Dead(missing_export(optional::ON_HTTP));
        }
        let answered = self.inner.in_scope(None, |engine| {
            let payload = Inbound::write(engine, body)?;
            payload
                .call(engine, optional::ON_HTTP, &[req_id as i32, status_code])
                .map_err(DeliveryFailure::Dead)
        });
        match answered {
            Ok(status) => {
                self.inner.count(status);
                Outcome::Live(self, status)
            }
            Err(DeliveryFailure::Refused) => {
                let status = self.inner.refused();
                Outcome::Live(self, status)
            }
            Err(DeliveryFailure::Dead(trap)) => Outcome::Dead(trap),
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

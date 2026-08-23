//! `eio:timer` — the periodic/one-shot timer capability (ABI-SPEC §7.3, DAEMON-SPEC §5).
//!
//! Two imports, one trait, and the ABI's half of both written once — the same split
//! `crate::state` draws, for the same reason. What a host supplies is a [`Timers`]: something
//! that can arm a delay and hand back an id, and cancel one it handed out. What this module
//! supplies is everything between that and the guest — decoding `(delay_ms, repeat)` and
//! `(timer_id)`, ABI §8's id and status conventions on the way out, and which refusal becomes
//! which code.
//!
//! # Why the trait is two methods and not more
//!
//! Exactly ABI §7.3's two imports. `eio_on_timer` itself is not here: it is a guest *export*
//! (ABI §4.2), called back through [`Engine::call`](crate::Engine::call) the same way every
//! other callback is, and a driver reaches it through
//! [`Running::on_timer`](crate::Running::on_timer) rather than through this trait.
//!
//! # No clock and no scheduler live here
//!
//! ABI §7.3 leaves timer resolution and drift to the host, and answering `eio_on_timer` at
//! all needs a way to call back into the guest later — which is exactly the `no_std`,
//! synchronous, no-timekeeping shape this crate is built to. So a [`Timers`] implementation
//! is the *whole* of the clock and the scheduler: it decides what "later" means, how a
//! repeating timer is re-armed, and how its firing reaches the guest's mailbox. This module
//! never sees a duration elapse; it only ever decodes one call and reports one answer.
//!
//! # A timer id is the host's to hand out, not the guest's to choose
//!
//! Unlike `eio:state`'s keys, which the guest supplies, `timer_set` returns an id the host
//! invents (ABI §8's "id-returning calls"). [`Timers::set`] therefore reports the id itself
//! rather than being handed one, and [`TimerError::NotFound`] is what a host answers a
//! `timer_cancel` naming an id nothing has armed — whether because it was never handed out or
//! because a one-shot already fired and is gone. ABI §7.3 does not say so explicitly, so this
//! is host policy rather than a pinned answer: a timer id behaves like a key into a *dynamic*
//! collection of the host's own making (like `eio:state`'s keys), not like a `prop_id` or a
//! port index fixed at configure time, so "does not exist" is `ERR_NOT_FOUND` and not
//! `ERR_INVALID_ARG`'s "bad index".

use alloc::boxed::Box;
use alloc::rc::Rc;
use core::cell::RefCell;

use eio_abi::ErrorCode;

use crate::engine::{Arg, Engine, EngineError, HostCall, Ret};
use crate::exports::{namespace, timer_fn};

/// One block instance's timer scheduler (ABI §7.3).
///
/// `&mut self` throughout, for the reason [`crate::state::StateStore`] gives: a host function
/// handler is an `FnMut`, and ABI §1.2 gives an instance one caller at a time, so the boundary
/// can afford it even where an implementation only ever needs shared, interior-mutable state
/// underneath (a daemon's scheduler is exactly that — see its own module docs).
pub trait Timers {
    /// Arms a new timer and reports its id (ABI §7.3's id convention).
    ///
    /// `repeat`: `false` fires once, `true` fires periodically until [`cancel`](Timers::cancel)
    /// or the instance stops. Resolution and drift are the implementation's to define (ABI
    /// §7.3: "timers are not real-time guarantees").
    fn set(&mut self, delay_ms: i64, repeat: bool) -> Result<u32, TimerError>;

    /// Cancels a timer this instance previously armed.
    fn cancel(&mut self, timer_id: u32) -> Result<(), TimerError>;
}

/// Why a [`Timers`] refused (ABI §7.3, §8).
///
/// Two variants, because these are the two ways a call here can be wrong: an argument that
/// makes no sense as a delay, and an id that names nothing this host has armed. Neither is
/// fatal — a non-zero return is logged and counted, never a death (§8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerError {
    /// `delay_ms` is not a delay a clock can honor — negative, or zero (ABI §7.3 sets no
    /// floor, so this host's is the smallest interval it is willing to schedule at all: more
    /// than zero).
    InvalidArg,
    /// `timer_cancel` named an id this host has no timer for — never armed, already
    /// cancelled, or a one-shot that already fired.
    NotFound,
}

impl TimerError {
    /// The ABI §8 code a guest sees.
    pub const fn as_code(self) -> ErrorCode {
        match self {
            TimerError::InvalidArg => ErrorCode::InvalidArg,
            TimerError::NotFound => ErrorCode::NotFound,
        }
    }
}

/// Registers `eio:timer`'s two functions on `guest` (ABI §7.3).
///
/// Both together, for the reason [`crate::state::register`] gives: a capability is granted
/// whole, or a guest's other import fails at the engine rather than at this host's choice.
///
/// `timers` is moved in and shared between the two handlers through an [`Rc`] — `Rc`, not
/// `Arc`, because `riscv32imc` has no atomics and nothing here needs them (ABI §1.2).
pub fn register<E: Engine, T: Timers + 'static>(
    guest: &mut E,
    timers: T,
) -> Result<(), EngineError> {
    /// One entry per import: the name a guest calls, and the function that answers it.
    type Handler = fn(HostCall<'_>, &mut dyn Timers) -> Ret;
    const HANDLERS: [(&str, Handler); 2] = [(timer_fn::SET, set), (timer_fn::CANCEL, cancel)];

    let timers = Rc::new(RefCell::new(timers));
    for (name, handler) in HANDLERS {
        let timers = Rc::clone(&timers);
        guest.register(
            namespace::TIMER,
            name,
            Box::new(move |call| handler(call, &mut *timers.borrow_mut())),
        )?;
    }
    Ok(())
}

/// `timer_set(delay_ms, repeat) -> i32` (ABI §7.3), under the id convention.
///
/// Public for the reason [`crate::state::get`] is: the reference conformance harness answers
/// `timer_set` with this same function, so the two hosts cannot drift on how a bad argument is
/// decoded.
pub fn set(call: HostCall<'_>, timers: &mut dyn Timers) -> Ret {
    let [Arg::I64(delay_ms), Arg::I32(repeat)] = *call.args else {
        return invalid();
    };
    match timers.set(delay_ms, repeat != 0) {
        Ok(timer_id) => Ret::I32(timer_id as i32),
        Err(error) => Ret::I32(error.as_code().as_i32()),
    }
}

/// `timer_cancel(timer_id) -> i32` (ABI §7.3). See [`set`].
pub fn cancel(call: HostCall<'_>, timers: &mut dyn Timers) -> Ret {
    let [Arg::I32(timer_id)] = *call.args else {
        return invalid();
    };
    match timers.cancel(timer_id as u32) {
        Ok(()) => Ret::I32(0),
        Err(error) => Ret::I32(error.as_code().as_i32()),
    }
}

/// A call whose arguments are not what ABI §7.3 declares. §8: "bad index, pointer, or
/// parameter".
fn invalid() -> Ret {
    Ret::I32(ErrorCode::InvalidArg.as_i32())
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloc::collections::BTreeMap;
    use alloc::vec;
    use alloc::vec::Vec;

    use crate::engine::HostFn;

    /// A scheduler that hands out sequential ids and remembers which are still armed, with no
    /// clock behind it at all — this module owns none, so its tests need none either.
    struct Fake {
        next_id: u32,
        armed: BTreeMap<u32, ()>,
    }

    impl Fake {
        fn new() -> Fake {
            Fake {
                next_id: 0,
                armed: BTreeMap::new(),
            }
        }
    }

    impl Timers for Fake {
        fn set(&mut self, delay_ms: i64, _repeat: bool) -> Result<u32, TimerError> {
            if delay_ms <= 0 {
                return Err(TimerError::InvalidArg);
            }
            let id = self.next_id;
            self.next_id += 1;
            self.armed.insert(id, ());
            Ok(id)
        }

        fn cancel(&mut self, timer_id: u32) -> Result<(), TimerError> {
            self.armed.remove(&timer_id).ok_or(TimerError::NotFound)
        }
    }

    /// Guest memory a timer call never touches, but [`HostCall`] still needs one.
    struct NoMemory;

    impl crate::engine::Memory for NoMemory {
        fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
            Err(EngineError::OutOfBounds { ptr, len })
        }

        fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
            Err(EngineError::OutOfBounds {
                ptr,
                len: bytes.len() as u32,
            })
        }
    }

    /// Calls `f` with `args` and reports the `i32` the guest would see.
    fn call(
        f: fn(HostCall<'_>, &mut dyn Timers) -> Ret,
        timers: &mut dyn Timers,
        args: &[Arg],
    ) -> i32 {
        let mut memory = NoMemory;
        let ret = f(
            HostCall {
                args,
                memory: &mut memory,
            },
            timers,
        );
        match ret {
            Ret::I32(value) => value,
            other => panic!("ABI §7.3 is all `-> i32`, got {other:?}"),
        }
    }

    #[test]
    fn timer_set_hands_back_sequential_ids() {
        let mut timers = Fake::new();
        assert_eq!(call(set, &mut timers, &[Arg::I64(1_000), Arg::I32(0)]), 0);
        assert_eq!(call(set, &mut timers, &[Arg::I64(1_000), Arg::I32(1)]), 1);
    }

    #[test]
    fn a_negative_or_zero_delay_is_invalid_arg() {
        let mut timers = Fake::new();
        for delay in [-1, 0] {
            assert_eq!(
                call(set, &mut timers, &[Arg::I64(delay), Arg::I32(0)]),
                ErrorCode::InvalidArg.as_i32(),
                "{delay}"
            );
        }
    }

    #[test]
    fn cancel_of_an_armed_timer_answers_ok() {
        let mut timers = Fake::new();
        let id = call(set, &mut timers, &[Arg::I64(1_000), Arg::I32(0)]);
        assert_eq!(call(cancel, &mut timers, &[Arg::I32(id)]), 0);
    }

    #[test]
    fn cancel_of_an_unknown_or_already_cancelled_id_is_not_found() {
        let mut timers = Fake::new();
        assert_eq!(
            call(cancel, &mut timers, &[Arg::I32(41)]),
            ErrorCode::NotFound.as_i32(),
            "never armed"
        );
        let id = call(set, &mut timers, &[Arg::I64(1_000), Arg::I32(0)]);
        assert_eq!(call(cancel, &mut timers, &[Arg::I32(id)]), 0);
        assert_eq!(
            call(cancel, &mut timers, &[Arg::I32(id)]),
            ErrorCode::NotFound.as_i32(),
            "already cancelled"
        );
    }

    #[test]
    fn arguments_of_the_wrong_shape_are_invalid_arg() {
        // Unreachable through a linked module (the engine checks the signature, ABI §4.3),
        // and answered rather than panicked all the same — see `state`'s test of the same
        // name.
        let mut timers = Fake::new();
        let bad = ErrorCode::InvalidArg.as_i32();
        assert_eq!(call(set, &mut timers, &[Arg::I64(1_000)]), bad);
        assert_eq!(call(set, &mut timers, &[Arg::I32(1_000), Arg::I32(0)]), bad);
        assert_eq!(call(cancel, &mut timers, &[]), bad);
        assert_eq!(call(cancel, &mut timers, &[Arg::I64(1)]), bad);
    }

    #[test]
    fn the_two_functions_are_registered_under_the_capabilitys_names() {
        // The names are `eio_manifest`'s (ABI §7.3) and the namespace is `eio:timer`'s; a host
        // registering something else would link against no real block's imports.
        struct Recorder(vec::Vec<(alloc::string::String, alloc::string::String)>);
        impl Engine for Recorder {
            fn call(&mut self, _export: &str, _args: &[i32]) -> Result<i32, crate::Trap> {
                unreachable!("registration calls nothing")
            }
            fn has_export(&self, _export: &str) -> bool {
                false
            }
            fn read(&self, _ptr: u32, _len: u32) -> Result<Vec<u8>, EngineError> {
                unreachable!()
            }
            fn write(&mut self, _ptr: u32, _bytes: &[u8]) -> Result<(), EngineError> {
                unreachable!()
            }
            fn register(&mut self, ns: &str, name: &str, _f: HostFn) -> Result<(), EngineError> {
                self.0.push((ns.into(), name.into()));
                Ok(())
            }
        }

        let mut recorder = Recorder(vec::Vec::new());
        register(&mut recorder, Fake::new()).expect("registration");
        let names: Vec<&str> = recorder.0.iter().map(|(_, name)| name.as_str()).collect();
        assert_eq!(
            names,
            eio_manifest::Capability::Timer.functions(),
            "both, in the capability's order"
        );
        assert!(
            recorder
                .0
                .iter()
                .all(|(ns, _)| ns == crate::exports::namespace::TIMER)
        );
    }
}

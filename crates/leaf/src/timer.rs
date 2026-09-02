//! `eio:timer`'s host side for the leaf (LEAF-SPEC §5's kin for time rather than storage): a
//! single-threaded, poll-driven scheduler, and a [`pump`] the driver calls between guest
//! callbacks to fire the ones that came due.
//!
//! `crates/host-core/src/timer.rs`'s module docs say what a host supplies: "something that can
//! arm a delay and hand back an id, and cancel one it handed out... the *whole* of the clock
//! and the scheduler". This module is that something, built the way [`crate::state`] is —
//! named as a bring-up stand-in for real hardware rather than presented as one — plus the two
//! things a scheduler needs beyond [`Timers`](eio_host_core::Timers) itself: a way for the
//! driver to ask "what is due now" from outside the guest ([`pump`]), and a way to answer ABI
//! §5.1 step 5's "host cancels outstanding timers ... after stop returns"
//! ([`Scheduler::cancel_all`]).
//!
//! # This is not LEAF §4's watchdog
//!
//! LEAF §4's watchdog decides when a *running* guest callback is killed — a hardware timer
//! armed before entering `eio_process_signals` or `eio_on_timer` and disarmed on return, which
//! this milestone does not build (wasm3 has no interruption entry point at all; see this
//! crate's own report). [`pump`] decides something else entirely: when the *next* callback
//! *starts*. `Running::on_timer` is still the only way into the guest — a scheduler picks the
//! moment, never the mechanism — and ABI §1.2's one-caller-at-a-time rule holds simply because
//! nothing here is concurrent: [`pump`] runs to completion, synchronously, strictly between two
//! guest calls, and is never itself called while another callback is in flight.
//!
//! # What an MCU would put here instead
//!
//! A hardware tick — an RTC compare register or a systick interrupt deciding when to next call
//! [`pump`] — in place of a caller choosing `now_ms` itself, and a fixed-capacity array in
//! place of the `Vec` `Scheduled` holds, since a leaf's own memory is not something to allocate
//! against at runtime. The scheduling algorithm itself (the sorted-by-due-time scan, the
//! re-arm-relative-to-now policy) is small enough that a real leaf could keep this one.

use std::cell::RefCell;
use std::rc::Rc;

use eio_host_core::{ClockSource, Outcome, Running, Status, TimerError, Timers, Trap};

use crate::core_fns::SystemClock;
use crate::engine::Guest;

/// One armed timer (ABI §7.3): when it next fires, how long its period is, and whether it
/// re-arms itself.
#[derive(Debug, Clone, Copy)]
struct Armed {
    id: u32,
    due_at_ms: i64,
    period_ms: i64,
    repeat: bool,
}

/// The scheduling algorithm, with no clock of its own — every method that needs "now" is
/// handed it, which is what makes this half testable without touching real time (see this
/// module's tests) and reusable verbatim on hardware with a different clock underneath.
///
/// A `Vec`, not a `BinaryHeap`: a leaf instance arms a handful of timers at most, so a linear
/// scan for the earliest due one costs nothing that matters at that scale while staying
/// trivial to read and, later, to port to a fixed-size array.
#[derive(Debug, Default)]
struct Scheduled {
    next_id: u32,
    armed: Vec<Armed>,
}

impl Scheduled {
    fn new() -> Scheduled {
        Scheduled::default()
    }

    /// Arms a new timer due `delay_ms` after `now_ms` (ABI §7.3).
    fn set(&mut self, now_ms: i64, delay_ms: i64, repeat: bool) -> Result<u32, TimerError> {
        if delay_ms <= 0 {
            return Err(TimerError::InvalidArg);
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.armed.push(Armed {
            id,
            due_at_ms: now_ms + delay_ms,
            period_ms: delay_ms,
            repeat,
        });
        Ok(id)
    }

    /// Cancels a timer this scheduler armed (ABI §7.3).
    fn cancel(&mut self, timer_id: u32) -> Result<(), TimerError> {
        let position = self
            .armed
            .iter()
            .position(|armed| armed.id == timer_id)
            .ok_or(TimerError::NotFound)?;
        self.armed.remove(position);
        Ok(())
    }

    /// The one timer due at or before `now_ms` that [`pump`] should fire next, or `None` if
    /// nothing is due.
    ///
    /// Ties — two timers due at the same instant — are broken by ascending id, which for this
    /// scheduler is also arm order (ids are handed out sequentially and never reused): a
    /// deterministic choice, stated here because an implementation that fired them in some
    /// other order (map iteration order, say) would be a leaf that replays a scenario
    /// differently from the daemon, which is exactly the divergence ABI §13 exists to catch.
    ///
    /// A repeating timer already due is re-armed for one period *from `now_ms`*, not from its
    /// old due time: a [`pump`] that is called late (a leaf busy servicing something else for a
    /// few periods, say) re-arms once rather than firing a burst to make up the gap. ABI §7.3
    /// leaves drift to the host ("timers are not real-time guarantees"); "no catch-up burst" is
    /// this scheduler's answer to that latitude, not a pinned one.
    fn due_one(&mut self, now_ms: i64) -> Option<u32> {
        let earliest = self
            .armed
            .iter()
            .enumerate()
            .filter(|(_, armed)| armed.due_at_ms <= now_ms)
            .min_by_key(|(_, armed)| (armed.due_at_ms, armed.id))
            .map(|(index, _)| index)?;

        let id = self.armed[earliest].id;
        if self.armed[earliest].repeat {
            let period = self.armed[earliest].period_ms;
            self.armed[earliest].due_at_ms = now_ms + period;
        } else {
            self.armed.remove(earliest);
        }
        Some(id)
    }
}

/// One instance's clock and scheduler together — what [`Scheduler::new`] holds, so that
/// `set`/`cancel` (which read the clock) and [`pump`] (which does not) can share one `Vec`
/// without either duplicating the other's state.
struct Inner {
    clock: SystemClock,
    scheduled: Scheduled,
}

/// A cheaply-cloned handle onto one instance's timer state (LEAF-SPEC §5's `state.rs` is the
/// same shape for a different capability).
///
/// [`eio_host_core::timer::register`] takes ownership of whatever implements [`Timers`] and
/// wraps it in its own `Rc<RefCell<_>>` so its two host functions can share it — which means
/// whatever a caller passed in is gone, from the caller's side, the moment it is registered.
/// [`Scheduler`] is *already* an `Rc<RefCell<_>>` handle for exactly that reason: [`crate::spawn`]
/// keeps one clone for [`pump`] to use and hands another to `register`, and both clones share
/// the one [`Inner`] underneath.
#[derive(Clone)]
pub struct Scheduler(Rc<RefCell<Inner>>);

impl Scheduler {
    /// A fresh, empty scheduler reading `clock` — the same [`SystemClock`] the instance's
    /// `eio:core` answers `time_mono_ms` from (see that type's own docs for why a copy of it is
    /// the same clock and not a second one).
    pub fn new(clock: SystemClock) -> Scheduler {
        Scheduler(Rc::new(RefCell::new(Inner {
            clock,
            scheduled: Scheduled::new(),
        })))
    }

    /// Answers `timer_cancel`'s question directly (ABI §7.3) — the same thing a guest's import
    /// reaches through [`Timers::cancel`], exposed here for a driver (or a test) that wants to
    /// cancel a timer without going through the guest at all.
    pub fn cancel(&self, timer_id: u32) -> Result<(), TimerError> {
        self.0.borrow_mut().scheduled.cancel(timer_id)
    }

    /// What this scheduler's own clock reads right now.
    ///
    /// A driver (or a test) needing `now_ms` for [`pump`] reads it from here rather than
    /// constructing a second [`SystemClock`] of its own — the same "one clock" rule
    /// `SystemClock`'s docs state, at the one seam outside `crate::spawn` that needs "now" at
    /// all.
    pub fn now_ms(&self) -> i64 {
        self.0.borrow().clock.mono_ms()
    }

    /// Cancels every timer this instance still has armed.
    ///
    /// ABI §5.1 step 5 is explicit that this is the host's to do, not the guest's: "Host
    /// cancels outstanding timers/watches/requests after stop returns." A driver calls this
    /// once [`Running::stop`] has returned — `crates/daemon`'s own scheduler makes the same
    /// pairing, for the same reason: a block that forgets to cancel its own timer on the way
    /// out (ABI §7.3's `Timer::cancel` doc even says a block need not bother) must not leave
    /// one armed against an instance nothing will ever call again.
    ///
    /// Idempotent: calling it twice, or on a scheduler with nothing armed, cancels nothing the
    /// second time.
    pub fn cancel_all(&self) {
        self.0.borrow_mut().scheduled.armed.clear();
    }
}

impl Timers for Scheduler {
    fn set(&mut self, delay_ms: i64, repeat: bool) -> Result<u32, TimerError> {
        let mut inner = self.0.borrow_mut();
        let now_ms = inner.clock.mono_ms();
        inner.scheduled.set(now_ms, delay_ms, repeat)
    }

    fn cancel(&mut self, timer_id: u32) -> Result<(), TimerError> {
        Scheduler::cancel(self, timer_id)
    }
}

/// What pumping every timer due at `now_ms` produced.
///
/// No `Debug`: [`Running`]'s engine (wasm3's `Guest`) does not implement it, for the same
/// reason `eio_host_core::Running` itself carries no such bound — a live guest instance is not
/// a value a test should be printing.
pub struct Pumped {
    /// The instance, if every fired timer's callback returned rather than killing it. `None`
    /// once [`Pumped::dead`] is set — there is nothing left to hand the caller.
    pub running: Option<Running<Guest>>,
    /// `(timer_id, status)` for every timer that fired, in the order [`Scheduled::due_one`]
    /// chose — including ones that fired before a later one killed the instance.
    pub fired: Vec<(u32, Status)>,
    /// Set if a fired timer's callback killed the instance (ABI §5.1 step 6, §8, §10). Any
    /// timer still due at `now_ms` after that is left armed rather than lost, the same answer
    /// the ABI already gives every other call made after an instance has died.
    pub dead: Option<Trap>,
}

/// Fires every timer `scheduler` reports due at `now_ms`, in [`Scheduled::due_one`]'s order,
/// calling [`Running::on_timer`] for each.
///
/// This is the "pump" this crate's report describes: the driver's own decision about *when*,
/// layered on top of `host-core`'s `Running::on_timer` as the only way *in*. It is not LEAF
/// §4's watchdog — see this module's own docs for the distinction — and it must be called
/// between guest callbacks, never nested inside one; a single-threaded caller gets that for
/// free by construction.
pub fn pump(scheduler: &Scheduler, mut running: Running<Guest>, now_ms: i64) -> Pumped {
    let mut fired = Vec::new();
    loop {
        let Some(id) = scheduler.0.borrow_mut().scheduled.due_one(now_ms) else {
            break;
        };
        match running.on_timer(id) {
            Outcome::Live(next, status) => {
                fired.push((id, status));
                running = next;
            }
            Outcome::Dead(trap) => {
                return Pumped {
                    running: None,
                    fired,
                    dead: Some(trap),
                };
            }
        }
    }
    Pumped {
        running: Some(running),
        fired,
        dead: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_hands_back_sequential_ids() {
        let mut scheduled = Scheduled::new();
        assert_eq!(scheduled.set(0, 1_000, false).unwrap(), 0);
        assert_eq!(scheduled.set(0, 1_000, false).unwrap(), 1);
    }

    #[test]
    fn a_negative_or_zero_delay_is_invalid_arg() {
        let mut scheduled = Scheduled::new();
        for delay in [-1, 0] {
            assert_eq!(scheduled.set(0, delay, false), Err(TimerError::InvalidArg));
        }
    }

    #[test]
    fn cancel_of_an_armed_timer_answers_ok() {
        let mut scheduled = Scheduled::new();
        let id = scheduled.set(0, 1_000, false).unwrap();
        assert_eq!(scheduled.cancel(id), Ok(()));
    }

    #[test]
    fn cancel_of_an_unknown_or_already_cancelled_id_is_not_found() {
        let mut scheduled = Scheduled::new();
        assert_eq!(scheduled.cancel(41), Err(TimerError::NotFound));
        let id = scheduled.set(0, 1_000, false).unwrap();
        assert_eq!(scheduled.cancel(id), Ok(()));
        assert_eq!(scheduled.cancel(id), Err(TimerError::NotFound));
    }

    #[test]
    fn a_timer_does_not_fire_before_it_is_due() {
        let mut scheduled = Scheduled::new();
        scheduled.set(0, 1_000, false).unwrap();
        assert_eq!(scheduled.due_one(999), None);
        assert_eq!(scheduled.due_one(1_000), Some(0));
    }

    #[test]
    fn a_one_shot_fires_once_and_is_gone() {
        let mut scheduled = Scheduled::new();
        let id = scheduled.set(0, 1_000, false).unwrap();
        assert_eq!(scheduled.due_one(1_000), Some(id));
        assert_eq!(scheduled.due_one(1_000), None);
        assert_eq!(scheduled.cancel(id), Err(TimerError::NotFound));
    }

    #[test]
    fn a_repeating_timer_re_arms_relative_to_now() {
        let mut scheduled = Scheduled::new();
        let id = scheduled.set(0, 1_000, true).unwrap();
        assert_eq!(scheduled.due_one(1_000), Some(id));
        // Re-armed from `now_ms` (1_000), not from the old due time: the next fire is at
        // 2_000, not 2_000 twice over from a burst.
        assert_eq!(scheduled.due_one(1_999), None);
        assert_eq!(scheduled.due_one(2_000), Some(id));
    }

    #[test]
    fn a_late_pump_does_not_burst_a_repeating_timer() {
        let mut scheduled = Scheduled::new();
        let id = scheduled.set(0, 10, true).unwrap();
        // A pump that only runs once every 500ms still only fires once, however many periods
        // have elapsed underneath it.
        assert_eq!(scheduled.due_one(500), Some(id));
        assert_eq!(scheduled.due_one(500), None);
    }

    #[test]
    fn two_timers_due_at_once_fire_in_ascending_id_order() {
        let mut scheduled = Scheduled::new();
        let first = scheduled.set(0, 1_000, false).unwrap();
        let second = scheduled.set(0, 1_000, false).unwrap();
        assert_eq!(scheduled.due_one(1_000), Some(first));
        assert_eq!(scheduled.due_one(1_000), Some(second));
        assert_eq!(scheduled.due_one(1_000), None);
    }

    #[test]
    fn the_earliest_due_timer_fires_first_regardless_of_arm_order() {
        let mut scheduled = Scheduled::new();
        let later = scheduled.set(0, 2_000, false).unwrap();
        let sooner = scheduled.set(0, 1_000, false).unwrap();
        assert_eq!(scheduled.due_one(2_000), Some(sooner));
        assert_eq!(scheduled.due_one(2_000), Some(later));
    }

    #[test]
    fn cancel_all_clears_every_armed_timer_and_is_idempotent() {
        let mut scheduled = Scheduled::new();
        let first = scheduled.set(0, 1_000, false).unwrap();
        let second = scheduled.set(0, 1_000, true).unwrap();
        scheduled.armed.clear();
        assert_eq!(
            scheduled.due_one(1_000),
            None,
            "nothing should be armed any more"
        );
        assert_eq!(scheduled.cancel(first), Err(TimerError::NotFound));
        assert_eq!(scheduled.cancel(second), Err(TimerError::NotFound));
        // Idempotent: clearing an already-empty schedule is not an error.
        scheduled.armed.clear();
    }

    #[test]
    fn schedulers_cancel_all_reaches_the_shared_state() {
        let scheduler = Scheduler::new(SystemClock::new());
        let mut setter = scheduler.clone();
        let id = setter.set(1_000, false).unwrap();
        scheduler.cancel_all();
        assert_eq!(scheduler.cancel(id), Err(TimerError::NotFound));
    }

    #[test]
    fn a_scheduler_reads_the_clock_it_was_given_and_nothing_else() {
        // `Scheduler::set` computes `due_at` from `SystemClock::mono_ms`, so a timer armed
        // immediately is not due at `mono_ms()` itself (elapsed time since construction is
        // never negative) but is due comfortably after its delay has passed.
        let clock = SystemClock::new();
        let mut scheduler = Scheduler::new(clock);
        let now = clock.mono_ms();
        let id = scheduler.set(50, false).unwrap();
        assert_eq!(scheduler.0.borrow_mut().scheduled.due_one(now), None);
        assert_eq!(
            scheduler.0.borrow_mut().scheduled.due_one(now + 1_000),
            Some(id)
        );
    }
}

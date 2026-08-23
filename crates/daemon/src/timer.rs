//! `eio:timer`'s host side (ABI-SPEC §7.3): the clock and scheduler `eio_host_core::timer`
//! deliberately does not have.
//!
//! That crate owns decoding `timer_set`/`timer_cancel` and ABI §8's status convention; this
//! module owns everything downstream of "arm a timer" — a clock (`tokio::time`), a place to
//! run it (the instance's own `LocalSet`, DAEMON §5), and a way back into the guest.
//!
//! # A timer firing does not call the guest — it posts to the instance's own mailbox
//!
//! ABI §1.2 gives an instance one caller at a time, and this crate has exactly one thing that
//! ever enters a guest: the loop in `crate::instance::instance_task` draining its
//! [`Mailbox`](crate::executor::Mailbox). A [`Scheduler`] does not know how to call
//! `eio_on_timer` and does not try to; when a timer is due it sends
//! [`Work::Timer`](crate::executor::Work::Timer) into the *same* mailbox every `Deliver` and
//! every other work item goes through. A timer that fires while the instance is mid-callback
//! therefore simply waits behind whatever the loop is doing — there is no second path into
//! the guest to serialize against, which is the same shape [`crate::instance`]'s module docs
//! state for `Work` in general.
//!
//! # Repeating: anchored, not re-armed, and willing to drop a tick
//!
//! A naive repeat — sleep `delay_ms`, fire, sleep `delay_ms` again — drifts by however long
//! each fire and its wakeup took. [`tokio::time::interval`] anchors every tick to the first
//! deadline instead, so the schedule does not slide (no drift). But an anchored interval that
//! is not read for a while has to decide what happens to the ticks it missed, and the default
//! ("Burst") replays every one of them back-to-back the moment someone asks — which is exactly
//! the pile-up a slow guest must not cause. [`tokio::time::MissedTickBehavior::Skip`] is the
//! other choice: it fires at most once per poll and skips forward to the next tick still ahead
//! of "now" rather than queuing the ones behind it. Combined with
//! [`Mailbox::try_send`](crate::executor::Mailbox::try_send) — which drops a tick outright
//! rather than waiting for room — a repeating timer can fall behind a slow guest and lose
//! ticks, but it can never queue an unbounded backlog of them or race ahead of its own clock.
//! ABI §7.3 says plainly that "timers are not real-time guarantees"; this is that sentence
//! turned into a scheduling policy.
//!
//! # Cancellation is explicit, at two points
//!
//! [`Scheduler::cancel`] aborts one timer's task and forgets its id, so a `timer_cancel` on it
//! afterward is answered [`TimerError::NotFound`](eio_host_core::TimerError::NotFound) — the
//! same answer a one-shot gets once it has fired and removed itself. [`Scheduler::cancel_all`]
//! is `crate::instance::Live`'s: it is called once `eio_stop` has returned, which is ABI
//! §5.1 step 5's "host cancels outstanding timers ... after stop returns" done as an explicit
//! action rather than left to whenever the instance's `LocalSet` happens to be torn down.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use eio_host_core::{TimerError, Timers};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use crate::executor::{Mailbox, Work};

/// One instance's `eio:timer` host side.
///
/// Cheap to clone — an [`Rc`] around the map of outstanding timers — because
/// [`eio_host_core::timer::register`] wants to own one `T: Timers` per handler closure while
/// [`crate::instance::Live`] separately keeps one to call [`cancel_all`](Scheduler::cancel_all)
/// from. `Rc`, not `Arc`: this never leaves the instance's own thread (ABI §1.2 again).
#[derive(Clone)]
pub struct Scheduler {
    inner: Rc<RefCell<Inner>>,
}

struct Inner {
    /// The next id [`Scheduler::set`] will try. Wrapping is fine: a `u32` worth of timers
    /// outstanding on one instance at once is not a case this host needs to guard against
    /// specially, and [`fresh_id`](Inner::fresh_id) would simply keep searching if it ever
    /// happened.
    next_id: u32,
    /// This instance's own way back into itself (see the module docs).
    mailbox: Mailbox,
    /// Every timer this instance currently has armed, keyed by the id `timer_set` returned.
    armed: HashMap<u32, JoinHandle<()>>,
}

impl Inner {
    fn fresh_id(&mut self) -> u32 {
        loop {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1);
            if !self.armed.contains_key(&id) {
                return id;
            }
        }
    }
}

impl Scheduler {
    /// A scheduler with nothing armed, posting into `mailbox` when something fires.
    ///
    /// `mailbox` is this instance's *own* — the same one `crate::executor::Instance` hands
    /// external callers — so a timer's firing is indistinguishable, from the loop's side, from
    /// any other sender's `Work`.
    pub fn new(mailbox: Mailbox) -> Scheduler {
        Scheduler {
            inner: Rc::new(RefCell::new(Inner {
                next_id: 0,
                mailbox,
                armed: HashMap::new(),
            })),
        }
    }

    /// Cancels every timer this instance still has armed (ABI §5.1 step 5). See the module
    /// docs. Idempotent: calling it twice, or on a scheduler with nothing armed, aborts
    /// nothing the second time.
    pub fn cancel_all(&self) {
        for (_, handle) in self.inner.borrow_mut().armed.drain() {
            handle.abort();
        }
    }
}

impl Timers for Scheduler {
    fn set(&mut self, delay_ms: i64, repeat: bool) -> Result<u32, TimerError> {
        // `0` is refused rather than treated as "fire on the next poll": `tokio::time::interval`
        // panics on a zero period, and a one-shot of delay `0` is not a case ABI §7.3 asks a
        // host to make instant rather than merely soon (§7.3: "not real-time guarantees").
        let millis = u64::try_from(delay_ms).map_err(|_| TimerError::InvalidArg)?;
        if millis == 0 {
            return Err(TimerError::InvalidArg);
        }
        let period = Duration::from_millis(millis);

        let mut inner = self.inner.borrow_mut();
        let id = inner.fresh_id();
        let mailbox = inner.mailbox.clone();
        // A second handle on the same map, so the spawned task can remove its own entry once
        // a one-shot has fired — see the module docs on why `cancel` must stop answering `Ok`
        // for an id that no longer names anything outstanding.
        let scheduler = self.clone();

        // `spawn_local` rather than a bespoke timer wheel: this instance already runs on a
        // `LocalSet` (`crate::instance::run_instance`), which is exactly where DAEMON §5 says
        // the capability completions of ABI §7.3 and §7.6 belong, and it is reachable here
        // because `Timers::set` only ever runs synchronously from inside a guest callback,
        // itself running inside that same `LocalSet`'s `block_on` — the ambient task context
        // `spawn_local` needs is therefore always present when a guest can call `timer_set` at
        // all.
        let handle = tokio::task::spawn_local(async move {
            if repeat {
                let mut ticks = tokio::time::interval(period);
                ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
                // The first tick of a `tokio::time::interval` completes immediately; consuming
                // it here is what makes `delay_ms` the wait before the *first* fire rather than
                // an instant one, matching a one-shot armed with the same delay.
                ticks.tick().await;
                loop {
                    ticks.tick().await;
                    // Dropped, not awaited, if the mailbox has no room: see the module docs.
                    let _ = mailbox.try_send(Work::Timer { timer_id: id });
                }
            } else {
                tokio::time::sleep(period).await;
                let _ = mailbox.try_send(Work::Timer { timer_id: id });
                // This id no longer names an outstanding timer (see `Timers::cancel` below).
                scheduler.inner.borrow_mut().armed.remove(&id);
            }
        });
        inner.armed.insert(id, handle);
        Ok(id)
    }

    fn cancel(&mut self, timer_id: u32) -> Result<(), TimerError> {
        match self.inner.borrow_mut().armed.remove(&timer_id) {
            Some(handle) => {
                handle.abort();
                Ok(())
            }
            None => Err(TimerError::NotFound),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `LocalSet`, for the reason every test here needs one: [`Scheduler::set`] calls
    /// `tokio::task::spawn_local`, which panics off one.
    ///
    /// Real time, not `tokio::time::pause`/`advance`: those need tokio's `test-util` feature,
    /// which this crate does not carry (its `time` feature is enough for everything but a
    /// virtual clock), and adding it is a `Cargo.toml` edit this task's file-level ownership
    /// does not cover. [`PERIOD_MS`] is small and every assertion below leaves generous
    /// margin, which is what "if you must sleep, keep it small and say why" comes to here.
    macro_rules! local_test {
        ($name:ident, $body:expr) => {
            #[test]
            fn $name() {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .expect("a current-thread runtime");
                let local = tokio::task::LocalSet::new();
                local.block_on(&runtime, $body);
            }
        };
    }

    /// The delay every test below arms its timers with. Small enough that even the slowest
    /// test here (five periods) finishes well under a second; large enough that scheduling
    /// jitter on a loaded CI box cannot turn "did it fire yet" into a coin flip.
    const PERIOD_MS: u64 = 20;

    /// Generous enough to catch one tick of [`PERIOD_MS`] even under CI jitter, tight enough
    /// that a `Scheduler` bug that stops ticking entirely still fails the test rather than
    /// hanging it.
    const ONE_TICK: Duration = Duration::from_millis(PERIOD_MS * 5);

    local_test!(a_one_shot_timer_posts_exactly_one_work_item, async {
        let (mailbox, mut rx) = Mailbox::pair(4);
        let mut scheduler = Scheduler::new(mailbox);
        let id = scheduler.set(PERIOD_MS as i64, false).expect("armed");

        let item = tokio::time::timeout(ONE_TICK, rx.recv())
            .await
            .expect("it fires within one tick")
            .expect("the mailbox is still open");
        assert_eq!(item, Work::Timer { timer_id: id });

        // And it is gone: a one-shot that already fired is not outstanding any more.
        assert_eq!(scheduler.cancel(id), Err(TimerError::NotFound));
    });

    local_test!(a_repeating_timer_fires_more_than_once, async {
        let (mailbox, mut rx) = Mailbox::pair(4);
        let mut scheduler = Scheduler::new(mailbox);
        let id = scheduler.set(PERIOD_MS as i64, true).expect("armed");

        for _ in 0..3 {
            let item = tokio::time::timeout(ONE_TICK, rx.recv())
                .await
                .expect("a tick within one period")
                .expect("the mailbox is still open");
            assert_eq!(item, Work::Timer { timer_id: id });
        }

        scheduler.cancel(id).expect("still outstanding");
    });

    local_test!(cancelling_a_repeating_timer_stops_it, async {
        let (mailbox, mut rx) = Mailbox::pair(4);
        let mut scheduler = Scheduler::new(mailbox);
        let id = scheduler.set(PERIOD_MS as i64, true).expect("armed");

        let item = tokio::time::timeout(ONE_TICK, rx.recv())
            .await
            .expect("the first tick")
            .expect("the mailbox is still open");
        assert_eq!(item, Work::Timer { timer_id: id });

        scheduler.cancel(id).expect("cancelled");
        tokio::time::sleep(Duration::from_millis(PERIOD_MS * 4)).await;
        assert_eq!(
            rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty),
            "nothing fires after cancellation"
        );
    });

    local_test!(a_slow_guest_drops_ticks_rather_than_queuing_them, async {
        // The mailbox holds one item and nobody reads it until the sleep below ends, so every
        // tick after the first has nowhere to go. `try_send` drops those (see the module
        // docs) instead of this task blocking on `send` forever or the mailbox growing without
        // bound — and the mailbox's own capacity is what makes the assertion true regardless
        // of exactly how many ticks a slow CI box lets fire in the window.
        let (mailbox, mut rx) = Mailbox::pair(1);
        let mut scheduler = Scheduler::new(mailbox);
        let id = scheduler.set(PERIOD_MS as i64, true).expect("armed");

        tokio::time::sleep(Duration::from_millis(PERIOD_MS * 6)).await;

        assert_eq!(rx.recv().await, Some(Work::Timer { timer_id: id }));
        assert_eq!(
            rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty),
            "later ticks were dropped rather than queued"
        );
        scheduler.cancel_all();
    });

    local_test!(cancel_all_stops_every_armed_timer, async {
        let (mailbox, mut rx) = Mailbox::pair(4);
        let mut scheduler = Scheduler::new(mailbox);
        let a = scheduler.set(PERIOD_MS as i64, true).expect("armed");
        let b = scheduler.set(PERIOD_MS as i64, false).expect("armed");

        // Before either has had a chance to run at all: `set` returns without yielding, so
        // nothing spawned by it has been polled yet on this single-threaded `LocalSet`.
        scheduler.cancel_all();

        tokio::time::sleep(Duration::from_millis(PERIOD_MS * 5)).await;
        assert_eq!(
            rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty),
            "neither {a} nor {b} fires once cancelled"
        );
        // And both ids are gone, the same as an explicit `cancel` of each would leave them.
        assert_eq!(scheduler.cancel(a), Err(TimerError::NotFound));
        assert_eq!(scheduler.cancel(b), Err(TimerError::NotFound));

        // Idempotent: nothing left to abort, and nothing panics over it.
        scheduler.cancel_all();
    });

    local_test!(
        a_negative_or_zero_delay_is_refused_before_anything_is_armed,
        async {
            let (mailbox, _rx) = Mailbox::pair(1);
            let mut scheduler = Scheduler::new(mailbox);
            assert_eq!(scheduler.set(-1, false), Err(TimerError::InvalidArg));
            assert_eq!(scheduler.set(0, false), Err(TimerError::InvalidArg));
        }
    );

    local_test!(cancel_of_an_id_never_handed_out_is_not_found, async {
        let (mailbox, _rx) = Mailbox::pair(1);
        let mut scheduler = Scheduler::new(mailbox);
        assert_eq!(scheduler.cancel(41), Err(TimerError::NotFound));
    });
}

//! `eio:timer` — delayed and periodic callbacks (SDK §3, ABI §7.3).

use crate::convention::{id, status};
use crate::error::BlockError;

/// A timer, as ABI §7.3 identifies one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimerId(u32);

impl TimerId {
    /// The `u32` the ABI carries, and what `Block::on_timer` is handed.
    pub const fn get(self) -> u32 {
        self.0
    }
}

super::handle! {
    /// The `timer` capability (ABI §7.3).
    ///
    /// Timers fire as `Block::on_timer`, serialized with every other callback — so a timer
    /// cannot interrupt a delivery, and ABI §1.2's one-caller-at-a-time holds across them.
    ///
    /// **Not real-time.** ABI §7.3 makes resolution and drift host-defined. A block that needs
    /// a deadline rather than a nudge is a block the platform cannot promise anything to.
    Timer
}

impl Timer<'_> {
    /// Fires once after `delay_ms` (ABI §7.3).
    pub fn once(&mut self, delay_ms: i64) -> Result<TimerId, BlockError> {
        self.set(delay_ms, false)
    }

    /// Fires every `delay_ms` until cancelled or the instance stops (ABI §7.3).
    pub fn repeating(&mut self, delay_ms: i64) -> Result<TimerId, BlockError> {
        self.set(delay_ms, true)
    }

    /// Cancels a timer (ABI §7.3).
    ///
    /// The host also cancels every outstanding timer after `eio_stop` returns, so a block
    /// does not have to unwind its own on the way out.
    pub fn cancel(&mut self, timer: TimerId) -> Result<(), BlockError> {
        status("timer_cancel", crate::raw::timer_cancel(timer.get()))
    }

    fn set(&mut self, delay_ms: i64, repeat: bool) -> Result<TimerId, BlockError> {
        id("timer_set", crate::raw::timer_set(delay_ms, repeat)).map(TimerId)
    }
}

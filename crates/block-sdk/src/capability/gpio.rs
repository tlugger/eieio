//! `eio:gpio` — pins, levels and edges (SDK §3, ABI §7.4).

use crate::convention::{id, status};
use crate::error::{BlockError, HostError};

/// A pin's direction and pull (ABI §7.4).
///
/// A typed enum rather than the bare number, so `gpio_mode(pin, 2)` cannot be written and
/// a reader does not have to remember which of four numbers means pull-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mode {
    /// `0` — input, floating.
    Input,
    /// `1` — output.
    Output,
    /// `2` — input with a pull-up.
    InputPullup,
    /// `3` — input with a pull-down.
    InputPulldown,
}

impl Mode {
    /// The wire value (ABI §7.4).
    pub const fn as_i32(self) -> i32 {
        match self {
            Mode::Input => 0,
            Mode::Output => 1,
            Mode::InputPullup => 2,
            Mode::InputPulldown => 3,
        }
    }
}

/// Which transitions a watch fires on (ABI §7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Edge {
    /// `1` — low to high.
    Rising,
    /// `2` — high to low.
    Falling,
    /// `3` — both.
    Both,
}

impl Edge {
    /// The wire value (ABI §7.4).
    pub const fn as_i32(self) -> i32 {
        match self {
            Edge::Rising => 1,
            Edge::Falling => 2,
            Edge::Both => 3,
        }
    }
}

/// A pin's state (ABI §7.4).
///
/// `PinLevel` rather than `Level`, because [`eio_abi::Level`](crate::Level) already holds
/// ABI §7.0's log levels and the prelude exports it. Two things called `Level` in one
/// scope is the kind of collision a block author should never have to think about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PinLevel {
    /// `0`.
    Low,
    /// `1`.
    High,
}

impl PinLevel {
    /// The wire value (ABI §7.4).
    pub const fn as_i32(self) -> i32 {
        match self {
            PinLevel::Low => 0,
            PinLevel::High => 1,
        }
    }

    /// The level `value` names (ABI §7.4), or `None` if it is neither `0` nor `1`.
    ///
    /// Total in the sense that matters: `gpio_read` answers "0/1 or error", and a host
    /// returning some other non-negative number has said something the ABI does not
    /// define. That is reported rather than rounded, because guessing which way a `2`
    /// leans is guessing about a physical pin.
    pub const fn from_i32(value: i32) -> Option<PinLevel> {
        match value {
            0 => Some(PinLevel::Low),
            1 => Some(PinLevel::High),
            _ => None,
        }
    }
}

/// A GPIO watch, as ABI §7.4 identifies one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WatchId(u32);

impl WatchId {
    /// The `u32` the ABI carries, and what `Block::on_gpio` is handed.
    pub const fn get(self) -> u32 {
        self.0
    }
}

super::handle! {
    /// The `gpio` capability (ABI §7.4).
    ///
    /// Pin numbering is host and platform defined, surfaced through node configuration rather
    /// than the ABI — so a pin number here means whatever the node says it means, and a block
    /// that hard-codes one is a block tied to one board.
    Gpio
}

impl Gpio<'_> {
    /// Sets a pin's direction and pull (ABI §7.4).
    pub fn mode(&mut self, pin: u32, mode: Mode) -> Result<(), BlockError> {
        status("gpio_mode", crate::raw::gpio_mode(pin, mode.as_i32()))
    }

    /// Reads a pin (ABI §7.4).
    pub fn read(&mut self, pin: u32) -> Result<PinLevel, BlockError> {
        let returned = crate::raw::gpio_read(pin);
        if returned < 0 {
            let code = eio_abi::ErrorCode::from_i32(returned)
                .unwrap_or(eio_abi::ErrorCode::Unknown(returned));
            return Err(HostError::new("gpio_read", code).into());
        }
        PinLevel::from_i32(returned).ok_or_else(|| {
            BlockError::Decode(alloc::format!(
                "gpio_read answered {returned}, which ABI §7.4 does not define — it is 0, 1, \
                 or an error"
            ))
        })
    }

    /// Drives a pin (ABI §7.4).
    pub fn write(&mut self, pin: u32, level: PinLevel) -> Result<(), BlockError> {
        status("gpio_write", crate::raw::gpio_write(pin, level.as_i32()))
    }

    /// Watches a pin for edges, firing `Block::on_gpio` (ABI §7.4).
    pub fn watch(&mut self, pin: u32, edge: Edge) -> Result<WatchId, BlockError> {
        id("gpio_watch", crate::raw::gpio_watch(pin, edge.as_i32())).map(WatchId)
    }

    /// Stops watching (ABI §7.4).
    pub fn unwatch(&mut self, watch: WatchId) -> Result<(), BlockError> {
        status("gpio_unwatch", crate::raw::gpio_unwatch(watch.get()))
    }
}

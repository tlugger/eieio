//! `eio:i2c` — synchronous bus transactions (SDK §3, ABI §7.5).

use alloc::vec::Vec;

use crate::convention::{sized, status};
use crate::error::BlockError;

super::handle! {
    /// The `i2c` capability (ABI §7.5).
    ///
    /// **Synchronous by design**, and ABI §7.5 says why: an I2C transaction is microseconds to
    /// milliseconds and fits inside a callback deadline. That is also why the daemon places
    /// each instance on its own thread (DAEMON §5) — a host function that can block for
    /// milliseconds must not be able to stall another instance.
    ///
    /// A block still owes ABI §10 a prompt return. A long chain of transactions belongs in a
    /// timer, chunked, not in one callback.
    I2c
}

impl I2c<'_> {
    /// Writes to a device (ABI §7.5).
    pub fn write(&mut self, bus: u32, address: u32, bytes: &[u8]) -> Result<(), BlockError> {
        status("i2c_write", crate::raw::i2c_write(bus, address, bytes))
    }

    /// Reads from a device (ABI §7.5).
    ///
    /// `None` is ABI §8's `ERR_NOT_FOUND` — nothing at that address.
    pub fn read(&mut self, bus: u32, address: u32) -> Result<Option<Vec<u8>>, BlockError> {
        sized("i2c_read", |buffer| {
            crate::raw::i2c_read(bus, address, buffer)
        })
    }

    /// Writes then reads without releasing the bus (ABI §7.5).
    ///
    /// The register-read shape almost every I2C sensor wants: a repeated START rather than
    /// a write, a stop, and a read that another master could interleave with.
    pub fn write_read(
        &mut self,
        bus: u32,
        address: u32,
        write: &[u8],
    ) -> Result<Option<Vec<u8>>, BlockError> {
        sized("i2c_write_read", |buffer| {
            crate::raw::i2c_write_read(bus, address, write, buffer)
        })
    }
}

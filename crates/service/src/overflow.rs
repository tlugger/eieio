//! The service-wide overflow policy (SERVICE-SPEC §5, DAEMON-SPEC §6.2).
//!
//! One choice for the whole service, not one per connection: DAEMON §6.2 amends its earlier
//! per-connection framing on this crate's say-so, because a file where two edges into one
//! block behaved differently would be harder to read than the guarantee is worth. `boot`
//! (DAEMON §3) is the only caller that turns this into `eio-host-core`'s [`Overflow`], which
//! this crate does not depend on: what a full mailbox does is the router's concern, and what a
//! deployer is allowed to write is this format's.
//!
//! [`Overflow`]: https://docs.rs/eio-host-core (not a dependency of this crate)

/// What a full mailbox does to a sender, for every connection in the service.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Overflow {
    /// Wait for room, so the pressure propagates back to whoever is producing too fast.
    ///
    /// The default (SERVICE §5, DAEMON §6.2).
    #[default]
    Backpressure,
    /// Keep the newest batch and discard the older one, for sensor-style flows.
    DropOldest,
}

impl Overflow {
    /// The spellings SERVICE §5 accepts, in the order they are listed there.
    pub const ACCEPTED: [&'static str; 2] = ["backpressure", "drop-oldest"];

    /// Parses the `overflow` key's value.
    ///
    /// Returns the value as written on failure, so the caller can say what was given and what
    /// is accepted (SERVICE §7) — a misspelled policy is a deployer believing something about
    /// backpressure that is not true, so it is refused rather than quietly defaulted.
    pub fn parse(value: &str) -> Result<Overflow, &str> {
        match value {
            "backpressure" => Ok(Overflow::Backpressure),
            "drop-oldest" => Ok(Overflow::DropOldest),
            other => Err(other),
        }
    }
}

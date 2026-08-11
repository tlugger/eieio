//! One safe wrapper per `eio:*` namespace (SDK §3).
//!
//! Each handle is a zero-sized token reached through the extension trait the `#[block]`
//! macro generates, and the macro only puts a capability's accessor on that trait when the
//! block declared it. So `ctx.gpio()` without `capabilities(gpio)` is a missing method —
//! a compile error, as SDK §3 requires — rather than a runtime `ERR_CAPABILITY` that a
//! deployer discovers.
//!
//! # Why the handles are empty
//!
//! ABI §7's capability functions are free imports, not methods on anything: there is no
//! per-namespace state for a handle to carry. Each exists to *scope* the calls — so a
//! block author reads `ctx.state().put(..)` rather than a flat namespace of forty
//! functions — and to be the thing the macro can withhold.
//!
//! They borrow `Ctx` mutably for the same reason every other host call does — ABI §1.2
//! gives an instance one caller at a time — though the borrow's real work is narrower than
//! that: a callback can only reach `Ctx` through the `&mut Ctx` it was handed, so
//! containment across callbacks is already free. What the borrow adds is that two handles
//! cannot be live at once inside one callback.
//!
//! # What is deliberately not here
//!
//! No retries. ABI §7.2 makes persistence best-effort and says blocks "MUST treat
//! persistence as best-effort and not as a message queue" — a wrapper that retried
//! `ERR_THROTTLED` internally would be building exactly the queue the spec refuses, and
//! hiding from the block the one signal it needs to back off.
//!
//! No `async`. SDK §3 is firm: no runtime exists in an instance and the ABI is
//! callback-shaped, so `http` is a request-id pattern and correlating `ReqId` back to
//! purpose is the block's job through its own fields.

mod gpio;
mod http;
mod i2c;
mod state;
mod timer;

pub use gpio::{Edge, Gpio, Mode, PinLevel, WatchId};
pub use http::{Http, HttpRequest, HttpResponse, ReqId};
pub use i2c::I2c;
pub use state::State;
pub use timer::{Timer, TimerId};

/// Declares a capability handle.
///
/// Five of these, differing only in name and in the prose above them: each is a zero-sized
/// token that borrows the [`Ctx`](crate::Ctx) for its life. The *shape* is written once —
/// if a handle ever needs to carry something, it should start carrying it in one place —
/// while each capability keeps its own documentation, which is the part worth differing.
macro_rules! handle {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        ///
        /// Reached through the `Capabilities` trait the `#[block]` macro generates, and
        /// present only when the block declared it (SDK §3.1).
        #[derive(Debug)]
        pub struct $name<'ctx> {
            /// Borrows the [`Ctx`](crate::Ctx) for the handle's life.
            ///
            /// What that buys, precisely: two handles cannot be live at once *within* one
            /// callback. Containment to the callback itself comes for free, since `Ctx` is
            /// only reachable as the `&mut Ctx` a callback was handed. Invariant rather
            /// than covariant, which is what makes the exclusion hold.
            ///
            /// `pub` because the generated `Capabilities` impl constructs it, and
            /// generated code lives in the block's own crate.
            pub _ctx: core::marker::PhantomData<&'ctx mut ()>,
        }
    };
}

pub(crate) use handle;

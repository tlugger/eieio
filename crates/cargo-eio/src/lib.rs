//! `cargo-eio`'s library half.
//!
//! Every module here is also compiled into the `cargo-eio` binary (`src/main.rs`'s module doc
//! describes what each subcommand does); this file exists so that a test outside this crate
//! can call [`publish::run`] directly — eieio-7d8.33's publish/pull round trip, which has to
//! run the real publish path against the real `eio_daemon::registry::Registry::pull` and
//! cannot do that as a subprocess, since `Registry` is not reachable from outside
//! `eio-daemon` at all (its module stays private — see that crate's `registry.rs`). This is
//! the same fix, for the same reason, that `crates/cli` and `crates/daemon` already took
//! (eieio-yck.3): a lib target with a test as its only consumer.
//!
//! # Why `fake_registry` is exposed at all
//!
//! [`fake_registry`] is `publish`'s own in-process OCI registry — the only thing in this
//! crate that already speaks the *full* distribution API (push **and** pull, including the
//! `401`-then-token dance and the chunked blob upload `cosign` uses), because it has to: a
//! real, separate `cosign` process pushes a signature to it. The round trip needs exactly
//! that — one registry both `cargo-eio`'s real publish and the daemon's real pull can agree
//! on, with nobody's fake standing in for a rule either side is supposed to enforce itself —
//! so it is reused rather than duplicated. Reachable only behind the `testing` feature (never
//! part of an ordinary build or publish): `#[cfg(test)]` alone will not do, because that
//! attribute is only ever set while *this* crate itself is under test, not when it is pulled
//! in as another crate's dev-dependency (`eio-daemon`'s round-trip test needs it compiled into
//! *its* test binary, where `cfg(test)` here would never be set).
//!
//! # The other two options this issue named, and why they lose to this one
//!
//! - **Promoting `eio-daemon`'s own `registry::fake::Fake` out of `cfg(test)`.** That fixture
//!   is deliberately GET-only (a pull never writes), so making it reachable would still not
//!   give a real publish anywhere to push *to* — it would have to grow a write side first,
//!   duplicating exactly the OCI push protocol [`fake_registry`] already implements and this
//!   crate's own suite already exercises against a real `cosign`. Reusing tested code beats
//!   rewriting it a second time to avoid a one-line feature gate.
//! - **A new test-only crate depending on both `cargo-eio` and `eio-daemon`.** Adds a whole
//!   workspace member — a new directory, a new `Cargo.toml`, a new entry in the root
//!   manifest's `members` list — for one test. `eio-daemon` can already dev-depend on
//!   `cargo-eio` with no cycle (`cargo-eio` depends on `eio-manifest` and `eio-conformance`;
//!   neither depends back on `eio-daemon`), so a third crate buys nothing a dev-dependency
//!   does not already give for less.
//!
//! Both alternatives were also the *wider* change to `eio-daemon`'s own surface — DAEMON's
//! `registry` module stays exactly as private as it was before this issue, which is what the
//! plan asked for ("needs no visibility change at all" on that side): the only new surface is
//! this crate's own `publish` module and its feature-gated `fake_registry`, which this crate
//! already owns.

pub mod build;
pub mod new;
pub mod publish;
pub mod test;

mod oci;
mod template;

/// `publish`'s own in-process OCI registry (push and pull) — see this module's doc for why it
/// is reachable outside its own crate's tests at all, and only behind the `testing` feature.
#[cfg(any(test, feature = "testing"))]
pub mod fake_registry;

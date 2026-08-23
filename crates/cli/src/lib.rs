//! `eio-cli`'s library half.
//!
//! Every module here is also compiled into the `eio` binary (`src/main.rs`'s module doc
//! describes what each one does); this file exists so that `tests/api_surface.rs` can check
//! [`client::ENDPOINTS`] — DAEMON-SPEC §9's surface as `client.rs` addresses it — without
//! spawning the binary, let alone a real daemon (eieio-yck.1's verification rule). `crates/cli`
//! has exactly the same problem `crates/daemon` does (no lib target for a test outside the
//! crate to import), and this is the fix, applied to the crate that could take it.

pub mod blocks;
pub mod client;
pub mod config;
pub mod logs;
pub mod node;
pub mod service;
pub mod services;
mod show;
pub mod state;
pub mod taps;

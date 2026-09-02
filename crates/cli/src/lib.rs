//! `eio-cli`'s library half.
//!
//! Every module here is also compiled into the `eio` binary (`src/main.rs`'s module doc
//! describes what each one does); this file exists so that `tests/openapi_surface.rs` can
//! check [`client::ENDPOINTS`] — DAEMON-SPEC §9's surface as `client.rs` addresses it —
//! against `eio-daemon`'s live OpenAPI document, without spawning either binary, let alone a
//! real daemon (eieio-yck.1's verification rule). `crates/daemon` took the same fix afterwards,
//! for the same reason and the same way (eieio-yck.3): a lib target with a test as its only
//! consumer.

pub mod blocks;
pub mod client;
pub mod config;
pub mod logs;
pub mod mcp;
pub mod node;
pub mod nodes;
pub mod service;
pub mod services;
mod show;
pub mod state;
pub mod taps;

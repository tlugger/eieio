//! The engine-agnostic host side of the eieio block ABI (ABI-SPEC).
//!
//! This crate is the half of a host that is *not* about a particular WASM engine: the
//! lifecycle state machine (ABI §5.1), the instance descriptor (§5.2), the memory and
//! ownership conventions (§6.1, §9), and the status/size/id return protocol (§8). The
//! daemon drives it with wasmtime; the leaf runtime will drive it with WAMR or wasm3.
//!
//! …and the property access protocol (§7.1), which is where the three crates below this
//! one meet: `eio_expr` parses and evaluates, `eio_signal` supplies the batch and carries
//! the result, `eio_manifest` says what type that result must be.
//!
//! `no_std` (`alloc` permitted) is a hard requirement, because the leaf runtime compiles
//! this crate onto an MCU (DAEMON §1). Its dependencies are the other three ★ crates, and
//! there is no engine anywhere in it.
//!
//! # Why one crate, driven twice
//!
//! ABI §13 says "divergence between the two hosts is a conformance bug by definition". That
//! is only enforceable if there is one implementation of the contract with two engines
//! plugged into it, rather than two implementations tested against the same suite. The
//! [`Engine`] trait is that plug, and it is deliberately four methods wide: everything it
//! grows, a leaf engine must also provide.
//!
//! # What the types make impossible
//!
//! The ABI has two rules that are easy to state and easy to erode, so both are structural
//! here rather than documented:
//!
//! - **Traps are death, status codes are life** (ABI §8). A non-zero callback return comes
//!   back with the live instance and is counted; a trap comes back as a [`Trap`] with no
//!   instance attached. Neither can be mistaken for the other, because they are not the
//!   same type.
//! - **A stopped instance is never restarted** (ABI §5.1). [`Stopped`] has no `start`, and
//!   every call consumes the instance and returns its next state, so the illegal
//!   transitions of §5.1 cannot be written at all.
//!
//! # Example
//!
//! ```
//! use eio_host_core::{Configured, Configuring, Descriptor, Limits, Outcome, Starting};
//! # use eio_host_core::{Engine, EngineError, HostFn, Trap};
//! # use std::collections::BTreeMap;
//! # /// A guest that accepts everything and allocates from a bump pointer.
//! # struct FakeGuest { memory: Vec<u8>, next: u32 }
//! # impl Engine for FakeGuest {
//! #     fn call(&mut self, export: &str, args: &[i32]) -> Result<i32, Trap> {
//! #         if export == "eio_alloc" {
//! #             let ptr = self.next;
//! #             self.next += (args[0] as u32).next_multiple_of(8);
//! #             return Ok(ptr as i32);
//! #         }
//! #         Ok(0)
//! #     }
//! #     fn has_export(&self, _export: &str) -> bool { true }
//! #     fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
//! #         let (start, end) = (ptr as usize, (ptr + len) as usize);
//! #         self.memory.get(start..end).map(<[u8]>::to_vec)
//! #             .ok_or(EngineError::OutOfBounds { ptr, len })
//! #     }
//! #     fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
//! #         let (start, end) = (ptr as usize, ptr as usize + bytes.len());
//! #         let slot = self.memory.get_mut(start..end)
//! #             .ok_or(EngineError::OutOfBounds { ptr, len: bytes.len() as u32 })?;
//! #         slot.copy_from_slice(bytes);
//! #         Ok(())
//! #     }
//! #     fn register(&mut self, _ns: &str, _name: &str, _f: HostFn) -> Result<(), EngineError> {
//! #         Ok(())
//! #     }
//! # }
//! # let engine = FakeGuest { memory: vec![0; 4096], next: 8 };
//! let descriptor = Descriptor {
//!     instance_id: "filter-1".into(),
//!     block: "filter".into(),
//!     inputs: vec!["in".into()],
//!     outputs: vec!["true".into(), "false".into()],
//!     props: vec!["predicate".into()],
//!     // Host configuration, with no floor to fall back on (ABI §9.7, SCOPE §3.4).
//!     limits: Limits::new(64 * 1024, 256),
//! };
//!
//! // instantiate → CONFIGURED
//! let Configuring::Configured(configured) = Configured::configure(engine, &descriptor) else {
//!     panic!("the guest accepted its configuration")
//! };
//!
//! // CONFIGURED → RUNNING
//! let Starting::Running(running) = configured.start() else {
//!     panic!("the guest started")
//! };
//!
//! // RUNNING → STOPPED. `stopped.start()` does not exist, which is ABI §5.1's
//! // "a stopped instance is never restarted" as a compile error.
//! let Outcome::Live(stopped, status) = running.stop() else {
//!     panic!("the guest stopped")
//! };
//! assert!(status.is_ok());
//! assert_eq!(stopped.errors(), 0);
//! ```

#![no_std]

extern crate alloc;

mod descriptor;
mod engine;
mod instance;
mod memory;
mod prop;
mod status;

pub mod exports;

pub use descriptor::{Descriptor, Limits};
pub use engine::{Engine, EngineError, HostCall, HostFn, Memory, Trap, TrapKind};
pub use instance::{
    Configured, Configuring, Outcome, Running, Starting, Stopped, abi_version,
    check_required_exports,
};
pub use memory::{ALLOC_ALIGN, DeliveryFailure, Inbound, OutBuffer};
pub use prop::{CompileError, PropContext, PropFailure, PropertySource};
pub use status::{ErrorCode, Id, Size, Status};

/// No signal context: property evaluation outside `process_signals` (ABI §3, §7.1).
///
/// Carried as an `i32` across the boundary, where it is `-1`; a host compares the *unsigned*
/// interpretation, which is what this constant is.
pub const SIGNAL_NONE: u32 = 0xFFFF_FFFF;

/// The reserved error output port (ABI §3, §6.4).
///
/// Every block has it without declaring it, which is why it is a sentinel rather than an
/// index into the descriptor's `outputs`.
pub const PORT_ERR: u32 = 0xFFFF_FFFE;

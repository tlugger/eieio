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
//! …and the router core (DAEMON §1, §6): the connection table and fan-out, which are about
//! the service graph rather than about any queue a host delivers into.
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
//! - **A guest call carries its property scope** (ABI §7.1). The driver holds the
//!   [`PropContext`] and opens the scope itself, and `process_signals` takes the batch that
//!   `prop` will index rather than bytes beside it — so a callback without a scope, or a
//!   scope disagreeing with the batch the guest was handed, has no way to be written.
//!
//! # Example
//!
//! ```
//! use eio_host_core::{
//!     Configured, Configuring, Descriptor, Limits, Outcome, PropContext, PropertySource,
//!     Starting,
//! };
//! use eio_manifest::PropertyType;
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
//! // The property expressions, parsed and analysed before the guest sees anything
//! // (ABI §7.1). The driver keeps it and opens a scope around every callback.
//! let properties = PropContext::compile(&[PropertySource::new(
//!     "predicate", PropertyType::Bool, "(> $temp 20)",
//! )]).expect("the expression compiles");
//!
//! // instantiate → CONFIGURED
//! let Configuring::Configured(configured) =
//!     Configured::configure(engine, &descriptor, properties)
//! else {
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

mod budget;
mod core_fns;
mod descriptor;
mod engine;
mod instance;
mod memory;
mod prop;
mod router;

pub mod exports;
pub mod state;
pub mod timer;

// `eio:core`'s host side (DAEMON §1.1): the six functions every block may use
// unconditionally, minus `prop` (`crate::prop`'s) and minus the clock and entropy a host
// alone can answer. Re-exported flat like `eio:state` and `eio:timer`'s traits, so a host
// has one import for the capability rather than one per submodule.
pub use core_fns::{Clock, ClockSource, Core, Detail, Emission, Entropy, EntropyError, LogLine};

pub use budget::ExprBudgets;
pub use descriptor::{Descriptor, Limits};
pub use engine::{
    Arg, Engine, EngineError, HostCall, HostFn, Memory, Ret, Trap, TrapKind, memory_range,
};
pub use instance::{
    Configured, Configuring, Delivering, Outcome, Refusal, Running, Starting, Stopped, abi_version,
    check_required_exports,
};
pub use memory::{DeliveryFailure, Inbound, OutBuffer, Outbound};
pub use prop::{
    CompileError, PropContext, PropFailure, PropertyError, PropertySource, ResolveError, resolve,
};
pub use router::{
    Connection, Deliveries, End, Endpoint, Overflow, PORT_ERR_NAME, Port, RouteError, Routes,
    Target,
};
// `eio:state`'s host side. The trait is re-exported flat like everything else a host
// implements against; the three call handlers stay behind `state::` because a host reaches
// for them only when it is answering the imports itself rather than registering them.
pub use state::{StateError, StateStore};
// `eio:timer`'s host side, exported the same way and for the same reason.
pub use timer::{TimerError, Timers};
// The ABI's shared vocabulary lives in `eio-abi`, below both halves of the boundary, so
// that a guest can read ABI §8's codes without compiling this crate's expression
// interpreter and manifest parser (see that crate's module docs). Re-exported rather than
// merely available: a host has one import for the ABI, and moving these out was not meant
// to be visible at the call sites.
//
// [`PORT_ERR`] is the sentinel; [`PORT_ERR_NAME`] is the name a service file routes it by.
pub use eio_abi::{ALLOC_ALIGN, ErrorCode, Id, Level, PORT_ERR, SIGNAL_NONE, Size, Status};

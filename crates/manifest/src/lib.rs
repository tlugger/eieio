//! The eieio block manifest (ABI-SPEC §11).
//!
//! A manifest is what a block says about itself: its ports, its configurable
//! properties, the host capabilities it needs, and the ABI it was built against. It
//! is published in the registry beside the OCI artifact and embedded in the module
//! as the `eio:manifest` custom section (ABI §4.4, SCOPE §3.6). This crate is the
//! one implementation of its schema and its validation rules, shared by the daemon,
//! the leaf runtime, and the block tooling — `no_std` (`alloc` permitted) is
//! therefore a hard requirement.
//!
//! # It is four contracts at once
//!
//! - **Capability negotiation.** `capabilities` is what deploy-time validation
//!   checks a node against (SCOPE §3.3), and what a module's import section is
//!   cross-checked against at load time (ABI §4.3).
//! - **Port and property numbering.** Position in `inputs`, `outputs`, and
//!   `properties` defines the port indices and `prop_id`s the instance descriptor
//!   carries (ABI §5.2). Every collection here is a [`Vec`](alloc::vec::Vec) for
//!   that reason; reordering a manifest's properties renumbers a deployed block.
//! - **The configuration surface.** `properties` renders the Designer's config
//!   panel and is what an agent reads to configure a block without a canvas
//!   (SCOPE §4). Descriptions are user-facing documentation.
//! - **Version compatibility.** `abi` is the claim a host checks against the
//!   module's exported `eio_abi_version` (ABI §12).
//!
//! # The module cross-check
//!
//! [`validate`] is the other half: given a `.wasm` and optionally a registry manifest,
//! it answers "can this module be loaded here, and under what manifest" (ABI §4). It
//! checks that every import is an `eio:*` function that exists, that imports stay within
//! the declared capabilities, that capability-paired callbacks are present — and absent
//! when their capability is — that §4.1's exports exist with the right signatures, and
//! that the embedded and registry manifests agree.
//!
//! It deliberately does not validate the WASM itself. MVP conformance (§1) is enforced
//! by engine configuration, which is complete and which engines already track; a second
//! partial gate in the loader would only invite the belief that it was checked here.
//!
//! # Strict on purpose
//!
//! [`parse`] refuses unknown fields, duplicate keys, `null` in place of a missing
//! field, capabilities and property types outside their closed sets, malformed names
//! and versions, duplicate names within a list, and a `default` expression that does
//! not parse or does not pass static analysis (ABI §11.1, EXPR §10). The failure
//! being prevented is a typo that means nothing and is therefore ignored — a
//! `"capabilites"` list that grants no capability, discovered when the block fails
//! on a node at 2 a.m. Rejecting the whole document is also the only coherent
//! option: a partially accepted manifest leaves port indices ambiguous, and those
//! are load-bearing.
//!
//! # Example
//!
//! ```
//! use eio_manifest::{Abi, Capability, parse};
//!
//! let manifest = parse(
//!     r#"{
//!         "name": "filter",
//!         "version": "1.2.0",
//!         "abi": { "major": 1, "minor": 0 },
//!         "description": "Route signals by predicate",
//!         "capabilities": [],
//!         "inputs":  [ { "name": "in" } ],
//!         "outputs": [ { "name": "true" }, { "name": "false" } ],
//!         "properties": [
//!             {
//!                 "name": "predicate",
//!                 "type": "bool",
//!                 "description": "Evaluated per signal",
//!                 "default": "(> $temp 20)",
//!                 "required": true
//!             }
//!         ]
//!     }"#,
//! )
//! .unwrap();
//!
//! assert_eq!(manifest.abi, Abi::CURRENT);
//! assert_eq!(manifest.output_index("false"), Some(1));
//! assert!(!manifest.declares(Capability::Gpio));
//!
//! // A default is an expression, and an invalid one is an invalid manifest.
//! let broken = manifest.to_json().replace("(> $temp 20)", "(frobnicate)");
//! assert!(parse(&broken).is_err());
//! ```

#![no_std]

extern crate alloc;

mod abi;
mod check;
mod error;
mod module;
mod name;
mod parse;
mod schema;
mod validate;

pub use abi::{
    CORE_FUNCTIONS, CORE_NAMESPACE, ExportSpec, MEMORY_EXPORT, REQUIRED_EXPORTS, Signature, ValType,
};
pub use check::{validate, validate_against};
pub use error::{Error, ModuleError, NameSite};
pub use module::{Export, ExportKind, FuncType, Import, MANIFEST_SECTION, Module};
pub use name::{
    MAX_NAME_BYTES, PORT_ERR_NAME, PORT_NAME_PATTERN, REF_NAME_PATTERN, VERSION_PATTERN,
    is_port_name, is_ref_name, is_version,
};
pub use parse::{MAX_MANIFEST_BYTES, MIN_MANIFEST_BYTES, parse, parse_with_max_bytes};
pub use schema::{Abi, Capability, Manifest, PORTABLE_TARGET, Port, Property, PropertyType};

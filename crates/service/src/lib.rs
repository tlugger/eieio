//! The eieio service file (SERVICE-SPEC).
//!
//! A **service** is a graph of block instances on one node, and it is one file. This crate is
//! the one implementation of what that file says: its schema, its connection grammar, and the
//! two validation stages of SERVICE §7.
//!
//! # A block instance is its id, never its name
//!
//! The decision the rest of the format follows from (SERVICE §2). The TOML table key *is* the
//! instance's id; `name` is a label with no meaning to a host. Connections, `[ui]` entries and
//! API paths address the id, so renaming a block touches exactly one field and no wiring — and
//! two blocks may share a label, because a label identifies nothing.
//!
//! The predecessor got this right and it is worth saying why. nio identified a configured
//! block by a generated UUID and carried `name` beside it, so its services wired
//! `54b735e8-…` to `42c0915d-…` and a rename was a rename. The alternative — the name is the
//! identity — makes every connection a second place the name is written down.
//!
//! # Who writes a service file
//!
//! Not a host. Ids are minted by tooling at authoring time ([`id::generate`]), and SERVICE §2
//! makes it normative that a daemon never writes a service file: editing one by hand and
//! calling reload is a first-class path (SCOPE §3.8), and a reload that rewrote the file would
//! leave a git checkout dirty after every deploy.
//!
//! The tooling that *does* write one writes it through [`edit`], which is this crate's second
//! half and SERVICE §9's contract: an edit changes what it was asked to change and leaves the
//! comments, formatting and `[ui]` of the rest of the file alone. One implementation, because
//! a Designer canvas whose idea of what a service file may say differed from the CLI's would be
//! two formats (DESIGNER §4).
//!
//! # Two stages, because they need different things
//!
//! [`parse`] is everything checkable from the file alone — syntax, ids, connection grammar,
//! dangling references, duplicate edges, and every property expression parsed and statically
//! analysed by the real `expr` front end (EXPR §10). No registry needs to be reachable.
//!
//! [`validate`] is what needs the blocks resolved: that a connection's ports exist on the
//! manifests, in the right direction, and that every configured property is one the block
//! declares. It takes the manifests as an argument rather than fetching them, so the daemon,
//! the Designer and a CLI all run the same checks over whatever they happen to have.
//!
//! # Example
//!
//! ```
//! let parsed = eio_service::parse(
//!     r#"
//!         name = "kitchen"
//!         connections = [ "b7k2.out -> f3m9.in" ]
//!
//!         [blocks.b7k2]
//!         name = "Thermometer"
//!         block = "temp-sensor:1.0.0"
//!
//!         [blocks.b7k2.props]
//!         interval_ms = "5000"
//!
//!         [blocks.f3m9]
//!         block = "filter:1.2.0"
//!     "#,
//! )
//! .expect("it parses and passes stage 1");
//!
//! assert_eq!(parsed.connections[0].from.instance, "b7k2");
//! assert_eq!(parsed.connections[0].to.port, "in");
//! // The name is a label; the id is the identity.
//! assert_eq!(parsed.service.blocks["b7k2"].name.as_deref(), Some("Thermometer"));
//! ```

mod connection;
mod error;
mod parse;
mod schema;
mod validate;

pub mod edit;
pub mod id;

pub use connection::{Connection, Terminal};
pub use edit::{Document, EditError};
pub use error::{ConnectionError, Error, ResolvedError, Span};
pub use parse::{Parsed, parse};
pub use schema::{Instance, Service};
pub use validate::validate;

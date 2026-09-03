//! DESIGNER-SPEC §3.1's own HTTP surface: session, systems, nodes, registries, blocks, and
//! the one catch-all proxy to a node. `lib.rs::router` is where these are wired together and
//! gated; each module here is one resource.

pub mod blocks;
pub mod nodes;
pub mod openapi;
pub mod proxy;
pub mod registries;
pub mod service_edit;
pub mod session;
pub mod systems;

//! The `#[block]` attribute macro (SDK-SPEC §1).
//!
//! A block author writes a struct, some `#[prop]` fields, and an `impl Block`. This turns
//! that into a conformant ABI module: every export ABI §4.1 requires, the §4.2 callbacks
//! the declared capabilities pair with, the `In`/`Out` port enums, the `Prop<T>`
//! initializers bound to their `prop_id`s, and the `eio:manifest` custom section of §4.4.
//!
//! Not used directly — `eio-sdk` re-exports it, and a block author writes
//! `use eio_sdk::prelude::*`. It is a separate crate because the language requires it: a
//! `proc-macro = true` crate can export nothing but macros.
//!
//! # The single source of truth
//!
//! SDK §1's claim is that "manifest/import mismatches become unrepresentable rather than
//! merely validated", and that is a claim about *where the facts live*. The port list is
//! written once and produces the enum, the export set and the manifest's `inputs`/`outputs`
//! together; the capability list produces the §4.2 exports and the manifest's
//! `capabilities` together. There is no second place to update and therefore no drift to
//! validate against.
//!
//! # Errors
//!
//! ABI §11.1's rules are enforced here rather than at load: a reserved port name, a
//! duplicate property, a capability outside the closed set, a name the pattern refuses.
//! All of them are things a host would reject at deploy, and a block author should hear
//! them from `cargo build`. `crates/block-sdk/tests/ui/` pins the messages.

use proc_macro::TokenStream;
use syn::{ItemStruct, parse_macro_input};

mod generate;
mod parse;

/// Turns a struct into a conformant eieio block (SDK §1).
///
/// ```ignore
/// use eio_sdk::prelude::*;
///
/// #[block(
///     name = "threshold_filter",
///     description = "Route signals by comparing an attribute to a threshold",
///     inputs(default),
///     outputs(above, below),
///     capabilities(),
/// )]
/// struct ThresholdFilter {
///     #[prop(ty = "float", desc = "Compared per signal", default = "(float $value)")]
///     reading: Prop<f64>,
///     #[prop(ty = "float", default = "50.0")]
///     threshold: Prop<f64>,
/// }
/// ```
///
/// The grammar is normative in SDK §1.1.
#[proc_macro_attribute]
pub fn block(args: TokenStream, input: TokenStream) -> TokenStream {
    let item = parse_macro_input!(input as ItemStruct);
    match expand(args.into(), item) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

/// Parse, check, and generate — the two gates in the order they give the best message.
///
/// `parse` refuses what it can point a token at; `check_manifest` then asks `eio-manifest`
/// whether the document about to be emitted is one a host would accept. The second is what
/// makes the macro complete against ABI §11.1 rather than complete against a list.
fn expand(
    args: proc_macro2::TokenStream,
    item: ItemStruct,
) -> syn::Result<proc_macro2::TokenStream> {
    let block = parse::Block::parse(args, item)?;
    generate::check_manifest(&block)?;
    Ok(generate::expand(&block))
}

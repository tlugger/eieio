//! Moving what was just built to `'static`.
//!
//! [`eio_leaf::BakedGraph`] is `&'static` throughout because in an image it lives in
//! `.rodata` (LEAF §6.4.2, §6.3). On the build host the same value has to be *built*, and the
//! only allocation strategy that produces a `&'static` from a runtime `String` is to give the
//! allocation away.
//!
//! That is deliberate rather than expedient, and it buys the property LEAF §6.4.4 needs:
//! `crate::bake` produces the very value `crate::emit` prints, so a parity suite that asserts
//! on the value is asserting on what the file will say. The alternative — an owned mirror
//! type beside the borrowed one — would be two models of one thing, and §6.4.2 already went
//! out of its way to avoid a mirror ("used directly rather than mirrored").
//!
//! The cost is a process that never frees the graph, which is the right cost for a build tool
//! that generates one and exits. Nothing here is reachable from an image: this crate is the
//! build host's, and none of it is linked into the firmware (`crate`'s own docs).

use eio_host_core::PropertySource;
use eio_leaf::graph::BakedGraph;

/// The same string, at `'static`.
pub fn str(text: &str) -> &'static str {
    Box::leak(text.to_string().into_boxed_str())
}

/// The same strings, as a `'static` slice of `'static` strs — a port-name list, in index
/// order (ABI §5.2), or a candidate list.
pub fn strs<S: AsRef<str>>(items: &[S]) -> &'static [&'static str] {
    slice(items.iter().map(|item| str(item.as_ref())).collect())
}

/// The same bytes, at `'static` — a block artifact, or a bus key.
pub fn bytes(data: Vec<u8>) -> &'static [u8] {
    Box::leak(data.into_boxed_slice())
}

/// The same values, as a `'static` slice.
pub fn slice<T>(items: Vec<T>) -> &'static [T] {
    Box::leak(items.into_boxed_slice())
}

/// A resolved property list at `'static`.
///
/// The one place the borrow structure has to be rebuilt rather than moved, because a
/// [`PropertySource`] borrows its name from the manifest and its source text from the
/// manifest or the service file — two documents this function outlives. Nothing is *decided*
/// here: `ty` is copied, and `source` is `Some`/`None` exactly as
/// [`eio_host_core::resolve`] returned it, which is ABI §11.1's rule already applied.
pub fn props(sources: &[PropertySource<'_>]) -> &'static [PropertySource<'static>] {
    slice(
        sources
            .iter()
            .map(|property| PropertySource {
                name: str(property.name),
                ty: property.ty,
                source: property.source.map(str),
            })
            .collect(),
    )
}

/// The graph itself.
pub fn graph(graph: BakedGraph) -> &'static BakedGraph {
    Box::leak(Box::new(graph))
}

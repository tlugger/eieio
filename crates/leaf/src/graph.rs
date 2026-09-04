//! The baked graph (LEAF-SPEC §6.4): the types a generated file declares data of.
//!
//! A daemon reads `node.toml` and a service file at boot; a leaf has neither at runtime, so a
//! firmware build resolves both on the build host and bakes the results (§6). §6.4 settles
//! what "bakes" means, and it is not a format: **it is generated Rust source containing one
//! `static` of hand-written types**. The types are here. The generated file declares data of
//! them and nothing else — no `fn`, no control flow — which is the rule that keeps §2's MUST
//! NOT list true, because generated logic is where a second lifecycle driver, a second router
//! and a second property-resolution rule are born one convenience at a time.
//!
//! **A leaf's `main` is not generated.** It is hand-written per target, `include!`s the
//! generated file and hands its `static GRAPH: BakedGraph` to [`crate::spawn_graph`] — which
//! is hand-written too, in this crate, for the same reason.
//!
//! # Borrowed mirrors, and what is *not* mirrored
//!
//! [`eio_host_core::Descriptor`] and [`eio_host_core::Connection`] own their strings, so they
//! cannot be a `static`; [`BakedInstance`] and [`BakedConnection`] are their borrowed forms,
//! and a leaf builds the owned ones once at boot ([`BakedGraph::descriptors`],
//! [`BakedGraph::wiring`]). Everything else is used directly rather than mirrored:
//! [`Limits`], [`Overflow`], [`PropertySource`] and [`Capability`] are `eio-host-core`'s and
//! `eio-manifest`'s own types. `PropertySource` in particular has `const` constructors and
//! `&'a str` fields, which is what makes §6.4.1's "serialise what `resolve` returned" a shape
//! a generator can actually emit.
//!
//! # What stays underived, on purpose (§6.4.1)
//!
//! - **Connections are names**, resolved on the device by [`Routes::resolve`]. Precomputing
//!   [`eio_host_core::Endpoint`] pairs would put the router's numbering into generated code.
//! - **Property expressions are source text**, compiled on the device at configure time by
//!   `PropContext::compile_with_limits` under §4's budgets.
//!
//! The converse — everything that *could* have been computed on the build host — is
//! `host-core`'s own output, serialised: a generator calls `eio_manifest::validate`,
//! `Descriptor::from_manifest` and `eio_host_core::resolve` and prints what they returned.
//!
//! # Instance order is the numbering
//!
//! [`Routes::resolve`] indexes descriptors positionally, so a [`BakedInstance`]'s position
//! *is* its `Endpoint::instance`. The order is part of the artifact rather than an
//! implementation detail: ascending instance-id order, which is what `eio-service` already
//! yields (its `blocks` is a `BTreeMap`), so rebuilding the same file numbers the same
//! instances the same way.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

// Re-exported rather than merely imported, so that a *generated* file — which is `include!`d
// into a per-target `main` and may assume nothing about that crate's imports — can spell every
// type it names under one path, `eio_leaf::graph::`. These are `eio-host-core`'s and
// `eio-manifest`'s own types, used directly and never mirrored (LEAF §6.4.2).
pub use eio_host_core::{Connection, Descriptor, Limits, Overflow, Port, PropertySource, Routes};
pub use eio_manifest::{Capability, PropertyType};

/// A leaf's whole configuration, as one `static` (LEAF §6.4.2).
///
/// The three things §6 says a firmware build bakes: the service graph (`instances`,
/// `connections`, `overflow`), the node's identity and limits (`node`), and the transport
/// configuration (`transport`).
#[derive(Debug)]
pub struct BakedGraph {
    /// What `node.toml` would have carried (§6.4.3).
    pub node: BakedNode,
    /// The service's instances, in ascending instance-id order — which *is* the
    /// `Endpoint::instance` numbering.
    pub instances: &'static [BakedInstance],
    /// The wiring, as names. Resolved on the device by [`Routes::resolve`].
    pub connections: &'static [BakedConnection],
    /// One policy for the whole service (SERVICE §5, DAEMON §6.2).
    pub overflow: Overflow,
    /// The bus this node speaks on, or [`None`] for a node that runs no bridge
    /// (DAEMON §7.1: no `pubsub.toml` is the normal case).
    pub transport: Option<BakedTransport>,
}

/// The node's own identity and limits — what `node.toml` carried (§6.4.3).
#[derive(Debug)]
pub struct BakedNode {
    /// DAEMON §2.1's node id.
    ///
    /// **A required build input.** A leaf has no first boot that could write one, and a
    /// build that minted one would hand a device a new identity every reflash — a Designer
    /// registry entry, a state namespace and a bus identity all quietly ceasing to refer to
    /// the same thing (§6.4.3).
    pub id: &'static str,
    /// A label. Nothing resolves by it.
    pub name: Option<&'static str>,
    /// The service's name — LEAF §5's key component, kept even though a leaf has one
    /// service, so the state key layout is a daemon's (§5).
    pub service: &'static str,
    /// ABI §9.7's two numbers, per instance. Not a build input: §4.2 fixes them.
    pub limits: Limits,
}

/// One block instance (SERVICE §2: a block instance is its **id**, never its name).
///
/// Its [`fmt::Debug`] is hand-written for one reason: `module` is a whole compiled block, and
/// a derived `Debug` would render a `{graph:?}` in a log line or a test failure as tens of
/// thousands of byte literals. It prints the length instead.
pub struct BakedInstance {
    /// The instance id, unique within the service.
    pub id: &'static str,
    /// The registry reference the service file named, for diagnostics only — nothing on a
    /// leaf resolves it (§1: there is no registry to pull from at runtime).
    pub block: &'static str,
    /// The block's compiled artifact, linked into the image (§6.3).
    ///
    /// A `&'static [u8]` into `.rodata`, which on the v1 target is memory-mapped flash, so
    /// no RAM is spent holding it. **A leaf never reads a block's code out of a flash region
    /// it did not link.** A generator emits [`include_module!`] rather than a bare
    /// `include_bytes!` — see that macro for why alignment is not optional.
    pub module: &'static [u8],
    /// Input port names; position is the port index (ABI §5.2), in manifest order.
    pub inputs: &'static [&'static str],
    /// Output port names; position is the port index (ABI §5.2), in manifest order.
    pub outputs: &'static [&'static str],
    /// Every property the block declares, in `prop_id` order, as
    /// [`eio_host_core::resolve`] returned it (ABI §11.1).
    ///
    /// Expression *source text*, not a compiled form: §6 settles that pre-parsing on the
    /// build host is deliberately not specified, because it would put a second
    /// representation of an expression into the platform.
    pub props: &'static [PropertySource<'static>],
    /// The capabilities the block's manifest declares (ABI §4.3).
    pub capabilities: &'static [Capability],
}

/// One connection, as a service file writes it: two `(instance id, port name)` pairs.
///
/// The overflow policy is [`BakedGraph::overflow`] rather than a field here, because SERVICE
/// §5 gives a service one policy for all of its connections.
#[derive(Debug)]
pub struct BakedConnection {
    /// The emitting output port — or ABI §6.4's reserved error port.
    pub from: (&'static str, &'static str),
    /// The receiving input port. Never the error port (§6.4).
    pub to: (&'static str, &'static str),
}

/// The bus configuration, baked — what `pubsub.toml` carried (DAEMON §7.1).
#[derive(Debug)]
pub struct BakedTransport {
    /// The bus name. ABI §11.1's name pattern, the same as a block's.
    pub bus: &'static str,
    /// The ranked broker candidates, `<node-id>@<host>:<port>` each (DAEMON §7.1).
    pub candidates: &'static [&'static str],
    /// The candidate id to dial exclusively, if the bus is pinned.
    pub pinned: Option<&'static str>,
    /// SCOPE §3.11's bus pre-shared key.
    ///
    /// Bytes rather than a `&str` because it is a credential rather than text, and because
    /// what a transport client presents it as is LEAF §11's transport item to settle.
    pub key: Option<&'static [u8]>,
}

impl core::fmt::Debug for BakedInstance {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BakedInstance")
            .field("id", &self.id)
            .field("block", &self.block)
            .field("module", &format_args!("<{} bytes>", self.module.len()))
            .field("inputs", &self.inputs)
            .field("outputs", &self.outputs)
            .field("props", &self.props)
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

impl BakedGraph {
    /// The owned [`Descriptor`] list, in instance order — which is the `Endpoint::instance`
    /// numbering [`Routes::resolve`] indexes against.
    ///
    /// Building the owned form once at boot is what §6.4.2 says a leaf does with the
    /// borrowed mirrors. Nothing is *derived* here: every field is copied out of the baked
    /// instance, and `limits` is the node's, because ABI §9.7's two numbers are a host's and
    /// this host states one pair.
    pub fn descriptors(&self) -> Vec<Descriptor> {
        self.instances
            .iter()
            .map(|instance| Descriptor {
                instance_id: instance.id.to_string(),
                block: instance.block.to_string(),
                inputs: names(instance.inputs),
                outputs: names(instance.outputs),
                props: instance
                    .props
                    .iter()
                    .map(|property| property.name.to_string())
                    .collect(),
                limits: self.node.limits,
            })
            .collect()
    }

    /// The owned [`Connection`] list, every one carrying the service's one overflow policy.
    pub fn wiring(&self) -> Vec<Connection> {
        self.connections
            .iter()
            .map(|connection| {
                Connection::new(
                    Port::new(connection.from.0, connection.from.1),
                    Port::new(connection.to.0, connection.to.1),
                )
                .with_overflow(self.overflow)
            })
            .collect()
    }

    /// The connection table, resolved on the device by the router core (DAEMON §6).
    ///
    /// **A failure here means the generator is wrong** (§6.4.1). What resolution refuses — an
    /// unknown id or port, `err` as a destination, a duplicate edge — was refused on the
    /// build host too, because the service file was validated there (SERVICE §7, §10). So a
    /// leaf treats this as fatal at boot rather than running a partial table.
    pub fn routes(&self) -> Result<Routes, String> {
        let descriptors = self.descriptors();
        Routes::resolve(&descriptors, &self.wiring()).map_err(|error| {
            alloc::format!(
                "the baked connection table does not resolve, which means the firmware build \
                 that generated it is wrong: {error}"
            )
        })
    }
}

/// The port names as the descriptor wants them, order preserved — the port numbering.
fn names(ports: &'static [&'static str]) -> Vec<String> {
    ports.iter().map(|name| name.to_string()).collect()
}

/// What a module buffer's **base address** must be a multiple of, for the engines LEAF §3
/// names (`eieio-x7g.2.20`).
///
/// **Four, measured from the engines' own source rather than their documentation**, in
/// `wamrx-sys` 0.3.0's vendored WAMR 2.4.3 and `wasm3x-sys` 0.1.0's vendored wasm3. The three
/// loaders a leaf can reach, and what each does with the buffer it is handed:
///
/// - **WAMR, `.aot`: 4.** Normative in WAMR's own public header —
///   `core/iwasm/include/wasm_export.h` on `wasm_runtime_load`: *"If it is AOT binary data, it
///   must be 4-byte aligned."* `core/iwasm/aot/aot_loader.c` agrees and bounds it: every read
///   goes through `TEMPLATE_READ`, which advances the cursor with `align_ptr(p, sizeof(type))`
///   and then loads through a bare `*(type *)p` cast, and no call site in the file aligns to
///   more than 4. The one type that would need 8 is read as two 4-byte loads on purpose —
///   `GET_U64_FROM_ADDR` copies `addr[0]` and `addr[1]` into a union — so **8 is deliberately
///   never required.**
/// - **WAMR, `.wasm`: 4.** `core/iwasm/interpreter/wasm_loader.c` parses the body bytewise
///   through LEB decoders, but its `read_uint32` is `TEMPLATE_READ_VALUE`, a bare
///   `*(uint32 *)` cast with *no* `align_ptr` at all. It is used exactly twice — on the magic
///   number and the version word, at buffer offsets 0 and 4 — so both loads inherit the base's
///   alignment directly.
/// - **wasm3, `.wasm`: 1.** `source/m3_core.c`'s `Read_u32`/`Read_u64`/`Read_f64` each go
///   through `memcpy`, so wasm3 reads no multi-byte field out of the buffer with a cast and
///   imposes no requirement.
///
/// So the requirement is WAMR's, it is the same 4 for both artifact kinds, and it is on the
/// **base**: the AOT loader's `align_ptr` works on the absolute address, so a misaligned base
/// does not merely misalign a load — it advances the cursor to a *different file offset* than
/// `wamrc` wrote, and the parse desynchronises.
///
/// **Which is why the guard has to be structural.** On this repository's dev host every one of
/// those loads succeeds unaligned — x86-64 and aarch64 both permit it — so no host test can
/// fail on an under-aligned buffer. The tier where it bites is §6.2's, and there it is a fault
/// at boot on a flashed image. [`include_module!`] is therefore checked at compile time
/// against this constant rather than at run time against a device.
///
/// **16 was the previous value and is not wrong, only unmeasured**: it was chosen as "a
/// superset of any field either engine plausibly reads". The measurement above replaces the
/// plausibility with a number. Over-aligning further stays legal and buys nothing — the cost
/// of the old value was at most fifteen bytes of `.rodata` per artifact, which is why it was
/// never a bug.
pub const MODULE_ALIGN: usize = 4;

/// A block artifact, linked into the image and **aligned** (LEAF §6.4.2, §6.3).
///
/// `include_bytes!` alone is not enough: it yields an align-1 array, and WAMR reads a module's
/// magic number, its version word and every field of an AOT header through direct casts, while
/// §6.2's target gives no unaligned-access guarantee. A generator MUST emit this rather than a
/// bare `include_bytes!`.
///
/// The alignment is [`MODULE_ALIGN`] — **4, measured from both engines' loaders**, not read
/// off their documentation. That constant carries the measurement and its provenance; this
/// macro only has to satisfy it, and a `const` assertion below makes an edit that stopped
/// satisfying it a compile error rather than §6.1's fault at boot.
///
/// The path is a string literal and MUST be absolute: the generated file is a build artifact
/// written into the build directory and `include!`d from there, so a relative path would
/// resolve against a directory that is not the one it was generated for (§6.4.2).
///
/// ```
/// // A generated file's module statics look exactly like this.
/// static MODULE: &[u8] = eio_leaf::include_module!(concat!(
///     env!("CARGO_MANIFEST_DIR"),
///     "/tests/fixtures/aligned.bin"
/// ));
/// assert_eq!(MODULE.as_ptr() as usize % eio_leaf::graph::MODULE_ALIGN, 0);
/// assert_eq!(MODULE, b"eieio\n");
/// ```
#[macro_export]
macro_rules! include_module {
    ($path:expr) => {{
        /// The aligning wrapper. `_align` is a zero-length array of the alignment type,
        /// which contributes the alignment and no bytes; `bytes` is unsized so the `&`
        /// below coerces `[u8; N]` to `[u8]` and the length stops being part of the type.
        #[repr(C)]
        struct Aligned<Bytes: ?Sized> {
            _align: [u32; 0],
            bytes: Bytes,
        }
        // The measurement, enforced. `_align`'s type is what supplies the alignment, so a
        // future edit that narrowed it — or a platform where it aligned to less than
        // `MODULE_ALIGN` — fails the build here instead of faulting on a flashed image.
        const _: () = assert!(
            ::core::mem::align_of::<Aligned<[u8; 0]>>() >= $crate::graph::MODULE_ALIGN,
            "include_module! must align a module buffer to at least MODULE_ALIGN (LEAF §6.4.2)"
        );
        static ALIGNED: &Aligned<[u8]> = &Aligned {
            _align: [],
            bytes: *include_bytes!($path),
        };
        &ALIGNED.bytes
    }};
}

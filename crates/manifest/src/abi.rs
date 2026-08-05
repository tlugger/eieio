//! The module contract of ABI-SPEC §4 and §7, as tables.
//!
//! What a conforming module MUST export (§4.1), what it MUST export *because* it
//! imports something (§4.2), and what each import namespace contains (§7). These are
//! data, not code: the checks in [`crate::check`] are a walk over them, and a new
//! host function is a new row rather than a new branch.
//!
//! # Why the table lives here
//!
//! `host-core` implements these functions and the daemon binds them to an engine, so
//! either could plausibly own the list. It lives in `manifest` because this is the
//! crate that already knows what a capability *is* — [`Capability::namespace`] is
//! what makes the import cross-check possible at all — and because two copies of
//! this table would eventually disagree, which is exactly the divergence between
//! hosts the shared crates exist to prevent (DAEMON §1).

use core::fmt;

use crate::schema::Capability;

/// A WASM value type.
///
/// Core WASM MVP has exactly four, and ABI §3 uses two of them: every pointer, length,
/// identifier and status is an `i32`, and the clock functions return `i64`. Defined
/// here rather than re-exported from the WASM parser so that this crate's public
/// surface does not carry a parser version with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValType {
    /// 32-bit integer.
    I32,
    /// 64-bit integer.
    I64,
    /// 32-bit float. Never appears in this ABI; present because MVP has it.
    F32,
    /// 64-bit float. Never appears in this ABI; present because MVP has it.
    F64,
    /// A type outside core WASM MVP — a vector, a reference (§1).
    ///
    /// Unreachable in a module an MVP-configured engine accepts, and it deliberately
    /// equals nothing in the tables below, so a signature written in a post-MVP type
    /// can never match a required one.
    Other,
}

impl ValType {
    /// The type's spelling in WASM text format, for diagnostics.
    pub const fn as_str(self) -> &'static str {
        match self {
            ValType::I32 => "i32",
            ValType::I64 => "i64",
            ValType::F32 => "f32",
            ValType::F64 => "f64",
            ValType::Other => "a non-MVP type",
        }
    }
}

/// A function signature: parameters and results.
///
/// Results are a slice rather than an `Option` because MVP allows zero or one and the
/// shape then matches what a parser reports. Multi-value returns are outside MVP
/// (§1), so a two-result signature is unrepresentable in a conforming module and
/// simply never matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signature {
    /// Parameter types, in order.
    pub params: &'static [ValType],
    /// Result types. Empty or one element.
    pub results: &'static [ValType],
}

impl Signature {
    /// `(params...) -> i32`, which is every callback and every status-returning
    /// export in ABI §4.1.
    const fn status(params: &'static [ValType]) -> Signature {
        Signature {
            params,
            results: &[ValType::I32],
        }
    }
}

/// An export a conforming module must provide, and the signature it must have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportSpec {
    /// The export's name.
    pub name: &'static str,
    /// The signature it must have.
    pub signature: Signature,
}

/// The exported linear memory every module must have (§4.1).
///
/// Not an [`ExportSpec`]: it is a memory, not a function, so it has no signature —
/// only its presence and its kind are checked. Its size is the guest's business.
pub const MEMORY_EXPORT: &str = "memory";

const I32: &[ValType] = &[ValType::I32];
const I32_I32: &[ValType] = &[ValType::I32, ValType::I32];
const I32_I32_I32: &[ValType] = &[ValType::I32, ValType::I32, ValType::I32];
const I32_I32_I32_I32: &[ValType] = &[ValType::I32, ValType::I32, ValType::I32, ValType::I32];
const NONE: &[ValType] = &[];

/// Every required function export, with its signature (ABI §4.1).
///
/// In the spec's order. `eio_free` is the one entry that returns nothing, which is
/// why the table carries whole signatures rather than just parameter lists.
pub const REQUIRED_EXPORTS: [ExportSpec; 7] = [
    ExportSpec {
        name: "eio_abi_version",
        signature: Signature::status(NONE),
    },
    ExportSpec {
        name: "eio_alloc",
        signature: Signature::status(I32),
    },
    ExportSpec {
        name: "eio_free",
        signature: Signature {
            params: I32_I32,
            results: NONE,
        },
    },
    ExportSpec {
        name: "eio_configure",
        signature: Signature::status(I32_I32),
    },
    ExportSpec {
        name: "eio_start",
        signature: Signature::status(NONE),
    },
    ExportSpec {
        name: "eio_stop",
        signature: Signature::status(NONE),
    },
    ExportSpec {
        name: "eio_process_signals",
        signature: Signature::status(I32_I32_I32),
    },
];

/// The namespace available to every module without a manifest capability (§7.0).
pub const CORE_NAMESPACE: &str = "eio:core";

/// The functions `eio:core` provides (§7.0).
pub const CORE_FUNCTIONS: [&str; 7] = [
    "log",
    "emit",
    "prop",
    "error",
    "time_unix_ms",
    "time_mono_ms",
    "rand",
];

impl Capability {
    /// The optional export a module MUST provide because it imports this namespace,
    /// or `None` for a capability with no callback (ABI §4.2).
    ///
    /// `state` and `i2c` are synchronous — a call returns its result — so they have
    /// nothing to call back into. The pairing holds in both directions: exporting a
    /// callback without importing its namespace is equally a rejection, because the
    /// host would never invoke it (§4.2).
    pub const fn callback(self) -> Option<ExportSpec> {
        match self {
            Capability::State | Capability::I2c => None,
            Capability::Timer => Some(ExportSpec {
                name: "eio_on_timer",
                signature: Signature::status(I32),
            }),
            Capability::Gpio => Some(ExportSpec {
                name: "eio_on_gpio",
                signature: Signature::status(I32_I32),
            }),
            Capability::Http => Some(ExportSpec {
                name: "eio_on_http",
                signature: Signature::status(I32_I32_I32_I32),
            }),
        }
    }

    /// The functions this namespace provides (ABI §7.2–§7.6).
    ///
    /// An import naming something outside this list is rejected at load time rather
    /// than left to fail at instantiation, so the rejection can say which import was
    /// wrong (§4.3).
    pub const fn functions(self) -> &'static [&'static str] {
        match self {
            Capability::State => &["state_get", "state_put", "state_del"],
            Capability::Timer => &["timer_set", "timer_cancel"],
            Capability::Gpio => &[
                "gpio_mode",
                "gpio_read",
                "gpio_write",
                "gpio_watch",
                "gpio_unwatch",
            ],
            Capability::I2c => &["i2c_write", "i2c_read", "i2c_write_read"],
            Capability::Http => &["http_request"],
        }
    }

    /// The capability that grants `namespace`, or `None` if it is not a capability
    /// namespace.
    ///
    /// `eio:core` returns `None` — it is always available and is not declarable
    /// (§7.0), so a caller distinguishes it with [`CORE_NAMESPACE`].
    pub fn from_namespace(namespace: &str) -> Option<Capability> {
        Capability::ALL
            .into_iter()
            .find(|capability| capability.namespace() == namespace)
    }
}

impl fmt::Display for Signature {
    /// `(i32, i32) -> i32`, the shape a spec table row reads as.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "(")?;
        for (index, ty) in self.params.iter().enumerate() {
            if index > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", ty.as_str())?;
        }
        write!(f, ")")?;
        match self.results {
            [] => Ok(()),
            results => {
                write!(f, " -> ")?;
                for (index, ty) in results.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", ty.as_str())?;
                }
                Ok(())
            }
        }
    }
}

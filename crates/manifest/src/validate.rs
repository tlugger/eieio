//! Semantic validation: the ABI-SPEC §11.1 rules the deserializer cannot express.
//!
//! The split is deliberate. Presence, unknown fields, duplicate keys, JSON types
//! and the closed sets are structural, so `serde` enforces them while decoding and
//! reports a line and column. What is left over — patterns, semver, uniqueness
//! within a list, the portable target, and whether a `default` is a real expression
//! — needs the whole document in hand, and lives here.
//!
//! Validation stops at the first violation, with one exception: property `default`
//! expressions are collected, because EXPR §10 asks for every diagnostic at once rather
//! than one typo per attempt. Everything else is all-or-nothing — a manifest is generated
//! by the SDK from a block's source (SDK §1), so for the structural rules the interesting
//! case is "which rule did the generator break", not "how many".
//!
//! One rule here reaches past the document: a signal-independent property `default` is
//! *evaluated*, and its value checked against the property's declared type (ABI §11.1).
//! That is the only place this crate runs the interpreter, and
//! [`check_folded_type`]'s documentation says what it deliberately does not reject.

use alloc::collections::BTreeSet;
use alloc::string::ToString;
use alloc::vec::Vec;

use eio_expr::Expr;
use eio_signal::Value;

use crate::error::{Error, InvalidDefault, NameSite};
use crate::name::{PORT_ERR_NAME, is_port_name, is_ref_name, is_version};
use crate::schema::{Manifest, PORTABLE_TARGET, PropertyType};

impl Manifest {
    /// Checks every ABI §11.1 rule that survives deserialization.
    ///
    /// [`parse`](crate::parse) calls this, so a parsed manifest is always a valid
    /// one. It is public for the other direction: tooling that *builds* a manifest
    /// in memory (the `#[block]` macro, `cargo eio`) has to be able to check it
    /// without serializing first.
    pub fn validate(&self) -> Result<(), Error> {
        if !is_ref_name(&self.name) {
            return Err(Error::InvalidName {
                site: NameSite::Block,
                name: self.name.clone(),
            });
        }
        if !is_version(&self.version) {
            return Err(Error::InvalidVersion {
                version: self.version.clone(),
            });
        }

        // Capabilities cannot be misspelled — they are an enum — but they can be
        // repeated, and a repeat means the generator or the author is confused about
        // what the list is.
        unique(
            NameSite::Capability,
            self.capabilities.iter().map(|c| c.as_str()),
        )?;

        for (site, ports) in [
            (NameSite::Input, &self.inputs),
            (NameSite::Output, &self.outputs),
        ] {
            for port in ports {
                if !is_port_name(&port.name) {
                    return Err(Error::InvalidName {
                        site,
                        name: port.name.clone(),
                    });
                }
                // Shape first, then the one reserved string: `err` is a well-formed port
                // name and is refused for what it collides with, not for how it looks
                // (ABI §6.4, §11.1). A separate variant because a host reporting it is
                // saying something different from "that is not a name".
                if port.name == PORT_ERR_NAME {
                    return Err(Error::ReservedName { site });
                }
            }
            unique(site, ports.iter().map(|port| port.name.as_str()))?;
        }

        // Two passes over the defaults, because EXPR §10 wants every diagnostic at once and
        // the two halves are not peers. A default that will not parse or analyse is broken
        // in a way that makes the fold below meaningless, so all of those are collected and
        // reported together first; only a document where every default is a valid
        // expression goes on to be type-checked. That ordering is what
        // `parse_and_analysis_failures_still_come_first` pins, kept across the change.
        let mut invalid = Vec::new();
        let mut analysed = Vec::new();
        for property in &self.properties {
            if !is_port_name(&property.name) {
                return Err(Error::InvalidName {
                    site: NameSite::Property,
                    name: property.name.clone(),
                });
            }
            if let Some(default) = &property.default {
                match analyse_default(default) {
                    // Kept rather than dropped: the type pass below needs the same tree,
                    // and parsing a second time to get it back would double the work every
                    // valid manifest does.
                    Ok(expr) => analysed.push((property, expr)),
                    Err(source) => invalid.push(InvalidDefault {
                        property: property.name.clone(),
                        source,
                    }),
                }
            }
        }
        if !invalid.is_empty() {
            return Err(Error::InvalidDefaults(invalid));
        }
        for (property, expr) in &analysed {
            check_folded_type(&property.name, property.ty, expr)?;
        }
        unique(
            NameSite::Property,
            self.properties.iter().map(|p| p.name.as_str()),
        )?;

        for (site, targets) in [
            (NameSite::Target, &self.targets),
            (NameSite::Aot, &self.aot),
        ] {
            for target in targets {
                if !is_ref_name(target) {
                    return Err(Error::InvalidName {
                        site,
                        name: target.clone(),
                    });
                }
            }
            unique(site, targets.iter().map(|target| target.as_str()))?;
        }

        // An empty list is a legal, distinct claim — "no compiled artifact at all"
        // (ABI §11.1) — not a partial one, so it owes nothing to the portable-target
        // rule below. A *non-empty* list still MUST contain the portable target:
        // that is the rule an AOT-only `targets: ["esp32s3"]` breaks, and emptiness
        // is not a bigger version of that break, it is a different statement.
        // Whether `[]` is legitimate for *this* block — a host-implemented one — or
        // a bug that dropped every target is not decidable from the document alone;
        // [`crate::validate`] decides it once it has the module bytes the document
        // claims not to describe.
        if !self.targets.is_empty() && !self.targets.iter().any(|target| target == PORTABLE_TARGET)
        {
            return Err(Error::MissingPortableTarget);
        }

        Ok(())
    }
}

/// Rejects the first name that has already been seen in this namespace.
///
/// `BTreeSet` rather than sorting in place: the lists are short, and the borrow is
/// read-only so a caller's ordering — which *is* the port numbering — is untouched.
fn unique<'a>(site: NameSite, names: impl Iterator<Item = &'a str>) -> Result<(), Error> {
    let mut seen = BTreeSet::new();
    for name in names {
        if !seen.insert(name) {
            return Err(Error::DuplicateName {
                site,
                name: name.to_string(),
            });
        }
    }
    Ok(())
}

/// Parses, analyses and — where it can — evaluates a property default
/// (ABI §11.1, EXPR §10).
///
/// The real parser, the real static analyser and the real interpreter, not
/// approximations of them: a manifest that ships `(frobnicate 1)` as a default is
/// broken at build time, and the only thing that knows `frobnicate` is not a builtin
/// is the `expr` crate.
///
/// A **signal-independent** default is evaluated here and its value checked against
/// the declared type, because `"type": "int"` with `"default": "true"` can never
/// produce an int and there is no reason to wait until configure time to say so.
/// Three things are deliberately *not* rejected:
///
/// - A **signal-dependent** default, which is not evaluated at all — there is no
///   signal to evaluate it against, and it is checked per signal at run time.
/// - A default that **fails to evaluate**. An evaluation failure is a per-signal
///   outcome (ABI §7.1) and budgets are host configuration (EXPR §9), so rejecting one
///   would make a document's validity depend on which host read it. `(/ 1 0)` is a
///   valid declaration that fails at configure time.
/// - Anything about a property with no default at all.
fn analyse_default(source: &str) -> Result<Expr, eio_expr::Error> {
    let expr = eio_expr::parse(source)?;
    match eio_expr::analyze(&expr).first_error() {
        Some(error) => Err(*error),
        None => Ok(expr),
    }
}

/// The type half: a signal-independent default MUST fold to a value its declared type
/// admits (ABI §11.1). Only reached once every default in the document analyses.
fn check_folded_type(property: &str, declared: PropertyType, expr: &Expr) -> Result<(), Error> {
    if expr.is_signal_dependent() {
        return Ok(());
    }
    // `None` is ABI §7.1's `SIGNAL_NONE`, which is what a signal-independent
    // expression is defined against; the classifier above is what guarantees no sigil
    // can reach it and turn this into `NO_SIGNAL`.
    let Ok(folded) = eio_expr::eval(expr, None) else {
        return Ok(());
    };
    if declared.accepts(&folded) {
        return Ok(());
    }
    Err(Error::DefaultTypeMismatch {
        property: property.to_string(),
        declared,
        folded: value_kind(&folded),
    })
}

/// What to call the type of a folded value in an error message.
///
/// Covers the whole ABI §6.3 space, not just the six property types: a default can fold
/// to an array, a map or null, none of which any `type` but `any` admits, and "folded to
/// a map" is the sentence that explains the rejection.
fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Str(_) => "string",
        Value::Bytes(_) => "bytes",
        Value::Array(_) => "array",
        Value::Map(_) => "map",
    }
}

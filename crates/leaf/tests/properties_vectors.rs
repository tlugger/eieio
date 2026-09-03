//! `expr-tests/properties/`'s corpus (ABI-SPEC §7.1, §11.1), run through the leaf's own
//! property path at its own budgets (LEAF-SPEC §9, §4).
//!
//! `crates/host-core/tests/properties.rs` already proves this corpus against
//! `PropContext::compile` — `EvalLimits::DEFAULT`, the daemon's choice. LEAF §9 asks a
//! second, narrower question: does the same corpus still hold when compiled through
//! `eio_host_core::PropContext::compile_with_limits(&sources, EvalLimits::FLOORS)`, which is
//! the exact call `crates/leaf/src/lib.rs::spawn` makes for every instance it starts? This
//! file drives the vectors through that call rather than reconstructing what it does —
//! per this bead's brief, `compile_with_limits` is host-core's, not something a leaf-side
//! test should re-derive.
//!
//! What is genuinely new here, rather than reused, is the ABI-level harness that reads a
//! compiled property back out: `PropContext` answers `eio:core`'s `prop` import (ABI §7.1)
//! as a boxed [`HostFn`], which expects a [`HostCall`] carrying guest memory. There is no
//! guest here — this is a host-side conformance vector, not an ABI §13 scenario — so
//! [`FlatMemory`] below is the minimum needed to call that import at all: a `Vec<u8>` and
//! the two bounds-checked operations [`Memory`] requires. This is deliberately not a reuse
//! of `crates/host-core/tests/mock.rs`: that file's ~500 lines simulate a whole guest
//! (exports, a scriptable allocator, trap injection) for exercising the lifecycle driver,
//! none of which this file's one `prop` call needs — reaching for it here would be pulling
//! in machinery to answer a question it was never about.
//!
//! `expr-tests/README.md`'s vector notation (`crates/expr/tests/support/vector_format.rs`)
//! is reached the same way `crates/host-core/tests/properties.rs` reaches it: `#[path]`
//! across the crate boundary, by design (see that file's own doc comment).

#[path = "../../expr/tests/support/vector_format.rs"]
mod vector_format;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use eio_expr::EvalLimits;
use eio_host_core::{
    Arg, EngineError, ErrorCode, HostCall, Memory, PropContext, PropertySource, Ret, SIGNAL_NONE,
    Size, memory_range,
};
use eio_manifest::PropertyType;
use eio_signal::{Batch, Signal, Value};
use serde::Deserialize;
use vector_format::VectorValue;

/// How much room a vector's `prop` call offers. Large enough that no vector in the corpus
/// retries — `expr-tests/properties/`'s values are all small — so the grow-and-retry path
/// (`Size::Required`) is not something this suite exercises.
const CAP: u32 = 4096;

/// The minimal guest memory a `prop` call needs: one flat buffer, bounds-checked exactly as
/// every real [`Memory`] implementation must be (ABI §9.1).
struct FlatMemory(Vec<u8>);

impl FlatMemory {
    fn new(size: u32) -> FlatMemory {
        FlatMemory(vec![0u8; size as usize])
    }
}

impl Memory for FlatMemory {
    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
        let range = memory_range(self.0.len(), ptr, len)?;
        Ok(self.0[range].to_vec())
    }

    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
        let range = memory_range(self.0.len(), ptr, bytes.len() as u32)?;
        self.0[range].copy_from_slice(bytes);
        Ok(())
    }
}

/// One vector. The language suite's format (`expr-tests/README.md`) plus `type`, identical
/// to `crates/host-core/tests/properties.rs`'s `Vector` — it is the same corpus.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Vector {
    name: String,
    expr: String,
    #[serde(rename = "type")]
    declared: PropertyType,
    #[serde(default)]
    expect: Option<VectorValue>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    signal: Option<BTreeMap<String, VectorValue>>,
    #[expect(
        dead_code,
        reason = "documentation fields, read by humans not by the runner"
    )]
    #[serde(default)]
    spec: Option<String>,
    #[expect(
        dead_code,
        reason = "documentation fields, read by humans not by the runner"
    )]
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VectorFile {
    vectors: Vec<Vector>,
}

/// `expr-tests/properties/`, three directories up from this crate.
fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../expr-tests/properties")
}

/// Every vector in the corpus, paired with the file it came from.
fn corpus() -> Vec<(String, Vector)> {
    let mut all = Vec::new();
    for (file, text) in vector_format::json_files(&corpus_dir()) {
        let parsed: VectorFile = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{file} is not a valid vector file: {error}"));

        let mut names = std::collections::BTreeSet::new();
        for vector in &parsed.vectors {
            assert!(
                names.insert(vector.name.clone()),
                "{file}: two vectors named {:?}",
                vector.name,
            );
            assert!(
                vector.expect.is_some() != vector.error.is_some(),
                "{file}: {:?} must have exactly one of `expect` and `error`",
                vector.name,
            );
        }
        all.extend(parsed.vectors.into_iter().map(|v| (file.clone(), v)));
    }
    all
}

/// Every vector produces exactly what a guest reads back, compiled at `EvalLimits::FLOORS` —
/// the leaf's own budgets (LEAF §4) — through the exact call `eio_leaf::spawn` makes.
#[test]
fn property_vectors_pass_at_leaf_floors() {
    let mut executed = 0;
    for (file, vector) in corpus() {
        let at = format!("{file}: {}", vector.name);

        let context = PropContext::compile_with_limits(
            &[PropertySource::new(
                "under-test",
                vector.declared,
                &vector.expr,
            )],
            EvalLimits::FLOORS,
        )
        .unwrap_or_else(|error| panic!("{at}: does not compile at the leaf's floors: {error}"));

        let (batch, signal_idx) = match &vector.signal {
            Some(attributes) => {
                let mut signal = Signal::new();
                for (name, value) in attributes {
                    signal.set(name, value.value());
                }
                let mut batch = Batch::new();
                batch.push(signal);
                (Some(Rc::new(batch)), 0)
            }
            None => (None, SIGNAL_NONE),
        };

        context.during(batch, || {
            let mut memory = FlatMemory::new(CAP);
            let mut host_fn = context.host_fn();
            let args = [
                Arg::I32(0),
                Arg::I32(signal_idx as i32),
                Arg::I32(0),
                Arg::I32(CAP as i32),
            ];
            let ret = host_fn(HostCall {
                args: &args,
                memory: &mut memory,
            });
            let raw = match ret {
                Ret::I32(value) => value,
                other => panic!("{at}: prop returned {other:?}, not an i32 (ABI §7.1)"),
            };
            let size = Size::decode(raw, CAP as usize);

            match (&vector.expect, &vector.error) {
                (Some(expected), None) => match size {
                    Size::Written(written) => {
                        let bytes = &memory.0[..written];
                        let value =
                            Value::from_cbor(bytes).expect("prop writes canonical CBOR (§6.3.1)");
                        assert_eq!(value, expected.value(), "{at}: the value a guest decodes");
                    }
                    other => panic!("{at}: expected a value, got {other}"),
                },
                (None, Some(code)) => {
                    assert_eq!(code, "RESULT_TYPE", "{at}: the only code this suite pins");
                    match size {
                        // ABI §7.1: `RESULT_TYPE` reaches the guest as `ERR_EXPR`.
                        Size::Failed(ErrorCode::Expr) => {}
                        other => panic!("{at}: expected ERR_EXPR, got {other}"),
                    }
                    let failures = context.take_failures();
                    assert_eq!(failures.len(), 1, "{at}: one failure recorded");
                    assert_eq!(
                        failures[0].error.code,
                        eio_expr::ErrorCode::ResultType,
                        "{at}: the host records RESULT_TYPE, not some other expression failure",
                    );
                }
                _ => unreachable!(),
            }
        });
        executed += 1;
    }
    assert!(executed > 0, "the corpus is empty");
    println!("{executed} property vectors, 0 failed, at EvalLimits::FLOORS");
}

/// Every value kind and every declared type meet somewhere in the corpus.
///
/// The same audit `crates/host-core/tests/properties.rs` performs, ported here for the
/// reason `crates/leaf/tests/expr_vectors.rs` gives: a runner that trusts the corpus without
/// checking it is exactly the vacuous-pass risk this bead's negative proofs are about.
#[test]
fn the_corpus_covers_every_declared_type() {
    let mut satisfied: std::collections::BTreeSet<&str> = Default::default();
    let mut refused: std::collections::BTreeSet<&str> = Default::default();
    for (_, vector) in corpus() {
        let seen = if vector.expect.is_some() {
            &mut satisfied
        } else {
            &mut refused
        };
        seen.insert(vector.declared.as_str());
    }

    let missing: Vec<&str> = PropertyType::ALL
        .into_iter()
        .map(PropertyType::as_str)
        .filter(|ty| !satisfied.contains(ty))
        .collect();
    assert!(
        missing.is_empty(),
        "no vector shows what satisfies these types: {missing:?}",
    );

    let missing: Vec<&str> = PropertyType::ALL
        .into_iter()
        .filter(|ty| *ty != PropertyType::Any)
        .map(PropertyType::as_str)
        .filter(|ty| !refused.contains(ty))
        .collect();
    assert!(
        missing.is_empty(),
        "no vector shows what these types refuse: {missing:?}",
    );
    assert!(
        !refused.contains(PropertyType::Any.as_str()),
        "`any` accepts the whole §6.3 space, so a vector refusing one is wrong about the spec",
    );
}

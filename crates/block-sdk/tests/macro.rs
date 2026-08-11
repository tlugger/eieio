//! The `#[block]` macro end to end (SDK §1).
//!
//! SDK §1's `ThresholdFilter` is the specimen, written as the spec prints it. That is the
//! point of the test: ABI §14 makes SDK friction a spec bug, so if the example needs
//! adjusting to compile, the spec is what changes — and this file is where the two are
//! held together.
//!
//! Behaviour runs against `eio_sdk::raw`'s recording stub. The `TestHost` of SDK §6.1 —
//! scripted property tables evaluated by the real `expr` interpreter — is eieio-7d8.4, and
//! this issue blocks it, so exercising the example under `TestHost` is deferred there.

use eio_sdk::prelude::*;
use eio_sdk::raw::{Call, Recorder};

// ── SDK §1's example, verbatim ───────────────────────────────────────────────

#[block(
    name = "threshold_filter",
    description = "Route signals by comparing an attribute to a threshold",
    inputs(default),
    outputs(above, below),
    capabilities()
)]
struct ThresholdFilter {
    #[prop(ty = "float", desc = "Compared per signal", default = "(float $value)")]
    reading: Prop<f64>,
    #[prop(ty = "float", default = "50.0")]
    threshold: Prop<f64>,
}

impl Block for ThresholdFilter {
    fn process_signals(&mut self, ctx: &mut Ctx, _input: u32, batch: Batch) -> BlockResult {
        let mut above = Batch::new();
        let mut below = Batch::new();
        for (index, signal) in batch.iter().enumerate() {
            let index = index as u32;
            if self.reading.get(ctx, index)? > self.threshold.get(ctx, index)? {
                above.push(signal.clone());
            } else {
                below.push(signal.clone());
            }
        }
        ctx.emit(Out::Above, &above)?;
        ctx.emit(Out::Below, &below)?;
        Ok(())
    }
}

// ── the manifest the macro emitted (ABI §4.4, §11) ───────────────────────────

/// The exact bytes the macro put in the `eio:manifest` section (ABI §4.4).
///
/// Read from the generated static itself, not from a copy kept beside it: a fixture file
/// holding "what the macro should emit" is a second source of truth, and the whole claim of
/// SDK §1 is that there is only one. These are the bytes a host reads out of the module.
fn manifest_json() -> &'static str {
    core::str::from_utf8(&EIO_MANIFEST).expect("the section is UTF-8 JSON (ABI §4.4)")
}

/// Those bytes, through the one implementation of ABI §11.1 (DAEMON §1).
///
/// `parse` validates as well as parses: it applies every §11.1 rule, including the five
/// the JSON Schema cannot express — name patterns, uniqueness, the reserved `err` port,
/// and whether each `default` parses and passes EXPR §10's static analysis. So calling it
/// at all is the assertion; what each test adds is which field it then looks at.
fn manifest() -> eio_manifest::Manifest {
    eio_manifest::parse(manifest_json()).expect("it parses and validates")
}

#[test]
fn the_emitted_manifest_parses_and_validates() {
    // The loop that keeps a hand-written JSON emitter honest: the manifest crate is the
    // one implementation of ABI §11.1, so the emitter is held to the real rules rather
    // than to its author's memory of them.
    let manifest = manifest();

    assert_eq!(manifest.name, "threshold_filter");
    assert_eq!(manifest.abi.major, 1);
    assert_eq!(manifest.abi.minor, 0);
}

#[test]
fn port_order_in_the_manifest_is_the_generated_index() {
    // ABI §5.2: "index in array = port index". The enum discriminant and the manifest
    // position are two renderings of one fact, and this is where they are checked against
    // each other — a reordering that touched only one of them lands here.
    let manifest = manifest();

    let outputs: Vec<&str> = manifest
        .outputs
        .iter()
        .map(|port| port.name.as_str())
        .collect();
    assert_eq!(outputs, ["above", "below"]);
    assert_eq!(Out::Above.index(), 0);
    assert_eq!(Out::Below.index(), 1);
    assert_eq!(outputs[Out::Above.index() as usize], Out::Above.name());
    assert_eq!(outputs[Out::Below.index() as usize], Out::Below.name());

    let inputs: Vec<&str> = manifest
        .inputs
        .iter()
        .map(|port| port.name.as_str())
        .collect();
    assert_eq!(inputs, ["default"]);
    assert_eq!(In::Default.index(), 0);
}

#[test]
fn property_order_in_the_manifest_is_the_prop_id() {
    // ABI §5.2: "index in array = prop_id", and ABI §11 makes appending compatible while
    // reordering is not. The macro binds `Prop<T>` from field order, so this is the check
    // that field order and manifest order are the same order.
    let manifest = manifest();
    let names: Vec<&str> = manifest
        .properties
        .iter()
        .map(|property| property.name.as_str())
        .collect();
    assert_eq!(names, ["reading", "threshold"]);

    let filter = ThresholdFilter {
        reading: Prop::new(PropId::new(0)),
        threshold: Prop::new(PropId::new(1)),
    };
    assert_eq!(filter.reading.id().index(), 0);
    assert_eq!(filter.threshold.id().index(), 1);
    assert_eq!(names[filter.reading.id().index() as usize], "reading");
    assert_eq!(names[filter.threshold.id().index() as usize], "threshold");
}

#[test]
fn the_declared_defaults_survive_into_the_manifest() {
    // A `default` is an expression string (ABI §11.1) and the manifest crate checks that
    // it parses and passes static analysis — so this also proves `(float $value)` is a
    // real expression rather than a plausible-looking one.
    let manifest = manifest();
    let defaults: Vec<Option<&str>> = manifest
        .properties
        .iter()
        .map(|property| property.default.as_deref())
        .collect();
    assert_eq!(defaults, [Some("(float $value)"), Some("50.0")]);
}

// ── the generated code, running (ABI §6.1, §6.2, §7.1) ───────────────────────

/// The `Ctx` a callback would be handed, with limits a test is not about.
fn ctx() -> Ctx {
    Ctx::new(Limits {
        max_payload: 64 * 1024,
        max_batch: 1024,
    })
}

fn signal(key: &str, value: Value) -> eio_sdk::Signal {
    let mut signal = eio_sdk::Signal::new();
    signal.set(key, value);
    signal
}

fn filter() -> ThresholdFilter {
    ThresholdFilter {
        reading: Prop::new(PropId::new(0)),
        threshold: Prop::new(PropId::new(1)),
    }
}

#[test]
fn the_example_routes_each_signal_by_its_property() {
    // What the block is *for*, and the reason `Prop<T>`'s grow-and-retry and decode are
    // worth having: the block author's code is four lines of comparison.
    let recorder = Recorder::new();
    // Two signals, each needing `reading` then `threshold` — ABI §7.1's per-signal pull.
    recorder
        .queue_prop(&Value::Float(70.0).to_cbor())
        .queue_prop(&Value::Float(50.0).to_cbor())
        .queue_prop(&Value::Float(20.0).to_cbor())
        .queue_prop(&Value::Float(50.0).to_cbor());

    let mut ctx = ctx();
    let batch = Batch::from_vec(vec![
        signal("value", Value::Float(70.0)),
        signal("value", Value::Float(20.0)),
    ]);

    filter()
        .process_signals(&mut ctx, In::Default.index(), batch)
        .expect("it routes");

    let emitted: Vec<(i32, usize)> = recorder
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            Call::Emit(port, bytes) => {
                Some((port, Batch::from_cbor(&bytes).expect("canonical").len()))
            }
            _ => None,
        })
        .collect();

    // One signal above, one below, each on its own port — and both ports emitted on even
    // though one batch could have been empty (ABI §6.3: an empty batch is legal and
    // routable like any other).
    assert_eq!(emitted, [(0, 1), (1, 1)]);
}

#[test]
fn a_property_that_fails_for_one_signal_fails_that_call_and_nothing_else() {
    // ABI §7.1: a per-signal failure is `ERR_EXPR` "for that call only; the instance is
    // unaffected". The SDK's job is to surface it as an ordinary `Err` the block can `?`
    // on — which is what makes "skip the signal, substitute a default, or route it to
    // PORT_ERR" a choice the block actually has.
    let _recorder = Recorder::new();
    let mut ctx = ctx();
    let batch = Batch::from_vec(vec![signal("value", Value::Float(1.0))]);

    // Nothing queued, so the stub answers ABI §7.1's `ERR_NOT_FOUND`.
    let error = filter()
        .process_signals(&mut ctx, In::Default.index(), batch)
        .expect_err("the property has no value");

    assert_eq!(error.host_code(), Some(ErrorCode::NotFound));
}

#[test]
fn a_property_whose_type_disagrees_with_the_manifest_is_reported_not_guessed() {
    // The run-time half of SDK §1.2's mapping. The compile-time half — a `Prop<f64>` field
    // declared `ty = "int"` — is in `tests/ui/`, and between them the two readings of a
    // property's type cannot disagree unnoticed.
    let recorder = Recorder::new();
    recorder.queue_prop(&Value::Int(70).to_cbor());

    let mut ctx = ctx();
    let batch = Batch::from_vec(vec![signal("value", Value::Float(1.0))]);

    let error = filter()
        .process_signals(&mut ctx, In::Default.index(), batch)
        .expect_err("an int where the field reads a float");

    // Not silently converted. ABI §11.1 promotes int to float *host-side* and requires the
    // host to encode it as a float, precisely so a guest never sees this — so an int here
    // means the manifest said `int`, and converting would hide the disagreement.
    assert!(
        matches!(&error, BlockError::Decode(message) if message.contains("float")),
        "{error:?}"
    );
}

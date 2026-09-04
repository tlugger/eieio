//! LEAF-SPEC §6.4.4: what a generator has to prove.
//!
//! > A generator is correct when, for every service file it accepts:
//! >
//! > 1. every `BakedInstance`'s `inputs`, `outputs` and `props` equal the fields
//! >    `Descriptor::from_manifest` and `eio_host_core::resolve` produce from that instance's
//! >    manifest and its service-supplied properties; and
//! > 2. `Routes::resolve` over the emitted instances and connections succeeds, and yields the
//! >    table a daemon resolves from the same file.
//!
//! Both are asserted here, over every file in `examples/services/`, with no target, no board
//! and no `wamrc` (§6.3). Together they are what makes §6.4.1's "serialise, do not compute"
//! rule *checkable* rather than merely stated: the generator is not asked whether it followed
//! the rule, it is asked to produce the same answer the shared crates produce, recomputed
//! here from the same inputs by the same functions the daemon calls.
//!
//! **This file recomputes deliberately.** Everywhere the generator called a ★ crate, this
//! test calls it again independently and compares. A test that reused the generator's own
//! intermediate values would prove only that it is self-consistent.
//!
//! Three things are asserted beyond §6.4.4's two, because the bead that built this generator
//! asks for them and because each is a refusal that has to be a *message about a service
//! file* rather than a compiler error (§10):
//!
//! - an invalid service file is refused, naming what is wrong with it;
//! - a module that fails ABI §4.3's load-time cross-check is refused (LEAF §3.1: **a leaf
//!   MUST run it**, at firmware build time);
//! - a module declaring more linear memory than §4.2's per-instance page budget is refused —
//!   and, today, every golden block does (eieio-x7g.2.21).
//!
//! And one beyond that: the baked graph actually *runs*, on both engines, reaching the
//! routed result `eio_leaf::run_demo`'s hand-written table reaches. That is the bead's own
//! regression target, and it is what makes the whole exercise a runtime and not a printer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use eio_host_core::{
    Connection, Descriptor, Endpoint, Overflow, PORT_ERR, Port, PropertySource, Routes, Target,
};
use eio_leaf_gen::{Baked, Error, Inputs, V1_MEMORY_PAGES};

// ── fixtures ─────────────────────────────────────────────────────────────────

/// The golden blocks, by the block reference an example service names them with.
///
/// `examples/services/` names blocks a registry would supply, and this repository builds
/// exactly five of them (ABI §13.2). The two an example service can be *run* from are here;
/// every other reference in that directory has no artifact, which is itself a refusal this
/// suite asserts on.
fn artifacts() -> BTreeMap<String, PathBuf> {
    let out = eio_leaf::fixtures::build();
    BTreeMap::from([
        ("counter:1.0.0".to_string(), out.join("counter.wasm")),
        ("transform:1.0.0".to_string(), out.join("transform.wasm")),
    ])
}

/// Every file in `examples/services/`, sorted, so a new one joins this suite by existing.
fn example_services() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/services");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("reading {}: {error}", dir.display()))
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "examples/services/ has service files in it"
    );
    files
}

/// The golden-block page budget: what every one of ABI §13.2's five declares.
///
/// **Not a value any leaf may build with**, and it is spelled out here rather than passed as
/// a bare `17` so that it is impossible to read this suite as endorsing it. LEAF §4.2's v1
/// budget is [`V1_MEMORY_PAGES`], one page, and the golden blocks now meet it exactly:
/// SDK §5.2's link default (`-C link-arg=-zstack-size=16384`, eieio-x7g.2.21) brought all five
/// from the 17 pages LEAF §4.2 records down to one, with no change to any block.
/// [`the_v1_page_budget_admits_every_golden_block`] is the test that keeps that true.
const GOLDEN_BLOCK_PAGES: u64 = 1;

/// Bakes one service file with the golden-block artifacts.
fn bake(path: &Path, memory_pages: u64) -> Result<Baked, Error> {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
    eio_leaf_gen::bake(&Inputs {
        service_path: path,
        service_text: &text,
        node_id: "n-parity",
        node_name: Some("the parity suite's node"),
        artifacts: &artifacts(),
        transport: None,
        memory_pages,
    })
}

// ── §6.4.4 rule 1: the baked fields are host-core's own output ────────────────

/// Every accepted service file's instances carry exactly what `Descriptor::from_manifest` and
/// `eio_host_core::resolve` produce (LEAF §6.4.4 rule 1).
///
/// Recomputed here from the same manifest and the same service-supplied properties, by the
/// same two functions — which is the only way to check §6.4.1's rule, because the rule is not
/// "the generator looks like it calls them" but "the answer is theirs".
#[test]
fn baked_instances_are_what_descriptor_and_resolve_produce() {
    let mut accepted = 0;
    for path in example_services() {
        let Ok(baked) = bake(&path, GOLDEN_BLOCK_PAGES) else {
            continue;
        };
        accepted += 1;

        let text = std::fs::read_to_string(&path).expect("it read a moment ago");
        let parsed = eio_service::parse(&text).expect("it baked, so it parses");
        let limits = eio_leaf::leaf_limits();

        // §6.4.2: instance order *is* the `Endpoint::instance` numbering, and it is ascending
        // instance-id order — which is what `eio-service`'s `BTreeMap` yields.
        let ids: Vec<&str> = baked.graph.instances.iter().map(|i| i.id).collect();
        let expected_ids: Vec<&str> = parsed.service.blocks.keys().map(String::as_str).collect();
        assert_eq!(
            ids,
            expected_ids,
            "{}: instance order is ascending instance-id order, because it is the router's \
             instance numbering (LEAF §6.4.2)",
            path.display()
        );

        for (baked_instance, (id, instance)) in
            baked.graph.instances.iter().zip(&parsed.service.blocks)
        {
            let wasm = std::fs::read(&artifacts()[&instance.block]).expect("the fixture reads");
            let manifest = eio_manifest::validate(&wasm, None).expect("a golden block validates");

            let descriptor = Descriptor::from_manifest(&manifest, Some(id.clone()), limits);
            let props: Vec<PropertySource<'_>> = eio_host_core::resolve(&manifest, &instance.props)
                .expect("it baked, so it resolves");

            assert_eq!(
                baked_instance.id, descriptor.instance_id,
                "{id}: instance id"
            );
            assert_eq!(
                baked_instance.block, instance.block,
                "{id}: block reference"
            );
            assert_eq!(
                baked_instance.inputs,
                descriptor
                    .inputs
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                "{id}: input port names, in the index order ABI §5.2 fixes"
            );
            assert_eq!(
                baked_instance.outputs,
                descriptor
                    .outputs
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                "{id}: output port names, in the index order ABI §5.2 fixes"
            );
            assert_eq!(
                baked_instance.props,
                props.as_slice(),
                "{id}: the property sources ABI §11.1's required/default rule produced, in \
                 `prop_id` order"
            );
            assert_eq!(
                baked_instance.capabilities,
                manifest.capabilities.as_slice(),
                "{id}: the capabilities the manifest declares"
            );
            assert_eq!(
                baked_instance.module,
                wasm.as_slice(),
                "{id}: the artifact linked into the image (LEAF §6.3)"
            );
        }

        // §6.4.3: the node's limits are baked, and they are §4.2's rather than the service
        // file's — `[budgets]` is not a build input at all.
        assert_eq!(baked.graph.node.limits, limits, "{}", path.display());
        assert_eq!(
            baked.graph.node.service,
            parsed.service.name,
            "{}",
            path.display()
        );
        assert_eq!(baked.graph.node.id, "n-parity", "{}", path.display());
    }
    assert!(
        accepted >= 3,
        "at least `minimal`, `self-loop` and `counter-transform` bake; got {accepted}"
    );
}

/// One artifact, one `static`, however many instances share it (LEAF §6.4.2).
///
/// Two instances of one block, because the single-instance case cannot fail this: §6.4.2's
/// example is "three instances of `filter` are three `BakedInstance`s pointing at one module,
/// not three copies of it in flash", and a leaf that spent 128 KiB of a 313 KiB part on the
/// same block twice would have made the mistake this rule exists to prevent.
#[test]
fn instances_of_one_block_share_one_artifact() {
    let baked = eio_leaf_gen::bake(&Inputs {
        service_path: Path::new("two-of-one.toml"),
        service_text: "name = \"two-of-one\"\n\nconnections = [ \"a.out -> b.in\" ]\n\n\
                       [blocks.a]\nblock = \"transform:1.0.0\"\n\n\
                       [blocks.b]\nblock = \"transform:1.0.0\"\n",
        node_id: "n-parity",
        node_name: None,
        artifacts: &artifacts(),
        transport: None,
        memory_pages: GOLDEN_BLOCK_PAGES,
    })
    .expect("two instances of one block bake");

    assert_eq!(baked.graph.instances.len(), 2);
    assert_eq!(
        baked.artifacts.len(),
        1,
        "one block reference, so one module `static` however many instances name it"
    );
    assert_eq!(
        baked.instance_artifact,
        [0, 0],
        "both instances point at the one artifact"
    );
    assert!(
        std::ptr::eq(
            baked.graph.instances[0].module,
            baked.graph.instances[1].module
        ),
        "and at the same bytes, not two copies of them"
    );
    assert!(
        baked.artifacts[0].path.is_absolute(),
        "a generated file is `include!`d from the build directory, so its paths MUST be \
         absolute (LEAF §6.4.2)"
    );

    let source = eio_leaf_gen::emit(&baked);
    assert_eq!(
        source.matches("include_module!").count(),
        1,
        "and the emitted file includes it once:\n{source}"
    );
}

// ── §6.4.4 rule 2: the routes are the daemon's ───────────────────────────────

/// The table `Routes::resolve` yields from the baked graph is the table a daemon resolves
/// from the same service file (LEAF §6.4.4 rule 2).
///
/// The daemon's half is rebuilt here the way `crates/daemon/src/boot.rs` builds it — one
/// `eio_host_core::Connection` per parsed connection, every one carrying the service's single
/// overflow policy (SERVICE §5, DAEMON §6.2) — and the two tables are compared endpoint by
/// endpoint, over every output port of every instance plus ABI §6.4's reserved error port.
#[test]
fn the_baked_table_is_the_table_a_daemon_resolves() {
    for path in example_services() {
        let Ok(baked) = bake(&path, GOLDEN_BLOCK_PAGES) else {
            continue;
        };
        let baked_routes = baked.graph.routes().unwrap_or_else(|error| {
            panic!("{}: the baked table resolves: {error}", path.display())
        });

        let text = std::fs::read_to_string(&path).expect("it read a moment ago");
        let parsed = eio_service::parse(&text).expect("it baked, so it parses");
        let limits = eio_leaf::leaf_limits();

        let descriptors: Vec<Descriptor> = parsed
            .service
            .blocks
            .iter()
            .map(|(id, instance)| {
                let wasm = std::fs::read(&artifacts()[&instance.block]).expect("the fixture reads");
                let manifest = eio_manifest::validate(&wasm, None).expect("it validates");
                Descriptor::from_manifest(&manifest, Some(id.clone()), limits)
            })
            .collect();
        let overflow = match parsed.overflow {
            eio_service::Overflow::Backpressure => Overflow::Backpressure,
            eio_service::Overflow::DropOldest => Overflow::DropOldest,
        };
        let connections: Vec<Connection> = parsed
            .connections
            .iter()
            .map(|connection| {
                Connection::new(
                    Port::new(&*connection.from.instance, &*connection.from.port),
                    Port::new(&*connection.to.instance, &*connection.to.port),
                )
                .with_overflow(overflow)
            })
            .collect();
        let daemon_routes =
            Routes::resolve(&descriptors, &connections).expect("the daemon's table resolves");

        assert_eq!(
            table(&baked_routes, &descriptors),
            table(&daemon_routes, &descriptors),
            "{}: the leaf's baked table and the daemon's differ, which ABI §13 calls a \
             conformance bug by definition",
            path.display()
        );
    }
}

/// Every source endpoint's targets, as a comparable value.
///
/// [`Routes`] has no `PartialEq` — it is a lookup structure, not a document — so the
/// comparison is over what it *answers*, which is the thing two hosts have to agree on.
/// Every output port of every instance is asked, plus [`PORT_ERR`], because a table that
/// agreed on the wired ports and disagreed about an unwired one would still be a divergence.
fn table(routes: &Routes, descriptors: &[Descriptor]) -> Vec<(Endpoint, Vec<Target>)> {
    let mut rows = Vec::new();
    for (index, descriptor) in descriptors.iter().enumerate() {
        let ports = (0..descriptor.outputs.len() as u32).chain(std::iter::once(PORT_ERR));
        for port in ports {
            let from = Endpoint::new(index as u32, port);
            rows.push((from, routes.targets(from).to_vec()));
        }
    }
    rows
}

// ── the bead's own regression target: the baked graph runs ───────────────────

/// The baked graph drives the same two instances to the same routed result the hand-written
/// table in `eio_leaf::run_demo` does — on both engines (LEAF §3.2, §9).
///
/// This is what makes the generator a runtime concern rather than a printing exercise:
/// `spawn_graph_host` takes the `&'static BakedGraph` `bake` produced, starts every instance
/// through `spawn_resolved` from the *baked* descriptor and the *baked* property sources, and
/// resolves the *baked* connection names. `transform_val == 44` only if `counter`'s
/// `eio:state` round-tripped, `transform`'s manifest default `(+ $n 41)` was resolved by ABI
/// §11.1's rule on the build host and compiled on the device, and the router hop landed on
/// the right instance and port.
fn the_graph_runs<E: eio_host_core::Engine>(
    engine: &str,
    instantiate: impl Fn(&[u8]) -> Result<E, String>,
) {
    use std::rc::Rc;

    use eio_host_core::{Delivering, Outcome};
    use eio_signal::{Batch, Signal, Value};

    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/services/counter-transform.toml");
    let baked = bake(&path, GOLDEN_BLOCK_PAGES).expect("counter-transform bakes");

    // `counter`'s count is durable by design (LEAF §5), so a run that inherited a previous
    // one's state would answer a different number for a reason that has nothing to do with
    // whether the graph is wired right — the same reasoning `run_demo` states.
    let state_dir = std::env::temp_dir().join(format!(
        "eio-leaf-gen-parity-{engine}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&state_dir);

    let mut running = eio_leaf::spawn_graph_host(baked.graph, Some(&state_dir), instantiate)
        .unwrap_or_else(|error| panic!("the baked graph starts on {engine}: {error}"));

    let mut inbound = Batch::with_capacity(3);
    for _ in 0..3 {
        inbound.push(Signal::new());
    }

    let counter = running.instances.remove(0);
    let transform = running.instances.remove(0);

    let Delivering::Delivered(counter_running, _) =
        counter.running.process_signals(0, Rc::new(inbound))
    else {
        panic!("counter died or was refused on {engine}");
    };

    let emissions = counter.core.take_emissions();
    let [emission] = emissions.as_slice() else {
        panic!("counter emitted {} batch(es) on {engine}", emissions.len());
    };

    let mut transform_running = transform.running;
    let mut routed_to = None;
    for (target, batch) in running
        .routes
        .deliveries(Endpoint::new(0, emission.port), emission.batch.clone())
    {
        routed_to = Some(target.to);
        let Delivering::Delivered(next, _) =
            transform_running.process_signals(target.to.port, Rc::new(batch))
        else {
            panic!("transform died or was refused on {engine}");
        };
        transform_running = next;
    }

    assert_eq!(
        routed_to,
        Some(Endpoint::new(1, 0)),
        "counter.out reaches transform.in — instance 1, its only input port — on {engine}"
    );

    let emissions = transform.core.take_emissions();
    let [emission] = emissions.as_slice() else {
        panic!(
            "transform emitted {} batch(es) on {engine}",
            emissions.len()
        );
    };
    let signal = emission.batch.get(0).expect("transform emitted a signal");
    assert_eq!(
        signal.get("val"),
        Some(&Value::Int(44)),
        "a first run's count is 3, so transform's `(+ $n 41)` answers 44 on {engine} — the \
         same number `eio_leaf::run_demo`'s hand-written table produces"
    );

    let Outcome::Live(counter_stopped, _) = counter_running.stop() else {
        panic!("counter died on stop on {engine}");
    };
    let Outcome::Live(transform_stopped, _) = transform_running.stop() else {
        panic!("transform died on stop on {engine}");
    };
    assert_eq!(
        (counter_stopped.errors(), transform_stopped.errors()),
        (0, 0),
        "no callback returned non-zero on {engine} (ABI §8)"
    );

    let _ = std::fs::remove_dir_all(&state_dir);
}

#[test]
fn the_baked_graph_runs_end_to_end_on_wasm3() {
    the_graph_runs("wasm3", eio_leaf::wasm3::instantiate);
}

#[test]
fn the_baked_graph_runs_end_to_end_on_wamr() {
    the_graph_runs("wamr", eio_leaf::wamr::instantiate);
}

// ── refusals: a message about a service file, never a compiler error (§10) ───

/// A service file naming a block the build cannot supply is refused, naming the block.
///
/// `kitchen.toml` is the case: it names four blocks a registry would carry and this
/// repository does not build. A leaf links every block's code into the image (§6.3), so
/// "the build was not given this artifact" is a build failure and not something a device
/// could recover from.
#[test]
fn a_block_with_no_artifact_is_refused_by_name() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/services/kitchen.toml");
    let error = bake(&path, GOLDEN_BLOCK_PAGES).expect_err("kitchen names blocks nobody built");
    assert!(matches!(error, Error::NoArtifact { .. }), "{error}");
    let message = error.to_string();
    assert!(message.contains("b7k2"), "{message}");
    assert!(message.contains("temp-sensor"), "{message}");
}

/// An invalid service file is refused with what is wrong with *it* (LEAF §10, SERVICE §7).
#[test]
fn an_invalid_service_file_is_refused_before_the_build() {
    let text = "name = \"broken\"\n\n[blocks.a]\nblock = \"counter:1.0.0\"\n\n\
                [blocks.a.props]\nn = \"(+ 1\"\n";
    let error = eio_leaf_gen::bake(&Inputs {
        service_path: Path::new("broken.toml"),
        service_text: text,
        node_id: "n-parity",
        node_name: None,
        artifacts: &artifacts(),
        transport: None,
        memory_pages: GOLDEN_BLOCK_PAGES,
    })
    .expect_err("an unclosed expression is a stage-1 rejection");
    assert!(matches!(error, Error::Parse(_)), "{error}");
    let message = error.to_string();
    assert!(
        message.contains("SERVICE §7 stage 1"),
        "the refusal names the validation stage, not rustc: {message}"
    );
}

/// A module that fails ABI §4.3's load-time cross-check is refused at generation time.
///
/// LEAF §3.1: **a leaf MUST run it**, "at firmware build time where a refusal costs a build
/// rather than a field failure". The module below is a well-formed WASM that imports from a
/// namespace the ABI does not define, which is the §4.3 refusal in its purest form — the
/// import section *is* the capability system.
#[test]
fn a_module_failing_the_abi_4_3_cross_check_is_refused_at_generation_time() {
    let wasm = wat::parse_str(
        r#"(module
             (import "not:eio" "something" (func))
             (memory (export "memory") 1))"#,
    )
    .expect("the fixture assembles");
    let dir = std::env::temp_dir().join(format!("eio-leaf-gen-badblock-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp directory");
    let path = dir.join("bad.wasm");
    std::fs::write(&path, &wasm).expect("writing the fixture");

    let error = eio_leaf_gen::bake(&Inputs {
        service_path: Path::new("uses-a-bad-block.toml"),
        service_text: "name = \"bad\"\n\n[blocks.a]\nblock = \"bad:1.0.0\"\n",
        node_id: "n-parity",
        node_name: None,
        artifacts: &BTreeMap::from([("bad:1.0.0".to_string(), path)]),
        transport: None,
        memory_pages: GOLDEN_BLOCK_PAGES,
    })
    .expect_err("an undeclarable import is an ABI §4.3 refusal");
    assert!(matches!(error, Error::Manifest { .. }), "{error}");
    let message = error.to_string();
    assert!(message.contains("ABI §4.3"), "{message}");
    assert!(
        message.contains("\"a\""),
        "the instance is named: {message}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// LEAF §4.2's per-instance page budget refuses every golden block as they build today.
///
/// §4.2 predicted 17 pages from a measurement — 1088 KiB, three and a half times the whole v1
/// part — and made refusing that a MUST at firmware build time. SDK §5.2's link default then
/// closed the gap (eieio-x7g.2.21), so this is where the *fix* is checked rather than believed:
/// a graph of golden blocks bakes against the real v1 budget, with no budget relaxation.
///
/// It is the regression test for both halves at once. Lose the link flag and `declared` climbs
/// back to 17 and this fails; break the generator's check and
/// [`a_module_over_the_page_budget_is_refused`] fails instead.
#[test]
fn the_v1_page_budget_admits_every_golden_block() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/services/counter-transform.toml");
    bake(&path, V1_MEMORY_PAGES)
        .expect("SDK §5.2's link default brings every golden block to LEAF §4.2's one page");
}

/// The refusal LEAF §4.2 makes a MUST, exercised on the one axis still available now that the
/// golden blocks fit: a budget below what they declare.
///
/// Kept deliberately after the fix landed. §4.2's refusal is what stops an oversized module
/// reaching a device, and a check that never fires once its motivating case is fixed is a check
/// nobody notices losing.
#[test]
fn a_module_over_the_page_budget_is_refused() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/services/counter-transform.toml");
    let error = bake(&path, 0).expect_err("one page is over a zero-page budget");
    let Error::MemoryBudget {
        declared, budget, ..
    } = &error
    else {
        panic!("expected a memory-budget refusal, got {error}");
    };
    assert_eq!(
        *declared, GOLDEN_BLOCK_PAGES,
        "what the golden blocks declare after SDK §5.2's default"
    );
    assert_eq!(*budget, 0, "the budget asked for");
    let message = error.to_string();
    assert!(message.contains("LEAF §4.2"), "{message}");
    assert!(
        message.contains("stack-size"),
        "the refusal names the fix: {message}"
    );
}

// ── what the generated text says ─────────────────────────────────────────────

/// The emitted source is a rendering of the baked graph and carries every field of it.
///
/// The emitter derives nothing (see its module docs), so this asserts only that each field
/// reaches the page — the *values* are §6.4.4's two tests above, on the very same value this
/// text is printed from.
#[test]
fn the_emitted_source_renders_the_graph_it_was_given() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/services/counter-transform.toml");
    let baked = bake(&path, GOLDEN_BLOCK_PAGES).expect("counter-transform bakes");
    let source = eio_leaf_gen::emit(&baked);

    for expected in [
        "DO NOT EDIT",
        "eio_leaf::include_module!(",
        "pub static GRAPH: eio_leaf::graph::BakedGraph",
        "id: \"n-parity\",",
        "service: \"counter-transform\",",
        "limits: eio_leaf::leaf_limits(),",
        "id: \"counter\",",
        "block: \"counter:1.0.0\",",
        "module: MODULE_0,",
        "inputs: &[\"in\"],",
        "outputs: &[\"out\"],",
        "capabilities: &[eio_leaf::graph::Capability::State],",
        "PropertySource::new(\"val\", eio_leaf::graph::PropertyType::Int, \"(+ $n 41)\")",
        "BakedConnection { from: (\"counter\", \"out\"), to: (\"transform\", \"in\") }",
        "overflow: eio_leaf::graph::Overflow::Backpressure,",
        "transport: None,",
    ] {
        assert!(
            source.contains(expected),
            "the generated file carries {expected:?}:\n{source}"
        );
    }

    // §6.4: no `fn` and no control flow. Stated as a rule in the spec and as a grep here,
    // because the failure mode it guards against is a convenience added later — a helper
    // "just to keep the file short" is how a second lifecycle driver gets born.
    for forbidden in ["fn ", "if ", "match ", "for ", "while ", "loop "] {
        assert!(
            !source.contains(forbidden),
            "the generated file contains {forbidden:?}, and §6.4 says it contains no `fn` and \
             no control flow:\n{source}"
        );
    }
}

/// A transport configuration reaches the graph and the page (DAEMON §7.1, LEAF §6.4.2).
#[test]
fn a_bus_configuration_is_baked() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/services/minimal.toml");
    let text = std::fs::read_to_string(&path).expect("reading minimal.toml");
    let baked = eio_leaf_gen::bake(&Inputs {
        service_path: &path,
        service_text: &text,
        node_id: "n-parity",
        node_name: None,
        artifacts: &artifacts(),
        transport: Some(eio_leaf_gen::TransportInput {
            bus: "kitchen".to_string(),
            candidates: vec!["n-abc@10.0.0.4:1883".to_string()],
            pinned: Some("n-abc".to_string()),
            key: Some(b"sh".to_vec()),
        }),
        memory_pages: V1_MEMORY_PAGES,
    })
    .expect("minimal.toml bakes");

    let transport = baked.graph.transport.as_ref().expect("a bus was given");
    assert_eq!(transport.bus, "kitchen");
    assert_eq!(transport.candidates, ["n-abc@10.0.0.4:1883"]);
    assert_eq!(transport.pinned, Some("n-abc"));
    assert_eq!(transport.key, Some(b"sh".as_slice()));

    let source = eio_leaf_gen::emit(&baked);
    assert!(source.contains("bus: \"kitchen\","), "{source}");
    assert!(source.contains("key: Some(&[0x73, 0x68]),"), "{source}");
}

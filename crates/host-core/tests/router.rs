//! The connection table and fan-out (DAEMON-SPEC §6, ABI-SPEC §6.2, §6.4).
//!
//! Everything here is about the *table*: what a service's names resolve to, what order
//! receivers are in, and what a fan-out hands each of them. Delivery — mailboxes, overflow
//! policies, backpressure — is a host's and is tested where the queues are.

use eio_host_core::{
    Connection, Descriptor, End, Endpoint, Limits, Overflow, PORT_ERR, PORT_ERR_NAME, Port,
    RouteError, Routes,
};
use eio_signal::{Batch, Signal, Value};

/// A descriptor with these ports. Properties and limits are irrelevant to routing.
fn descriptor(id: &str, inputs: &[&str], outputs: &[&str]) -> Descriptor {
    Descriptor {
        instance_id: String::from(id),
        block: String::from("test"),
        inputs: inputs.iter().map(|name| String::from(*name)).collect(),
        outputs: outputs.iter().map(|name| String::from(*name)).collect(),
        props: Vec::new(),
        limits: Limits::new(64 * 1024, 1024),
    }
}

/// A source with one output, and two sinks with one input each.
fn service() -> Vec<Descriptor> {
    vec![
        descriptor("source", &["in"], &["out", "spare"]),
        descriptor("sink-a", &["in"], &[]),
        descriptor("sink-b", &["left", "right"], &[]),
    ]
}

/// `from.port → to.port`, with the default overflow policy.
fn wire(from: &str, from_port: &str, to: &str, to_port: &str) -> Connection {
    Connection::new(Port::new(from, from_port), Port::new(to, to_port))
}

/// A one-signal batch carrying `n`.
fn batch(n: i64) -> Batch {
    let mut signal = Signal::new();
    signal.set("n", Value::Int(n));
    let mut batch = Batch::new();
    batch.push(signal);
    batch
}

#[test]
fn names_resolve_to_the_indices_the_descriptor_fixes() {
    // ABI §5.2: the descriptor's name lists *are* the numbering, so resolution is a position
    // lookup and nothing else. `sink-b`'s second input is port 1 because it is listed second.
    let routes = Routes::resolve(
        &service(),
        &[
            wire("source", "out", "sink-a", "in"),
            wire("source", "spare", "sink-b", "right"),
        ],
    )
    .expect("both connections resolve");

    let targets = routes.targets(Endpoint::new(0, 0));
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].to, Endpoint::new(1, 0));

    let spare = routes.targets(Endpoint::new(0, 1));
    assert_eq!(spare[0].to, Endpoint::new(2, 1));
}

#[test]
fn a_source_nothing_is_connected_to_answers_with_an_empty_slice() {
    // An unwired output is an ordinary service shape — a block that emits on a port nobody
    // wanted — so it is an empty answer rather than a missing one.
    let routes =
        Routes::resolve(&service(), &[wire("source", "out", "sink-a", "in")]).expect("it resolves");
    assert!(routes.targets(Endpoint::new(0, 1)).is_empty());
    assert!(routes.targets(Endpoint::new(9, 9)).is_empty());
    assert!(!routes.is_empty(), "the table itself is not empty");
}

#[test]
fn the_error_port_is_routable_as_a_source() {
    // ABI §6.4: PORT_ERR is a reserved *output* on every block, absent from the manifest —
    // so it has no index to resolve and is named instead.
    let routes = Routes::resolve(
        &service(),
        &[wire("source", PORT_ERR_NAME, "sink-b", "left")],
    )
    .expect("the error port is routable");
    let targets = routes.targets(Endpoint::new(0, PORT_ERR));
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].to, Endpoint::new(2, 0));
}

#[test]
fn the_error_port_is_not_routable_as_a_destination() {
    let error = Routes::resolve(
        &service(),
        &[wire("source", "out", "sink-a", PORT_ERR_NAME)],
    )
    .expect_err("an output port cannot receive");
    assert!(matches!(error, RouteError::ErrorPortInbound { .. }));
    assert!(error.to_string().contains("6.4"), "{error}");
}

#[test]
fn a_block_declaring_an_output_named_err_is_refused_by_name() {
    // ABI §6.4 reserves the name by making PORT_ERR absent from the manifest's outputs. A
    // block that declares one anyway makes `err` in a service file mean two things, and
    // guessing which is worse than refusing.
    let descriptors = vec![
        descriptor("source", &["in"], &["err"]),
        descriptor("sink-a", &["in"], &[]),
    ];
    let error = Routes::resolve(&descriptors, &[wire("source", "err", "sink-a", "in")])
        .expect_err("the name is reserved");
    assert!(matches!(error, RouteError::ReservedOutputName { .. }));
    assert!(error.to_string().contains("source"), "{error}");
}

#[test]
fn an_unknown_instance_or_port_names_the_offender() {
    let error = Routes::resolve(&service(), &[wire("sorce", "out", "sink-a", "in")])
        .expect_err("no such instance");
    assert!(matches!(error, RouteError::UnknownInstance { .. }));
    assert!(error.to_string().contains("sorce"), "{error}");

    let error = Routes::resolve(&service(), &[wire("source", "outt", "sink-a", "in")])
        .expect_err("no such output");
    assert!(
        matches!(
            &error,
            RouteError::UnknownPort {
                end: End::Output,
                ..
            }
        ),
        "{error}"
    );
    assert!(error.to_string().contains("outt"), "{error}");

    let error = Routes::resolve(&service(), &[wire("source", "out", "sink-b", "middle")])
        .expect_err("no such input");
    assert!(
        matches!(
            &error,
            RouteError::UnknownPort {
                end: End::Input,
                ..
            }
        ),
        "{error}"
    );
    assert!(error.to_string().contains("middle"), "{error}");
}

#[test]
fn the_same_connection_twice_is_refused() {
    // Two identical connections would deliver the same batch twice. Fan-out is expressed by
    // connecting to *different* receivers, so this is a service-file mistake.
    let error = Routes::resolve(
        &service(),
        &[
            wire("source", "out", "sink-a", "in"),
            wire("source", "out", "sink-a", "in"),
        ],
    )
    .expect_err("declared twice");
    assert!(matches!(error, RouteError::Duplicate { .. }));
    assert!(error.to_string().contains("source.out"), "{error}");
}

#[test]
fn two_instances_sharing_an_id_are_refused() {
    // ABI §5.2: an instance id is unique within the service. Resolving one of two would pick
    // an instance by position, which is not a thing a service author wrote down.
    let descriptors = vec![
        descriptor("twin", &["in"], &["out"]),
        descriptor("twin", &["in"], &["out"]),
    ];
    let error =
        Routes::resolve(&descriptors, &[wire("twin", "out", "twin", "in")]).expect_err("ambiguous");
    assert!(matches!(error, RouteError::DuplicateInstance { .. }));
}

#[test]
fn fan_out_keeps_the_order_the_service_declared() {
    // Not an incidental property: a service author looking at two receivers of one port can
    // see which is fed first, and a replay has to reproduce it.
    let routes = Routes::resolve(
        &service(),
        &[
            wire("source", "out", "sink-b", "right"),
            wire("source", "out", "sink-a", "in"),
            wire("source", "out", "sink-b", "left"),
        ],
    )
    .expect("all three resolve");

    let order: Vec<Endpoint> = routes
        .targets(Endpoint::new(0, 0))
        .iter()
        .map(|target| target.to)
        .collect();
    assert_eq!(
        order,
        [
            Endpoint::new(2, 1),
            Endpoint::new(1, 0),
            Endpoint::new(2, 0)
        ]
    );
}

#[test]
fn fan_out_hands_every_receiver_an_independent_copy() {
    // DAEMON §6's "duplicate batch per receiver". The point is not that there are N of them
    // but that they share nothing: a receiver that changes what it was given cannot change
    // what another receiver is holding.
    let routes = Routes::resolve(
        &service(),
        &[
            wire("source", "out", "sink-a", "in"),
            wire("source", "out", "sink-b", "left"),
            wire("source", "out", "sink-b", "right"),
        ],
    )
    .expect("all three resolve");

    let mut copies: Vec<Batch> = routes
        .deliveries(Endpoint::new(0, 0), batch(1))
        .map(|(_, batch)| batch)
        .collect();
    assert_eq!(copies.len(), 3);

    let mut mutated = Signal::new();
    mutated.set("n", Value::Int(99));
    copies[0].push(mutated);

    assert_eq!(
        copies[0].len(),
        2,
        "the first receiver changed its own copy"
    );
    assert_eq!(copies[1].len(), 1, "and nobody else's");
    assert_eq!(copies[2].len(), 1);
    assert_eq!(copies[1].get(0).unwrap().get("n"), Some(&Value::Int(1)));
}

#[test]
fn an_emission_with_nowhere_to_go_yields_no_deliveries() {
    let routes =
        Routes::resolve(&service(), &[wire("source", "out", "sink-a", "in")]).expect("it resolves");
    assert_eq!(routes.deliveries(Endpoint::new(0, 1), batch(1)).count(), 0);
}

#[test]
fn every_connection_has_a_stable_key_for_per_connection_state() {
    // DAEMON §6's drop-oldest slot is per connection, so a host needs to size an array by
    // `connections()` and index it by `Target::connection`.
    let routes = Routes::resolve(
        &service(),
        &[
            wire("source", "out", "sink-a", "in"),
            wire("source", "spare", "sink-b", "left"),
            wire("source", "out", "sink-b", "right"),
        ],
    )
    .expect("all three resolve");
    assert_eq!(routes.connections(), 3);

    let mut keys: Vec<u32> = Vec::new();
    for port in [0, 1] {
        keys.extend(
            routes
                .targets(Endpoint::new(0, port))
                .iter()
                .map(|target| target.connection),
        );
    }
    keys.sort_unstable();
    assert_eq!(keys, [0, 1, 2], "every key is in range and distinct");
}

#[test]
fn the_overflow_policy_travels_with_the_connection() {
    // DAEMON §6: the policy is per connection, not per instance and not per port — two
    // receivers of one output may want different answers to a full queue.
    let routes = Routes::resolve(
        &service(),
        &[
            wire("source", "out", "sink-a", "in"),
            wire("source", "out", "sink-b", "left").with_overflow(Overflow::DropOldest),
        ],
    )
    .expect("both resolve");

    let targets = routes.targets(Endpoint::new(0, 0));
    assert_eq!(targets[0].overflow, Overflow::Backpressure, "the default");
    assert_eq!(targets[1].overflow, Overflow::DropOldest);
}

#[test]
fn an_instance_may_be_connected_to_itself() {
    // A block that feeds itself is legal, and it is the shape ABI §6.2 is about: the copy
    // reaches its own queue, never its own stack.
    let descriptors = vec![descriptor("loop", &["in", "back"], &["out"])];
    let routes = Routes::resolve(&descriptors, &[wire("loop", "out", "loop", "back")])
        .expect("a self-connection resolves");
    let targets = routes.targets(Endpoint::new(0, 0));
    assert_eq!(targets[0].to, Endpoint::new(0, 1));
}

#[test]
fn an_empty_service_routes_nothing() {
    let routes = Routes::resolve(&service(), &[]).expect("nothing to resolve");
    assert!(routes.is_empty());
    assert_eq!(routes.connections(), 0);
    assert!(routes.targets(Endpoint::new(0, 0)).is_empty());
}

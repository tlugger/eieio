//! SERVICE-SPEC §7 stage 2, and the id rules of §2.1.

use eio_manifest::Manifest;
use eio_service::{ResolvedError, id, parse, validate};

/// A manifest with the ports and properties named.
fn manifest(name: &str, inputs: &[&str], outputs: &[&str], props: &[&str]) -> Manifest {
    let ports = |names: &[&str]| {
        names
            .iter()
            .map(|n| format!(r#"{{ "name": "{n}" }}"#))
            .collect::<Vec<_>>()
            .join(",")
    };
    let properties = props
        .iter()
        .map(|n| format!(r#"{{ "name": "{n}", "type": "any" }}"#))
        .collect::<Vec<_>>()
        .join(",");
    eio_manifest::parse(&format!(
        r#"{{
            "name": "{name}",
            "version": "1.0.0",
            "abi": {{ "major": 1, "minor": 0 }},
            "inputs": [{}],
            "outputs": [{}],
            "properties": [{properties}]
        }}"#,
        ports(inputs),
        ports(outputs)
    ))
    .expect("the fixture manifest parses")
}

/// A service with one wired pair, and the manifests to resolve it against.
fn wired(connections: &str, props: &str) -> (eio_service::Parsed, Vec<(&'static str, Manifest)>) {
    let text = format!(
        r#"
            name = "wiring"
            connections = [ {connections} ]

            [blocks.b7k2]
            block = "source:1.0.0"

            [blocks.f3m9]
            block = "sink:1.0.0"

            [blocks.f3m9.props]
            {props}
        "#
    );
    let parsed = parse(&text).expect("stage 1 passes");
    let manifests = vec![
        ("b7k2", manifest("source", &["in"], &["out"], &[])),
        (
            "f3m9",
            manifest("sink", &["in"], &["above", "below"], &["threshold"]),
        ),
    ];
    (parsed, manifests)
}

/// Resolves ids the way the block manager would (DAEMON §4), from a list.
fn resolver<'a>(
    manifests: &'a [(&'static str, Manifest)],
) -> impl Fn(&str) -> Option<Manifest> + 'a {
    move |id: &str| {
        manifests
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, manifest)| manifest.clone())
    }
}

#[test]
fn a_wiring_whose_ports_exist_is_accepted() {
    let (parsed, manifests) = wired(r#""b7k2.out -> f3m9.in""#, "");
    assert!(validate(&parsed, resolver(&manifests)).is_empty());
}

#[test]
fn a_source_port_the_block_does_not_declare_is_rejected() {
    let (parsed, manifests) = wired(r#""b7k2.nope -> f3m9.in""#, "");
    let errors = validate(&parsed, resolver(&manifests));
    let [
        ResolvedError::UnknownPort {
            instance,
            port,
            direction,
            ..
        },
    ] = &errors[..]
    else {
        panic!("{errors:#?}");
    };
    assert_eq!(
        (instance.as_str(), port.as_str(), *direction),
        ("b7k2", "nope", "output")
    );
}

#[test]
fn a_destination_port_the_block_does_not_declare_is_rejected() {
    let (parsed, manifests) = wired(r#""b7k2.out -> f3m9.nope""#, "");
    let errors = validate(&parsed, resolver(&manifests));
    assert!(
        matches!(
            &errors[..],
            [ResolvedError::UnknownPort { direction, .. }] if *direction == "input"
        ),
        "{errors:#?}"
    );
}

#[test]
fn an_output_used_as_an_input_is_rejected_in_the_direction_it_was_used() {
    // `above` is an output on `f3m9`, so naming it as a destination is wrong even though the
    // port exists — a connection is directional and the manifest is asked accordingly.
    let (parsed, manifests) = wired(r#""b7k2.out -> f3m9.above""#, "");
    let errors = validate(&parsed, resolver(&manifests));
    assert!(
        matches!(
            &errors[..],
            [ResolvedError::UnknownPort { port, direction, .. }]
                if port == "above" && *direction == "input"
        ),
        "{errors:#?}"
    );
}

#[test]
fn the_error_port_resolves_against_no_manifest() {
    // ABI §6.4: every block has it and no block declares it, so stage 2 must not go looking
    // for it in `outputs` and must not reject it for being absent.
    let (parsed, manifests) = wired(r#""b7k2.err -> f3m9.in""#, "");
    assert!(validate(&parsed, resolver(&manifests)).is_empty());
}

#[test]
fn a_property_the_block_does_not_declare_is_rejected() {
    let (parsed, manifests) = wired(r#""b7k2.out -> f3m9.in""#, r#"nosuchprop = "1""#);
    let errors = validate(&parsed, resolver(&manifests));
    let [ResolvedError::UnknownProperty { id, property }] = &errors[..] else {
        panic!("{errors:#?}");
    };
    assert_eq!((id.as_str(), property.as_str()), ("f3m9", "nosuchprop"));
}

#[test]
fn an_instance_that_cannot_be_resolved_is_skipped_rather_than_blamed() {
    // A registry that is unreachable is not the file being wrong. Stage 2 checks what it can
    // see and says nothing about what it cannot.
    let (parsed, _) = wired(r#""b7k2.nope -> f3m9.also_nope""#, r#"nosuchprop = "1""#);
    assert!(validate(&parsed, |_| None).is_empty());
}

// ── ids (SERVICE §2.1) ──────────────────────────────────────────────────────

#[test]
fn the_id_validator_is_the_published_pattern() {
    // The hand-written check and the regex the schema publishes have to be the same rule, or
    // the Designer and the daemon disagree about a name nobody thought to test — the
    // arrangement `manifest` uses for ABI §11.1.
    let pattern = regex::Regex::new(id::ID_PATTERN).expect("the published pattern compiles");
    for candidate in [
        "a",
        "b7k2",
        "thermo",
        "temp-sensor",
        "temp_sensor",
        "a1",
        "0",
        "x9-y_z",
        // and the rejections
        "",
        "A",
        "Thermo",
        "-lead",
        "trail-",
        "_lead",
        "trail_",
        "a.b",
        "a b",
        "a!",
        "é",
    ] {
        assert_eq!(
            id::is_id(candidate),
            pattern.is_match(candidate),
            "{candidate:?}"
        );
    }
}

#[test]
fn an_id_longer_than_the_bound_is_refused() {
    // The regex says nothing about length, so this is the one rule the two sources state
    // separately — asserted here so that stays deliberate.
    let long = "a".repeat(id::MAX_ID_BYTES);
    assert!(id::is_id(&long));
    assert!(!id::is_id(&format!("{long}a")));
}

#[test]
fn a_generated_id_is_a_valid_id_and_avoids_what_is_taken() {
    // Randomness is the caller's (SERVICE §2): a fixed array here is exactly what a test
    // wants, and it is the same code path a `getrandom` caller takes.
    let random: Vec<u8> = (0u8..=63).collect();
    let first = id::generate(&random, |_| false).expect("an id");
    assert_eq!(first.len(), id::GENERATED_LEN);
    assert!(id::is_id(&first), "{first:?}");

    // Asked again with the first one taken, it answers a different one.
    let second = id::generate(&random, |candidate| candidate == first).expect("another id");
    assert_ne!(second, first);
    assert!(id::is_id(&second), "{second:?}");

    // And it gives up rather than looping forever when everything is taken.
    assert_eq!(id::generate(&random, |_| true), None);
}

#[test]
fn a_generated_id_avoids_the_letters_that_read_back_wrong() {
    // No `i`, `l`, `o` or `u`: the first three are misread off a screen and the fourth keeps
    // a generated id from spelling something unfortunate.
    let random: Vec<u8> = (0u8..=255).collect();
    for chunk in random.chunks_exact(4) {
        let id = id::generate(chunk, |_| false).expect("an id");
        assert!(
            !id.contains(['i', 'l', 'o', 'u']),
            "{id:?} contains a confusable"
        );
    }
}

//! The fixture corpus: every valid example parses, and every error class has one file that
//! produces exactly it (SERVICE-SPEC §7).
//!
//! Fixtures rather than inline strings because two audiences read them. `examples/services/`
//! is what a person opens to learn the format — it is documentation that has to keep
//! working — and `tests/invalid/` is one file per error class, which is how "each a distinct
//! structured error" stays a claim about the code rather than about its messages.

use std::path::{Path, PathBuf};

use eio_service::{ConnectionError, Error, Overflow, parse};

/// `examples/services/`, from this crate.
fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/services")
}

/// One invalid fixture, by filename.
fn invalid(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/invalid")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Parses an invalid fixture and returns the errors it produced.
fn errors(name: &str) -> Vec<Error> {
    match parse(&invalid(name)) {
        Ok(_) => panic!("{name} parsed, and it is a fixture for a rejection"),
        Err(errors) => errors,
    }
}

#[test]
fn every_example_service_parses() {
    let mut seen = 0;
    for entry in std::fs::read_dir(examples()).expect("examples/services exists") {
        let path = entry.expect("a readable entry").path();
        if path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("readable");
        let parsed = match parse(&text) {
            Ok(parsed) => parsed,
            Err(errors) => panic!("{}: {errors:#?}", path.display()),
        };
        // SERVICE §1: a service file's stem is its `name`. This crate cannot enforce it —
        // `parse` takes a string and never sees a filename, and the host that reads the
        // directory is the one that refuses a mismatch (DAEMON §3). What it can do is keep
        // the examples honest, since they are what a person copies onto a real node.
        assert_eq!(
            path.file_stem().and_then(|stem| stem.to_str()),
            Some(parsed.service.name.as_str()),
            "{} would be refused at boot: its stem and its name disagree",
            path.display()
        );
        seen += 1;
    }
    assert!(seen >= 3, "only {seen} example service(s) were checked");
}

#[test]
fn the_kitchen_example_says_what_it_looks_like() {
    // The example is documentation, so what it demonstrates is asserted rather than assumed:
    // fan-out, an `err` source, a name that is only a label, and properties as expressions.
    let text = std::fs::read_to_string(examples().join("kitchen.toml")).expect("readable");
    let parsed = parse(&text).expect("it is valid");

    assert_eq!(parsed.service.name, "kitchen");
    assert!(parsed.service.autostart);
    assert_eq!(parsed.overflow, Overflow::DropOldest);
    assert_eq!(parsed.service.blocks.len(), 4);
    assert_eq!(parsed.connections.len(), 4);

    // Fan-out: `f3m9.above` reaches two destinations.
    let above: Vec<&str> = parsed
        .connections
        .iter()
        .filter(|c| c.from.instance == "f3m9" && c.from.port == "above")
        .map(|c| c.to.instance.as_str())
        .collect();
    assert_eq!(above, ["k1p8", "q4tv"]);

    // The error port, as a source.
    assert!(
        parsed
            .connections
            .iter()
            .any(|c| c.from.port == "err" && c.from.instance == "f3m9")
    );

    // A label, and an instance that has none — both legal (SERVICE §2).
    assert_eq!(
        parsed.service.blocks["b7k2"].name.as_deref(),
        Some("Thermometer")
    );
    assert_eq!(parsed.service.blocks["q4tv"].name.as_deref(), Some("Log"));

    // `[ui]` is held whole and never looked inside (§6).
    assert!(parsed.service.ui.is_some());
}

#[test]
fn a_hand_written_id_is_as_valid_as_a_generated_one() {
    // SERVICE §2: an id is opaque, and a host must not require that it look generated.
    let text = std::fs::read_to_string(examples().join("self-loop.toml")).expect("readable");
    let parsed = parse(&text).expect("it is valid");

    assert!(parsed.service.blocks.contains_key("accumulator"));
    // And a self-edge is legal: `emit` enqueues, so it cannot re-enter the guest (ABI §6.2).
    assert_eq!(parsed.connections[0].from.instance, "accumulator");
    assert_eq!(parsed.connections[0].to.instance, "accumulator");
}

// ── one fixture per error class (SERVICE §7) ────────────────────────────────

#[test]
fn malformed_toml_is_reported_as_the_parser_saw_it() {
    assert!(matches!(
        errors("malformed-toml.toml")[..],
        [Error::Toml(_)]
    ));
}

#[test]
fn an_unknown_field_is_rejected_rather_than_ignored() {
    // The `autostrat = true` that silently meant nothing.
    let errors = errors("unknown-field.toml");
    assert!(matches!(errors[..], [Error::Toml(_)]));
    assert!(format!("{}", errors[0]).contains("autostrat"));
}

#[test]
fn a_service_name_outside_the_pattern_is_rejected() {
    assert!(matches!(
        errors("bad-service-name.toml")[..],
        [Error::ServiceName { .. }]
    ));
}

#[test]
fn an_instance_id_outside_the_pattern_is_rejected() {
    assert!(matches!(
        errors("bad-instance-id.toml")[..],
        [Error::InstanceId { .. }]
    ));
}

#[test]
fn an_instance_with_no_block_reference_is_rejected() {
    assert!(matches!(
        errors("empty-block-ref.toml")[..],
        [Error::EmptyBlockRef { .. }]
    ));
}

#[test]
fn a_connection_that_does_not_parse_names_what_was_wrong_and_where() {
    let errors = errors("bad-connection-syntax.toml");
    let [Error::ConnectionSyntax { index, error, .. }] = &errors[..] else {
        panic!("{errors:#?}");
    };
    assert_eq!(*index, 0);
    // `=>` shares no substring with `->`, so what the parser finds is no arrow at all —
    // which is the right answer and a better one than guessing at what was meant.
    assert!(matches!(error, ConnectionError::NoArrow), "{error:?}");
}

#[test]
fn a_connection_naming_an_instance_the_file_does_not_define_is_rejected() {
    let errors = errors("dangling-connection.toml");
    let [Error::DanglingConnection { instance, side, .. }] = &errors[..] else {
        panic!("{errors:#?}");
    };
    assert_eq!(instance, "nope");
    assert_eq!(*side, "destination");
}

#[test]
fn the_same_edge_twice_is_rejected_and_names_the_first() {
    let errors = errors("duplicate-connection.toml");
    let [Error::DuplicateConnection { index, first }] = &errors[..] else {
        panic!("{errors:#?}");
    };
    assert_eq!((*index, *first), (1, 0));
}

#[test]
fn the_error_port_cannot_be_a_destination() {
    // ABI §6.4: it is an output every block has and no block declares.
    assert!(matches!(
        errors("err-as-destination.toml")[..],
        [Error::ErrorPortDestination { .. }]
    ));
}

#[test]
fn a_property_that_is_not_a_string_is_rejected() {
    // A TOML float is not an expression, and accepting one would invent the second kind of
    // property ABI §11 exists to refuse.
    let errors = errors("non-string-property.toml");
    assert!(matches!(errors[..], [Error::Toml(_)]), "{errors:#?}");
}

#[test]
fn an_expression_that_does_not_parse_is_rejected() {
    let errors = errors("unparsable-expression.toml");
    let [
        Error::Property {
            id, property, code, ..
        },
    ] = &errors[..]
    else {
        panic!("{errors:#?}");
    };
    assert_eq!((id.as_str(), property.as_str()), ("f3m9", "threshold"));
    // The code, not the message: this is what makes §7's last two rows two classes.
    assert_eq!(*code, eio_expr::ErrorCode::Parse);
}

#[test]
fn an_expression_static_analysis_rejects_is_rejected() {
    // EXPR §10, through the real front end: a service file that validates here configures on
    // a node, and the only way to promise that is to ask the same code.
    let errors = errors("rejected-expression.toml");
    let [
        Error::Property {
            id,
            property,
            code,
            span,
            message,
        },
    ] = &errors[..]
    else {
        panic!("{errors:#?}");
    };
    assert_eq!((id.as_str(), property.as_str()), ("f3m9", "threshold"));
    // A different code from the parse failure above, which is the whole point of carrying it
    // rather than a rendering of it: §7's last two rows are two classes.
    assert_eq!(*code, eio_expr::ErrorCode::Unbound);
    // And a span an editor can underline (EXPR §8).
    assert_eq!((span.start, span.end), (1, 9));

    // What the span covers is `nosuchfn`, but the message does not name it — that is
    // eieio-7d8.15, filed against EXPR §8. Asserted in its current form deliberately: this
    // should fail and be updated when the name arrives, rather than quietly keep passing on
    // a message nobody can act on.
    assert!(
        !message.contains("nosuchfn"),
        "eieio-7d8.15 has landed: {message}"
    );
}

// ── the overflow policy (SERVICE §5, DAEMON §6.2, eieio-8yq.9) ──────────────

#[test]
fn an_absent_overflow_key_resolves_to_backpressure() {
    let parsed = parse("name = \"minimal\"\n").expect("valid");
    assert_eq!(parsed.overflow, Overflow::Backpressure);
    // Undistinguished from writing the default out explicitly (SERVICE §5).
    let explicit = parse("name = \"minimal\"\noverflow = \"backpressure\"\n").expect("valid");
    assert_eq!(explicit.overflow, Overflow::Backpressure);
}

#[test]
fn an_explicit_drop_oldest_is_one_choice_for_the_whole_file() {
    // SERVICE §5: `overflow` is a top-level key, a sibling of `name`, and it says nothing
    // about any one connection — it is asserted here as a single field on `Parsed`, not as a
    // property of an individual edge.
    let text = "name = \"kitchen\"\noverflow = \"drop-oldest\"\n\
                 connections = [ \"a.out -> c.in\", \"b.out -> c.in\" ]\n\n\
                 [blocks.a]\nblock = \"transform:1.0.0\"\n\
                 [blocks.b]\nblock = \"transform:1.0.0\"\n\
                 [blocks.c]\nblock = \"transform:1.0.0\"\n";
    let parsed = parse(text).expect("valid");
    assert_eq!(parsed.overflow, Overflow::DropOldest);
    assert_eq!(parsed.connections.len(), 2, "both edges into `c` are read");
}

#[test]
fn an_overflow_value_outside_the_accepted_set_is_rejected() {
    let errors = errors("bad-overflow.toml");
    let [Error::Overflow { value }] = &errors[..] else {
        panic!("{errors:#?}");
    };
    assert_eq!(value, "dropoldest");
    let message = format!("{}", errors[0]);
    // Names what was given and what is accepted (SERVICE §7) — not a generic TOML error.
    assert!(message.contains("dropoldest"), "{message}");
    assert!(message.contains("backpressure"), "{message}");
    assert!(message.contains("drop-oldest"), "{message}");
}

#[test]
fn an_underscored_spelling_is_also_rejected() {
    // The verification this bead calls for by name: `drop_oldest` is not `drop-oldest`, and
    // a deployer who typed it should be told, not silently backpressured.
    let errors = match parse("name = \"kitchen\"\noverflow = \"drop_oldest\"\n") {
        Ok(_) => panic!("\"drop_oldest\" parsed, and it is not an accepted spelling"),
        Err(errors) => errors,
    };
    let [Error::Overflow { value }] = &errors[..] else {
        panic!("{errors:#?}");
    };
    assert_eq!(value, "drop_oldest");
}

#[test]
fn an_overflow_key_inside_a_block_table_is_an_unknown_field_not_the_service_policy() {
    // SERVICE §5's existing TOML rule: a top-level key belongs to the table it follows. An
    // `overflow` written after `[blocks.a]` is that block's field, and `deny_unknown_fields`
    // must say so rather than silently reading it as nothing or as the service's policy.
    let text = "name = \"kitchen\"\n\n[blocks.a]\nblock = \"transform:1.0.0\"\noverflow = \"drop-oldest\"\n";
    let errors = match parse(text) {
        Ok(_) => panic!("an `overflow` inside a block table parsed"),
        Err(errors) => errors,
    };
    assert!(matches!(errors[..], [Error::Toml(_)]), "{errors:#?}");
    assert!(format!("{}", errors[0]).contains("overflow"));
}

#[test]
fn every_error_class_has_a_fixture() {
    // The corpus is the claim that §7's table is implemented, so a class added to the spec
    // without a fixture should fail here rather than be discovered missing later.
    let mut count = 0;
    for entry in std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/invalid"))
        .expect("tests/invalid exists")
    {
        let path = entry.expect("a readable entry").path();
        if path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("readable");
        assert!(
            parse(&text).is_err(),
            "{} parsed, and everything here is a rejection",
            path.display()
        );
        count += 1;
    }
    assert_eq!(count, 13, "one fixture per SERVICE §7 stage-1 error class");
}

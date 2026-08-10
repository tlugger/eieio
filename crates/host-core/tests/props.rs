//! ABI-SPEC §11.1's `required`/`default` precedence (`eio_host_core::resolve`).
//!
//! `resolve`'s own documentation states the rule; these are the cases it admits, one test
//! per branch of it.
//!
//! An integration test rather than a `#[cfg(test)]` module, because it exercises `resolve`
//! through the crate's public surface — which is the surface both hosts get.
//!
//! Moved here verbatim from the daemon (eieio-cq4), unchanged, which is the point: the rule
//! did not change, only where its one copy lives.

use std::collections::BTreeMap;

use eio_host_core::{ResolveError, resolve};
use eio_manifest::{Manifest, PropertyType};

/// A block with three properties covering the combinations §11.1 admits.
fn manifest() -> Manifest {
    eio_manifest::parse(
        r#"{
                "name": "probe",
                "version": "1.0.0",
                "abi": { "major": 1, "minor": 0 },
                "properties": [
                    { "name": "threshold", "type": "int", "required": true, "default": "22" },
                    { "name": "label", "type": "string", "required": true },
                    { "name": "filter", "type": "string" }
                ]
            }"#,
    )
    .expect("a valid manifest")
}

fn supplied(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn a_supplied_value_wins_over_the_default() {
    let manifest = manifest();
    let supplied = supplied(&[("threshold", "30"), ("label", "\"kitchen\"")]);
    let resolved = resolve(&manifest, &supplied).expect("resolves");
    assert_eq!(resolved[0].source, Some("30"));
}

#[test]
fn a_required_property_is_satisfied_by_its_default_alone() {
    let manifest = manifest();
    let supplied = supplied(&[("label", "\"kitchen\"")]);
    let resolved = resolve(&manifest, &supplied).expect("resolves");
    assert_eq!(
        resolved[0].source,
        Some("22"),
        "the manifest default satisfies `required`"
    );
}

#[test]
fn a_required_property_with_neither_fails_and_names_itself() {
    let manifest = manifest();
    let error = resolve(&manifest, &supplied(&[])).expect_err("label has no value");
    assert_eq!(
        error,
        ResolveError::Required {
            name: String::from("label")
        }
    );
    assert!(error.to_string().contains("label"));
}

#[test]
fn an_unrequired_property_with_neither_keeps_its_slot() {
    let manifest = manifest();
    let supplied = supplied(&[("label", "\"kitchen\"")]);
    let resolved = resolve(&manifest, &supplied).expect("resolves");
    assert_eq!(resolved.len(), 3, "every declared property comes back");
    assert_eq!(resolved[2].name, "filter");
    assert_eq!(resolved[2].source, None, "unset, not omitted (ABI §7.1)");
}

#[test]
fn order_is_the_manifests_so_prop_ids_match_the_descriptor() {
    // ABI §5.2: position in `properties` is the `prop_id`, and the descriptor's `props`
    // list is built from the same order. Supplied in reverse to prove the output order
    // is not the input's.
    let manifest = manifest();
    let supplied = supplied(&[("label", "\"kitchen\""), ("threshold", "30")]);
    let resolved = resolve(&manifest, &supplied).expect("resolves");
    let names: Vec<&str> = resolved.iter().map(|source| source.name).collect();
    assert_eq!(names, ["threshold", "label", "filter"]);
    for (index, name) in names.iter().enumerate() {
        assert_eq!(manifest.prop_id(name), Some(index as u32));
    }
}

#[test]
fn a_value_for_an_undeclared_property_is_refused() {
    let manifest = manifest();
    let supplied = supplied(&[("label", "\"kitchen\""), ("tempreature", "1")]);
    assert_eq!(
        resolve(&manifest, &supplied),
        Err(ResolveError::Unknown {
            name: String::from("tempreature")
        }),
        "a typo that was silently ignored would be a block running on its defaults"
    );
}

#[test]
fn a_block_with_no_properties_resolves_to_nothing() {
    let manifest = eio_manifest::parse(
        r#"{ "name": "probe", "version": "1.0.0", "abi": { "major": 1, "minor": 0 } }"#,
    )
    .expect("valid");
    assert_eq!(resolve(&manifest, &supplied(&[])), Ok(Vec::new()));
    assert_eq!(
        resolve(&manifest, &supplied(&[("x", "1")])),
        Err(ResolveError::Unknown {
            name: String::from("x")
        })
    );
}

#[test]
fn the_declared_type_travels_with_the_source() {
    let manifest = manifest();
    let supplied = supplied(&[("label", "\"kitchen\"")]);
    let resolved = resolve(&manifest, &supplied).expect("resolves");
    assert_eq!(resolved[0].ty, PropertyType::Int);
    assert_eq!(resolved[1].ty, PropertyType::String);
}

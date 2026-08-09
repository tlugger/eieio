//! Resolving each property to the expression it is evaluated as (ABI-SPEC §11.1).
//!
//! `eio_host_core` takes already-resolved
//! [`PropertySource`]s in `prop_id` order and asks no
//! questions about where they came from, because the answer is not the ABI's: §11.1's
//! `required`/`default` rule is about a *deployment*, and it belongs to whatever describes
//! one. Today that is `dev run-block`'s `--prop` flags; when service files land (DAEMON §2)
//! they call this same function with the table they parsed, so the rule has one
//! implementation rather than one per caller.
//!
//! The rule, in full:
//!
//! 1. The supplied expression, if the deployment gave one.
//! 2. Otherwise the manifest's `default`, if it has one. A default is an expression like any
//!    other and may be signal-dependent.
//! 3. Otherwise nothing — and *that* is a configuration failure exactly when the property is
//!    `required`. An unrequired property with no value keeps its `prop_id` and answers
//!    `ERR_NOT_FOUND` (ABI §7.1).
//!
//! Order is the manifest's, because position in `properties` *is* the `prop_id` (ABI §5.2,
//! §11), and the instance descriptor is built from the same list.

use std::collections::BTreeMap;
use std::fmt;

use eio_host_core::PropertySource;
use eio_manifest::Manifest;

/// Why a property table could not be resolved (ABI §11.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// A `required` property with no supplied value and no `default`.
    Required {
        /// The property's name, which is what the deployer wrote or failed to.
        name: String,
    },
    /// A supplied value for a property the block does not declare.
    ///
    /// Rejected rather than ignored, for the reason ABI §11.1 rejects an unknown manifest
    /// field: a silently ignored `--prop tempreature=...` is a block running with its
    /// default and a deployer who believes otherwise.
    Unknown {
        /// The name that was supplied.
        name: String,
    },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::Required { name } => write!(
                f,
                "property {name:?} is required and has no value: supply one, or give the block a \
                 manifest default (ABI §11.1)"
            ),
            ResolveError::Unknown { name } => {
                write!(f, "the block declares no property named {name:?}")
            }
        }
    }
}

impl std::error::Error for ResolveError {}

/// Resolves every property the block declares, in `prop_id` order (ABI §11.1).
///
/// `supplied` is what the deployment provided, keyed by property name. Every entry in it
/// must name a declared property; every declared property comes back, in manifest order,
/// whether or not it has a value.
pub fn resolve<'a>(
    manifest: &'a Manifest,
    supplied: &'a BTreeMap<String, String>,
) -> Result<Vec<PropertySource<'a>>, ResolveError> {
    if let Some(name) = supplied
        .keys()
        .find(|name| manifest.prop_id(name).is_none())
    {
        return Err(ResolveError::Unknown { name: name.clone() });
    }

    manifest
        .properties
        .iter()
        .map(|property| {
            let source = supplied
                .get(&property.name)
                .map(String::as_str)
                .or(property.default.as_deref());
            match source {
                Some(source) => Ok(PropertySource::new(&property.name, property.ty, source)),
                None if property.required => Err(ResolveError::Required {
                    name: property.name.clone(),
                }),
                None => Ok(PropertySource::unset(&property.name, property.ty)),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use eio_manifest::PropertyType;

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
}

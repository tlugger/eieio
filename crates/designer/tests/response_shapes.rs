//! Response-*shape* drift detection for `crates/designer`'s own REST surface (eieio-m9s.33).
//!
//! # What this closes
//!
//! `crates/cli/tests/response_shapes.rs` (eieio-m9s.11) reads `eio_daemon`'s live `utoipa`
//! document and compares its schemas, field for field, against `designer/src/lib/api/types.ts`.
//! eieio-m9s.20 gave `crates/designer` a document of its own (`GET /api/openapi.json`) and fixed
//! three fields that had drifted against it — `SystemOut.id`/`NodeOut.id`/`.system_id` declared
//! `string` where the wire sends `integer`, and `NodeOut.capabilities`/`.limits` declared
//! required where the wire omits them until a probe succeeds — but found them by hand, because
//! nothing read this crate's document the way the daemon's was already being read. eieio-m9s.30
//! then wired `designer/src/lib/api/client.ts`'s `listSystems`/`listNodes` to call
//! `GET /api/systems`/`GET /api/nodes` for real, so a fourth drift here would not be a paper cut
//! — it would be wrong data rendered in a real browser. This file is that missing read, scoped
//! to this crate exactly the way `crates/cli/tests/response_shapes.rs` is scoped to the daemon's.
//!
//! # Scope: two schemas, and one deliberately excluded
//!
//! - **`SystemOut`** (`GET /api/systems`, and the 200 body of `POST /api/systems`) — a real,
//!   field-for-field mirror of `designer/src/lib/api/types.ts`'s `SystemSummary`.
//! - **`NodeOut`** (`GET /api/nodes`, and the 200 body of `POST /api/nodes` and
//!   `POST /api/nodes/{id}/probe`) — the historical bug's own schema, mirrored by `NodeSummary`.
//!
//! **`ManifestCacheEntry` (`GET /api/blocks`) is deliberately absent**, and not by oversight:
//! `designer/src/lib/api/types.ts`'s `BlockManifest` is not a mirror of it. `ManifestCacheEntry`
//! is `{block_ref, manifest, fetched_at}` (`crates/designer/src/api/blocks.rs`); `BlockManifest`
//! flattens `manifest`'s own fields (`name`, `version`, `abi`, `capabilities`, `inputs`,
//! `outputs`, `properties`, ...) up to its own top level, keeps `block_ref`, and drops
//! `fetched_at` entirely — a parsed client-side model built from the nested wire shape, the same
//! relation `ServiceDefinition` has to the daemon's `ServiceDetail` (see this crate's sibling
//! `schema-parity.test.ts`, whose `PAIRS` comment already excludes that pair for the identical
//! reason). A field-set diff between `ManifestCacheEntry` and `BlockManifest` would fail on
//! every run for a reason that says nothing about drift: `manifest`'s own fields are real fields
//! `BlockManifest` reads, just never inside a nested `manifest` object, and `ManifestCacheEntry`
//! renders `manifest` as `serde_json::Value` (an untyped `AnyValue` schema — see
//! `crates/designer/src/api/blocks.rs`'s own doc for why: the real, typed schema is
//! `eio_manifest::schema::Manifest`, a `no_std` ★ crate with no `utoipa` dependency by design),
//! so this file's `schema_kind`/`flatten` could not see into it even if the comparison were
//! reshaped to expect the flattening. `designer/src/lib/api/schema-parity.test.ts`'s own module
//! doc records the TypeScript half of this exclusion beside its `PAIRS`, the way `BlockManifest`
//! belongs "if `ServiceDefinition` or `BlockManifest` ever grow a wire twin" per that file's own
//! words about the daemon side.
//!
//! # Why this is a second test file in a second crate, not a second target in
//! `crates/cli/tests/response_shapes.rs`
//!
//! That file reaches `eio_daemon::api::openapi::Document`; this crate's document
//! (`eio_designer::api::openapi::Document`) is a different `OpenApi` impl entirely, assembled
//! from a disjoint set of handlers. Reaching both from one test binary would mean `eio-cli`
//! depending on `eio-designer` for a check that has nothing to do with the CLI, or vice versa —
//! neither crate needs the other at runtime, and this bead's own file-ownership list keeps them
//! apart. Emitting to a second, sibling generated file
//! (`designer/src/lib/api/__generated__/designer-response-shapes.json`, gitignored the same way
//! its daemon counterpart is) rather than a second key in the same file avoids a shared writer
//! two independent `cargo test` invocations would otherwise race on.
//!
//! # Why this cannot race eieio-m9s.22's bug back into existence
//!
//! That bug was two vitest suites, each shelling out to `cargo test` in their own `beforeAll`,
//! running as separate worker processes that could both be holding (or waiting on) the
//! workspace target-directory lock at once, while a third, much longer `cargo test --workspace`
//! job (the `test` stage) held it too — the wait exceeded the hook's own timeout on CI.
//!
//! It cannot recur, because no vitest file invokes cargo at all any more (eieio-m9s.42). This
//! file's generator is invoked from exactly **one** place: the `just shapes` recipe, which runs
//! this crate's `cargo test` right after the daemon's — one shell script, two commands,
//! strictly sequential. Every recipe that runs the SPA's suite depends on it (`just ci` before
//! its parallel stages start, `just test-designer` on its own), and `EIO_SHAPES_PREGENERATED`
//! makes the recipe a no-op for the second of those when `ci` has already run the first, so
//! there is never a second cargo on the lock. The vitest side reads the two generated files and
//! fails loudly when either is missing or stale — see
//! `designer/src/lib/api/generated-shapes.ts`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use eio_designer::api::openapi::Document;
use utoipa::OpenApi as _;
use utoipa::openapi::schema::SchemaType;
use utoipa::openapi::{RefOr, Schema, Type};

/// How many nested-object hops [`flatten`] follows before it stops recursing. Both of this
/// file's targets are flat today (`NodeOut`'s deepest fields, `capabilities`/`limits`, are
/// untyped `serde_json::Value` leaves, not objects with their own named properties) — kept at
/// the daemon side's own `3` rather than a smaller number specific to today's shapes, so a
/// property added one level deep tomorrow is caught rather than silently uncompared.
const MAX_DEPTH: u8 = 3;

/// [`crates/cli/tests/response_shapes.rs`]'s `flatten`, unchanged in behavior: one schema's field
/// names, flattened to dotted paths, unioned across any `oneOf`/`anyOf`/`allOf` composition
/// (neither of this file's targets is one at the top level, but a `$ref` inside a property can
/// still resolve through one).
fn flatten(
    schema: &RefOr<Schema>,
    prefix: &str,
    components: &BTreeMap<String, RefOr<Schema>>,
    depth: u8,
    out: &mut BTreeSet<String>,
) {
    match schema {
        RefOr::Ref(reference) => {
            let name = reference
                .ref_location
                .rsplit('/')
                .next()
                .unwrap_or_default();
            if let Some(resolved) = components.get(name) {
                flatten(resolved, prefix, components, depth, out);
            }
        }
        RefOr::T(Schema::Object(object)) => {
            if depth == 0 {
                return;
            }
            for (name, property) in object.properties.iter() {
                let path = dotted(prefix, name);
                out.insert(path.clone());
                flatten(property, &path, components, depth - 1, out);
            }
        }
        RefOr::T(Schema::OneOf(one_of)) => {
            for item in &one_of.items {
                flatten(item, prefix, components, depth, out);
            }
        }
        RefOr::T(Schema::AnyOf(any_of)) => {
            for item in &any_of.items {
                flatten(item, prefix, components, depth, out);
            }
        }
        RefOr::T(Schema::AllOf(all_of)) => {
            for item in &all_of.items {
                flatten(item, prefix, components, depth, out);
            }
        }
        RefOr::T(_) => {}
    }
}

fn dotted(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}.{name}")
    }
}

/// Every named schema this crate's live document declares.
fn components() -> BTreeMap<String, RefOr<Schema>> {
    Document::openapi()
        .components
        .expect("the document declares component schemas")
        .schemas
        .into_iter()
        .collect()
}

fn fields_of(name: &str, components: &BTreeMap<String, RefOr<Schema>>) -> BTreeSet<String> {
    let schema = components
        .get(name)
        .unwrap_or_else(|| panic!("no schema named `{name}` in the live document"));
    let mut out = BTreeSet::new();
    flatten(schema, "", components, MAX_DEPTH, &mut out);
    out
}

/// [`crates/cli/tests/response_shapes.rs`]'s `required_fields`/`required_of`, unchanged: one
/// schema's own top-level `required` set.
fn required_fields(
    schema: &RefOr<Schema>,
    components: &BTreeMap<String, RefOr<Schema>>,
) -> BTreeSet<String> {
    match schema {
        RefOr::Ref(reference) => {
            let name = reference
                .ref_location
                .rsplit('/')
                .next()
                .unwrap_or_default();
            components
                .get(name)
                .map(|resolved| required_fields(resolved, components))
                .unwrap_or_default()
        }
        RefOr::T(Schema::Object(object)) => object.required.iter().cloned().collect(),
        _ => BTreeSet::new(),
    }
}

fn required_of(name: &str, components: &BTreeMap<String, RefOr<Schema>>) -> BTreeSet<String> {
    let schema = components
        .get(name)
        .unwrap_or_else(|| panic!("no schema named `{name}` in the live document"));
    required_fields(schema, components)
}

/// [`crates/cli/tests/response_shapes.rs`]'s five-family kind vocabulary, unchanged — see that
/// file's module doc for the reasoning (`string`, `number` folding `integer`, `boolean`,
/// `array` never its item type, `object` never past "has properties").
fn primitive_kind(kind: &Type) -> Option<&'static str> {
    match kind {
        Type::String => Some("string"),
        Type::Integer | Type::Number => Some("number"),
        Type::Boolean => Some("boolean"),
        Type::Array => Some("array"),
        Type::Object => Some("object"),
        Type::Null => None,
    }
}

fn is_null_schema(schema: &RefOr<Schema>, components: &BTreeMap<String, RefOr<Schema>>) -> bool {
    match schema {
        RefOr::T(Schema::Object(object)) => {
            object.schema_type == SchemaType::Type(Type::Null) && object.properties.is_empty()
        }
        RefOr::Ref(reference) => {
            let name = reference
                .ref_location
                .rsplit('/')
                .next()
                .unwrap_or_default();
            components
                .get(name)
                .is_some_and(|resolved| is_null_schema(resolved, components))
        }
        _ => false,
    }
}

fn schema_kind(
    schema: &RefOr<Schema>,
    components: &BTreeMap<String, RefOr<Schema>>,
) -> Option<&'static str> {
    match schema {
        RefOr::Ref(reference) => {
            let name = reference
                .ref_location
                .rsplit('/')
                .next()
                .unwrap_or_default();
            components
                .get(name)
                .and_then(|resolved| schema_kind(resolved, components))
        }
        RefOr::T(Schema::Array(_)) => Some("array"),
        RefOr::T(Schema::Object(object)) => match &object.schema_type {
            SchemaType::Type(kind) => primitive_kind(kind),
            SchemaType::Array(kinds) => {
                let mut concrete = kinds.iter().filter(|kind| **kind != Type::Null);
                match (concrete.next(), concrete.next()) {
                    (Some(only), None) => primitive_kind(only),
                    _ => None,
                }
            }
            // `NodeOut.capabilities`/`.limits` (`Option<serde_json::Value>`) land here: an
            // untyped "any value" schema is honestly none of the five families, so this file
            // leaves them out of `types`/`required`'s companion map rather than guessing —
            // exactly the family `ApiError.detail` already left out on the daemon side.
            SchemaType::AnyValue => None,
        },
        RefOr::T(Schema::OneOf(one_of)) => {
            let mut concrete = one_of
                .items
                .iter()
                .filter(|item| !is_null_schema(item, components));
            match (concrete.next(), concrete.next()) {
                (Some(only), None) => schema_kind(only, components),
                _ => None,
            }
        }
        RefOr::T(_) => None,
    }
}

fn flatten_types(
    schema: &RefOr<Schema>,
    prefix: &str,
    components: &BTreeMap<String, RefOr<Schema>>,
    depth: u8,
    out: &mut BTreeMap<String, String>,
) {
    if !prefix.is_empty()
        && let Some(kind) = schema_kind(schema, components)
    {
        out.insert(prefix.to_string(), String::from(kind));
    }
    if depth == 0 {
        return;
    }
    match schema {
        RefOr::Ref(reference) => {
            let name = reference
                .ref_location
                .rsplit('/')
                .next()
                .unwrap_or_default();
            if let Some(resolved) = components.get(name) {
                flatten_types(resolved, prefix, components, depth, out);
            }
        }
        RefOr::T(Schema::Object(object)) => {
            for (name, property) in object.properties.iter() {
                let path = dotted(prefix, name);
                flatten_types(property, &path, components, depth - 1, out);
            }
        }
        RefOr::T(Schema::OneOf(one_of)) => {
            let mut concrete = one_of
                .items
                .iter()
                .filter(|item| !is_null_schema(item, components));
            if let (Some(only), None) = (concrete.next(), concrete.next()) {
                flatten_types(only, prefix, components, depth, out);
            }
        }
        RefOr::T(_) => {}
    }
}

fn types_of(name: &str, components: &BTreeMap<String, RefOr<Schema>>) -> BTreeMap<String, String> {
    let schema = components
        .get(name)
        .unwrap_or_else(|| panic!("no schema named `{name}` in the live document"));
    let mut out = BTreeMap::new();
    flatten_types(schema, "", components, MAX_DEPTH, &mut out);
    out
}

/// Where `schema-parity.test.ts` reads what this test just wrote — a sibling of, never the same
/// file as, `daemon-response-shapes.json` (see this module's doc for why a shared writer would
/// be exactly the race this bead's brief warns against).
fn generated_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../designer/src/lib/api/__generated__/designer-response-shapes.json")
}

/// Emits the field sets, required sets and kind maps `schema-parity.test.ts` asserts against for
/// this crate's own two targets. See this module's doc for why `ManifestCacheEntry` is not a
/// third.
#[test]
fn emit_response_shapes() {
    let components = components();
    let targets = ["SystemOut", "NodeOut"];

    let mut shapes = serde_json::Map::new();
    let mut required = serde_json::Map::new();
    let mut types = serde_json::Map::new();
    for name in targets {
        let fields = fields_of(name, &components);
        assert!(
            !fields.is_empty(),
            "`{name}` resolved to a schema with no fields at all — almost certainly a typo in \
             this test, not a fact about the Designer"
        );
        shapes.insert(
            String::from(name),
            serde_json::Value::Array(fields.into_iter().map(serde_json::Value::String).collect()),
        );
        required.insert(
            String::from(name),
            serde_json::Value::Array(
                required_of(name, &components)
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
        types.insert(
            String::from(name),
            serde_json::Value::Object(
                types_of(name, &components)
                    .into_iter()
                    .map(|(field, kind)| (field, serde_json::Value::String(kind)))
                    .collect(),
            ),
        );
    }
    shapes.insert(
        String::from("required"),
        serde_json::Value::Object(required),
    );
    shapes.insert(String::from("types"), serde_json::Value::Object(types));

    let path = generated_path();
    std::fs::create_dir_all(path.parent().expect("has a parent"))
        .unwrap_or_else(|error| panic!("creating {}: {error}", path.parent().unwrap().display()));
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::Value::Object(shapes)).unwrap(),
    )
    .unwrap_or_else(|error| panic!("writing {}: {error}", path.display()));
}

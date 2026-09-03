//! Response-*shape* drift detection, for the Designer (eieio-m9s.11).
//!
//! # What this closes, over `tests/openapi_surface.rs`
//!
//! `tests/openapi_surface.rs` (eieio-yck.3) proves the daemon's live OpenAPI document and
//! `eio_cli::client::ENDPOINTS` agree on *paths*. Nothing anywhere compared response **shapes**,
//! and `designer/src/lib/api/types.ts` hand-writes a TypeScript type for every body it reads.
//! That gap produced a real bug, fixed in `b00f430`: the Designer's `NodeInfo` invented
//! `versions: {abi:{major,minor}, daemon}` where the daemon serves flat `version`/`abi` strings,
//! `budgets.expr` as a nested object where the daemon serves a flat `expr_max_fuel`, and omitted
//! `capabilities` entirely — and `openapi_surface.rs`'s path-level test stayed green throughout,
//! because nothing about that bug ever touched a path.
//!
//! # The approach, and why this one
//!
//! The sub-plan offers two shapes that keep one source of truth for the daemon side (its own
//! `utoipa` schemas, read through [`Document::openapi`] the same way `openapi_surface.rs`
//! already does) and asks which. This picks **the smaller one**: this test emits each targeted
//! schema's field names — recursing through nested objects, so a field that moved *inside*
//! another object (`budgets.expr` vs `budgets.expr_max_fuel`, the historical bug's other half)
//! is caught and not just a field that disappeared entirely — to a generated JSON file, and
//! `designer/src/lib/api/schema-parity.test.ts` reads it and compares the field set against the
//! *actual* TypeScript interface, extracted from `types.ts`'s own AST via the `typescript`
//! package rather than a second hand-copied list (which is exactly the third-source-of-truth
//! CLAUDE.md's prime directive and this bead's own brief warn against).
//!
//! The generated file (`designer/src/lib/api/__generated__/daemon-response-shapes.json`,
//! gitignored) is **not trusted**: `schema-parity.test.ts` regenerates it itself, by shelling
//! out to `cargo test -p eio-cli --test response_shapes` before reading it, so a stale or
//! missing file is never silently compared against — see that file's module doc for why that
//! had to live on the TypeScript side rather than in `just ci`'s stage ordering.
//!
//! Approach two — generating the TypeScript types outright and diffing committed output — is
//! the stronger check (it would also police shapes nothing here names), but it is real design
//! work this bead's scope does not need: the Designer's types are hand-written today because
//! there is no code generator anywhere in this repository yet, and building the first one as a
//! side effect of a parity test would be exactly the kind of scope creep CLAUDE.md's "never
//! implement past what's asked" spirit warns about. If the Designer ever gains a real generated
//! client, this file's approach should be deleted in the same commit that lands it.
//!
//! # Scope: which bodies, and which do not compare cleanly
//!
//! Three schema pairs are asserted, byte for byte, field for field:
//!
//! - **`NodeInfo`** (`GET /node`) — the historical bug's own schema. The proof this file exists
//!   to satisfy — reintroducing `b00f430`'s drift into `types.ts` and watching
//!   `schema-parity.test.ts` name the field — is a verification step, not a permanent test (a
//!   permanent one would have to carry the bug's exact field names hard-coded somewhere, which
//!   is the third list this bead exists to prevent); see the final report for its transcript.
//! - **`TapRequest`** (`POST /taps`'s body) — already an exact match; kept in the asserted set
//!   so the mechanism is proven on more than one schema, not just the one it was built for.
//! - **`ApiError`** (DAEMON §9.2's failure envelope, and the *actual* 200 body of
//!   `GET /services/{s}/errors` — see below) — matched against a **new** `ApiError` interface
//!   added to `types.ts` by this bead. Nothing in `designer/src/` reads it yet (see next
//!   paragraph), so nothing breaks by adding it; it exists so a future `crates/designer` fetch
//!   has the right shape waiting rather than another guess to discover the hard way.
//!
//! **What this deliberately does not cover, and why:**
//!
//! - **The service listing (`ServiceSummary`) and `/services/{s}/errors`.** The daemon serves
//!   `ServiceSummary { name, state, error? }` for `GET /services`, and literally an `ApiError`
//!   (not a list of anything) for `GET /services/{s}/errors`. `designer/src/lib/api/types.ts`'s
//!   existing `ServiceSummary` has `autostart: boolean` instead of `error` — a field the wire
//!   response never carries, sourced today only from `mock.ts`'s fabricated fixture — and its
//!   `ServiceErrorReport`/`InstanceError` pair for the errors endpoint has no relationship to
//!   `ApiError` at all. Both are **real, currently-existing drift this check would catch**, but
//!   fixing either one cleanly touches `NodeDashboard.svelte`, `Toolbar.svelte` and `App.svelte`
//!   (all of which read `.autostart` off a `ServiceSummary`) or `NodeDashboard.svelte` again
//!   (which reads `.errors`/`.instance`/`.restarts` off a `ServiceErrorReport`) — none of which
//!   this bead owns (`designer/src/lib/api/types.ts`, `mock.ts`, and new test files only).
//!   Forcing them into the strict, asserted set would mean either quietly weakening the check
//!   (an allowlist of "known-okay" mismatches — the exact third list this bead exists to avoid)
//!   or shipping a change with unreviewed collateral damage outside this worktree's remit. Both
//!   are noted in `types.ts` at the point they matter, and reported to the driving agent as
//!   follow-up work.
//! - **The service listing (`ServiceSummary`) and `/services/{s}/errors`**, exactly as above —
//!   unchanged and still out of scope for this bead (eieio-m9s.13 owns the SSE side only).
//!
//! # The SSE payloads, covered as of eieio-m9s.13
//!
//! `taps::stream` and `logs::stream` are not *in* the OpenAPI document at all — both declare
//! only `content_type = "text/event-stream"` with no `body`, so there is no schema for utoipa to
//! collect from a `#[utoipa::path]`. And a plain field-set diff against `Observation`/`What`
//! would not be a fair test even with a schema in hand: the wire shape is `Observation`'s own
//! fields flattened with whichever `What` variant applied (`#[serde(untagged)]`, so no JSON tag
//! names the variant), and it is the *SSE frame's `event:` line* that does — DAEMON §9.6's
//! amendment, quoted in the sub-plan, spells this out as "the event name is the discriminant,
//! and the payload is flat".
//!
//! So this file also emits an `"sse"` entry: one field set per event name, built by pulling
//! `What::schema()`'s `oneOf` (one branch per variant, in the enum's declaration order — the
//! same order [`What::event`] switches on, confirmed by printing both side by side while writing
//! this, and re-checked every run below rather than trusted once) and `Observation::schema()`'s
//! common fields (the `allOf` branch that is not the `$ref` to `What`), and pairing each `oneOf`
//! branch with the event name a real value of that variant reports through `What::event` — never
//! a name typed out by hand a second time, which is the whole reason `What::event` exists
//! (`crates/daemon/src/observe.rs`'s module doc and the bead itself). `designer/src/lib/api/
//! schema-parity.test.ts` derives its own side of the pairing the same way, from `types.ts`'s own
//! AST — see its module doc.
//!
//! Two live bugs were found by hand before this check existed, and are now covered by it:
//! `a36f7a7` fixed a required `timestamp` the daemon never sent (`decodeLogFrame` now reads the
//! wire's `at`, keeping `LogLineEvent.timestamp` as the field's name for the panel that already
//! reads it — an intentional rename, not a gap, and `schema-parity.test.ts`'s `@wire` tag is how
//! it tells the checker so), and a `span` decoded as an object where the wire carries the string
//! `"12..34"`. A third, not previously reported: `What::ExprFailure` carries `prop`, a numeric
//! property index, where `ExprFailureEvent` had only ever invented `property`, a name with no
//! wire source at all. Unlike the other two, this one could not be fixed by re-pointing the
//! decoder at the real field under the existing name: `mock.ts` and `mock-taps.test.ts` (neither
//! owned by this bead) already exercise `property` as a fabricated name string, so `types.ts`
//! instead gained a real `prop` field alongside the untouched, `@legacy`-tagged `property` —
//! excluded from the comparison rather than fixed, and reported as follow-up (see `types.ts`'s
//! doc on `ExprFailureEvent.property` for the detail).
//!
//! # `"required"` / `"sseRequired"` (eieio-m9s.15)
//!
//! `designer/src/lib/api/mock.ts` is a *third* statement of every wire shape above — it
//! manufactures the SSE frames and API responses the Designer is developed and demoed against —
//! and nothing compared it to the two schema-parity already reconciles. A field-name diff alone
//! (this file's `fields_of`/`sse_shapes`) catches half of that: a field the mock invents. It
//! cannot catch the other half, a field the daemon *always* sends that the mock omits, because
//! DAEMON §9.6 and ABI §11 both make a *sometimes*-present field absent rather than null, so a
//! flat field-name set cannot distinguish "legitimately absent this time" from "never sent at
//! all". `required_of`/`sse_required` add exactly that: each target schema's and each SSE
//! event's own **required** field names (`Schema::Object.required`, i.e. not `Option` — `port`
//! and `ExprFailure`'s `signal` are the two fields this excludes on purpose), so
//! `mock-parity.test.ts` can assert "every field the mock invents is one the daemon has" and
//! "every field the daemon always sends is one the mock actually included" as two separate
//! rules, never conflating an optional field's occasional absence with an invented or a missing
//! one. See that file's own module doc for the mechanism (it exercises the mock rather than
//! reading its source, and says plainly which emitters it reaches).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use eio_daemon::api::openapi::Document;
use eio_daemon::observe::{Observation, What};
use utoipa::openapi::schema::SchemaType;
use utoipa::openapi::{RefOr, Schema, Type};
use utoipa::{OpenApi as _, PartialSchema};

/// How many nested-object hops [`flatten`] follows before it stops recursing.
///
/// Two is enough for every schema this file targets (`NodeInfo.limits.max_payload` is the
/// deepest path any of them has); a composition (`oneOf`/`anyOf`/`allOf`) or a `$ref` does not
/// itself spend a hop — only stepping through a *named property* does, so this bounds the
/// number of dots in a path, not the number of schemas visited resolving one level of it.
const MAX_DEPTH: u8 = 3;

/// One schema's field names, flattened to dotted paths (`"limits.max_payload"`), and unioned
/// across every branch of a `oneOf`/`anyOf`/`allOf` composition rather than kept per-branch —
/// which is the right call for the schemas this file asserts on (none of them is itself a
/// tagged union) and is documented as a simplification, not hidden as one, for `Observation`/
/// `What` above, where it would not be.
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
        // `Schema::Array` (none of this file's targeted schemas has an array of objects; a
        // signal batch's own array-of-string ports, `NodeInfo::capabilities`, are already fully
        // described by the property name that holds them) and anything else: `Schema` is
        // `#[non_exhaustive]`, so this wildcard is utoipa's own reserved-variant guarantee, not
        // a case this file chooses to ignore.
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

/// Every named schema this file can resolve a `$ref` against: the live document's own
/// components (everything reachable from a `#[utoipa::path]` the document lists), plus
/// [`Observation`] and [`What`], which are not — see this module's doc for why.
fn components() -> BTreeMap<String, RefOr<Schema>> {
    let mut map: BTreeMap<String, RefOr<Schema>> = Document::openapi()
        .components
        .expect("the document declares component schemas")
        .schemas
        .into_iter()
        .collect();
    map.insert(String::from("Observation"), Observation::schema());
    map.insert(String::from("What"), What::schema());
    map
}

/// [`flatten`]'s entry point for one named schema.
fn fields_of(name: &str, components: &BTreeMap<String, RefOr<Schema>>) -> BTreeSet<String> {
    let schema = components
        .get(name)
        .unwrap_or_else(|| panic!("no schema named `{name}` in the live document"));
    let mut out = BTreeSet::new();
    flatten(schema, "", components, MAX_DEPTH, &mut out);
    out
}

/// One example value per [`What`] variant, in the enum's declaration order — the same order its
/// `oneOf` schema lists them in (confirmed empirically by [`sse_shapes`]'s own subset check
/// below, every run, not just once by eye).
///
/// This is the one place a variant is named by hand in this file, and unavoidably so: nothing
/// short of a derive macro can enumerate an enum's variants at runtime, and something has to
/// call [`What::event`] on *a value* to read off the mapping — schemas alone have no values.
/// What is never hand-typed anywhere is which *event name* a variant corresponds to: that always
/// comes from calling [`What::event`], never from a string written out to match it. Content of
/// the example values does not matter (empty strings, zero); only their variant and the order
/// they are listed in do.
fn what_examples() -> Vec<What> {
    vec![
        What::Signals {
            signals: Vec::new(),
        },
        What::ExprFailure {
            code: String::new(),
            span: String::new(),
            message: String::new(),
            prop: 0,
            signal: None,
        },
        What::Discarded {
            reason: String::new(),
        },
        What::Log {
            level: String::new(),
            message: String::new(),
        },
        What::Lagged { missed: 0 },
    ]
}

/// [`Observation`]'s fields that every event carries, regardless of which `What` variant applied
/// (DAEMON §9.6: "every payload carries `service`, `instance`, `at`... and `event`, plus `port`
/// where the observation has one").
///
/// `Observation::schema()` is an `allOf` of `[$ref: What, {the plain fields}]` (utoipa's
/// rendering of a struct with one `#[serde(flatten)]` field beside ordinary ones) — this walks
/// that `allOf` and flattens everything that is *not* the `$ref` to `What`, since `What`'s own
/// fields are per-variant and handled separately, per event, by [`sse_shapes`].
fn common_observation_fields(components: &BTreeMap<String, RefOr<Schema>>) -> BTreeSet<String> {
    let RefOr::T(Schema::AllOf(all_of)) = Observation::schema() else {
        panic!(
            "`Observation::schema()` is no longer an `allOf` of `[What, {{common fields}}]` — \
             the SSE parity check's `common_observation_fields` assumed utoipa renders a \
             `#[serde(flatten)]` field this way; find another route or STOP and report"
        );
    };
    let mut out = BTreeSet::new();
    for item in &all_of.items {
        // The `$ref` branch is `What` itself — its fields are per-variant, not common, and
        // `sse_shapes` adds them per event name rather than unioning them in here.
        if matches!(item, RefOr::Ref(_)) {
            continue;
        }
        flatten(item, "", components, MAX_DEPTH, &mut out);
    }
    out
}

/// One schema's own **required** field names — `Schema::Object.required`, top-level only, not
/// recursed the way [`flatten`] is.
///
/// eieio-m9s.15: the field-name sets [`fields_of`]/[`sse_shapes`] emit answer "did the mock
/// invent a field the daemon never sends" but not "did it omit one the daemon always does" —
/// DAEMON §9.6 and ABI §11 both make a *sometimes*-present field absent rather than null, so
/// `designer/src/lib/api/mock-parity.test.ts`'s companion rule needs to know which fields are
/// **never** absent before it can complain about one that is. Top-level only, because every
/// schema this file targets is one the mock either populates a nested object of in full or
/// omits entirely — nothing here needs "is `budgets.expr_max_fuel` required given `budgets` is
/// present" the way a partially-populated nested object would.
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
        // `oneOf`/`anyOf`/`allOf` and anything else: none of [`emit_response_shapes`]'s named
        // targets is itself a composition at the top level (unlike `Observation`/`What`, which
        // [`sse_required`] below walks by hand for exactly that reason), so an empty set here
        // would be a bug in which schema this was called on, not a case to render.
        _ => BTreeSet::new(),
    }
}

/// [`required_fields`]'s entry point for one named schema, mirroring [`fields_of`].
fn required_of(name: &str, components: &BTreeMap<String, RefOr<Schema>>) -> BTreeSet<String> {
    let schema = components
        .get(name)
        .unwrap_or_else(|| panic!("no schema named `{name}` in the live document"));
    required_fields(schema, components)
}

// --- `"types"` / `"sseTypes"` (eieio-m9s.16) --------------------------------------------------
//
// `fields_of`/`sse_shapes` answer "does this field exist"; `required_of`/`sse_required` answer
// "is it ever absent"; neither answers "is it the same *kind* of thing" — which is exactly how
// the historical `span` bug (a batch's failed-expression span rendered as the string `"12..34"`
// on the wire, decoded as `{start, end}` in `mock.ts`) would have kept passing a name-only diff:
// `span` is a real field name on both sides, so nothing about a name-set ever saw the object-vs-
// string drift. `types_of`/`sse_types` add a field's **kind** to the generated JSON, from the
// same live `utoipa` schemas the rest of this file already reads.
//
// Scoped deliberately, not maximally: the five JSON Schema primitive families this repo's wire
// shapes actually use — `string`, `number` (folding `integer` in with it; nothing here compares
// `Vec<u8>` against `Vec<i32>`), `boolean`, `array` (never its item type), `object` (a nested
// schema with properties, never further compared structurally here — `fields_of`'s own
// dotted-path recursion already does that at the name level). Anything a schema resolves to that
// isn't cleanly one of those five — `AnyValue` (`ApiError.detail`, deliberately untyped: DAEMON
// §9.2 says its shape is per-slug), a real multi-branch `oneOf`/`anyOf` union, `AllOf` at a leaf
// (none of this file's targets has one below the top level) — is left out of the emitted map
// rather than guessed at. `designer/src/lib/api/schema-parity.test.ts`'s TypeScript side applies
// the identical scope: a union of more than one substantive member, a type alias, or a reference
// to another interface is "honestly unmappable" there too, and the comparison only ever runs over
// paths *both* sides managed to give a kind for — see that file's own doc for the reasoning and
// for the one field (`ExprFailureEvent.span`) and one field's required-ness
// (`service`/`instance`/`at`/`prop`/`span` across the SSE payloads) this scope boundary and a
// file outside this bead's ownership combine to make permanently, verifiably unfixable from here.
fn primitive_kind(kind: &Type) -> Option<&'static str> {
    match kind {
        Type::String => Some("string"),
        Type::Integer | Type::Number => Some("number"),
        Type::Boolean => Some("boolean"),
        Type::Array => Some("array"),
        Type::Object => Some("object"),
        // `Null` alone (never paired with a real type — that case is `SchemaType::Array` below)
        // describes nothing comparable.
        Type::Null => None,
    }
}

/// Whether `schema` is JSON Schema's `{"type": "null"}` with nothing else — the shape utoipa
/// renders for the `null` half of an `Option<SomeRef>` (a $ref can't fold `null` into its own
/// `type` array the way a primitive can, so it becomes a two-branch `oneOf` instead;
/// `ServiceSummary.error: Option<ApiError>` is this file's one live example).
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

/// One schema's kind, in the five-family vocabulary this module's doc scopes to — `None` when it
/// is not honestly one of them.
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
            // `Option<String>` etc. renders as `"type": ["string", "null"]` rather than folding
            // the field into `required`'s absence alone — this is the nullable-primitive shape
            // [`stripOptionality`]'s TypeScript counterpart handles for `T | undefined`, mirrored
            // here for the wire side: exactly one non-`null` entry gives that entry's kind, two
            // or more substantive entries is a real union this scope does not compare.
            SchemaType::Array(kinds) => {
                let mut concrete = kinds.iter().filter(|kind| **kind != Type::Null);
                match (concrete.next(), concrete.next()) {
                    (Some(only), None) => primitive_kind(only),
                    _ => None,
                }
            }
            SchemaType::AnyValue => None,
        },
        // The `Option<$ref>` shape [`is_null_schema`] documents: exactly one non-null branch
        // recurses into that branch's own kind, the same "strip the null, map what remains" rule
        // as the nullable-primitive case just above.
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
        // A real `oneOf`/`anyOf` union (more than one substantive branch) and `AllOf` at a leaf:
        // no single kind to report, and none of this file's targeted schemas has one below the
        // top level today — left absent rather than guessed at.
        RefOr::T(_) => None,
    }
}

/// [`flatten`]'s counterpart for kinds rather than names: the same dotted-path recursion through
/// `Object` properties (a `$ref`/`Option<$ref>` costs no depth, stepping into a named property
/// does — identical to `flatten`'s own rule), but recording [`schema_kind`] at each path instead
/// of just the path's existence. A path [`flatten`] would include but this leaves out of `out`
/// is not a bug: it means the field's kind was not honestly one of the five families, and the
/// generated JSON simply carries no entry for it, which is this module's doc's "left out rather
/// than guessed at" applied to the actual emission.
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

/// [`flatten_types`]'s entry point for one named schema, mirroring [`fields_of`]/[`required_of`].
fn types_of(name: &str, components: &BTreeMap<String, RefOr<Schema>>) -> BTreeMap<String, String> {
    let schema = components
        .get(name)
        .unwrap_or_else(|| panic!("no schema named `{name}` in the live document"));
    let mut out = BTreeMap::new();
    flatten_types(schema, "", components, MAX_DEPTH, &mut out);
    out
}

/// [`common_observation_fields`]'s counterpart for kinds — flat, since DAEMON §9.6's SSE
/// payloads are (`port`'s own kind is folded out of its `Option` the same way [`schema_kind`]
/// does for any nullable primitive).
fn common_observation_types(
    components: &BTreeMap<String, RefOr<Schema>>,
) -> BTreeMap<String, String> {
    let RefOr::T(Schema::AllOf(all_of)) = Observation::schema() else {
        panic!(
            "`Observation::schema()` is no longer an `allOf` of `[What, {{common fields}}]` — \
             see `common_observation_fields`'s identical panic message for why this assumption \
             matters; find another route or STOP and report"
        );
    };
    let mut out = BTreeMap::new();
    for item in &all_of.items {
        if matches!(item, RefOr::Ref(_)) {
            continue;
        }
        flatten_types(item, "", components, MAX_DEPTH, &mut out);
    }
    out
}

/// [`sse_shapes`]'s counterpart for kinds, keyed the same way (by [`What::event`]'s name).
fn sse_types(
    components: &BTreeMap<String, RefOr<Schema>>,
) -> serde_json::Map<String, serde_json::Value> {
    let common = common_observation_types(components);

    let RefOr::T(Schema::OneOf(one_of)) = What::schema() else {
        panic!(
            "`What::schema()` is no longer a `oneOf` — see `sse_shapes`'s identical panic \
             message; find another route or STOP and report"
        );
    };
    let examples = what_examples();
    assert_eq!(
        examples.len(),
        one_of.items.len(),
        "`What::schema()` has {} `oneOf` branches but `what_examples()` only names {} — \
         `sse_shapes` already asserts this with the fuller message; find another route or STOP \
         and report",
        one_of.items.len(),
        examples.len(),
    );

    let mut out = serde_json::Map::new();
    for (branch, example) in one_of.items.iter().zip(examples.iter()) {
        let mut types = common.clone();
        flatten_types(branch, "", components, MAX_DEPTH, &mut types);
        out.insert(
            String::from(example.event()),
            serde_json::Value::Object(
                types
                    .into_iter()
                    .map(|(field, kind)| (field, serde_json::Value::String(kind)))
                    .collect(),
            ),
        );
    }
    out
}

/// Per-event **required** field sets for the SSE payloads, the required-ness counterpart of
/// [`sse_shapes`]: [`Observation`]'s own common fields that are not `Option` (`service`,
/// `instance`, `event`, `at` — `port` is `Option<String>` and deliberately excluded) unioned
/// with whichever of a `What` variant's fields are not `Option` either (`ExprFailure`'s `signal`
/// is the one per-variant exclusion: DAEMON §9.6 names it present "when the failure was
/// per-signal", so it is never in this set).
fn sse_required(
    components: &BTreeMap<String, RefOr<Schema>>,
) -> serde_json::Map<String, serde_json::Value> {
    let RefOr::T(Schema::AllOf(all_of)) = Observation::schema() else {
        panic!(
            "`Observation::schema()` is no longer an `allOf` of `[What, {{common fields}}]` — \
             see `common_observation_fields`'s identical panic message for why this assumption \
             matters; find another route or STOP and report"
        );
    };
    let mut common = BTreeSet::new();
    for item in &all_of.items {
        if matches!(item, RefOr::Ref(_)) {
            continue;
        }
        common.extend(required_fields(item, components));
    }

    let RefOr::T(Schema::OneOf(one_of)) = What::schema() else {
        panic!(
            "`What::schema()` is no longer a `oneOf` — see `sse_shapes`'s identical panic \
             message; find another route or STOP and report"
        );
    };
    let examples = what_examples();
    assert_eq!(
        examples.len(),
        one_of.items.len(),
        "`What::schema()` has {} `oneOf` branches but `what_examples()` only names {} — \
         `sse_shapes` already asserts this with the fuller message; find another route or STOP \
         and report",
        one_of.items.len(),
        examples.len(),
    );

    let mut out = serde_json::Map::new();
    for (branch, example) in one_of.items.iter().zip(examples.iter()) {
        let mut required = common.clone();
        required.extend(required_fields(branch, components));
        out.insert(
            String::from(example.event()),
            serde_json::Value::Array(
                required
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }
    out
}

/// Per-event field sets for the SSE payloads (DAEMON §9.6): `{"signals": [...], ...}`, one entry
/// per [`What`] variant, keyed by the event name [`What::event`] reports for it — see this
/// module's doc for the derivation and why a field-name diff needed this rather than the
/// existing [`fields_of`].
fn sse_shapes(
    components: &BTreeMap<String, RefOr<Schema>>,
) -> serde_json::Map<String, serde_json::Value> {
    let common = common_observation_fields(components);

    let RefOr::T(Schema::OneOf(one_of)) = What::schema() else {
        panic!(
            "`What::schema()` is no longer a `oneOf` — the SSE parity check assumed \
             `#[serde(untagged)]` on an enum of struct variants produces one schema branch per \
             variant; find another route or STOP and report"
        );
    };

    let examples = what_examples();
    assert_eq!(
        examples.len(),
        one_of.items.len(),
        "`What::schema()` has {} `oneOf` branches but `what_examples()` only names {} — a \
         variant was added to `observe.rs`'s `What` without a matching entry in \
         `what_examples()` above; add one so the SSE parity check covers it too, rather than \
         silently skipping it",
        one_of.items.len(),
        examples.len(),
    );

    let mut out = serde_json::Map::new();
    for (index, (branch, example)) in one_of.items.iter().zip(examples.iter()).enumerate() {
        let mut fields = common.clone();
        flatten(branch, "", components, MAX_DEPTH, &mut fields);

        // The empirical check `what_examples()`'s own doc promises: this branch's declared
        // fields had better actually describe the example paired with it, or `oneOf`'s
        // declaration order does not match the enum's declaration order the way `What::event`'s
        // match arms (and this pairing) assume it does.
        if let serde_json::Value::Object(serialized) =
            serde_json::to_value(example).expect("`What` serializes")
        {
            for key in serialized.keys() {
                assert!(
                    fields.contains(key.as_str()),
                    "`what_examples()`'s {index}th example (`{}`) serialized a `{key}` field \
                     that `What::schema()`'s {index}th `oneOf` branch does not declare — the \
                     assumption that `oneOf` order matches `What`'s declaration order does not \
                     hold; find another route or STOP and report",
                    example.event(),
                );
            }
        }

        out.insert(
            String::from(example.event()),
            serde_json::Value::Array(fields.into_iter().map(serde_json::Value::String).collect()),
        );
    }
    out
}

/// Where `schema-parity.test.ts` reads what this test just wrote.
///
/// Generated, gitignored (`designer/.gitignore`), and overwritten on every run — see this
/// module's doc for why the TypeScript side regenerates it itself rather than trusting whatever
/// is already on disk.
fn generated_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../designer/src/lib/api/__generated__/daemon-response-shapes.json")
}

/// Emits the field sets `schema-parity.test.ts` asserts against.
///
/// The names on the left are this file's own choice of what to call each schema in the
/// generated JSON — they do not have to be the daemon's Rust type name, only a name
/// `schema-parity.test.ts` and this file agree on. They are the Rust names here because there
/// is no reason to invent different ones.
///
/// `"required"` and `"sseRequired"` are eieio-m9s.15's addition, read by
/// `designer/src/lib/api/mock-parity.test.ts` and by nothing else: `schema-parity.test.ts`
/// predates them and only ever reads the keys above by name, so adding entries here cannot
/// perturb it (and is not itself sufficient reason to touch a file eieio-m9s.15 does not own).
///
/// `"types"` and `"sseTypes"` are eieio-m9s.16's addition, read by `schema-parity.test.ts` and by
/// nothing else — new top-level keys, same rule: additive, and no reason for
/// `mock-parity.test.ts` to ever read them.
#[test]
fn emit_response_shapes() {
    let components = components();
    let targets = ["NodeInfo", "TapRequest", "ApiError", "ServiceSummary"];

    let mut shapes = serde_json::Map::new();
    let mut required = serde_json::Map::new();
    let mut types = serde_json::Map::new();
    for name in targets {
        let fields = fields_of(name, &components);
        assert!(
            !fields.is_empty(),
            "`{name}` resolved to a schema with no fields at all — almost certainly a typo in \
             this test, not a fact about the daemon"
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
        String::from("sse"),
        serde_json::Value::Object(sse_shapes(&components)),
    );
    shapes.insert(
        String::from("required"),
        serde_json::Value::Object(required),
    );
    shapes.insert(
        String::from("sseRequired"),
        serde_json::Value::Object(sse_required(&components)),
    );
    shapes.insert(String::from("types"), serde_json::Value::Object(types));
    shapes.insert(
        String::from("sseTypes"),
        serde_json::Value::Object(sse_types(&components)),
    );

    let path = generated_path();
    std::fs::create_dir_all(path.parent().expect("has a parent"))
        .unwrap_or_else(|error| panic!("creating {}: {error}", path.parent().unwrap().display()));
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::Value::Object(shapes)).unwrap(),
    )
    .unwrap_or_else(|error| panic!("writing {}: {error}", path.display()));
}

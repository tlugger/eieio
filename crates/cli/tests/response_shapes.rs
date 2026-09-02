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
//! - **The tap-stream and log-stream SSE event payloads (`Observation`/`What`).** These are not
//!   even *in* the OpenAPI document — `taps::stream` and `logs::stream` declare only
//!   `content_type = "text/event-stream"` with no `body`, so there is no schema for utoipa to
//!   collect. Pulling `Observation`/`What`'s schema directly via `PartialSchema::schema()`
//!   (both types are `pub`, via `eio_daemon::observe`) shows why a field-set diff against
//!   `TapStreamEvent`/`LogLineEvent` would not be a fair test even if it were wired up: the wire
//!   shape is `Observation`'s own fields (`service`, `instance`, `event`, `port?`) flattened
//!   with whichever `What` variant applied (`#[serde(untagged)]`, so no tag field at all), and
//!   the *SSE frame's `event:` line* — not a JSON field — is what names the variant. The
//!   Designer's `decodeTapFrame`/`decodeLogFrame` (`stream-events.ts`, not owned by this bead)
//!   know this and decode accordingly, but the decoded shape they produce differs from the wire
//!   shape in ways a naive field-name diff cannot see through: `span` is a rendered `"12..34"`
//!   string on the wire, decoded into a `{start,end}` object; `What::ExprFailure` carries `prop`
//!   (a numeric index) where `ExprFailureEvent` expects `property` (a name) and decodes it as
//!   `undefined` since the wire never has one; and neither `Observation` nor any `What` variant
//!   carries a `timestamp` field at all, so `decodeLogFrame`'s requirement that `payload.timestamp`
//!   be a string means **every real log line fails to decode** — `LogLineEvent.timestamp` has no
//!   wire source. That last one is a genuine, live bug in the log-streaming path, found while
//!   scoping this check; it is reported rather than fixed here because the fix is a decoder
//!   change (`stream-events.ts`) outside this bead's owned files, and because CLAUDE.md's rule
//!   for `crates/daemon/src` cuts the other way here too: the daemon's shape is the reasoned
//!   one (`span` as a byte-offset string mirrors EXPR §8; `prop` as an index is what the
//!   descriptor already numbers properties by) and it is the client's guess that is wrong.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use eio_daemon::api::openapi::Document;
use eio_daemon::observe::{Observation, What};
use utoipa::openapi::{RefOr, Schema};
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
#[test]
fn emit_response_shapes() {
    let components = components();
    let targets = ["NodeInfo", "TapRequest", "ApiError"];

    let mut shapes = serde_json::Map::new();
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
    }

    let path = generated_path();
    std::fs::create_dir_all(path.parent().expect("has a parent"))
        .unwrap_or_else(|error| panic!("creating {}: {error}", path.parent().unwrap().display()));
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::Value::Object(shapes)).unwrap(),
    )
    .unwrap_or_else(|error| panic!("writing {}: {error}", path.display()));
}

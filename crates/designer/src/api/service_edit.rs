//! `POST /api/service-edit` (DESIGNER-SPEC §3.2): editing a service file is a stateless
//! transform.
//!
//! The Designer holds no service file (SCOPE §3.8): the browser `GET`s a service's text from a
//! node through the proxy, sends it here with what the operator just did, and `PUT`s what comes
//! back. This handler carries no session, no draft and no service identity — it takes text and
//! a batch of operations and returns text, or the reasons it could not.
//!
//! # Why this is a thin wrapper and stays one
//!
//! SERVICE §9 requires a structural edit to preserve everything it did not change — comments,
//! key order, alignment, blank lines, quoting — and `eio-service`'s [`Document`] is this
//! repository's one implementation of that (`toml_edit` underneath). Reimplementing any part of
//! it here, even to make one operation more convenient, would be a second editor to keep in
//! agreement with the CLI's — which is exactly what SERVICE §9's one-editor rule exists to
//! prevent. Every operation below is a direct call to a `Document` method; nothing here parses
//! or renders TOML on its own.
//!
//! # The operations
//!
//! [`Operation`] is not spec'd — DESIGNER §3.2 leaves the JSON shape to this crate — so it is
//! designed to stay obvious: one variant per `Document` mutator, `#[serde(tag = "op")]` naming
//! it the same snake_case as the Rust method, and fields named the same as that method's
//! parameters. A caller reading `Document`'s doc comments already knows this shape.
//!
//! # All-or-nothing, in order
//!
//! DESIGNER §3.2 and SERVICE §9: a batch that fails at its third operation must not have
//! applied the first two. `edit` builds one [`Document`] in memory and applies every operation
//! to it in order, stopping at the first failure; nothing is written anywhere in this process
//! (there is nothing to roll back), and the response is the reasons it failed rather than a
//! half-applied document. Once every operation has applied cleanly, [`Document::check`] runs
//! SERVICE §7 stage 1 over the whole result — needed because [`Document::set_prop`] writes an
//! expression string without parsing it, so a batch that only breaks EXPR §10's static analysis
//! would otherwise sail through every per-operation check and land on disk anyway.
//!
//! # Minting an id
//!
//! [`Operation::AddBlock`] takes `id` as *optional*. `Document::add_block` itself always
//! requires one — this handler mints it when the caller omits it, following the same pattern
//! `eio-cli`'s `add_block` and `eio-daemon`'s `node::mint_id` both use: bytes from
//! [`getrandom::fill`], fed to `Document::mint_id`, which is this crate's only source of
//! randomness for an id (SERVICE §2's rule that a service file's contents must not depend on
//! which binary minted it). A minted id is returned in [`Out::created`], keyed by that
//! operation's index in the request.
//!
//! **A minted id cannot be referenced by a later operation in the same batch.** A drag that
//! both adds a block and wires it up in one gesture needs the new id before the request is
//! built, and this handler does not invent forward references to solve that — see this
//! module's tests and the plan's own report for why that was left out rather than designed in.
//! A caller wiring up a block it just added supplies `id` itself on `add_block`; SERVICE §2.1
//! and `Document::add_block`'s own `BadId`/`DuplicateInstance` checks are exactly what keeps
//! that safe to do.

use std::collections::BTreeMap;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use eio_service::Error;
use eio_service::edit::{Document, EditError};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;

/// `POST /api/service-edit`'s body (DESIGNER-SPEC §3.2).
#[derive(Debug, Deserialize)]
pub struct In {
    /// The service file's current text, exactly as a `GET` through the proxy returned it.
    pub toml: String,
    /// What to do to it, applied in order and all-or-nothing.
    pub operations: Vec<Operation>,
}

/// One `Document` mutation (SERVICE §9), named and shaped after the method it calls.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    /// [`Document::add_block`]. `id` is minted when omitted — see the module doc.
    AddBlock {
        /// The instance id. Minted when absent.
        #[serde(default)]
        id: Option<String>,
        /// A label for people. `Document::add_block`'s own `name`.
        #[serde(default)]
        name: Option<String>,
        /// The block registry reference.
        block: String,
    },
    /// [`Document::remove_block`].
    RemoveBlock {
        /// The instance to remove, and every connection naming it.
        id: String,
    },
    /// [`Document::set_prop`].
    SetProp {
        /// The instance.
        id: String,
        /// The property.
        property: String,
        /// The expression to set it to. Not parsed here — see the module doc.
        expression: String,
    },
    /// [`Document::remove_prop`].
    RemoveProp {
        /// The instance.
        id: String,
        /// The property to unset.
        property: String,
    },
    /// [`Document::set_name`]. Adds the key if the instance has none (SERVICE §9); touches
    /// nothing but that line — the id, connections, properties and `[ui]` all survive.
    SetName {
        /// The instance.
        id: String,
        /// The new label.
        name: String,
    },
    /// [`Document::remove_name`]. Removes the key rather than writing an empty string
    /// (SERVICE §9): absent and empty are not the same thing to a reader.
    RemoveName {
        /// The instance.
        id: String,
    },
    /// [`Document::connect`].
    Connect {
        /// `<id>.<port>`.
        from: String,
        /// `<id>.<port>`.
        to: String,
    },
    /// [`Document::disconnect`].
    Disconnect {
        /// `<id>.<port>`.
        from: String,
        /// `<id>.<port>`.
        to: String,
    },
    /// [`Document::set_autostart`]. Infallible in `Document`, and here.
    SetAutostart {
        /// Whether the service starts at boot.
        autostart: bool,
    },
    /// [`Document::set_ui`]. `value` is TOML source, not a JSON value — see that method's own
    /// doc for why: `[ui]` has no schema here, and converting a JSON value into one would be
    /// this handler inventing an encoding `eio-service` was deliberately not given.
    SetUi {
        /// Bare-key path under `[ui]`, e.g. `["f3m9", "x"]`.
        path: Vec<String>,
        /// A TOML value, as source text: `"148"`, `"\"a note\""`, `"{ x = 1, y = 2 }"`.
        value: String,
    },
    /// [`Document::remove_ui`].
    RemoveUi {
        /// The path to clear.
        path: Vec<String>,
    },
}

/// The success response: the edited file, and any ids this handler minted along the way.
#[derive(Debug, Serialize)]
pub struct Out {
    /// The file, as `Document::render` produced it. What the caller `PUT`s next is this, byte
    /// for byte (DESIGNER §3.2).
    pub toml: String,
    /// `add_block` operations that omitted `id`, keyed by their index in the request's
    /// `operations` array, valued with the id this handler minted for them.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub created: BTreeMap<String, String>,
}

/// One entry of a 422 response's `errors` array (DESIGNER-SPEC §3.2).
///
/// `message` is one sentence for a person and MUST NOT be parsed, matching
/// `eio-daemon`'s own `ApiError` convention. Everything else is structure the SPA maps onto an
/// editor position (the plan's own words) — present only when the failure actually carries it,
/// which is why every field past `message` is optional.
#[derive(Debug, Serialize)]
pub struct ErrorOut {
    /// What went wrong, for a person.
    pub message: String,
    /// The index into the request's `operations` array this failure came from, when it can be
    /// attributed to one operation rather than to the document as a whole.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<usize>,
    /// The block instance this names, when it names one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// The property this names, when it names one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub property: Option<String>,
    /// EXPR §8's code, for a property-expression error (`eio_service::Error::Property`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<&'static str>,
    /// Byte span into the property's own expression text (EXPR §8), for a property-expression
    /// error — the same span shape the SPA's own keystroke-time `expr` linting already uses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<SpanOut>,
}

/// A byte range, half-open (EXPR §8).
#[derive(Debug, Serialize)]
pub struct SpanOut {
    /// First byte of the span.
    pub start: u32,
    /// One past the last byte of the span.
    pub end: u32,
}

/// What this handler answers a failure with: DESIGNER §3.2's `422 { errors }` for anything the
/// request itself got wrong, or this crate's own `ApiError` envelope for the one failure that
/// is not the caller's fault (this host has no randomness to mint an id from).
#[derive(Debug)]
pub enum Failure {
    /// SERVICE §9: the edit did not apply, or the result would not be a valid service file.
    /// Nothing was written anywhere — there was nothing to roll back.
    Invalid(Vec<ErrorOut>),
    /// This process could not mint an id. Not the caller's mistake, so not a 422: the same
    /// request would succeed on a host with working randomness.
    Internal(ApiError),
}

impl From<ApiError> for Failure {
    fn from(error: ApiError) -> Failure {
        Failure::Internal(error)
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        match self {
            Failure::Invalid(errors) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorsBody { errors }),
            )
                .into_response(),
            Failure::Internal(error) => error.into_response(),
        }
    }
}

/// The `422` envelope's shape (DESIGNER-SPEC §3.2): `{ errors }`, nothing else.
#[derive(Debug, Serialize)]
struct ErrorsBody {
    errors: Vec<ErrorOut>,
}

/// `POST /api/service-edit`.
pub async fn edit(Json(body): Json<In>) -> Result<Json<Out>, Failure> {
    let mut document =
        Document::parse(&body.toml).map_err(|errors| service_errors(errors, None))?;

    let mut created = BTreeMap::new();
    for (index, operation) in body.operations.iter().enumerate() {
        apply(&mut document, operation, index, &mut created)?;
    }

    // SERVICE §9's own rule, run unconditionally: `set_prop` above wrote an expression string
    // without parsing it, so this is the one place a batch that only broke EXPR §10's static
    // analysis gets caught, rather than rendering a file `check` would refuse.
    document
        .check()
        .map_err(|errors| service_errors(errors, Some(&body.operations)))?;

    Ok(Json(Out {
        toml: document.render(),
        created,
    }))
}

/// Applies one operation to `document`, minting an id for `AddBlock` when it omitted one.
fn apply(
    document: &mut Document,
    operation: &Operation,
    index: usize,
    created: &mut BTreeMap<String, String>,
) -> Result<(), Failure> {
    match operation {
        Operation::AddBlock { id, name, block } => {
            let id = match id {
                Some(id) => id.clone(),
                None => {
                    let minted = mint_id(document, index)?;
                    created.insert(index.to_string(), minted.clone());
                    minted
                }
            };
            document
                .add_block(&id, name.as_deref(), block)
                .map_err(|error| edit_error(index, error))
        }
        Operation::RemoveBlock { id } => document
            .remove_block(id)
            .map(|_removed| ())
            .map_err(|error| edit_error(index, error)),
        Operation::SetProp {
            id,
            property,
            expression,
        } => document
            .set_prop(id, property, expression)
            .map_err(|error| edit_error(index, error)),
        Operation::RemoveProp { id, property } => document
            .remove_prop(id, property)
            .map_err(|error| edit_error(index, error)),
        Operation::SetName { id, name } => document
            .set_name(id, name)
            .map_err(|error| edit_error(index, error)),
        Operation::RemoveName { id } => document
            .remove_name(id)
            .map_err(|error| edit_error(index, error)),
        Operation::Connect { from, to } => document
            .connect(from, to)
            .map_err(|error| edit_error(index, error)),
        Operation::Disconnect { from, to } => document
            .disconnect(from, to)
            .map_err(|error| edit_error(index, error)),
        Operation::SetAutostart { autostart } => {
            document.set_autostart(*autostart);
            Ok(())
        }
        Operation::SetUi { path, value } => {
            let path: Vec<&str> = path.iter().map(String::as_str).collect();
            document
                .set_ui(&path, value)
                .map_err(|error| edit_error(index, error))
        }
        Operation::RemoveUi { path } => {
            let path: Vec<&str> = path.iter().map(String::as_str).collect();
            document
                .remove_ui(&path)
                .map_err(|error| edit_error(index, error))
        }
    }
}

/// How many random bytes to mint an id from. Same width `eio-cli`'s `add_block` draws
/// (enough for sixteen attempts at `Document::mint_id`'s four-character alphabet).
const MINT_RANDOM_BYTES: usize = 64;

/// Mints an id for the `add_block` operation at `index`, following `eio-cli`'s own pattern:
/// randomness from [`getrandom::fill`], handed to `Document::mint_id`, which is this crate's
/// only source of one (see the module doc).
fn mint_id(document: &Document, index: usize) -> Result<String, Failure> {
    let mut random = [0_u8; MINT_RANDOM_BYTES];
    getrandom::fill(&mut random).map_err(|error| {
        ApiError::internal(format!("no randomness to mint an id from: {error}"))
    })?;
    document.mint_id(&random).ok_or_else(|| {
        Failure::Invalid(vec![ErrorOut {
            message: String::from("could not mint an unused instance id; supply one explicitly"),
            operation: Some(index),
            instance: None,
            property: None,
            code: None,
            span: None,
        }])
    })
}

/// One [`EditError`], as the operation at `index` failed it.
fn edit_error(index: usize, error: EditError) -> Failure {
    let message = error.to_string();
    let (instance, property) = match &error {
        EditError::BadId { id }
        | EditError::DuplicateInstance { id }
        | EditError::NoSuchInstance { id } => (Some(id.clone()), None),
        EditError::NoSuchProperty { id, property } => (Some(id.clone()), Some(property.clone())),
        EditError::BadName { name } => (None, Some(name.clone())),
        _ => (None, None),
    };
    Failure::Invalid(vec![ErrorOut {
        message,
        operation: Some(index),
        instance,
        property,
        code: None,
        span: None,
    }])
}

/// Every stage-1 [`Error`] from a failed [`Document::parse`] or [`Document::check`].
///
/// `operations` is `None` for the initial parse (nothing has applied yet, so nothing to
/// attribute a pre-existing error to) and `Some` for the final check, where an
/// `Error::Property` can be traced back to the `set_prop` operation that wrote it — see the
/// module doc for why that is the only stage-1 violation a per-operation check does not
/// already catch.
fn service_errors(errors: Vec<Error>, operations: Option<&[Operation]>) -> Failure {
    Failure::Invalid(
        errors
            .into_iter()
            .map(|error| service_error(error, operations))
            .collect(),
    )
}

fn service_error(error: Error, operations: Option<&[Operation]>) -> ErrorOut {
    let message = error.to_string();
    match error {
        Error::InstanceId { id } | Error::EmptyBlockRef { id } => ErrorOut {
            message,
            operation: None,
            instance: Some(id),
            property: None,
            code: None,
            span: None,
        },
        Error::DanglingConnection { instance, .. } => ErrorOut {
            message,
            operation: None,
            instance: Some(instance),
            property: None,
            code: None,
            span: None,
        },
        Error::Property {
            id,
            property,
            code,
            span,
            ..
        } => {
            let operation = operations.and_then(|operations| {
                operations
                    .iter()
                    .enumerate()
                    .rev()
                    .find_map(|(index, op)| match op {
                        Operation::SetProp {
                            id: op_id,
                            property: op_property,
                            ..
                        } if *op_id == id && *op_property == property => Some(index),
                        _ => None,
                    })
            });
            ErrorOut {
                message,
                operation,
                instance: Some(id),
                property: Some(property),
                code: Some(code.as_str()),
                span: Some(SpanOut {
                    start: span.start,
                    end: span.end,
                }),
            }
        }
        _ => ErrorOut {
            message,
            operation: None,
            instance: None,
            property: None,
            code: None,
            span: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SERVICE §9's own example, extended with the trivia a round trip must survive: a
    /// comment, a blank line, an oddly aligned key and an inline array-shaped `[ui]` value.
    const FIXTURE: &str = "\
# the kitchen
name = \"kitchen\"

connections = [ \"b7k2.out -> f3m9.in\" ]

[blocks.b7k2]
name    = \"Thermometer\"
block = \"temp-sensor:1.0.0\"

[blocks.f3m9]
block = \"filter:1.2.0\"

[blocks.f3m9.props]
threshold = \"18\"

[ui]
b7k2 = { x = 10, y = 20 }
";

    fn ops(json: serde_json::Value) -> Vec<Operation> {
        serde_json::from_value(json).expect("valid operations JSON")
    }

    async fn run(toml: &str, operations: Vec<Operation>) -> Result<Json<Out>, Failure> {
        edit(Json(In {
            toml: String::from(toml),
            operations,
        }))
        .await
    }

    #[tokio::test]
    async fn a_single_edit_changes_only_what_it_touched() {
        // SERVICE §9's hard requirement: the diff of before and after shows the edit and
        // nothing else. Proven directly on the two texts' lines, not just "it still parses".
        let out = run(
            FIXTURE,
            ops(serde_json::json!([
                { "op": "set_prop", "id": "f3m9", "property": "threshold", "expression": "21" },
            ])),
        )
        .await
        .expect("a valid edit succeeds")
        .0;

        let before: Vec<&str> = FIXTURE.lines().collect();
        let after: Vec<&str> = out.toml.lines().collect();
        let mut changed = Vec::new();
        for (index, line) in after.iter().copied().enumerate() {
            if before.get(index).copied() != Some(line) {
                changed.push((index, before.get(index).copied(), line));
            }
        }
        assert_eq!(before.len(), after.len(), "no line was added or removed");
        assert_eq!(
            changed,
            vec![(13, Some("threshold = \"18\""), "threshold = \"21\"")],
            "exactly one line changed, and it is the edited property"
        );
        // Comments, blank lines and the odd alignment on `name    = "Thermometer"` survive.
        assert!(out.toml.contains("# the kitchen\n"));
        assert!(out.toml.contains("name    = \"Thermometer\""));
        assert!(out.toml.contains("[ui]\nb7k2 = { x = 10, y = 20 }"));
    }

    #[tokio::test]
    async fn a_rename_through_the_endpoint_touches_only_its_name_line() {
        // SERVICE §9's new bullet, exercised through the endpoint rather than `Document`
        // directly: a rename's diff is the name line and nothing else — the id, connections,
        // properties and `[ui]` all survive.
        let out = run(
            FIXTURE,
            ops(serde_json::json!([
                { "op": "set_name", "id": "b7k2", "name": "Kitchen Thermometer" },
            ])),
        )
        .await
        .expect("a valid edit succeeds")
        .0;

        let before: Vec<&str> = FIXTURE.lines().collect();
        let after: Vec<&str> = out.toml.lines().collect();
        let mut changed = Vec::new();
        for (index, line) in after.iter().copied().enumerate() {
            if before.get(index).copied() != Some(line) {
                changed.push((index, before.get(index).copied(), line));
            }
        }
        assert_eq!(before.len(), after.len(), "no line was added or removed");
        assert_eq!(
            changed,
            vec![(
                6,
                Some("name    = \"Thermometer\""),
                "name    = \"Kitchen Thermometer\""
            )],
            "exactly one line changed, and it is the renamed block's name"
        );
        assert!(out.toml.contains("[blocks.b7k2]"), "the id is untouched");
        assert!(
            out.toml.contains("b7k2.out -> f3m9.in"),
            "connections naming it are untouched"
        );
        assert!(
            out.toml.contains("[blocks.f3m9.props]\nthreshold = \"18\""),
            "properties are untouched"
        );
        assert!(
            out.toml.contains("b7k2 = { x = 10, y = 20 }"),
            "[ui] is untouched"
        );
    }

    #[tokio::test]
    async fn removing_a_name_removes_the_key_not_just_its_text() {
        let out = run(
            FIXTURE,
            ops(serde_json::json!([
                { "op": "remove_name", "id": "b7k2" },
            ])),
        )
        .await
        .expect("a valid edit succeeds")
        .0;

        assert!(
            !out.toml.contains("Thermometer"),
            "the label is gone, not emptied: {}",
            out.toml
        );
        assert_eq!(
            out.toml.matches("name").count(),
            1,
            "only the service's own `name` key remains, not an emptied `blocks.b7k2.name`: {}",
            out.toml
        );
        let parsed = eio_service::parse(&out.toml).expect("still valid");
        assert_eq!(parsed.service.blocks["b7k2"].name, None);
    }

    #[tokio::test]
    async fn a_batch_with_a_rename_is_still_all_or_nothing() {
        // SERVICE §9 and DESIGNER §3.2: a batch that fails partway must not have applied any
        // of it, a rename included.
        let result = run(
            FIXTURE,
            ops(serde_json::json!([
                { "op": "set_name", "id": "b7k2", "name": "Kitchen Thermometer" },
                { "op": "connect", "from": "nope.out", "to": "f3m9.in" },
            ])),
        )
        .await;

        match result {
            Err(Failure::Invalid(errors)) => {
                assert_eq!(errors.len(), 1);
                assert_eq!(errors[0].operation, Some(1));
            }
            other => panic!("expected a 422 naming operation 1, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_batch_with_a_failing_rename_applies_nothing_after_it() {
        // The sharper half of "all-or-nothing": if `set_name`'s own error were ever swallowed
        // instead of stopping the batch, the `connect` below would go on to apply and this
        // request would come back as a success it should not be.
        let result = run(
            FIXTURE,
            ops(serde_json::json!([
                { "op": "set_name", "id": "nope", "name": "x" },
                { "op": "connect", "from": "f3m9.out", "to": "b7k2.in" },
            ])),
        )
        .await;

        match result {
            Err(Failure::Invalid(errors)) => {
                assert_eq!(errors.len(), 1);
                assert_eq!(errors[0].operation, Some(0));
                assert_eq!(errors[0].instance.as_deref(), Some("nope"));
            }
            other => panic!("expected a 422 naming operation 0, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_batch_failing_partway_leaves_the_document_moot() {
        // "All-or-nothing" here means: this handler never returns a partially-applied
        // document. It has nothing on disk to leave unchanged (DESIGNER §3.2 is stateless),
        // so the property to prove is narrower and stronger: a failing batch's *only* visible
        // effect is the error, never a `toml` field with the first operation already in it.
        let result = run(
            FIXTURE,
            ops(serde_json::json!([
                { "op": "set_prop", "id": "f3m9", "property": "threshold", "expression": "21" },
                { "op": "connect", "from": "nope.out", "to": "f3m9.in" },
            ])),
        )
        .await;

        match result {
            Err(Failure::Invalid(errors)) => {
                assert_eq!(errors.len(), 1);
                assert_eq!(errors[0].operation, Some(1));
                assert_eq!(errors[0].instance.as_deref(), Some("nope"));
            }
            other => panic!("expected a 422 naming operation 1, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ui_survives_removing_a_block_of_the_same_name() {
        // SERVICE §6, §9: removing `b7k2` must not touch `[ui].b7k2`, even though the key
        // looks like it belongs to the block that is gone.
        let out = run(
            FIXTURE,
            ops(serde_json::json!([
                { "op": "remove_block", "id": "b7k2" },
            ])),
        )
        .await
        .expect("removing a block succeeds")
        .0;

        assert!(
            out.toml.contains("b7k2 = { x = 10, y = 20 }"),
            "the stale [ui] annotation is inert, not tidied: {}",
            out.toml
        );
    }

    #[tokio::test]
    async fn removing_a_block_removes_the_connection_naming_it() {
        // SERVICE §7's dangling-connection error, avoided by cascading rather than by
        // leaving a file that will not load.
        let out = run(
            FIXTURE,
            ops(serde_json::json!([
                { "op": "remove_block", "id": "b7k2" },
            ])),
        )
        .await
        .expect("removing a block succeeds")
        .0;

        assert!(
            !out.toml.contains("b7k2.out -> f3m9.in"),
            "the connection naming the removed block must be gone: {}",
            out.toml
        );
    }

    #[tokio::test]
    async fn an_edit_that_would_make_the_file_invalid_fails_and_changes_nothing() {
        // `set_prop` does not itself parse the expression; the final `check()` is what must
        // catch this, per the module doc.
        let result = run(
            FIXTURE,
            ops(serde_json::json!([
                { "op": "set_prop", "id": "f3m9", "property": "threshold", "expression": "(+ 1" },
            ])),
        )
        .await;

        match result {
            Err(Failure::Invalid(errors)) => {
                assert_eq!(errors.len(), 1);
                assert_eq!(errors[0].code, Some("PARSE"));
                assert_eq!(
                    errors[0].operation,
                    Some(0),
                    "traced back to the set_prop that wrote it"
                );
                assert!(errors[0].span.is_some());
            }
            other => panic!("expected a 422 with a PARSE code, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn add_block_mints_an_id_when_none_is_given() {
        let out = run(
            FIXTURE,
            ops(serde_json::json!([
                { "op": "add_block", "block": "rolling-average:1.0.0" },
            ])),
        )
        .await
        .expect("adding a block succeeds")
        .0;

        assert_eq!(out.created.len(), 1);
        let id = out.created.get("0").expect("operation 0 minted an id");
        assert!(out.toml.contains(&format!("[blocks.{id}]")));
    }

    #[tokio::test]
    async fn an_invalid_input_document_is_reported_without_applying_anything() {
        let result = run("not = [valid", ops(serde_json::json!([]))).await;
        assert!(matches!(result, Err(Failure::Invalid(_))));
    }
}

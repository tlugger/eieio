//! `POST /api/service-parse` (DESIGNER-SPEC §3.2, amended for eieio-m9s.37): reading a service
//! file is the same stateless transform as [`super::service_edit`], in the other direction.
//!
//! `GET /services/{s}` answers a service file's **text**, verbatim (DAEMON §9 is deliberate
//! about that: SERVICE §2 makes the daemon a reader, and a definition that came back
//! reformatted would make every round trip through that API a diff). A canvas cannot draw a
//! block graph from bytes, and nothing in the browser parses TOML — so the parse belongs here,
//! next to the edit it mirrors: text in, structure out, no service identity, no node reached,
//! nothing stored.
//!
//! # Why this is a thin wrapper and stays one
//!
//! Exactly [`super::service_edit`]'s own reason: `eio-service` is this repository's one
//! implementation of the format (SERVICE §9's one-editor rule), so this handler is a direct call
//! to [`eio_service::parse`] and a reshaping of what it returns into JSON. Nothing here parses
//! TOML syntax, resolves an id, checks a connection grammar, or runs EXPR §10's static analysis
//! a second time — [`eio_service::parse`] already did every one of those, and [`Out`] carries its
//! answer, not a rederivation of it.
//!
//! # `[ui]` is reshaped, never interpreted
//!
//! [`eio_service::schema::Service`] holds `[ui]` as an opaque `toml::Value` (SERVICE §6: it has
//! no schema there and never will). [`Out::ui`] is that same value, converted to JSON member for
//! member and nothing more — not the `{x, y, zoom}` shape DESIGNER §4.1 gives the *write* path,
//! which is this shell's own convention for a value it is about to send back through
//! `Document::set_ui`, not a fact about what `[ui]` universally contains. Interpreting known
//! keys for the canvas to draw with is `designer/src/lib/service/toml-values.ts`'s job on the
//! frontend, the same file that already owns writing them; duplicating that convention here
//! would be two implementations of DESIGNER §4.1 finding new ways to disagree.
//!
//! **This is a read, and only a read.** DESIGNER §3.2's amendment is explicit that the parsed
//! view this endpoint answers is derived fresh on every request and is never a second source of
//! truth: a canvas that drew from [`Out`] and then wrote a `[ui]` edit back *from* it, rather
//! than from the raw fragment text `/api/service-edit`'s `set_ui` operation always took, would
//! be exactly the second path that amendment forbids. Nothing in this module writes anything.
//!
//! # Errors are [`super::service_edit`]'s, reused rather than restated
//!
//! A file that fails to parse is the ordinary case, not a bug to swallow — an operator can
//! hand-edit a service file into an invalid state, and SERVICE §7's stage-1 classes exist so a
//! caller can tell them apart without matching on a message. [`eio_service::parse`] returns
//! exactly the `Vec<eio_service::Error>` [`super::service_edit::edit`]'s own initial
//! `Document::parse` call already turns into [`super::service_edit::ErrorOut`] via
//! [`super::service_edit::service_errors`] — the identical 422 `{ errors }` shape, because both
//! endpoints are answering the same question ("does this text parse?") from opposite directions.
//! Calling that function rather than a second copy of it is what keeps the two endpoints from
//! drifting on what "distinguishable" (this crate's own bar, not SERVICE-SPEC's) means in
//! practice.

use std::collections::BTreeMap;

use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::service_edit::{ErrorsBody, Failure, service_errors};

/// `POST /api/service-parse`'s body.
#[derive(Debug, Deserialize, ToSchema)]
pub struct In {
    /// The service file's text, exactly as a `GET` through the proxy returned it.
    pub toml: String,
}

/// The success response: SERVICE §7 stage 1's own value tree, reshaped for JSON.
///
/// Every field here is [`eio_service::parse::Parsed`]'s, renamed or reshaped only where JSON
/// needs a different spelling than the Rust value does (`overflow` as one of
/// [`eio_service::Overflow::ACCEPTED`]'s strings; a connection's four parts flattened instead of
/// nested `Terminal`s). Nothing here is computed from anything [`eio_service::parse`] did not
/// already produce.
#[derive(Debug, Serialize, ToSchema)]
pub struct Out {
    /// The service's name (SERVICE §3).
    pub name: String,
    /// Whether the daemon starts this service at boot (DAEMON §3).
    pub autostart: bool,
    /// One of [`eio_service::Overflow::ACCEPTED`]'s own spellings — never a third state for "the
    /// key was absent", which [`eio_service::parse`] already resolved to `"backpressure"`
    /// (SERVICE §5).
    pub overflow: &'static str,
    /// Block instances, keyed by id (SERVICE §2) — the same key [`BlockOut::id`] repeats, for a
    /// caller that destructures one entry at a time and would otherwise have to carry the map
    /// key alongside it by hand.
    pub blocks: BTreeMap<String, BlockOut>,
    /// The wiring, parsed (SERVICE §5), in file order.
    pub connections: Vec<ConnectionOut>,
    /// `[ui]`, reshaped to JSON member for member and never interpreted — see the module doc.
    /// Absent when the file has no `[ui]` table at all, which is not the same thing as an empty
    /// one (SERVICE §6 makes both legal, and a canvas may care which).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui: Option<serde_json::Value>,
}

/// One block instance (SERVICE §4), `id` folded in from the map key it was found under.
#[derive(Debug, Serialize, ToSchema)]
pub struct BlockOut {
    /// The instance id — SERVICE §2's identity, repeated from [`Out::blocks`]'s own key.
    pub id: String,
    /// A label for people. Meaningless to a host (SERVICE §2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The block registry reference.
    pub block: String,
    /// Property expressions by name, unparsed and unevaluated — every value here already passed
    /// EXPR §10's static analysis as part of [`eio_service::parse`], but the expression *text*
    /// is what a config modal edits, not its meaning.
    pub props: BTreeMap<String, String>,
}

/// One edge (SERVICE §5), flattened from [`eio_service::Connection`]'s two [`eio_service::Terminal`]s.
#[derive(Debug, Serialize, ToSchema)]
pub struct ConnectionOut {
    /// The source instance.
    pub from_id: String,
    /// The source's output port.
    pub from_port: String,
    /// The destination instance.
    pub to_id: String,
    /// The destination's input port.
    pub to_port: String,
}

/// Reads a service file's text into the structure a canvas draws — see the module doc.
#[utoipa::path(
    post,
    path = "/api/service-parse",
    tag = "service-parse",
    request_body = In,
    responses(
        (status = 200, description = "The file parsed and passed SERVICE §7 stage 1", body = Out),
        (status = 422, description = "The file is not TOML, not a service, or fails a stage-1 rule", body = ErrorsBody),
    ),
)]
pub async fn parse(Json(body): Json<In>) -> Result<Json<Out>, Failure> {
    let parsed = eio_service::parse(&body.toml).map_err(|errors| service_errors(errors, None))?;

    let blocks = parsed
        .service
        .blocks
        .into_iter()
        .map(|(id, instance)| {
            let out = BlockOut {
                id: id.clone(),
                name: instance.name,
                block: instance.block,
                props: instance.props,
            };
            (id, out)
        })
        .collect();

    let connections = parsed
        .connections
        .into_iter()
        .map(|connection| ConnectionOut {
            from_id: connection.from.instance,
            from_port: connection.from.port,
            to_id: connection.to.instance,
            to_port: connection.to.port,
        })
        .collect();

    // `toml::Value: Serialize` for any serializer, `[ui]`'s own included — this is a reshaping
    // of a value `eio_service::parse` already produced, not a second reading of the file
    // (`eio-designer` never spells `toml::Value` itself; the type is inferred from
    // `parsed.service.ui`'s own field, matching the module doc's "reshaped, never interpreted").
    let ui = parsed.service.ui.map(|value| {
        serde_json::to_value(value)
            .expect("a toml::Value's Serialize impl cannot fail into serde_json::Value")
    });

    Ok(Json(Out {
        name: parsed.service.name,
        autostart: parsed.service.autostart,
        overflow: overflow_str(parsed.overflow),
        blocks,
        connections,
        ui,
    }))
}

/// One of [`eio_service::Overflow::ACCEPTED`]'s own spellings, for the variant
/// [`eio_service::parse`] resolved the file's `overflow` key to.
///
/// Not [`eio_service::Overflow`]'s own `Debug`/some rendering of it: that enum carries no
/// `Serialize` (SERVICE §5's own module doc: the raw string is stage 1's to validate, not
/// serde's to reject), so this is the one place this crate turns the resolved variant back into
/// the spelling it came from — the exact reverse of what [`eio_service::Overflow::parse`] does,
/// checked against the same constant rather than hand-typed again.
fn overflow_str(overflow: eio_service::Overflow) -> &'static str {
    match overflow {
        eio_service::Overflow::Backpressure => eio_service::Overflow::ACCEPTED[0],
        eio_service::Overflow::DropOldest => eio_service::Overflow::ACCEPTED[1],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same fixture [`super::super::service_edit::tests`] uses, so a reader who already
    /// knows it does not have to learn a second one: a comment, a blank line, an oddly aligned
    /// key, and an inline array-shaped `[ui]` value.
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

    async fn run(toml: &str) -> Result<Json<Out>, Failure> {
        parse(Json(In {
            toml: String::from(toml),
        }))
        .await
    }

    #[tokio::test]
    async fn a_well_formed_file_parses_to_the_expected_structure() {
        let out = run(FIXTURE)
            .await
            .expect("the fixture is a valid service")
            .0;

        assert_eq!(out.name, "kitchen");
        assert!(!out.autostart);
        assert_eq!(out.overflow, "backpressure");

        assert_eq!(out.blocks.len(), 2);
        let b7k2 = &out.blocks["b7k2"];
        assert_eq!(b7k2.id, "b7k2");
        assert_eq!(b7k2.name.as_deref(), Some("Thermometer"));
        assert_eq!(b7k2.block, "temp-sensor:1.0.0");
        let f3m9 = &out.blocks["f3m9"];
        assert_eq!(f3m9.name, None, "a block with no name key carries none");
        assert_eq!(f3m9.props["threshold"], "18");

        assert_eq!(out.connections.len(), 1);
        assert_eq!(out.connections[0].from_id, "b7k2");
        assert_eq!(out.connections[0].from_port, "out");
        assert_eq!(out.connections[0].to_id, "f3m9");
        assert_eq!(out.connections[0].to_port, "in");
    }

    #[tokio::test]
    async fn ui_survives_into_the_structure_without_being_interpreted() {
        let out = run(FIXTURE)
            .await
            .expect("the fixture is a valid service")
            .0;

        // Reshaped to JSON, not reduced to `{x, y}`: a hand-written third member would still be
        // here, because nothing in this handler looks for `x`/`y`/`zoom` specifically.
        let ui = out.ui.expect("the fixture declares [ui]");
        assert_eq!(ui["b7k2"]["x"], serde_json::json!(10));
        assert_eq!(ui["b7k2"]["y"], serde_json::json!(20));
    }

    #[tokio::test]
    async fn a_file_with_no_ui_table_answers_no_ui_field_at_all() {
        let out = run("name = \"empty\"\n")
            .await
            .expect("valid, minimal service")
            .0;
        assert!(out.ui.is_none(), "absent, not an empty object");
    }

    #[tokio::test]
    async fn an_unknown_top_level_key_is_refused_the_way_service_5_requires() {
        // `Service` is `#[serde(deny_unknown_fields)]` (SERVICE §3): a typo'd top-level key
        // must be refused, not silently ignored.
        let result = run("name = \"kitchen\"\nautostrat = true\n").await;
        match result {
            Err(Failure::Invalid(errors)) => {
                assert_eq!(errors.len(), 1);
                assert!(
                    errors[0].message.contains("autostrat")
                        || errors[0].message.contains("unknown"),
                    "the message names what was wrong, not a generic refusal: {}",
                    errors[0].message
                );
            }
            other => panic!("expected a 422, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn text_that_is_not_toml_at_all_is_distinguishable_from_toml_that_is_not_a_service() {
        // Two different ways to fail stage 1, both refused, and — SERVICE §7's own bar — a
        // caller can tell them apart from the message rather than getting one generic
        // "invalid file" for both. (`eio_service::error::Error::Toml`'s own doc: TOML syntax
        // and an unknown/missing field share one *variant*, because both are `toml::de::Error`,
        // but they do not share a *message* — see that type's module doc for why splitting the
        // variant would mean matching on prose instead.)
        let not_toml = run("this is not { toml at all [[[").await;
        let toml_not_a_service = run("[completely]\nunrelated = \"table\"\n").await;

        let message_of = |result: Result<Json<Out>, Failure>| match result {
            Err(Failure::Invalid(errors)) => errors[0].message.clone(),
            other => panic!("expected a 422, got {other:?}"),
        };
        let not_toml_message = message_of(not_toml);
        let toml_not_a_service_message = message_of(toml_not_a_service);
        assert_ne!(
            not_toml_message, toml_not_a_service_message,
            "two different failures must not collapse into one indistinguishable message"
        );
    }

    #[tokio::test]
    async fn a_stage_1_property_failure_is_refused_with_expr_8s_own_code() {
        let result = run(
            "name = \"kitchen\"\n\n[blocks.b7k2]\nblock = \"temp-sensor:1.0.0\"\n\n[blocks.b7k2.props]\nthreshold = \"(+ 1\"\n",
        )
        .await;
        match result {
            Err(Failure::Invalid(errors)) => {
                assert_eq!(errors.len(), 1);
                assert_eq!(errors[0].code, Some("PARSE"));
                assert_eq!(errors[0].instance.as_deref(), Some("b7k2"));
                assert_eq!(errors[0].property.as_deref(), Some("threshold"));
                assert!(errors[0].span.is_some());
            }
            other => panic!("expected a 422 with a PARSE code, got {other:?}"),
        }
    }
}
